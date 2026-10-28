//! A small JSON Schema validator for the subset structured outputs and tool schemas use, plus
//! the schema transforms the adapters need.
//!
//! Supported keywords: `type` (string or array), `properties`, `required`,
//! `additionalProperties` (bool or schema), `items`, `enum`, `const`, `anyOf`, `oneOf`, `allOf`,
//! `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `minLength`, `maxLength`,
//! `minItems`, `maxItems`, and local `$ref`s (`#/$defs/..`, `#/definitions/..`). Other keywords
//! are ignored.

use nugget_core::{Error, Result};
use serde_json::{Map, Value};

/// Validate `instance` against `schema`. The error lists every violation found.
pub fn validate(schema: &Value, instance: &Value) -> Result<()> {
    let mut errors = Vec::new();
    check(schema, schema, instance, "$", &mut errors, 0);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(Error::Invalid(errors.join("; ")))
    }
}

const MAX_DEPTH: usize = 64;

fn check(
    root: &Value,
    schema: &Value,
    v: &Value,
    path: &str,
    errs: &mut Vec<String>,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        errs.push(format!("{path}: schema nesting too deep"));
        return;
    }
    let s = match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => {
            errs.push(format!("{path}: no value allowed"));
            return;
        }
        Value::Object(s) => s,
        _ => return,
    };
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        match resolve_ref(root, r) {
            Some(target) => check(root, target, v, path, errs, depth + 1),
            None => errs.push(format!("{path}: unresolvable $ref {r}")),
        }
    }
    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(t) => type_matches(t, v),
            Value::Array(ts) => ts
                .iter()
                .filter_map(Value::as_str)
                .any(|t| type_matches(t, v)),
            _ => true,
        };
        if !ok {
            errs.push(format!("{path}: expected type {t}, got {}", type_name(v)));
            return;
        }
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array)
        && !e.iter().any(|x| json_eq(x, v))
    {
        errs.push(format!("{path}: not one of the allowed values"));
    }
    if let Some(c) = s.get("const")
        && !json_eq(c, v)
    {
        errs.push(format!("{path}: must equal {c}"));
    }
    if let Some(all) = s.get("allOf").and_then(Value::as_array) {
        for sub in all {
            check(root, sub, v, path, errs, depth + 1);
        }
    }
    for (kw, exactly_one) in [("anyOf", false), ("oneOf", true)] {
        if let Some(alts) = s.get(kw).and_then(Value::as_array) {
            let passing = alts
                .iter()
                .filter(|sub| {
                    let mut e = Vec::new();
                    check(root, sub, v, path, &mut e, depth + 1);
                    e.is_empty()
                })
                .count();
            if passing == 0 || (exactly_one && passing != 1) {
                errs.push(format!("{path}: does not match {kw}"));
            }
        }
    }
    match v {
        Value::Object(obj) => check_object(root, s, obj, path, errs, depth),
        Value::Array(items) => {
            if let Some(item_schema) = s.get("items") {
                for (i, item) in items.iter().enumerate() {
                    check(
                        root,
                        item_schema,
                        item,
                        &format!("{path}[{i}]"),
                        errs,
                        depth + 1,
                    );
                }
            }
            bound(s, "minItems", items.len(), path, errs, |n, m| n >= m);
            bound(s, "maxItems", items.len(), path, errs, |n, m| n <= m);
        }
        Value::String(st) => {
            let n = st.chars().count();
            bound(s, "minLength", n, path, errs, |n, m| n >= m);
            bound(s, "maxLength", n, path, errs, |n, m| n <= m);
        }
        Value::Number(num) => {
            if let Some(x) = num.as_f64() {
                let cmp = |kw: &str, ok: fn(f64, f64) -> bool, errs: &mut Vec<String>| {
                    if let Some(m) = s.get(kw).and_then(Value::as_f64)
                        && !ok(x, m)
                    {
                        errs.push(format!("{path}: violates {kw} {m}"));
                    }
                };
                cmp("minimum", |x, m| x >= m, errs);
                cmp("maximum", |x, m| x <= m, errs);
                cmp("exclusiveMinimum", |x, m| x > m, errs);
                cmp("exclusiveMaximum", |x, m| x < m, errs);
            }
        }
        _ => {}
    }
}

