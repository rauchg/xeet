//! A small JSON Schema validator for the subset the contracts use.
//!
//! Supported: `type` (string or list), `properties`, `required`,
//! `additionalProperties: false`, `enum`, `const`, `minimum`, `maximum`,
//! `items`, `minLength`. Anything else is ignored, so every contract in this
//! crate sticks to this subset (a test checks that). Errors carry a JSON
//! pointer, so an agent can fix the exact field.

use serde_json::Value;

/// Validates `value` against `schema`. Returns every problem as
/// `(pointer, message)`.
pub fn validate(schema: &Value, value: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    check(schema, value, "", &mut out);
    out
}

/// Schema keywords this validator understands. Contracts must not use others.
pub const KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "enum",
    "const",
    "minimum",
    "maximum",
    "items",
    "minLength",
    "description",
    "default",
    "title",
    "examples",
];

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_ok(want: &str, v: &Value) -> bool {
    let got = type_name(v);
    want == got || (want == "number" && got == "integer")
}

fn check(schema: &Value, v: &Value, at: &str, out: &mut Vec<(String, String)>) {
    let Some(s) = schema.as_object() else { return };
    let here = if at.is_empty() {
        "/".to_string()
    } else {
        at.to_string()
    };
    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(w) => type_ok(w, v),
            Value::Array(ws) => ws.iter().filter_map(Value::as_str).any(|w| type_ok(w, v)),
            _ => true,
        };
        if !ok {
            out.push((here, format!("expected {t}, got {}", type_name(v))));
            return;
        }
    }
    if let Some(Value::Array(options)) = s.get("enum") {
        if !options.contains(v) {
            out.push((
                here.clone(),
                format!("must be one of {}", Value::Array(options.clone())),
            ));
        }
    }
    if let Some(c) = s.get("const") {
        if !c.is_null() && c != v {
            out.push((here.clone(), format!("must be {c}")));
        }
    }
    if let (Some(min), Some(n)) = (s.get("minimum").and_then(Value::as_f64), v.as_f64()) {
        if n < min {
            out.push((here.clone(), format!("must be at least {min}")));
        }
    }
    if let (Some(max), Some(n)) = (s.get("maximum").and_then(Value::as_f64), v.as_f64()) {
        if n > max {
            out.push((here.clone(), format!("must be at most {max}")));
        }
    }
    if let (Some(min), Some(text)) = (s.get("minLength").and_then(Value::as_u64), v.as_str()) {
        if (text.chars().count() as u64) < min {
            out.push((here.clone(), format!("must have at least {min} characters")));
        }
    }
    if let Value::Object(map) = v {
        let props = s.get("properties").and_then(Value::as_object);
        if let Some(Value::Array(req)) = s.get("required") {
            for r in req.iter().filter_map(Value::as_str) {
                if !map.contains_key(r) {
                    out.push((format!("{at}/{r}"), "is required".into()));
                }
            }
        }
        for (k, x) in map {
            match props.and_then(|p| p.get(k)) {
                Some(sub) => check(sub, x, &format!("{at}/{k}"), out),
                None => {
                    if s.get("additionalProperties") == Some(&Value::Bool(false)) {
                        let known: Vec<&String> =
                            props.map(|p| p.keys().collect()).unwrap_or_default();
                        out.push((
                            format!("{at}/{k}"),
                            format!("is not a known field; known: {known:?}"),
                        ));
                    }
                }
            }
        }
    }
    if let (Value::Array(items), Some(item)) = (v, s.get("items")) {
        for (i, x) in items.iter().enumerate() {
            check(item, x, &format!("{at}/{i}"), out);
        }
    }
}

/// Keywords a schema uses that this validator ignores (for the contract test).
pub fn unsupported_keywords(schema: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(schema, &mut out, false);
    out.sort();
    out.dedup();
    out
}

fn walk(schema: &Value, out: &mut Vec<String>, in_properties: bool) {
    let Some(s) = schema.as_object() else { return };
    for (k, v) in s {
        if in_properties {
            walk(v, out, false);
            continue;
        }
        if !KEYWORDS.contains(&k.as_str()) {
            out.push(k.clone());
        }
        match k.as_str() {
            "properties" => walk(v, out, true),
            "items" => walk(v, out, false),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reports_pointers() {
        let s = json!({"type": "object", "additionalProperties": false, "required": ["a"],
            "properties": {"a": {"type": "integer", "minimum": 1}, "b": {"enum": ["x"]},
                           "c": {"type": "array", "items": {"type": "string"}}}});
        assert!(validate(&s, &json!({"a": 2, "b": "x", "c": ["q"]})).is_empty());
        let errs = validate(&s, &json!({"a": 0, "b": "y", "c": [1], "d": true}));
        let at: Vec<&str> = errs.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(at, ["/a", "/b", "/c/0", "/d"]);
        assert_eq!(validate(&s, &json!({}))[0].0, "/a");
    }
}
