//! JSON-RPC 2.0 framing: one JSON object per line, at most [`MAX_MESSAGE_BYTES`] per line.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::MAX_MESSAGE_BYTES;
use crate::error::RpcError;

pub(crate) const JSONRPC: &str = "2.0";

/// A server → client notification (a JSON-RPC request without an id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Notification {
    /// Decode the params into a typed payload, e.g. [`nugget_core::ipc::ChatDelta`].
    pub fn params_as<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        T::deserialize(&self.params)
    }
}

pub(crate) fn line_reader<R: AsyncRead>(r: R) -> FramedRead<R, LinesCodec> {
    FramedRead::new(r, LinesCodec::new_with_max_length(MAX_MESSAGE_BYTES))
}

/// Serialize `msg` into one line (without the trailing newline), enforcing the size cap.
pub(crate) fn encode<T: Serialize>(msg: &T) -> Result<String, EncodeError> {
    let line = serde_json::to_string(msg).map_err(EncodeError::Serde)?;
    if line.len() > MAX_MESSAGE_BYTES {
        return Err(EncodeError::TooLarge(line.len()));
    }
    Ok(line)
}

#[derive(Debug)]
pub(crate) enum EncodeError {
    Serde(serde_json::Error),
    TooLarge(usize),
}

/// Coalesce up to this many bytes of queued lines into one write.
const WRITE_BATCH: usize = 64 * 1024;

/// Write `first` plus every line already queued in `rx` (up to [`WRITE_BATCH`] bytes) with one
/// `write_all` and one flush, reusing `buf`. Under load this turns one syscall per message into
/// one per batch; an idle writer still sends each line as soon as it is queued.
pub(crate) async fn write_batch<W: AsyncWrite + Unpin>(
    w: &mut W,
    buf: &mut Vec<u8>,
    first: String,
    rx: &mut mpsc::Receiver<String>,
) -> std::io::Result<()> {
    buf.clear();
    buf.extend_from_slice(first.as_bytes());
    buf.push(b'\n');
    while buf.len() < WRITE_BATCH {
        let Ok(line) = rx.try_recv() else { break };
        buf.extend_from_slice(line.as_bytes());
        buf.push(b'\n');
    }
    let res = async {
        w.write_all(buf).await?;
        w.flush().await
    }
    .await;
    // Do not keep a buffer sized for one huge message alive for the connection's lifetime.
    if buf.capacity() > 4 * WRITE_BATCH {
        *buf = Vec::new();
    }
    res
}

/// An outgoing request or notification. `params` is serialized straight into the line (no
/// intermediate `Value`), and first, so [`params_shape_ok`] can check it without reparsing.
#[derive(Serialize)]
pub(crate) struct OutRequest<'a, P: Serialize + ?Sized> {
    pub params: &'a P,
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub method: &'a str,
}

/// True when the `params` of an [`OutRequest`] line is an object, an array or null, as
/// JSON-RPC requires. Relies on `params` being the first field and on compact encoding.
pub(crate) fn params_shape_ok(line: &str) -> bool {
    const PREFIX: &str = "{\"params\":";
    line.strip_prefix(PREFIX)
        .is_some_and(|rest| rest.starts_with(['{', '[']) || rest.starts_with("null"))
}

#[derive(Serialize)]
pub(crate) struct OutResponse<'a> {
    pub jsonrpc: &'static str,
    pub id: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'a RpcError>,
}

pub(crate) fn response_line(
    id: &Value,
    result: &Result<Value, RpcError>,
) -> Result<String, EncodeError> {
    let (result, error) = match result {
        Ok(v) => (Some(v), None),
        Err(e) => (None, Some(e)),
    };
    encode(&OutResponse {
        jsonrpc: JSONRPC,
        id,
        result,
        error,
    })
}

/// Encode a response, replacing it with an error response when it cannot be sent as-is.
pub(crate) fn response_line_or_error(id: &Value, result: &Result<Value, RpcError>) -> String {
    match response_line(id, result) {
        Ok(line) => line,
        Err(e) => {
            let err = match e {
                EncodeError::TooLarge(n) => RpcError::internal(format!(
                    "response of {n} bytes exceeds the {MAX_MESSAGE_BYTES} byte limit"
                )),
                EncodeError::Serde(e) => RpcError::internal(format!("cannot encode response: {e}")),
            };
            // A bare error object is tiny and always encodes.
            response_line(id, &Err(err)).unwrap_or_default()
        }
    }
}

/// A decoded incoming line.
#[derive(Debug)]
pub(crate) enum Incoming {
    /// A request (`id: Some`) or a notification (`id: None`).
    Request {
        id: Option<Value>,
        method: String,
        params: Value,
    },
    /// A response to one of our requests.
    Response {
        id: Value,
        result: Result<Value, RpcError>,
    },
    /// Not a valid message; `id` is echoed when it could be recovered.
    Invalid { id: Value, error: RpcError },
}

