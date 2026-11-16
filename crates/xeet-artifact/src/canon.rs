//! Canonical JSON and identity.
//!
//! The canonical form is compact JSON with object keys sorted by their UTF-8
//! bytes at every level, numbers as `serde_json` prints them, and no
//! whitespace. Identity is `sha256:` + the lowercase hex SHA-256 of the
//! canonical form of the document with `id` set to `""`.
//!
//! Keys are sorted explicitly rather than relying on `serde_json`'s map type,
//! so a dependency that enables `preserve_order` cannot change identities.
//!
//! **Numbers are kept as written.** A reader computes identity from the
//! document's text with [`document_id_from_text`], which never converts a
//! number token to a float and back: a parser that is one ulp off (as
//! `serde_json`'s default one can be on 17-digit values) cannot change an id.
//! Producers write numbers with `serde_json`, and [`document_id`] hashes the
//! same tokens it writes, so the two agree by construction.

use serde_json::Value;
use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn sha256_id(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

/// Compact canonical JSON.
pub fn canonical(value: &Value) -> String {
    let mut out = String::new();
    write(value, &mut out);
    out
}

fn write(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("string serializes"));
                out.push(':');
                write(&map[*k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(v, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).expect("scalar serializes")),
    }
}

/// Pretty JSON with sorted keys, for the file humans open.
pub fn pretty(value: &Value) -> String {
    let sorted = sort(value);
    serde_json::to_string_pretty(&sorted).expect("value serializes")
}

fn sort(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), sort(&map[k]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
        other => other.clone(),
    }
}

/// The identity of a document: the hash of its canonical form with `id` = "".
pub fn document_id(document: &Value) -> String {
    let mut doc = document.clone();
    if let Value::Object(map) = &mut doc {
        map.insert("id".to_string(), Value::String(String::new()));
    }
    sha256_id(canonical(&doc).as_bytes())
}

/// The identity of a document given as JSON text, with number tokens kept
/// exactly as written. This is what verification uses.
pub fn document_id_from_text(text: &str) -> Result<String, String> {
    let mut p = Raw {
        s: text.as_bytes(),
        text,
        i: 0,
    };
    let mut value = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("trailing data at byte {}", p.i));
    }
    if let Node::Object(fields) = &mut value {
        for (k, v) in fields.iter_mut() {
            if k == "id" {
                *v = Node::Str(String::new());
            }
        }
    }
    let mut out = String::new();
    value.write(&mut out);
    Ok(sha256_id(out.as_bytes()))
}

/// A JSON tree whose numbers are their source tokens.
enum Node {
    Object(Vec<(String, Node)>),
    Array(Vec<Node>),
    Str(String),
    /// A number, `true`, `false` or `null`, verbatim.
    Token(String),
}

impl Node {
    fn write(&self, out: &mut String) {
        match self {
            Node::Object(fields) => {
                let mut sorted: Vec<&(String, Node)> = fields.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                out.push('{');
                for (i, (k, v)) in sorted.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).expect("string serializes"));
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
            Node::Array(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Node::Str(s) => out.push_str(&serde_json::to_string(s).expect("string serializes")),
            Node::Token(t) => out.push_str(t),
        }
    }
}

struct Raw<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
}

impl Raw<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<Node, String> {
        self.ws();
        match self.s.get(self.i) {
            Some(b'{') => {
                self.i += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Node::Object(fields));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return Err(format!("expected ':' at byte {}", self.i));
                    }
                    self.i += 1;
                    let v = self.value()?;
                    fields.push((key, v));
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Node::Object(fields));
                        }
                        _ => return Err(format!("expected ',' or '}}' at byte {}", self.i)),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Node::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Node::Array(items));
                        }
                        _ => return Err(format!("expected ',' or ']' at byte {}", self.i)),
                    }
                }
            }
            Some(b'"') => Ok(Node::Str(self.string()?)),
            Some(_) => {
                let start = self.i;
                while self.i < self.s.len()
                    && matches!(self.s[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E' | b'a'..=b'z')
                {
                    self.i += 1;
                }
                let token = &self.text[start..self.i];
                let valid = matches!(token, "true" | "false" | "null")
                    || serde_json::from_str::<serde_json::Number>(token).is_ok();
                if token.is_empty() || !valid {
                    return Err(format!("invalid token {token:?} at byte {start}"));
                }
                Ok(Node::Token(token.to_string()))
            }
            None => Err("unexpected end of document".to_string()),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.s.get(self.i) != Some(&b'"') {
            return Err(format!("expected a string at byte {}", self.i));
        }
        let start = self.i;
        self.i += 1;
        while self.i < self.s.len() {
            match self.s[self.i] {
                b'\\' => self.i += 2,
                b'"' => {
                    self.i += 1;
                    // Decoding a string literal involves no floats: exact.
                    return serde_json::from_str(&self.text[start..self.i])
                        .map_err(|e| e.to_string());
                }
                _ => self.i += 1,
            }
        }
        Err("unterminated string".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_does_not_change_identity() {
        let a: Value =
            serde_json::from_str(r#"{"b":1,"a":{"y":2,"x":[3,{"q":1,"p":2}]},"id":"z"}"#).unwrap();
        let b: Value =
            serde_json::from_str(r#"{"id":"other","a":{"x":[3,{"p":2,"q":1}],"y":2},"b":1}"#)
                .unwrap();
        assert_eq!(document_id(&a), document_id(&b));
        assert_eq!(canonical(&json!({"b":1,"a":2})), r#"{"a":2,"b":1}"#);
    }

    #[test]
    fn text_identity_keeps_number_tokens_and_matches_value_identity() {
        let v = json!({"b": [1, 2.5, -0.015625, 123.79888916015625, 1e3, true, null], "a": "x\u{e9}\"", "id": "q"});
        let text = pretty(&v);
        assert_eq!(document_id_from_text(&text).unwrap(), document_id(&v));
        // A 17-digit float survives even where a lossy parser would not.
        let t = r#"{"id":"","w":123.79888916015625}"#;
        assert_eq!(
            document_id_from_text(t).unwrap(),
            sha256_id(br#"{"id":"","w":123.79888916015625}"#)
        );
        assert!(document_id_from_text("{\"a\":1} x").is_err());
        assert!(document_id_from_text("{\"a\":nope}").is_err());
    }

    #[test]
    fn known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