fn check_object(
    root: &Value,
    s: &Map<String, Value>,
    obj: &Map<String, Value>,
    path: &str,
    errs: &mut Vec<String>,
    depth: usize,
) {
    let props = s.get("properties").and_then(Value::as_object);
    if let Some(req) = s.get("required").and_then(Value::as_array) {
        for k in req.iter().filter_map(Value::as_str) {
            if !obj.contains_key(k) {
                errs.push(format!("{path}: missing required property {k:?}"));
            }
        }
    }
    for (k, val) in obj {
        let child = format!("{path}.{k}");
        match props.and_then(|p| p.get(k)) {
            Some(ps) => check(root, ps, val, &child, errs, depth + 1),
            None => match s.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    errs.push(format!("{path}: unexpected property {k:?}"));
                }
                Some(extra @ Value::Object(_)) => check(root, extra, val, &child, errs, depth + 1),
                _ => {}
            },
        }
    }
}

fn bound(
    s: &Map<String, Value>,
    kw: &str,
    n: usize,
    path: &str,
    errs: &mut Vec<String>,
    ok: fn(u64, u64) -> bool,
) {
    if let Some(m) = s.get(kw).and_then(Value::as_u64)
        && !ok(n as u64, m)
    {
        errs.push(format!("{path}: violates {kw} {m}"));
    }
}

fn resolve_ref<'a>(root: &'a Value, r: &str) -> Option<&'a Value> {
    let pointer = r.strip_prefix('#')?;
    root.pointer(pointer)
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => match v {
            Value::Number(n) => {
                n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
            }
            _ => false,
        },
        _ => true,
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

/// Keywords whose value is a map of name -> subschema.
const SCHEMA_MAPS: [&str; 4] = ["properties", "$defs", "definitions", "patternProperties"];
/// Keywords whose value is a subschema or an array of subschemas.
const SCHEMA_VALUES: [&str; 8] = [
    "items",
    "prefixItems",
    "additionalProperties",
    "anyOf",
    "oneOf",
    "allOf",
    "not",
    "contains",
];

fn is_object_schema(s: &Map<String, Value>) -> bool {
    match s.get("type") {
        Some(Value::String(t)) => t == "object",
        Some(Value::Array(ts)) => ts.iter().any(|t| t == "object"),
        _ => s.contains_key("properties"),
    }
}

/// Every direct subschema of `s`.
fn subschemas(s: &Map<String, Value>) -> Vec<&Value> {
    let mut out = Vec::new();
    for k in SCHEMA_MAPS {
        if let Some(Value::Object(m)) = s.get(k) {
            out.extend(m.values());
        }
    }
    for k in SCHEMA_VALUES {
        match s.get(k) {
            Some(Value::Array(a)) => out.extend(a.iter()),
            Some(v @ Value::Object(_)) => out.push(v),
            _ => {}
        }
    }
    out
}

/// True if the schema meets OpenAI strict-mode rules: every object lists all its properties in
/// `required` and sets `additionalProperties: false`.
pub(crate) fn is_strict_compatible(schema: &Value) -> bool {
    fn walk(v: &Value, depth: usize) -> bool {
        let Value::Object(s) = v else {
            return true;
        };
        if depth > MAX_DEPTH {
            return false;
        }
        if is_object_schema(s) {
            if s.get("additionalProperties") != Some(&Value::Bool(false)) {
                return false;
            }
            let req: Vec<&str> = s
                .get("required")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if let Some(p) = s.get("properties").and_then(Value::as_object)
                && !p.keys().all(|k| req.contains(&k.as_str()))
            {
                return false;
            }
        }
        subschemas(s).into_iter().all(|c| walk(c, depth + 1))
    }
    walk(schema, 0)
}