pub(crate) fn decode(line: &str) -> Incoming {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Incoming::Invalid {
                id: Value::Null,
                error: RpcError::parse_error(format!("invalid JSON: {e}")),
            };
        }
    };
    let Value::Object(mut obj) = value else {
        let msg = if value.is_array() {
            "batch requests are not supported"
        } else {
            "a message must be a JSON object"
        };
        return Incoming::Invalid {
            id: Value::Null,
            error: RpcError::invalid_request(msg),
        };
    };
    let id = obj.remove("id");
    let invalid = |id: Option<Value>, msg: &str| Incoming::Invalid {
        id: id.unwrap_or(Value::Null),
        error: RpcError::invalid_request(msg),
    };

    if obj.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC) {
        return invalid(id, "`jsonrpc` must be \"2.0\"");
    }
    if let Some(id) = &id
        && !(id.is_string() || id.is_number() || id.is_null())
    {
        return invalid(None, "`id` must be a string, number or null");
    }

    match obj.remove("method") {
        Some(Value::String(method)) => {
            let params = obj.remove("params").unwrap_or(Value::Null);
            if !(params.is_object() || params.is_array() || params.is_null()) {
                return invalid(id, "`params` must be an object or array");
            }
            Incoming::Request { id, method, params }
        }
        Some(_) => invalid(id, "`method` must be a string"),
        None => decode_response(id, obj),
    }
}

fn decode_response(id: Option<Value>, mut obj: Map<String, Value>) -> Incoming {
    let Some(id) = id else {
        return Incoming::Invalid {
            id: Value::Null,
            error: RpcError::invalid_request("missing `method`"),
        };
    };
    let result = match (obj.remove("result"), obj.remove("error")) {
        (Some(r), None) => Ok(r),
        (None, Some(e)) => match serde_json::from_value::<RpcError>(e) {
            Ok(e) => Err(e),
            Err(e) => Err(RpcError::internal(format!("malformed error object: {e}"))),
        },
        _ => {
            return Incoming::Invalid {
                id,
                error: RpcError::invalid_request(
                    "a response needs exactly one of `result` or `error`",
                ),
            };
        }
    };
    Incoming::Response { id, result }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code;
    use serde_json::json;

    #[test]
    fn decode_classifies_messages() {
        assert!(matches!(
            decode(r#"{"jsonrpc":"2.0","id":1,"method":"status"}"#),
            Incoming::Request {
                id: Some(_),
                params: Value::Null,
                ..
            }
        ));
        assert!(matches!(
            decode(r#"{"jsonrpc":"2.0","method":"chat.delta","params":{"a":1}}"#),
            Incoming::Request { id: None, .. }
        ));
        assert!(matches!(
            decode(r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#),
            Incoming::Response { result: Ok(_), .. }
        ));
        match decode(r#"{"jsonrpc":"2.0","id":7,"error":{"code":-32601,"message":"nope"}}"#) {
            Incoming::Response { result: Err(e), .. } => assert_eq!(e.code, code::METHOD_NOT_FOUND),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn decode_rejects_invalid() {
        let code_of = |line: &str| match decode(line) {
            Incoming::Invalid { error, .. } => error.code,
            other => panic!("expected invalid, got {other:?}"),
        };
        assert_eq!(code_of("{not json"), code::PARSE_ERROR);
        assert_eq!(code_of("[1,2]"), code::INVALID_REQUEST);
        assert_eq!(code_of(r#"{"id":1,"method":"x"}"#), code::INVALID_REQUEST);
        assert_eq!(
            code_of(r#"{"jsonrpc":"2.0","id":1,"method":5}"#),
            code::INVALID_REQUEST
        );
        assert_eq!(
            code_of(r#"{"jsonrpc":"2.0","id":1,"method":"x","params":3}"#),
            code::INVALID_REQUEST
        );
        assert_eq!(
            code_of(r#"{"jsonrpc":"2.0","id":{},"method":"x"}"#),
            code::INVALID_REQUEST
        );
    }

    #[test]
    fn request_params_are_encoded_first_and_shape_checked() {
        let line = |p: &Value| {
            encode(&OutRequest {
                params: p,
                jsonrpc: JSONRPC,
                id: Some(1),
                method: "m\"x",
            })
            .unwrap()
        };
        for ok in [json!({"a": 1}), json!([1]), Value::Null] {
            let l = line(&ok);
            assert!(params_shape_ok(&l), "{l}");
            assert!(matches!(decode(&l), Incoming::Request { params, .. } if params == ok));
        }
        for bad in [json!(1), json!("null"), json!(true)] {
            assert!(!params_shape_ok(&line(&bad)));
        }
    }

    #[test]
    fn oversized_response_becomes_error() {
        let big = Value::String("x".repeat(MAX_MESSAGE_BYTES));
        let line = response_line_or_error(&Value::from(3), &Ok(big));
        assert!(line.len() < 1024);
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["error"]["code"], code::INTERNAL_ERROR);
        assert_eq!(v["id"], 3);
    }
}
