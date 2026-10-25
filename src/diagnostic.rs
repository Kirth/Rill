use crate::types::{ErrorKind, ErrorValue};
use miette::{Diagnostic, LabeledSpan, SourceCode};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A Rill diagnostic: parse, type, runtime, process, or I/O error, or a
/// checker warning.  Rendered through miette.
#[derive(Debug, Clone)]
pub struct RillError {
    pub source: Option<Arc<miette::NamedSource<String>>>,
    pub message: String,
    pub span: Option<Span>,
    pub exit_code: Option<i32>,
    pub kind: ErrorKind,
    pub severity: Severity,
    /// Rarely-populated details, boxed to keep `Result<_, RillError>` small.
    extras: Option<Box<Extras>>,
}

#[derive(Debug, Clone, Default)]
struct Extras {
    /// A short suggestion shown under the report ("did you mean ...").
    help: Option<String>,
    /// Further diagnostics reported together with this one.
    related: Vec<RillError>,
    /// Overrides the code derived from `kind`.
    code: Option<&'static str>,
    /// Extra labelled spans, e.g. "this block was opened here".
    secondary: Vec<(Span, String)>,
}

impl RillError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
            source: None,
            exit_code: None,
            kind: ErrorKind::Runtime,
            severity: Severity::Error,
            extras: None,
        }
    }

    fn extras(&mut self) -> &mut Extras {
        self.extras.get_or_insert_with(Default::default)
    }

    pub fn spanned(message: impl Into<String>, span: Span) -> Self {
        Self::new(message).with_span(span)
    }

    pub fn warning(message: impl Into<String>, code: &'static str) -> Self {
        let mut warning = Self::new(message);
        warning.severity = Severity::Warning;
        warning.extras().code = Some(code);
        warning
    }

    pub fn help_text(&self) -> Option<&str> {
        self.extras.as_ref().and_then(|extras| extras.help.as_deref())
    }

    pub fn related_errors(&self) -> &[RillError] {
        self.extras.as_ref().map_or(&[], |extras| extras.related.as_slice())
    }

    pub fn exit(code: i32) -> Self {
        let mut error = Self::new(format!("exit {code}"));
        error.exit_code = Some(code);
        error
    }

    pub fn with_kind(mut self, kind: ErrorKind) -> Self {
        self.kind = kind;
        self
    }

    /// Attach a span unless one is already present, so the innermost
    /// (most precise) location wins.
    pub fn with_span(mut self, span: Span) -> Self {
        if self.span.is_none() {
            self.span = Some(span);
        }
        self
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.extras().help = Some(help.into());
        self
    }

    /// Attach help only when there is some, keeping call sites tidy.
    pub fn with_optional_help(self, help: Option<String>) -> Self {
        match help {
            Some(help) => self.with_help(help),
            None => self,
        }
    }

    pub fn with_code(mut self, code: &'static str) -> Self {
        self.extras().code = Some(code);
        self
    }

    pub fn with_related(mut self, related: Vec<RillError>) -> Self {
        self.extras().related = related;
        self
    }

    /// Move every span by `offset`, for diagnostics produced while parsing an
    /// embedded fragment such as a `{...}` interpolation.
    pub fn shift_spans(mut self, offset: usize) -> Self {
        let shift = |span: Span| Span::new(span.start + offset, span.end + offset);
        self.span = self.span.map(shift);
        if let Some(extras) = &mut self.extras {
            for (span, _) in &mut extras.secondary {
                *span = shift(*span);
            }
            let related = std::mem::take(&mut extras.related);
            extras.related = related.into_iter().map(|e| e.shift_spans(offset)).collect();
        }
        self
    }

    /// Add a secondary labelled span to the report.
    pub fn with_label(mut self, span: Span, text: impl Into<String>) -> Self {
        self.extras().secondary.push((span, text.into()));
        self
    }

    /// The secondary labelled spans, e.g. "this block was opened here".
    pub fn labels(&self) -> &[(Span, String)] {
        self.extras.as_ref().map(|extras| extras.secondary.as_slice()).unwrap_or(&[])
    }

    pub fn from_error_value(error: &ErrorValue) -> Self {
        let mut diagnostic =
            Self::new(format!("{}: {}", error_kind_name(&error.kind), error.message))
                .with_kind(error.kind.clone());
        if let Some((start, end)) = error.span {
            diagnostic = diagnostic.with_span(Span::new(start, end));
        }
        diagnostic
    }

    pub fn with_source(mut self, name: impl Into<String>, source: impl Into<String>) -> Self {
        let source = Arc::new(miette::NamedSource::new(name.into(), source.into()));
        if self.source.is_none() {
            self.source = Some(source.clone());
        }
        if let Some(extras) = &mut self.extras {
            for related in &mut extras.related {
                if related.source.is_none() {
                    related.source = Some(source.clone());
                }
            }
        }
        self
    }

    /// The diagnostic code shown in reports, e.g. `rill::type`.
    pub fn code_name(&self) -> &'static str {
        let explicit = self.extras.as_ref().and_then(|extras| extras.code);
        explicit.unwrap_or(match self.kind {
            ErrorKind::Parse => "rill::parse",
            ErrorKind::Type => "rill::type",
            ErrorKind::Runtime => "rill::runtime",
            ErrorKind::Process => "rill::process",
            ErrorKind::Io => "rill::io",
        })
    }

    /// One-based line and column of the span start, when source is attached.
    pub fn location(&self) -> Option<(usize, usize)> {
        let span = self.span?;
        let source = self.source.as_ref()?;
        Some(byte_to_line_col(source.inner(), span.start))
    }

    /// One-based line and column of the span end, when source is attached.
    pub fn end_location(&self) -> Option<(usize, usize)> {
        let span = self.span?;
        let source = self.source.as_ref()?;
        Some(byte_to_line_col(source.inner(), span.end))
    }

    pub fn source_name(&self) -> Option<&str> {
        self.source.as_ref().map(|source| source.name())
    }

    /// `name:line:col: message`, for logs and machine-readable output.
    pub fn render_plain(&self) -> String {
        match (self.source_name(), self.location()) {
            (Some(name), Some((line, col))) => format!("{name}:{line}:{col}: {}", self.message),
            (Some(name), None) => format!("{name}: {}", self.message),
            _ => self.message.clone(),
        }
    }
}