/// Adapt a schema for Anthropic structured outputs: drop constraints the API does not support
/// (numeric bounds, string length, array size) and close every object with
/// `additionalProperties: false`. Validate the result client-side with [`validate`] against the
/// original schema if those constraints matter.
pub(crate) fn for_anthropic_output(schema: &Value) -> Value {
    const UNSUPPORTED: [&str; 9] = [
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
    ];
    fn walk(v: &Value, depth: usize) -> Value {
        let Value::Object(s) = v else {
            return v.clone();
        };
        if depth > MAX_DEPTH {
            return v.clone();
        }
        let mut out = Map::new();
        for (k, child) in s {
            let k = k.as_str();
            if UNSUPPORTED.contains(&k) {
                continue;
            }
            let new = if SCHEMA_MAPS.contains(&k) {
                match child {
                    Value::Object(m) => Value::Object(
                        m.iter()
                            .map(|(name, sub)| (name.clone(), walk(sub, depth + 1)))
                            .collect(),
                    ),
                    other => other.clone(),
                }
            } else if SCHEMA_VALUES.contains(&k) {
                match child {
                    Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, depth + 1)).collect()),
                    other => walk(other, depth + 1),
                }
            } else {
                child.clone()
            };
            out.insert(k.to_string(), new);
        }
        if is_object_schema(s) {
            out.insert("additionalProperties".into(), Value::Bool(false));
        }
        Value::Object(out)
    }
    walk(schema, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "minLength": 1},
                "age": {"type": "integer", "minimum": 0},
                "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 2},
                "kind": {"enum": ["a", "b"]},
                "nested": {"$ref": "#/$defs/n"},
                "either": {"anyOf": [{"type": "string"}, {"type": "null"}]}
            },
            "required": ["name", "age"],
            "additionalProperties": false,
            "$defs": {"n": {"type": "object", "properties": {"x": {"type": "number"}}, "required": ["x"]}}
        })
    }

    #[test]
    fn accepts_valid() {
        validate(
            &schema(),
            &json!({"name": "a", "age": 3, "tags": ["x"], "kind": "b", "nested": {"x": 1.5}, "either": null}),
        )
        .unwrap();
        validate(&json!({"type": "integer"}), &json!(3.0)).unwrap();
        validate(&json!(true), &json!({"anything": 1})).unwrap();
    }

    #[test]
    fn rejects_invalid() {
        let s = schema();
        let bad = [
            json!({"age": 3}),
            json!({"name": "a", "age": -1}),
            json!({"name": "", "age": 1}),
            json!({"name": "a", "age": 1.5}),
            json!({"name": "a", "age": 1, "extra": true}),
            json!({"name": "a", "age": 1, "tags": ["a", "b", "c"]}),
            json!({"name": "a", "age": 1, "tags": [1]}),
            json!({"name": "a", "age": 1, "kind": "c"}),
            json!({"name": "a", "age": 1, "nested": {}}),
            json!({"name": "a", "age": 1, "either": 3}),
            json!("not an object"),
        ];
        for b in bad {
            assert!(validate(&s, &b).is_err(), "{b}");
        }
        let err = validate(&s, &json!({})).unwrap_err().to_string();
        assert!(err.contains("name") && err.contains("age"), "{err}");
        assert!(
            validate(
                &json!({"oneOf": [{"type": "number"}, {"type": "integer"}]}),
                &json!(1)
            )
            .is_err()
        );
        assert!(validate(&json!({"$ref": "#/nope"}), &json!(1)).is_err());
    }

    #[test]
    fn strict_compatibility() {
        assert!(is_strict_compatible(&json!({
            "type": "object",
            "properties": {"a": {"type": "object", "properties": {"b": {"type": "string"}}, "required": ["b"], "additionalProperties": false}},
            "required": ["a"],
            "additionalProperties": false
        })));
        assert!(!is_strict_compatible(&json!({
            "type": "object", "properties": {"a": {"type": "string"}}, "required": [], "additionalProperties": false
        })));
        assert!(!is_strict_compatible(&json!({
            "type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]
        })));
        assert!(!is_strict_compatible(&schema()));
    }

    #[test]
    fn anthropic_transform() {
        let out = for_anthropic_output(&schema());
        assert!(out["properties"]["name"].get("minLength").is_none());
        assert!(out["properties"]["age"].get("minimum").is_none());
        assert_eq!(out["$defs"]["n"]["additionalProperties"], json!(false));
        assert_eq!(out["properties"]["kind"]["enum"], json!(["a", "b"]));
        // Property *names* that look like keywords are kept.
        let odd =
            json!({"type": "object", "properties": {"maximum": {"type": "number", "maximum": 3}}});
        let out = for_anthropic_output(&odd);
        assert!(out["properties"].get("maximum").is_some());
        assert!(out["properties"]["maximum"].get("maximum").is_none());
        assert!(!is_strict_compatible(&json!({
            "type": "object",
            "properties": {"type": {"type": "object", "properties": {}}},
            "required": ["type"],
            "additionalProperties": false
        })));
    }
}
