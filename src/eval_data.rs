use crate::types::Value;
pub(crate) fn json_to_value(value: serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Bool(value),
        serde_json::Value::Number(value) => value
            .as_i64().map(Value::Int)
            .or_else(|| value.as_f64().map(Value::Float))
            .unwrap_or(Value::Null),
        serde_json::Value::String(value) => Value::Str(value),
        serde_json::Value::Array(values) => Value::List(values.into_iter().map(json_to_value).collect()),
        serde_json::Value::Object(values) => Value::Map(values.into_iter().map(|(key, value)| (key, json_to_value(value))).collect()),
    }
}

pub(crate) fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(value) => serde_json::Value::Bool(*value),
        Value::Int(value) => serde_json::json!(*value),
        Value::Float(value) => serde_json::json!(*value),
        Value::Str(value) => serde_json::Value::String(value.clone()),
        Value::Bytes(value) => serde_json::Value::String(String::from_utf8_lossy(value).into_owned()),
        Value::List(values) => serde_json::Value::Array(values.iter().map(value_to_json).collect()),
        Value::Tuple(values) => serde_json::Value::Array(values.iter().map(value_to_json).collect()),
        Value::Map(values) => serde_json::Value::Object(values.iter().map(|(key, value)| (key.clone(), value_to_json(value))).collect()),
        other => serde_json::Value::String(other.as_string_lossy()),
    }
}