/// Render a diagnostic the way the CLI does, with or without colour.
pub fn render(error: &RillError, color: bool, width: usize) -> String {
    let theme = if color {
        miette::GraphicalTheme::unicode()
    } else {
        miette::GraphicalTheme::unicode_nocolor()
    };
    let handler = miette::GraphicalReportHandler::new_themed(theme).with_width(width);
    let mut output = String::new();
    if handler.render_report(&mut output, error).is_err() {
        output = error.render_plain();
    }
    output
}

impl Diagnostic for RillError {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code_name()))
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(match self.severity {
            Severity::Error => miette::Severity::Error,
            Severity::Warning => miette::Severity::Warning,
        })
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.help_text().map(|help| Box::new(help) as Box<dyn fmt::Display>)
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        self.source.as_deref().map(|source| source as &dyn SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let to_span = |span: Span| {
            miette::SourceSpan::from((span.start, span.end.saturating_sub(span.start)))
        };
        let primary = self.span.map(|span| {
            LabeledSpan::new_primary_with_span(Some(self.message.clone()), to_span(span))
        });
        let secondary: &[(Span, String)] =
            self.extras.as_ref().map_or(&[], |extras| extras.secondary.as_slice());
        if primary.is_none() && secondary.is_empty() {
            return None;
        }
        let secondary = secondary.iter().map(move |(span, text)| {
            LabeledSpan::new_with_span(Some(text.clone()), to_span(*span))
        });
        Some(Box::new(primary.into_iter().chain(secondary)))
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn Diagnostic> + 'a>> {
        let related = self.related_errors();
        if related.is_empty() {
            return None;
        }
        Some(Box::new(related.iter().map(|related| related as &dyn Diagnostic)))
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
        // The headline is the message alone; miette prints the location in
        // the source snippet, and `render_plain` gives `name:line:col`.
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RillError {}
