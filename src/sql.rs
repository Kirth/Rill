//! The `sql.*` namespace: database handles and the statement builder that
//! turns an interpolated query into SQL text plus bind parameters.
//!
//! `db.query("select * from t where foo = $foo and n > {n}")` never splices
//! `foo` or `n` into the SQL: each interpolation becomes `$1`, `$2`, … and
//! its value travels as a parameter.  Identifiers and trusted text go in as
//! fragments (`{sql.ident(name)}`, `{sql.raw(text)}`), which are spliced.
//!
//! The Postgres driver lives in [`postgres`] behind the `postgres` feature;
//! without it the [`Db`] type exists (so scripts type-check) but cannot be
//! created.

use crate::diagnostic::RillError;
use crate::types::{Map, Value};

#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "postgres")]
pub use postgres::{ConnectOptions, Db, TlsMode};

/// A database handle; this rill was built without a driver, so none exist.
#[cfg(not(feature = "postgres"))]
#[derive(Debug, Clone, PartialEq)]
pub enum Db {}

#[cfg(not(feature = "postgres"))]
impl std::fmt::Display for Db {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {}
    }
}

/// The key marking a fragment map made by `sql.ident` / `sql.raw`.
pub const SQL_MARKER: &str = "@sql";

/// A fragment spliced verbatim into a statement.
pub fn fragment(text: String) -> Value {
    Value::map(Map::from_iter([(SQL_MARKER.to_string(), Value::Str(text))]))
}

/// The text of a fragment made by [`fragment`], if `value` is one.
pub fn fragment_text(value: &Value) -> Option<&str> {
    match value {
        Value::Map(map) if map.len() == 1 => match map.get(SQL_MARKER) {
            Some(Value::Str(text)) => Some(text),
            _ => None,
        },
        _ => None,
    }
}

