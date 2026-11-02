//! Translation between the Anthropic Messages API and OpenAI Chat Completions.
//!
//! Used when `/llm/v1/messages` falls back to OpenAI: the Anthropic-shaped request is turned
//! into a Chat Completions body, and the OpenAI reply (streamed or not) is turned back into
//! Anthropic's wire format. Error strings never contain prompt or completion content.
//!
//! Token usage follows Anthropic semantics everywhere: `input_tokens` excludes cached prompt
//! tokens, which are reported separately as `cache_read_input_tokens`.

use serde_json::{Map, Value, json};

/// Anthropic Messages request → OpenAI Chat Completions request body.
pub fn to_openai_request(
    body: &Value,
    model: &str,
    stream: bool,
    cache_key: Option<&str>,
) -> Result<Value, String> {
    let mut messages = Vec::new();
    if let Some(system) = body.get("system").filter(|s| !s.is_null()) {
        let text = system_text(system)?;
        if !text.is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }
    let input = body
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("messages must be an array")?;
    for (i, msg) in input.iter().enumerate() {
        convert_message(msg, i, &mut messages)?;
    }
    let max_tokens = body
        .get("max_tokens")
        .and_then(Value::as_u64)
        .ok_or("max_tokens must be a positive integer")?;

    let mut out = Map::new();
    out.insert("model".into(), json!(model));
    out.insert("messages".into(), Value::Array(messages));
    out.insert("max_completion_tokens".into(), json!(max_tokens));
    out.insert("stream".into(), json!(stream));
    if stream {
        out.insert("stream_options".into(), json!({"include_usage": true}));
    }
    if let Some(key) = cache_key {
        out.insert("prompt_cache_key".into(), json!(key));
    }
    for field in ["temperature", "top_p"] {
        if let Some(v) = body.get(field).filter(|v| v.is_number()) {
            out.insert(field.into(), v.clone());
        }
    }
    if let Some(stops) = body.get("stop_sequences").and_then(Value::as_array)
        && !stops.is_empty()
    {
        out.insert("stop".into(), Value::Array(stops.clone()));
    }

    // Custom tools only; Anthropic server tools (typed, e.g. web_search) have no equivalent.
    let tools: Vec<Value> = body
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| {
            t.get("type")
                .and_then(Value::as_str)
                .is_none_or(|ty| ty == "custom")
        })
        .map(|t| {
            let name = t
                .get("name")
                .and_then(Value::as_str)
                .ok_or("tool missing name")?;
            let mut function = json!({
                "name": name,
                "parameters": t.get("input_schema").cloned().unwrap_or(json!({"type": "object"})),
            });
            if let Some(desc) = t.get("description").and_then(Value::as_str) {
                function["description"] = json!(desc);
            }
            Ok(json!({"type": "function", "function": function}))
        })
        .collect::<Result<_, String>>()?;
    if !tools.is_empty() {
        out.insert("tools".into(), Value::Array(tools));
        if let Some(choice) = body.get("tool_choice").filter(|c| c.is_object()) {
            let mapped = match choice.get("type").and_then(Value::as_str) {
                Some("auto") => json!("auto"),
                Some("any") => json!("required"),
                Some("none") => json!("none"),
                Some("tool") => {
                    let name = choice
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or("tool_choice.name is required for type tool")?;
                    json!({"type": "function", "function": {"name": name}})
                }
                _ => return Err("tool_choice.type is invalid".into()),
            };
            out.insert("tool_choice".into(), mapped);
            if choice
                .get("disable_parallel_tool_use")
                .and_then(Value::as_bool)
                == Some(true)
            {
                out.insert("parallel_tool_calls".into(), json!(false));
            }
        }
    }
    Ok(Value::Object(out))
}

