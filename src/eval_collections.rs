use crate::types::{ErrorKind, Value};
use std::collections::HashSet;
use std::hash::Hash;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum HashKey {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<HashKey>),
    Tuple(Vec<HashKey>),
    Map(Vec<(String, HashKey)>),
    Duration(std::time::Duration),
    Time(chrono::DateTime<chrono::Utc>),
    Path(PathBuf),
    Regex(String),
    Error {
        kind: u8,
        message: String,
        operation: Option<String>,
        status: Option<i32>,
        span: Option<(usize, usize)>,
    },
    Option(Option<Box<HashKey>>),
    Result(Result<Box<HashKey>, Box<HashKey>>),
}

fn hash_key(value: &Value) -> Option<HashKey> {
    Some(match value {
        Value::Null => HashKey::Null,
        Value::Bool(value) => HashKey::Bool(*value),
        Value::Int(value) => HashKey::Int(*value),
        Value::Float(_) => return None,
        Value::Str(value) => HashKey::Str(value.clone()),
        Value::Bytes(value) => HashKey::Bytes(value.clone()),
        Value::List(values) => {
            HashKey::List(values.iter().map(hash_key).collect::<Option<Vec<_>>>()?)
        }
        Value::Tuple(values) => {
            HashKey::Tuple(values.iter().map(hash_key).collect::<Option<Vec<_>>>()?)
        }
        Value::Map(values) => {
            // Maps compare order-insensitively, so the key must too.
            let mut pairs = values
                .iter()
                .map(|(key, value)| Some((key.clone(), hash_key(value)?)))
                .collect::<Option<Vec<_>>>()?;
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            HashKey::Map(pairs)
        }
        Value::Duration(value) => HashKey::Duration(*value),
        Value::Time(value) => HashKey::Time(*value),
        Value::Path(value) => HashKey::Path(value.clone()),
        Value::Regex(value) => HashKey::Regex(value.clone()),
        Value::Error(value) => HashKey::Error {
            kind: match value.kind {
                ErrorKind::Process => 0,
                ErrorKind::Io => 1,
                ErrorKind::Runtime => 2,
                ErrorKind::Type => 3,
                ErrorKind::Parse => 4,
            },
            message: value.message.clone(),
            operation: value.operation.clone(),
            status: value.status,
            span: value.span,
        },
        Value::Option(value) => HashKey::Option(match value {
            None => None,
            Some(value) => Some(Box::new(hash_key(value)?)),
        }),
        Value::Result(Ok(value)) => HashKey::Result(Ok(Box::new(hash_key(value)?))),
        Value::Result(Err(value)) => HashKey::Result(Err(Box::new(hash_key(value)?))),
        Value::ProcessResult(_)
        | Value::ProcessHandle(_)
        | Value::Stream(_)
        | Value::Function(_) => return None,
    })
}

fn hash_index(values: &[Value]) -> Option<HashSet<HashKey>> {
    values.iter().map(hash_key).collect()
}

pub(crate) fn difference_membership(a: &[Value], b: &[Value]) -> Option<Vec<bool>> {
    let index = hash_index(b)?;
    a.iter().map(|value| hash_key(value).map(|key| !index.contains(&key))).collect()
}

pub(crate) fn symmetric_membership(a: &[Value], b: &[Value]) -> Option<(Vec<bool>, Vec<bool>)> {
    let index_a = hash_index(a)?;
    let index_b = hash_index(b)?;
    let only_a = a
        .iter()
        .map(|value| hash_key(value).map(|key| !index_b.contains(&key)))
        .collect::<Option<Vec<_>>>()?;
    let only_b = b
        .iter()
        .map(|value| hash_key(value).map(|key| !index_a.contains(&key)))
        .collect::<Option<Vec<_>>>()?;
    Some((only_a, only_b))
}