/// `name` as a quoted identifier; `schema.table` quotes each part.
pub fn quote_ident(name: &str) -> String {
    name.split('.')
        .map(|part| format!("\"{}\"", part.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(".")
}

/// SQL text with its `$n` parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub text: String,
    pub params: Vec<Value>,
}

impl Statement {
    /// `{text, params}`, as `sql.statement` returns it.
    pub fn to_value(&self) -> Value {
        Value::map(Map::from_iter([
            ("text".to_string(), Value::Str(self.text.clone())),
            ("params".to_string(), Value::list(self.params.clone())),
        ]))
    }
}

/// Builds a [`Statement`] from the pieces of an interpolated string.
#[derive(Debug, Default)]
pub struct StatementBuilder {
    text: String,
    params: Vec<Value>,
    after_placeholder: bool,
}

impl StatementBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Literal SQL from the source.
    pub fn text(&mut self, text: &str) {
        if self.after_placeholder && text.starts_with(is_ident_char) {
            self.text.push(' ');
        }
        self.after_placeholder = false;
        self.text.push_str(text);
    }

    /// An interpolated value: a fragment is spliced, anything else becomes
    /// the next `$n` parameter.
    pub fn value(&mut self, value: Value) -> Result<(), RillError> {
        if let Some(text) = fragment_text(&value) {
            let text = text.to_string();
            self.text(&text);
            return Ok(());
        }
        let (state, _) = lex(&self.text);
        if let Some((place, help)) = state.inside() {
            return Err(RillError::new(format!(
                "sql: an interpolation inside {place} would not be a bind parameter"
            ))
            .with_help(help));
        }
        if self.text.ends_with(is_ident_char) || self.text.ends_with('$') {
            self.text.push(' ');
        }
        self.params.push(value);
        self.text.push_str(&format!("${}", self.params.len()));
        self.after_placeholder = true;
        Ok(())
    }

    pub fn finish(self) -> Result<Statement, RillError> {
        let (_, placeholders) = lex(&self.text);
        if !self.params.is_empty() && placeholders != self.params.len() {
            return Err(RillError::new(
                "sql: this query mixes interpolations with its own $n placeholders",
            )
            .with_help("use one or the other: interpolate every value, or pass a params list"));
        }
        Ok(Statement { text: self.text, params: self.params })
    }

    pub fn has_params(&self) -> bool {
        !self.params.is_empty()
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Where the lexer is at the end of some SQL text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Lex {
    Code,
    /// `'…'`.
    Quoted,
    /// `E'…'`, with backslash escapes.
    Escaped,
    /// `"…"`.
    Ident,
    LineComment,
    BlockComment(usize),
    /// `$tag$…$tag$`.
    Dollar(String),
}

impl Lex {
    fn inside(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Lex::Code => None,
            Lex::Quoted | Lex::Escaped => Some((
                "a quoted SQL string",
                "drop the quotes: write `= $x`, not `= '$x'`; the value is sent as a parameter \
                 and needs no SQL quoting. For a pattern, bind the whole value: \
                 `like {\"%\" + x + \"%\"}`",
            )),
            Lex::Ident => Some((
                "a quoted SQL identifier",
                "splice identifiers with {sql.ident(name)}, without the quotes",
            )),
            Lex::LineComment | Lex::BlockComment(_) => {
                Some(("a SQL comment", "write \\{ or \\$ for a literal brace or dollar"))
            }
            Lex::Dollar(_) => Some((
                "a dollar-quoted SQL body",
                "build the body as one value and interpolate that, or use {sql.raw(text)}",
            )),
        }
    }
}

/// The lexer state at the end of `text`, and how many `$n` placeholders
/// appear in code (not in strings or comments).
fn lex(text: &str) -> (Lex, usize) {
    let chars: Vec<char> = text.chars().collect();
    let at = |i: usize| chars.get(i).copied();
    let mut state = Lex::Code;
    let mut placeholders = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = at(i + 1);
        match &mut state {
            Lex::Code => match c {
                '\'' => {
                    let escaped = i > 0
                        && matches!(chars[i - 1], 'e' | 'E')
                        && (i < 2 || !is_ident_char(chars[i - 2]));
                    state = if escaped { Lex::Escaped } else { Lex::Quoted };
                }
                '"' => state = Lex::Ident,
                '-' if next == Some('-') => {
                    state = Lex::LineComment;
                    i += 1;
                }
                '/' if next == Some('*') => {
                    state = Lex::BlockComment(1);
                    i += 1;
                }
                '$' if i == 0 || !is_ident_char(chars[i - 1]) => {
                    if next.is_some_and(|n| n.is_ascii_digit()) {
                        placeholders += 1;
                        i += 1;
                        while at(i + 1).is_some_and(|n| n.is_ascii_digit()) {
                            i += 1;
                        }
                    } else {
                        let mut j = i + 1;
                        while at(j).is_some_and(is_ident_char) {
                            j += 1;
                        }
                        if at(j) == Some('$') {
                            state = Lex::Dollar(chars[i + 1..j].iter().collect());
                            i = j;
                        }
                    }
                }
                _ => {}
            },
            Lex::Quoted => {
                if c == '\'' {
                    if next == Some('\'') {
                        i += 1;
                    } else {
                        state = Lex::Code;
                    }
                }
            }
            Lex::Escaped => {
                if c == '\\' {
                    i += 1;
                } else if c == '\'' {
                    if next == Some('\'') {
                        i += 1;
                    } else {
                        state = Lex::Code;
                    }
                }
            }
            Lex::Ident => {
                if c == '"' {
                    if next == Some('"') {
                        i += 1;
                    } else {
                        state = Lex::Code;
                    }
                }
            }
            Lex::LineComment => {
                if c == '\n' {
                    state = Lex::Code;
                }
            }
            Lex::BlockComment(depth) => {
                if c == '*' && next == Some('/') {
                    *depth -= 1;
                    if *depth == 0 {
                        state = Lex::Code;
                    }
                    i += 1;
                } else if c == '/' && next == Some('*') {
                    *depth += 1;
                    i += 1;
                }
            }
            Lex::Dollar(tag) => {
                if c == '$' {
                    let len = tag.chars().count();
                    let closes = chars
                        .get(i + 1..i + 1 + len)
                        .is_some_and(|t| t.iter().copied().eq(tag.chars()))
                        && at(i + 1 + len) == Some('$');
                    if closes {
                        i += len + 1;
                        state = Lex::Code;
                    }
                }
            }
        }
        i += 1;
    }
    (state, placeholders)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(parts: &[Result<&str, Value>]) -> Result<Statement, RillError> {
        let mut builder = StatementBuilder::new();
        for part in parts {
            match part {
                Ok(text) => builder.text(text),
                Err(value) => builder.value(value.clone())?,
            }
        }
        builder.finish()
    }

    #[test]
    fn values_become_numbered_parameters() {
        let statement = build(&[
            Ok("select * from t where a = "),
            Err(Value::Int(1)),
            Ok(" and b = "),
            Err(Value::Str("x".into())),
        ])
        .unwrap();
        assert_eq!(statement.text, "select * from t where a = $1 and b = $2");
        assert_eq!(statement.params, vec![Value::Int(1), Value::Str("x".into())]);
    }

    #[test]
    fn fragments_are_spliced() {
        let statement = build(&[
            Ok("select * from "),
            Err(fragment(quote_ident("my \"table\""))),
            Ok(" where "),
            Err(fragment("true".into())),
        ])
        .unwrap();
        assert_eq!(statement.text, "select * from \"my \"\"table\"\"\" where true");
        assert!(statement.params.is_empty());
    }

    #[test]
    fn interpolations_inside_strings_and_comments_are_refused() {
        for prefix in
            ["select '%", "select E'\\'", "select \"", "select 1 -- ", "select /* ", "select $f$"]
        {
            let error = build(&[Ok(prefix), Err(Value::Int(1))]).unwrap_err();
            assert!(
                error.to_string().contains("would not be a bind parameter"),
                "{prefix}: {error}"
            );
        }
        for prefix in [
            "select 'it''s' || ",
            "select $f$ x $f$ || ",
            "select /* a /* b */ */ ",
            "select -- x\n",
        ] {
            build(&[Ok(prefix), Err(Value::Int(1))]).unwrap_or_else(|e| panic!("{prefix}: {e}"));
        }
    }

    #[test]
    fn placeholders_are_kept_apart_from_neighbouring_text() {
        let statement = build(&[Ok("select x"), Err(Value::Int(1)), Ok("0")]).unwrap();
        assert_eq!(statement.text, "select x $1 0");
    }

    #[test]
    fn mixing_with_explicit_placeholders_is_refused() {
        assert!(build(&[Ok("select $1, "), Err(Value::Int(1))]).is_err());
        assert_eq!(lex("select $1, '$2', $$ $3 $$, $10").1, 2);
    }
}
