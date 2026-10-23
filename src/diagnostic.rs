use crate::types::{ErrorKind, ErrorValue};
use std::fmt;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Debug, Clone, miette::Diagnostic)]
#[diagnostic(code(rill::error))]
pub struct RillError {
    #[source_code]
    pub source: Option<Arc<miette::NamedSource<String>>>,
    #[label("{message}")]
    pub label: Option<miette::SourceSpan>,
    pub message: String,
    pub span: Option<Span>,
    pub exit_code: Option<i32>,
    pub kind: ErrorKind,
}

impl RillError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
            source: None,
            label: None,
            exit_code: None,
            kind: ErrorKind::Runtime,
        }
    }

    pub fn spanned(message: impl Into<String>, span: Span) -> Self {
        let message = message.into();
        Self {
            label: Some((span.start, span.end.saturating_sub(span.start)).into()),
            message,
            span: Some(span),
            source: None,
            exit_code: None,
            kind: ErrorKind::Runtime,
        }
    }

    pub fn exit(code: i32) -> Self {
        Self {
            message: format!("exit {code}"),
            span: None,
            source: None,
            label: None,
            exit_code: Some(code),
            kind: ErrorKind::Runtime,
        }
    }

    pub fn with_kind(mut self, kind: ErrorKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn with_span(mut self, span: Span) -> Self {
        if self.span.is_none() {
            self.span = Some(span);
            self.label = Some((span.start, span.end.saturating_sub(span.start)).into());
        }
        self
    }

    pub fn from_error_value(error: &ErrorValue) -> Self {
        let mut diagnostic =
            Self::new(format!("{}: {}", error_kind_name(&error.kind), error.message))
                .with_kind(error.kind.clone());
        if let Some((start, end)) = error.span {
            diagnostic = Self::spanned(diagnostic.message, Span::new(start, end))
                .with_kind(error.kind.clone());
        }
        diagnostic
    }

    pub fn with_source(mut self, name: impl Into<String>, source: impl Into<String>) -> Self {
        let name = name.into();
        self.source = Some(Arc::new(miette::NamedSource::new(name, source.into())));
        if self.label.is_none() {
            if let Some(span) = self.span {
                self.label = Some((span.start, span.end.saturating_sub(span.start)).into());
            }
        }
        self
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

fn byte_to_line_col(source: &str, byte: usize) -> (usize, usize) {
    let before = &source[..byte.min(source.len())];
    let line = before.chars().filter(|&c| c == '\n').count() + 1;
    let col = before.rfind('\n').map(|p| byte - p - 1).unwrap_or(byte) + 1;
    (line, col)
}

impl fmt::Display for RillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(span) = self.span {
            if let Some(src) = &self.source {
                let (line, col) = byte_to_line_col(src.inner(), span.start);
                write!(f, " at {line}:{col}")?;
            } else {
                write!(f, " at {}..{}", span.start, span.end)?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for RillError {}
