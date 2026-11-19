use serde_json::Value;

/// Parses JSON strings only if they represent objects (`{...}`) or arrays (`[...]`).
/// Plain scalars (`"12345"`, `"true"`, `"null"`, `"hello"`) are returned as strings.
#[must_use]
pub fn json_safe_parse(val: &Value) -> Value {
    match val {
        Value::String(s) => {
            let trimmed = s.trim();
            if (trimmed.starts_with('{') && trimmed.ends_with('}'))
                || (trimmed.starts_with('[') && trimmed.ends_with(']'))
            {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    return json_safe_parse(&parsed);
                }
            }
            Value::String(s.clone())
        }
        Value::Array(arr) => Value::Array(arr.iter().map(json_safe_parse).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), json_safe_parse(v));
            }
            Value::Object(out)
        }
        scalar => scalar.clone(),
    }
}

/// Recursively stringifies object/array values inside maps.
#[must_use]
pub fn json_stringify(val: &Value) -> Value {
    match val {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if v.is_object() || v.is_array() {
                    #[allow(clippy::unwrap_used)]
                    out.insert(k.clone(), Value::String(serde_json::to_string(v).unwrap()));
                } else {
                    out.insert(k.clone(), v.clone());
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(json_stringify).collect()),
        scalar => scalar.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_scalar_pass_through() {
        assert_eq!(json_safe_parse(&json!("12345")), json!("12345"));
        assert_eq!(json_safe_parse(&json!("true")), json!("true"));
        assert_eq!(json_safe_parse(&json!("null")), json!("null"));
        assert_eq!(json_safe_parse(&json!("hello")), json!("hello"));
    }

    #[test]
    fn test_object_and_array_parsed() {
        assert_eq!(
            json_safe_parse(&json!(r#"{"a": 1, "b": "test"}"#)),
            json!({"a": 1, "b": "test"})
        );
        assert_eq!(json_safe_parse(&json!(r"[1, 2, 3]")), json!([1, 2, 3]));
    }

    #[test]
    fn test_big_int_fidelity() {
        let big = "9007199254740993";
        let parsed = json_safe_parse(&json!(format!(r#"{{"big": {big}}}"#)));
        assert_eq!(parsed["big"].to_string(), big);
    }
}
