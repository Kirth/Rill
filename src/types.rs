use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use std::borrow::Cow;
use std::str::Utf8Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    Process,
    Io,
    Runtime,
    Type,
    Parse,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ErrorValue {
    pub kind: ErrorKind,
    pub message: String,
    pub operation: Option<String>,
    pub status: Option<i32>,
    pub span: Option<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessResult {
    pub status: i32,
    pub out: Vec<u8>,
    pub err: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub value: Value,
    pub mutable: bool,
}

pub struct StreamHandle {
    rx: Arc<Mutex<mpsc::Receiver<(String, String)>>>,
}

impl StreamHandle {
    pub fn new(rx: mpsc::Receiver<(String, String)>) -> Self {
        Self { rx: Arc::new(Mutex::new(rx)) }
    }

    pub fn next(&self) -> Option<Value> {
        let rx = self.rx.lock().unwrap();
        rx.recv().ok().map(|(line, host)| {
            Value::Map(BTreeMap::from([
                ("line".to_string(), Value::Str(line)),
                ("host".to_string(), Value::Str(host)),
            ]))
        })
    }
}

impl Clone for StreamHandle {
    fn clone(&self) -> Self {
        Self { rx: Arc::clone(&self.rx) }
    }
}

impl PartialEq for StreamHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.rx, &other.rx)
    }
}

impl fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamHandle").finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Closure {
    pub function: crate::ast::Function,
    pub captured: Vec<Arc<BTreeMap<String, Binding>>>,
}

impl ProcessResult {
    pub fn ok(&self) -> bool {
        self.status == 0
    }

    pub fn stdout_text(&self) -> Result<&str, Utf8Error> {
        std::str::from_utf8(&self.out)
    }

    pub fn stderr_text(&self) -> Result<&str, Utf8Error> {
        std::str::from_utf8(&self.err)
    }

    pub fn stdout_text_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.out)
    }

    pub fn stderr_text_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.err)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
    Tuple(Vec<Value>),
    Map(BTreeMap<String, Value>),
    Duration(Duration),
    Path(PathBuf),
    Regex(String),
    Error(ErrorValue),
    Option(Option<Box<Value>>),
    Result(Result<Box<Value>, Box<Value>>),
    ProcessResult(ProcessResult),
    ProcessHandle(crate::runtime::ProcessHandle),
    Stream(StreamHandle),
    Function(crate::ast::Function),
    Closure(Closure),
}

impl Value {
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(v) => *v,
            Value::Int(v) => *v != 0,
            Value::Float(v) => *v != 0.0,
            Value::Str(v) => !v.is_empty(),
            Value::Bytes(v) => !v.is_empty(),
            Value::List(v) => !v.is_empty(),
            Value::Tuple(_) => true,
            Value::Map(v) => !v.is_empty(),
            Value::Duration(v) => !v.is_zero(),
            Value::Path(v) => !v.as_os_str().is_empty(),
            Value::Regex(v) => !v.is_empty(),
            Value::Error(_) => false,
            Value::Option(Some(v)) => v.truthy(),
            Value::Option(None) => false,
            Value::Result(Ok(v)) => v.truthy(),
            Value::Result(Err(_)) => false,
            Value::ProcessResult(v) => v.ok(),
            Value::ProcessHandle(_) | Value::Stream(_) => true,
            Value::Function(_) | Value::Closure(_) => true,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "Null",
            Value::Bool(_) => "Bool",
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Str(_) => "Str",
            Value::Bytes(_) => "Bytes",
            Value::List(_) => "List",
            Value::Tuple(_) => "Tuple",
            Value::Map(_) => "Map",
            Value::Duration(_) => "Duration",
            Value::Path(_) => "Path",
            Value::Regex(_) => "Regex",
            Value::Error(_) => "Error",
            Value::Option(_) => "Option",
            Value::Result(_) => "Result",
            Value::ProcessResult(_) => "ProcessResult",
            Value::ProcessHandle(_) => "ProcessHandle",
            Value::Stream(_) => "Stream",
            Value::Function(_) | Value::Closure(_) => "Function",
        }
    }

    pub fn as_string_lossy(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(v) => v.to_string(),
            Value::Int(v) => v.to_string(),
            Value::Float(v) => v.to_string(),
            Value::Str(v) => v.clone(),
            Value::Bytes(v) => String::from_utf8_lossy(v).into_owned(),
            Value::List(v) => v
                .iter()
                .map(Value::as_string_lossy)
                .collect::<Vec<_>>()
                .join("\n"),
            Value::Tuple(_) => format!("{self}"),
            Value::Map(_) => format!("{self}"),
            Value::Duration(v) => format!("{v:?}"),
            Value::Path(v) => v.display().to_string(),
            Value::Regex(v) => v.clone(),
            Value::Error(v) => v.message.clone(),
            Value::Option(Some(v)) => v.as_string_lossy(),
            Value::Option(None) => String::new(),
            Value::Result(Ok(v)) => v.as_string_lossy(),
            Value::Result(Err(v)) => v.as_string_lossy(),
            Value::ProcessResult(v) => v.stdout_text_lossy().into_owned(),
            Value::ProcessHandle(_) => "<process handle>".to_string(),
            Value::Stream(_) => "<stream>".to_string(),
            Value::Function(_) | Value::Closure(_) => "<function>".to_string(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "null"),
            Value::Bool(v) => write!(f, "{v}"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::Str(v) => write!(f, "{v}"),
            Value::Bytes(v) => write!(f, "{}", String::from_utf8_lossy(v)),
            Value::List(v) => {
                write!(f, "[")?;
                for (idx, item) in v.iter().enumerate() {
                    if idx > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Value::Tuple(v) => {
                write!(f, "(")?;
                for (idx, item) in v.iter().enumerate() {
                    if idx > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, ")")
            }
            Value::Map(v) => {
                write!(f, "{{")?;
                for (idx, (key, value)) in v.iter().enumerate() {
                    if idx > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{key}: {value}")?;
                }
                write!(f, "}}")
            }
            Value::Duration(v) => write!(f, "{v:?}"),
            Value::Path(v) => write!(f, "{}", v.display()),
            Value::Regex(v) => write!(f, "/{v}/"),
            Value::Error(v) => write!(f, "{}: {}", error_kind_name(&v.kind), v.message),
            Value::Option(Some(v)) => write!(f, "some({v})"),
            Value::Option(None) => write!(f, "none"),
            Value::Result(Ok(v)) => write!(f, "ok({v})"),
            Value::Result(Err(v)) => write!(f, "err({v})"),
            Value::ProcessResult(v) => write!(f, "{}", v.stdout_text_lossy()),
            Value::ProcessHandle(_) => write!(f, "<process handle>"),
            Value::Stream(_) => write!(f, "<stream>"),
            Value::Function(_) | Value::Closure(_) => write!(f, "<function>"),
        }
    }
}

fn error_kind_name(kind: &ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Process => "process error",
        ErrorKind::Io => "I/O error",
        ErrorKind::Runtime => "runtime error",
        ErrorKind::Type => "type error",
        ErrorKind::Parse => "parse error",
    }
}
