//! Reading the forges' JSON leniently: a missing or odd field is empty

use serde_json::Value;

use crate::error::{Error, Result};

/// Every JSON value in `bytes`: `gh api --paginate` prints one array per
/// page back to back. Arrays of pages are flattened into one array.
pub fn parse_pages(program: &str, bytes: &[u8]) -> Result<Vec<Value>> {
    let text = String::from_utf8_lossy(bytes);
    let mut items = Vec::new();
    for value in serde_json::Deserializer::from_str(&text).into_iter::<Value>() {
        let value = value.map_err(|e| Error::Parse {
            program: program.into(),
            message: e.to_string(),
        })?;
        match value {
            Value::Array(page) => {
                for item in page {
                    match item {
                        // `--slurp` wraps pages in another array
                        Value::Array(inner) => items.extend(inner),
                        other => items.push(other),
                    }
                }
            }
            other => items.push(other),
        }
    }
    Ok(items)
}

pub fn parse_object(program: &str, bytes: &[u8]) -> Result<Value> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| Error::Parse {
        program: program.into(),
        message: e.to_string(),
    })?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(Error::Parse {
            program: program.into(),
            message: "expected an object".into(),
        })
    }
}

/// The value at a dotted path, e.g. `author.login`
pub fn at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |v, key| v.get(key))
}

pub fn text(value: &Value, path: &str) -> String {
    match at(value, path) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

pub fn opt_text(value: &Value, path: &str) -> Option<String> {
    Some(text(value, path)).filter(|s| !s.is_empty())
}

pub fn number(value: &Value, path: &str) -> Option<u64> {
    match at(value, path)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

pub fn flag(value: &Value, path: &str) -> bool {
    matches!(at(value, path), Some(Value::Bool(true)))
}

pub fn array<'a>(value: &'a Value, path: &str) -> &'a [Value] {
    match at(value, path) {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

/// `%`-encodes a URL path segment or query value
pub fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_flattened() {
        let items = parse_pages("gh", b"[1,2][3]\n[[4],[5]]").unwrap();
        assert_eq!(items, [1, 2, 3, 4, 5].map(Value::from));
        assert!(parse_pages("gh", b"").unwrap().is_empty());
        assert!(parse_pages("gh", b"[1,").is_err());
    }

    #[test]
    fn paths_read_leniently() {
        let v: Value = serde_json::json!({"a": {"b": "x", "n": 3, "s": "4"}, "t": true});
        assert_eq!(text(&v, "a.b"), "x");
        assert_eq!(text(&v, "a.missing"), "");
        assert_eq!(number(&v, "a.n"), Some(3));
        assert_eq!(number(&v, "a.s"), Some(4));
        assert!(flag(&v, "t"));
        assert!(array(&v, "a").is_empty());
        assert_eq!(encode("GNOME/meld x"), "GNOME%2Fmeld%20x");
    }
}