/// `system` as a string or an array of text blocks (joined with blank lines).
fn system_text(system: &Value) -> Result<String, String> {
    match system {
        Value::String(s) => Ok(s.clone()),
        Value::Array(blocks) => Ok(blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n")),
        _ => Err("system must be a string or an array of text blocks".into()),
    }
}

/// Converts one Anthropic message, appending one or more OpenAI messages to `out`.
fn convert_message(msg: &Value, i: usize, out: &mut Vec<Value>) -> Result<(), String> {
    let role = match msg.get("role").and_then(Value::as_str) {
        Some(r @ ("user" | "assistant")) => r,
        _ => return Err(format!("messages[{i}].role must be user or assistant")),
    };
    let blocks = match msg.get("content") {
        Some(Value::String(s)) => {
            out.push(json!({"role": role, "content": s}));
            return Ok(());
        }
        Some(Value::Array(blocks)) => blocks,
        _ => {
            return Err(format!(
                "messages[{i}].content must be a string or an array"
            ));
        }
    };

    let mut texts: Vec<&str> = Vec::new();
    let mut parts: Vec<Value> = Vec::new(); // user text + image parts, in order
    let mut tool_calls: Vec<Value> = Vec::new(); // assistant only
    let mut tool_results: Vec<Value> = Vec::new(); // user only
    for (j, block) in blocks.iter().enumerate() {
        let field = |name: &str| {
            block
                .get(name)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("messages[{i}].content[{j}].{name} is missing"))
        };
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = field("text")?;
                texts.push(text);
                parts.push(json!({"type": "text", "text": text}));
            }
            Some("image") if role == "user" => {
                let source = block.get("source").unwrap_or(&Value::Null);
                let url = match source.get("type").and_then(Value::as_str) {
                    Some("base64") => {
                        let media = source.get("media_type").and_then(Value::as_str);
                        let data = source.get("data").and_then(Value::as_str);
                        match (media, data) {
                            (Some(m), Some(d)) => format!("data:{m};base64,{d}"),
                            _ => {
                                return Err(format!(
                                    "messages[{i}].content[{j}].source is invalid"
                                ));
                            }
                        }
                    }
                    Some("url") => source
                        .get("url")
                        .and_then(Value::as_str)
                        .ok_or_else(|| format!("messages[{i}].content[{j}].source.url is missing"))?
                        .to_string(),
                    _ => {
                        return Err(format!(
                            "messages[{i}].content[{j}].source.type is unsupported"
                        ));
                    }
                };
                parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
            }
            Some("tool_use") if role == "assistant" => {
                let input = block.get("input").cloned().unwrap_or(json!({}));
                tool_calls.push(json!({
                    "id": field("id")?,
                    "type": "function",
                    "function": {"name": field("name")?, "arguments": input.to_string()},
                }));
            }
            Some("tool_result") if role == "user" => {
                let mut content = match block.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(items)) => items
                        .iter()
                        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                    content.insert_str(0, "Error: ");
                }
                tool_results.push(json!({
                    "role": "tool",
                    "tool_call_id": field("tool_use_id")?,
                    "content": content,
                }));
            }
            // thinking, redacted_thinking, documents, server tool blocks, ...: no equivalent.
            _ => {}
        }
    }

    if role == "assistant" {
        let content = if texts.is_empty() {
            if tool_calls.is_empty() {
                json!("")
            } else {
                Value::Null
            }
        } else {
            json!(texts.join(""))
        };
        let mut m = json!({"role": "assistant", "content": content});
        if !tool_calls.is_empty() {
            m["tool_calls"] = Value::Array(tool_calls);
        }
        out.push(m);
        return Ok(());
    }

    // Tool results must directly follow the assistant tool calls, so they go first.
    let had_results = !tool_results.is_empty();
    out.extend(tool_results);
    if parts.is_empty() {
        if !had_results {
            out.push(json!({"role": "user", "content": ""}));
        }
    } else if parts.len() == texts.len() {
        out.push(json!({"role": "user", "content": texts.join("\n\n")}));
    } else {
        out.push(json!({"role": "user", "content": parts}));
    }
    Ok(())
}

/// OpenAI `finish_reason` → Anthropic `stop_reason`.
fn map_finish_reason(reason: &str) -> &'static str {
    match reason {
        "length" => "max_tokens",
        "tool_calls" | "function_call" => "tool_use",
        "content_filter" => "refusal",
        _ => "end_turn",
    }
}

