//! Bounded YAML → JSON for frontmatter. Built on the parser's event stream (no recursion in our
//! code) with caps on nesting depth and total nodes, so alias "billion laughs" inputs and deep
//! nesting fail cleanly instead of exhausting memory or stack.

use std::collections::HashMap;

use serde_json::{Map, Value};
use yaml_rust2::parser::{Event, Parser};
use yaml_rust2::scanner::TScalarStyle;

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 20_000;

enum Kind {
    Seq(Vec<Value>),
    Map(Map<String, Value>, Option<String>),
}

struct Frame {
    kind: Kind,
    anchor: usize,
    /// Nodes in this container's subtree so far.
    size: usize,
}

/// Parse the first YAML document in `src` to JSON.
pub(crate) fn to_json(src: &str) -> Result<Value, String> {
    let mut parser = Parser::new_from_str(src);
    let mut stack: Vec<Frame> = Vec::new();
    let mut anchors: HashMap<usize, (Value, usize)> = HashMap::new();
    let mut nodes = 0usize;
    loop {
        let (event, _) = parser.next_token().map_err(|e| e.to_string())?;
        let is_container = matches!(event, Event::SequenceEnd | Event::MappingEnd);
        let (value, anchor, size) = match event {
            Event::StreamEnd | Event::DocumentEnd => break,
            Event::Nothing | Event::StreamStart | Event::DocumentStart => continue,
            Event::SequenceStart(anchor, _) => {
                if stack.len() >= MAX_DEPTH {
                    return Err("frontmatter nested too deeply".into());
                }
                stack.push(Frame {
                    kind: Kind::Seq(Vec::new()),
                    anchor,
                    size: 1,
                });
                continue;
            }
            Event::MappingStart(anchor, _) => {
                if stack.len() >= MAX_DEPTH {
                    return Err("frontmatter nested too deeply".into());
                }
                stack.push(Frame {
                    kind: Kind::Map(Map::new(), None),
                    anchor,
                    size: 1,
                });
                continue;
            }
            Event::SequenceEnd | Event::MappingEnd => match stack.pop() {
                Some(Frame {
                    kind: Kind::Seq(v),
                    anchor,
                    size,
                }) => (Value::Array(v), anchor, size),
                Some(Frame {
                    kind: Kind::Map(m, _),
                    anchor,
                    size,
                }) => (Value::Object(m), anchor, size),
                None => return Err("unbalanced collection".into()),
            },
            Event::Scalar(s, style, anchor, _) => (scalar(s, style), anchor, 1),
            Event::Alias(id) => {
                let (v, n) = anchors
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| "unknown alias".to_owned())?;
                (v, 0, n)
            }
        };
        // Containers were counted as their children arrived; scalars and aliases count here.
        nodes += if is_container { 1 } else { size };
        if nodes > MAX_NODES {
            return Err("frontmatter too large".into());
        }
        if anchor > 0 {
            anchors.insert(anchor, (value.clone(), size));
        }
        let Some(parent) = stack.last_mut() else {
            return Ok(value);
        };
        parent.size += size;
        match &mut parent.kind {
            Kind::Seq(v) => v.push(value),
            Kind::Map(m, key) => match key.take() {
                None => *key = Some(key_string(value)),
                Some(k) => {
                    m.insert(k, value);
                }
            },
        }
    }
    Ok(Value::Null)
}

fn key_string(v: Value) -> String {
    match v {
        Value::String(s) => s,
        Value::Null => "null".into(),
        other => other.to_string(),
    }
}

fn scalar(s: String, style: TScalarStyle) -> Value {
    if style != TScalarStyle::Plain {
        return Value::String(s);
    }
    match s.as_str() {
        "" | "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    if let Ok(i) = s.parse::<i64>() {
        return Value::from(i);
    }
    if s.bytes().any(|b| b.is_ascii_digit())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E'))
        && let Some(n) = s.parse::<f64>().ok().and_then(serde_json::Number::from_f64)
    {
        return Value::Number(n);
    }
    Value::String(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn basic() {
        let v = to_json(
            "title: Hello\ntags: [a, b]\nn: 3\nf: 1.5\nok: true\nnone:\nlist:\n  - x\n  - 'y'\n",
        )
        .unwrap();
        assert_eq!(
            v,
            json!({"title":"Hello","tags":["a","b"],"n":3,"f":1.5,"ok":true,"none":null,"list":["x","y"]})
        );
    }

    #[test]
    fn billion_laughs_is_bounded() {
        let mut s = String::from("a: &a [x,x,x,x,x,x,x,x,x,x]\n");
        let mut prev = 'a';
        for c in 'b'..='k' {
            s.push_str(&format!(
                "{c}: &{c} [*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev}]\n"
            ));
            prev = c;
        }
        assert!(to_json(&s).is_err());
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let s = "- ".repeat(5000) + "x";
        assert!(to_json(&s).is_err());
        assert!(to_json(&"[".repeat(300)).is_err());
    }
}