/// Anthropic-style message id derived from the OpenAI completion id (or a fresh uuid).
fn message_id(v: &Value) -> String {
    let raw = v.get("id").and_then(Value::as_str).unwrap_or("");
    let raw = raw.strip_prefix("chatcmpl-").unwrap_or(raw);
    let clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if clean.is_empty() {
        format!("msg_{}", uuid::Uuid::new_v4().simple())
    } else {
        format!("msg_{clean}")
    }
}

/// The OpenAI tool call id, or a fresh one when the upstream omitted it.
fn tool_call_id(call: &Value) -> String {
    match call.get("id").and_then(Value::as_str) {
        Some(id) => id.to_string(),
        None => format!("toolu_{}", uuid::Uuid::new_v4().simple()),
    }
}

/// Short, content-free description of an `{"error": {...}}` payload.
fn error_summary(err: &Value) -> String {
    let kind = err
        .get("type")
        .or_else(|| err.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let kind: String = kind.chars().take(64).collect();
    format!("upstream error: {kind}")
}

/// OpenAI usage → (uncached input, output, cached input), Anthropic semantics.
fn parse_usage(usage: &Value) -> (u32, u32, u32) {
    let num =
        |v: Option<&Value>| v.and_then(Value::as_u64).unwrap_or(0).min(u32::MAX as u64) as u32;
    let prompt = num(usage.get("prompt_tokens"));
    let output = num(usage.get("completion_tokens"));
    let cached = num(usage.pointer("/prompt_tokens_details/cached_tokens"));
    (prompt.saturating_sub(cached), output, cached)
}

/// The content block currently open in the Anthropic stream.
enum OpenBlock {
    Text { index: usize },
    Tool { index: usize, openai_index: u64 },
}

/// Turns OpenAI streaming chunks into Anthropic SSE events.
pub struct StreamTranslator {
    model: String,
    started: bool,
    finished: bool,
    next_index: usize,
    open: Option<OpenBlock>,
    stop_reason: Option<&'static str>,
    usage: (u32, u32, u32),
    output_chars: usize,
}

impl StreamTranslator {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            started: false,
            finished: false,
            next_index: 0,
            open: None,
            stop_reason: None,
            usage: (0, 0, 0),
            output_chars: 0,
        }
    }

    /// Translates one SSE `data:` payload. `[DONE]` produces nothing; call `finish()` after it.
    pub fn on_data(&mut self, data: &str) -> Result<Vec<(&'static str, Value)>, String> {
        let data = data.trim();
        if data == "[DONE]" || data.is_empty() {
            return Ok(Vec::new());
        }
        let chunk: Value = serde_json::from_str(data)
            .map_err(|_| "malformed upstream stream chunk".to_string())?;
        if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
            return Err(error_summary(err));
        }
        let mut events = Vec::new();
        self.ensure_started(&chunk, &mut events);
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            self.usage = parse_usage(usage);
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return Ok(events);
        };
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        for key in ["content", "refusal"] {
            if let Some(text) = delta
                .get(key)
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                self.text_delta(text, &mut events);
            }
        }
        for call in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.tool_delta(call, &mut events)?;
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(map_finish_reason(reason));
        }
        Ok(events)
    }

    /// Emits the closing events still owed. Idempotent: later calls return nothing.
    pub fn finish(&mut self) -> Vec<(&'static str, Value)> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let mut events = Vec::new();
        self.ensure_started(&Value::Null, &mut events);
        self.close_block(&mut events);
        let (input, output, cached) = self.usage;
        events.push((
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": self.stop_reason.unwrap_or("end_turn"), "stop_sequence": null},
                "usage": {"output_tokens": output, "input_tokens": input, "cache_read_input_tokens": cached},
            }),
        ));
        events.push(("message_stop", json!({"type": "message_stop"})));
        events
    }

    /// (uncached input tokens, output tokens, cached input tokens) from the usage chunk.
    pub fn usage(&self) -> (u32, u32, u32) {
        self.usage
    }

    /// Characters of text and tool-call arguments emitted so far.
    pub fn output_chars(&self) -> usize {
        self.output_chars
    }

    fn ensure_started(&mut self, chunk: &Value, events: &mut Vec<(&'static str, Value)>) {
        if self.started {
            return;
        }
        self.started = true;
        events.push((
            "message_start",
            json!({"type": "message_start", "message": {
                "id": message_id(chunk), "type": "message", "role": "assistant",
                "model": self.model, "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0},
            }}),
        ));
    }

    fn close_block(&mut self, events: &mut Vec<(&'static str, Value)>) {
        if let Some(OpenBlock::Text { index } | OpenBlock::Tool { index, .. }) = self.open.take() {
            events.push((
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            ));
        }
    }

    /// Closes any open block and starts a new one at the next index.
    fn open_block(&mut self, block: Value, events: &mut Vec<(&'static str, Value)>) -> usize {
        self.close_block(events);
        let index = self.next_index;
        self.next_index += 1;
        events.push((
            "content_block_start",
            json!({"type": "content_block_start", "index": index, "content_block": block}),
        ));
        index
    }

    fn text_delta(&mut self, text: &str, events: &mut Vec<(&'static str, Value)>) {
        let index = match self.open {
            Some(OpenBlock::Text { index }) => index,
            _ => {
                let index = self.open_block(json!({"type": "text", "text": ""}), events);
                self.open = Some(OpenBlock::Text { index });
                index
            }
        };
        self.output_chars += text.chars().count();
        events.push((
            "content_block_delta",
            json!({"type": "content_block_delta", "index": index,
                   "delta": {"type": "text_delta", "text": text}}),
        ));
    }

    fn tool_delta(
        &mut self,
        call: &Value,
        events: &mut Vec<(&'static str, Value)>,
    ) -> Result<(), String> {
        let openai_index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
        let index = match self.open {
            // Continuation of the tool call already streaming (later fragments carry no id).
            Some(OpenBlock::Tool {
                index,
                openai_index: open,
            }) if open == openai_index => index,
            _ => {
                let name = call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .ok_or("tool call fragment missing function name")?;
                let id = tool_call_id(call);
                let block = json!({"type": "tool_use", "id": id, "name": name, "input": {}});
                let index = self.open_block(block, events);
                self.open = Some(OpenBlock::Tool {
                    index,
                    openai_index,
                });
                index
            }
        };
        if let Some(args) = call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .filter(|a| !a.is_empty())
        {
            self.output_chars += args.chars().count();
            events.push((
                "content_block_delta",
                json!({"type": "content_block_delta", "index": index,
                       "delta": {"type": "input_json_delta", "partial_json": args}}),
            ));
        }
        Ok(())
    }
}

/// Non-streaming OpenAI chat completion → Anthropic Message.
pub fn from_openai_response(v: &Value, model: &str) -> Result<Value, String> {
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        return Err(error_summary(err));
    }
    let choice = v
        .pointer("/choices/0")
        .ok_or("upstream response has no choices")?;
    let message = choice
        .get("message")
        .ok_or("upstream response has no message")?;
    let mut content = Vec::new();
    for key in ["content", "refusal"] {
        if let Some(text) = message
            .get(key)
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            content.push(json!({"type": "text", "text": text}));
        }
    }
    for call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .ok_or("upstream tool call missing function name")?;
        let id = tool_call_id(call);
        let input = call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .and_then(|a| serde_json::from_str::<Value>(a).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        content.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
    }
    let stop_reason = map_finish_reason(
        choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or("stop"),
    );
    let (input, output, cached) = v.get("usage").map(parse_usage).unwrap_or((0, 0, 0));
    Ok(json!({
        "id": message_id(v), "type": "message", "role": "assistant", "model": model,
        "content": content, "stop_reason": stop_reason, "stop_sequence": null,
        "usage": {"input_tokens": input, "output_tokens": output, "cache_read_input_tokens": cached},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(events: &[(&'static str, Value)]) -> Vec<&'static str> {
        events.iter().map(|(n, _)| *n).collect()
    }

    fn chunk(delta: Value, finish: Option<&str>) -> String {
        json!({"id": "chatcmpl-abc123", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
            .to_string()
    }

    #[test]
    fn request_basics_system_array_stop_and_sampling() {
        let body = json!({
            "max_tokens": 100, "temperature": 0.5, "top_p": 0.9, "top_k": 5,
            "stop_sequences": ["END"], "metadata": {"user_id": "u"}, "thinking": {"type": "enabled"},
            "system": [{"type": "text", "text": "A", "cache_control": {"type": "ephemeral"}}, {"type": "text", "text": "B"}],
            "messages": [{"role": "user", "content": "hi"}],
        });
        let out = to_openai_request(&body, "gpt-x", true, Some("key1")).unwrap();
        assert_eq!(
            out,
            json!({
                "model": "gpt-x", "max_completion_tokens": 100, "stream": true,
                "stream_options": {"include_usage": true}, "prompt_cache_key": "key1",
                "temperature": 0.5, "top_p": 0.9, "stop": ["END"],
                "messages": [{"role": "system", "content": "A\n\nB"}, {"role": "user", "content": "hi"}],
            })
        );
        let out = to_openai_request(&body, "gpt-x", false, None).unwrap();
        assert!(out.get("stream_options").is_none() && out.get("prompt_cache_key").is_none());
    }

    #[test]
    fn request_images_and_text_only_blocks() {
        let body = json!({"max_tokens": 10, "messages": [
            {"role": "user", "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]},
            {"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}},
                {"type": "image", "source": {"type": "url", "url": "https://x/y.jpg"}},
            ]},
        ]});
        let msgs = to_openai_request(&body, "m", false, None).unwrap()["messages"].clone();
        assert_eq!(msgs[0], json!({"role": "user", "content": "a\n\nb"}));
        assert_eq!(
            msgs[1]["content"],
            json!([
                {"type": "text", "text": "look"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}},
                {"type": "image_url", "image_url": {"url": "https://x/y.jpg"}},
            ])
        );
    }

    #[test]
    fn request_tool_use_and_tool_result_round_trip() {
        let body = json!({"max_tokens": 10,
        "tools": [{"name": "get_weather", "description": "Weather", "input_schema": {"type": "object"}},
                  {"type": "web_search_20250305", "name": "web_search"}],
        "tool_choice": {"type": "any", "disable_parallel_tool_use": true},
        "messages": [
            {"role": "user", "content": "weather?"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "s"},
                {"type": "text", "text": "Checking."},
                {"type": "tool_use", "id": "t1", "name": "get_weather", "input": {"city": "Paris"}},
                {"type": "tool_use", "id": "t2", "name": "get_weather", "input": {"city": "Rome"}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "sunny"},
                {"type": "tool_result", "tool_use_id": "t2", "is_error": true,
                 "content": [{"type": "text", "text": "timeout"}, {"type": "text", "text": "retry"}]},
                {"type": "text", "text": "thanks"},
            ]},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "t3", "name": "get_weather", "input": {}}]},
        ]});
        let out = to_openai_request(&body, "m", false, None).unwrap();
        assert_eq!(
            out["tools"],
            json!([{"type": "function", "function": {"name": "get_weather", "description": "Weather", "parameters": {"type": "object"}}}])
        );
        assert_eq!(out["tool_choice"], json!("required"));
        assert_eq!(out["parallel_tool_calls"], json!(false));
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 6);
        assert_eq!(msgs[1]["content"], json!("Checking."));
        assert_eq!(
            msgs[1]["tool_calls"][0],
            json!({"id": "t1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}})
        );
        assert_eq!(
            msgs[2],
            json!({"role": "tool", "tool_call_id": "t1", "content": "sunny"})
        );
        assert_eq!(
            msgs[3],
            json!({"role": "tool", "tool_call_id": "t2", "content": "Error: timeout\nretry"})
        );
        assert_eq!(msgs[4], json!({"role": "user", "content": "thanks"}));
        assert_eq!(msgs[5]["content"], Value::Null);
    }

    #[test]
    fn request_tool_choice_mapping_and_errors() {
        let with = |choice: Value| {
            let body = json!({"max_tokens": 1, "messages": [], "tool_choice": choice,
                              "tools": [{"name": "f", "input_schema": {"type": "object"}}]});
            to_openai_request(&body, "m", false, None)
        };
        assert_eq!(
            with(json!({"type": "auto"})).unwrap()["tool_choice"],
            json!("auto")
        );
        assert_eq!(
            with(json!({"type": "none"})).unwrap()["tool_choice"],
            json!("none")
        );
        assert_eq!(
            with(json!({"type": "tool", "name": "f"})).unwrap()["tool_choice"],
            json!({"type": "function", "function": {"name": "f"}})
        );
        assert!(
            with(json!({"type": "auto"}))
                .unwrap()
                .get("parallel_tool_calls")
                .is_none()
        );
        assert!(with(json!({"type": "tool"})).is_err());
        let bad = json!({"max_tokens": 1, "messages": [{"role": "system", "content": "secret"}]});
        let err = to_openai_request(&bad, "m", false, None).unwrap_err();
        assert!(!err.contains("secret"));
    }

    #[test]
    fn stream_text_reply_with_usage() {
        let mut t = StreamTranslator::new("claude-x");
        let ev = t
            .on_data(&chunk(json!({"role": "assistant", "content": ""}), None))
            .unwrap();
        assert_eq!(names(&ev), ["message_start"]);
        assert_eq!(
            ev[0].1,
            json!({"type": "message_start", "message": {"id": "msg_abc123", "type": "message", "role": "assistant",
                "model": "claude-x", "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}}})
        );
        let ev = t.on_data(&chunk(json!({"content": "Hel"}), None)).unwrap();
        assert_eq!(names(&ev), ["content_block_start", "content_block_delta"]);
        assert_eq!(
            ev[0].1,
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})
        );
        assert_eq!(
            ev[1].1,
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}})
        );
        let ev = t
            .on_data(&chunk(json!({"content": "lo"}), Some("stop")))
            .unwrap();
        assert_eq!(names(&ev), ["content_block_delta"]);
        let usage = json!({"id": "chatcmpl-abc123", "choices": [], "usage": {"prompt_tokens": 100,
            "completion_tokens": 7, "prompt_tokens_details": {"cached_tokens": 60}}});
        assert!(t.on_data(&usage.to_string()).unwrap().is_empty());
        assert!(t.on_data("[DONE]").unwrap().is_empty());
        assert_eq!(t.usage(), (40, 7, 60));
        assert_eq!(t.output_chars(), 5);
        let ev = t.finish();
        assert_eq!(
            names(&ev),
            ["content_block_stop", "message_delta", "message_stop"]
        );
        assert_eq!(ev[0].1, json!({"type": "content_block_stop", "index": 0}));
        assert_eq!(
            ev[1].1,
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                   "usage": {"output_tokens": 7, "input_tokens": 40, "cache_read_input_tokens": 60}})
        );
        assert_eq!(ev[2].1, json!({"type": "message_stop"}));
        assert!(t.finish().is_empty(), "finish is idempotent");
    }

    #[test]
    fn stream_text_then_two_fragmented_tool_calls() {
        let mut t = StreamTranslator::new("m");
        let mut all = Vec::new();
        let tc = |i: u64, id: Option<&str>, name: Option<&str>, args: &str| {
            let mut c = json!({"index": i, "function": {"arguments": args}});
            if let (Some(id), Some(name)) = (id, name) {
                c["id"] = json!(id);
                c["type"] = json!("function");
                c["function"]["name"] = json!(name);
            }
            chunk(json!({"tool_calls": [c]}), None)
        };
        for data in [
            chunk(json!({"content": "Let me check."}), None),
            tc(0, Some("call_a"), Some("get_weather"), ""),
            tc(0, None, None, "{\"city\":"),
            tc(0, None, None, "\"Paris\"}"),
            tc(1, Some("call_b"), Some("get_time"), "{\"tz\""),
            tc(1, None, None, ":\"CET\"}"),
            chunk(json!({}), Some("tool_calls")),
        ] {
            all.extend(t.on_data(&data).unwrap());
        }
        all.extend(t.finish());
        assert_eq!(
            names(&all),
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(all[3].1["index"], 0);
        assert_eq!(
            all[4].1,
            json!({"type": "content_block_start", "index": 1,
                   "content_block": {"type": "tool_use", "id": "call_a", "name": "get_weather", "input": {}}})
        );
        assert_eq!(
            all[5].1,
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"city\":"}})
        );
        assert_eq!(all[8].1["content_block"]["name"], "get_time");
        assert_eq!(all[8].1["index"], 2);
        assert_eq!(all[10].1["delta"]["partial_json"], ":\"CET\"}");
        assert_eq!(all[11].1["index"], 2);
        assert_eq!(all[12].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(t.output_chars(), "Let me check.".len() + 16 + 12);
        assert_eq!(t.usage(), (0, 0, 0));
    }

    #[test]
    fn stream_empty_finish_and_stop_reasons() {
        let mut t = StreamTranslator::new("m");
        let ev = t.finish();
        assert_eq!(
            names(&ev),
            ["message_start", "message_delta", "message_stop"]
        );
        assert!(
            ev[0].1["message"]["id"]
                .as_str()
                .unwrap()
                .starts_with("msg_")
        );
        assert_eq!(ev[1].1["usage"]["output_tokens"], 0);
        assert!(t.finish().is_empty());
        for (reason, want) in [
            ("length", "max_tokens"),
            ("content_filter", "refusal"),
            ("function_call", "tool_use"),
            ("x", "end_turn"),
        ] {
            let mut t = StreamTranslator::new("m");
            t.on_data(&chunk(json!({}), Some(reason))).unwrap();
            assert_eq!(t.finish()[0].1["delta"]["stop_reason"], want);
        }
    }

    #[test]
    fn stream_errors_are_content_free() {
        let mut t = StreamTranslator::new("m");
        let err = t
            .on_data(r#"{"error": {"message": "prompt says SECRET", "type": "server_error"}}"#)
            .unwrap_err();
        assert_eq!(err, "upstream error: server_error");
        assert!(t.on_data("{not json").is_err());
        assert!(
            t.on_data(&chunk(json!({"tool_calls": [{"index": 0}]}), None))
                .is_err()
        );
    }

    #[test]
    fn non_streaming_response() {
        let v = json!({"id": "chatcmpl-xyz", "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
            "role": "assistant", "content": "On it.",
            "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "f", "arguments": "{\"a\":1}"}},
                {"id": "call_2", "type": "function", "function": {"name": "g", "arguments": "{broken"}},
            ]}}],
            "usage": {"prompt_tokens": 50, "completion_tokens": 9, "prompt_tokens_details": {"cached_tokens": 20}}});
        assert_eq!(
            from_openai_response(&v, "claude-x").unwrap(),
            json!({"id": "msg_xyz", "type": "message", "role": "assistant", "model": "claude-x",
                "content": [
                    {"type": "text", "text": "On it."},
                    {"type": "tool_use", "id": "call_1", "name": "f", "input": {"a": 1}},
                    {"type": "tool_use", "id": "call_2", "name": "g", "input": {}},
                ],
                "stop_reason": "tool_use", "stop_sequence": null,
                "usage": {"input_tokens": 30, "output_tokens": 9, "cache_read_input_tokens": 20}})
        );
        assert_eq!(
            from_openai_response(&json!({"error": {"code": "rate_limit"}}), "m").unwrap_err(),
            "upstream error: rate_limit"
        );
        assert!(from_openai_response(&json!({"choices": []}), "m").is_err());
    }
}
