use crate::ast::*;
use crate::diagnostic::{RillError, Span};
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq)]
enum TokenKind {
    Ident(String),
    Number(String),
    Str(Vec<RawPart>),
    Regex(String),
    Substitution { pattern: String, replacement: String, all: bool },
    Word(String),
    CommandText(String),
    CommandString(Vec<RawPart>),
    CommandVar(String),
    CommandSplat(String),
    CommandVarDefault(String, Vec<RawPart>),
    CommandRedirect(RedirectKind),
    Let,
    Use,
    Args,
    Input,
    With,
    Fn,
    For,
    In,
    If,
    Else,
    Return,
    Continue,
    Break,
    While,
    Var,
    Emit,
    On,
    Run,
    MatchKw,
    True,
    False,
    Null,
    TypeIdent(String),
    Pipe,
    ValuePipe,
    Eq,
    EqEq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Match,
    FatArrow,
    Plus,
    Minus,
    Star,
    StarStar,
    Slash,
    Percent,
    Bang,
    AndAnd,
    OrOr,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    Question,
    QuestionQuestion,
    Dollar,
    Dot,
    DotDot,
    DotDotEq,
    Comma,
    Colon,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Newline,
    Semi,
    Eof,
}

/// A piece of a string template as the lexer sees it.  Expression parts are
/// kept as source text and parsed by the parser so their spans line up.
#[derive(Debug, Clone, PartialEq)]
enum RawPart {
    Text(String),
    Expr { source: String, offset: usize },
    Var(String),
    VarDefault(String, Vec<RawPart>),
}

impl TokenKind {
    /// Human-readable name for diagnostics.
    fn describe(&self) -> String {
        let keyword = |name: &str| format!("keyword `{name}`");
        let symbol = |text: &str| format!("`{text}`");
        match self {
            TokenKind::Ident(name) => format!("identifier `{name}`"),
            TokenKind::TypeIdent(name) => format!("type name `{name}`"),
            TokenKind::Number(text) => format!("number `{text}`"),
            TokenKind::Str(_) => "string literal".to_string(),
            TokenKind::Regex(_) => "regex literal".to_string(),
            TokenKind::Substitution { .. } => "regex substitution".to_string(),
            TokenKind::Word(text) => symbol(text),
            TokenKind::CommandText(text) => format!("command word `{text}`"),
            TokenKind::CommandString(_) => "quoted command argument".to_string(),
            TokenKind::CommandVar(name) => format!("`${name}`"),
            TokenKind::CommandSplat(name) => format!("`$*{name}`"),
            TokenKind::CommandVarDefault(name, _) => format!("`${{{name}:-...}}`"),
            TokenKind::CommandRedirect(kind) => format!(
                "redirection `{}`",
                match kind {
                    RedirectKind::Stdout => ">",
                    RedirectKind::Stderr => "err>",
                    RedirectKind::All => "all>",
                    RedirectKind::Stdin => "<",
                }
            ),
            TokenKind::Let => keyword("let"),
            TokenKind::Use => keyword("use"),
            TokenKind::Args => keyword("args"),
            TokenKind::Input => keyword("input"),
            TokenKind::With => keyword("with"),
            TokenKind::Fn => keyword("fn"),
            TokenKind::For => keyword("for"),
            TokenKind::In => keyword("in"),
            TokenKind::If => keyword("if"),
            TokenKind::Else => keyword("else"),
            TokenKind::Return => keyword("return"),
            TokenKind::Continue => keyword("continue"),
            TokenKind::Break => keyword("break"),
            TokenKind::While => keyword("while"),
            TokenKind::Var => keyword("var"),
            TokenKind::Emit => keyword("emit"),
            TokenKind::On => keyword("on"),
            TokenKind::Run => keyword("run"),
            TokenKind::MatchKw => keyword("match"),
            TokenKind::True => keyword("true"),
            TokenKind::False => keyword("false"),
            TokenKind::Null => keyword("null"),
            TokenKind::Pipe => symbol("|"),
            TokenKind::ValuePipe => symbol("|>"),
            TokenKind::Eq => symbol("="),
            TokenKind::EqEq => symbol("=="),
            TokenKind::Ne => symbol("!="),
            TokenKind::Gt => symbol(">"),
            TokenKind::Ge => symbol(">="),
            TokenKind::Lt => symbol("<"),
            TokenKind::Le => symbol("<="),
            TokenKind::Match => symbol("=~"),
            TokenKind::FatArrow => symbol("=>"),
            TokenKind::Plus => symbol("+"),
            TokenKind::Minus => symbol("-"),
            TokenKind::Star => symbol("*"),
            TokenKind::StarStar => symbol("**"),
            TokenKind::Slash => symbol("/"),
            TokenKind::Percent => symbol("%"),
            TokenKind::Bang => symbol("!"),
            TokenKind::AndAnd => symbol("&&"),
            TokenKind::OrOr => symbol("||"),
            TokenKind::PlusEq => symbol("+="),
            TokenKind::MinusEq => symbol("-="),
            TokenKind::StarEq => symbol("*="),
            TokenKind::SlashEq => symbol("/="),
            TokenKind::PercentEq => symbol("%="),
            TokenKind::Question => symbol("?"),
            TokenKind::QuestionQuestion => symbol("??"),
            TokenKind::Dollar => symbol("$"),
            TokenKind::Dot => symbol("."),
            TokenKind::DotDot => symbol(".."),
            TokenKind::DotDotEq => symbol("..="),
            TokenKind::Comma => symbol(","),
            TokenKind::Colon => symbol(":"),
            TokenKind::LParen => symbol("("),
            TokenKind::RParen => symbol(")"),
            TokenKind::LBrace => symbol("{"),
            TokenKind::RBrace => symbol("}"),
            TokenKind::LBracket => symbol("["),
            TokenKind::RBracket => symbol("]"),
            TokenKind::Newline => "end of line".to_string(),
            TokenKind::Semi => symbol(";"),
            TokenKind::Eof => "end of file".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Token {
    kind: TokenKind,
    span: Span,
}

pub fn parse(source: &str) -> Result<Program, RillError> {
    let tokens = lex(source)?;
    Parser::new(tokens).parse_program()
}

fn lex(source: &str) -> Result<Vec<Token>, RillError> {
    let mut tokens = Vec::new();
    let mut chars = source.char_indices().peekable();
    let mut at_line_start = true;
    let mut command_mode = false;

    if source.starts_with("#!") {
        for (_, ch) in chars.by_ref() {
            if ch == '\n' {
                tokens.push(Token { kind: TokenKind::Newline, span: Span::new(0, 1) });
                break;
            }
        }
    }

    while let Some((start, ch)) = chars.next() {
        if command_mode {
            match ch {
                ' ' | '\t' | '\r' => continue,
                '\n' => {
                    command_mode = false;
                    at_line_start = true;
                    tokens.push(Token {
                        kind: TokenKind::Newline,
                        span: Span::new(start, start + 1),
                    });
                    continue;
                }
                ';' => {
                    command_mode = false;
                    tokens.push(Token {
                        kind: TokenKind::Semi,
                        span: Span::new(
                            start,
                            chars.peek().map_or(source.len(), |(index, _)| *index),
                        ),
                    });
                    continue;
                }
                '|' => {
                    command_mode = false;
                    let (kind, end) = if matches!(chars.peek(), Some((_, '>'))) {
                        let (idx, next) = chars.next().expect("peeked token exists");
                        (TokenKind::ValuePipe, idx + next.len_utf8())
                    } else {
                        (TokenKind::Pipe, start + 1)
                    };
                    tokens.push(Token { kind, span: Span::new(start, end) });
                    continue;
                }
                '}' => {
                    command_mode = false;
                    tokens.push(Token {
                        kind: TokenKind::RBrace,
                        span: Span::new(
                            start,
                            chars.peek().map_or(source.len(), |(index, _)| *index),
                        ),
                    });
                    continue;
                }
                '{' => {
                    command_mode = false;
                    tokens.push(Token {
                        kind: TokenKind::LBrace,
                        span: Span::new(
                            start,
                            chars.peek().map_or(source.len(), |(index, _)| *index),
                        ),
                    });
                    continue;
                }
                '#' => {
                    while let Some((_, next)) = chars.peek().copied() {
                        if next == '\n' {
                            break;
                        }
                        chars.next();
                    }
                    continue;
                }
                '"' => {
                    let (text, end) = lex_string_body(source, &mut chars, start)?;
                    tokens.push(Token {
                        kind: TokenKind::CommandString(text),
                        span: Span::new(start, end),
                    });
                    continue;
                }
                '>' => {
                    tokens.push(Token {
                        kind: TokenKind::CommandRedirect(RedirectKind::Stdout),
                        span: Span::new(start, start + 1),
                    });
                    continue;
                }
                '<' => {
                    tokens.push(Token {
                        kind: TokenKind::CommandRedirect(RedirectKind::Stdin),
                        span: Span::new(start, start + 1),
                    });
                    continue;
                }
                '$' => {
                    // `${NAME}` or `${NAME:-default}` – balanced brace form.
                    if matches!(chars.peek(), Some((_, '{'))) {
                        chars.next(); // consume '{'
                        let (name, default, end) = lex_braced_var(&mut chars, start)?;
                        let kind = match default {
                            Some(d) => {
                                let offset = end - 1 - d.len();
                                let (parts, _) = lex_template(&d, offset, false)?;
                                TokenKind::CommandVarDefault(name, parts)
                            }
                            None => TokenKind::CommandVar(name),
                        };
                        tokens.push(Token { kind, span: Span::new(start, end) });
                        continue;
                    }
                    // `$*name` — word-split splat form.
                    let splat = matches!(chars.peek(), Some((_, '*')));
                    if splat {
                        chars.next(); // consume '*'
                    }
                    // Plain `$name` or `$*name` form.
                    let mut name = String::new();
                    let mut end = start + 1;
                    while let Some((idx, next)) = chars.peek().copied() {
                        if next.is_ascii_alphanumeric() || next == '_' {
                            chars.next();
                            name.push(next);
                            end = idx + next.len_utf8();
                        } else {
                            break;
                        }
                    }
                    if name.is_empty() {
                        return Err(RillError::spanned(
                            "expected identifier after $",
                            Span::new(start, end),
                        ));
                    }
                    tokens.push(Token {
                        kind: if splat {
                            TokenKind::CommandSplat(name)
                        } else {
                            TokenKind::CommandVar(name)
                        },
                        span: Span::new(start, end),
                    });
                    continue;
                }
                _ => {
                    let mut text = String::from(ch);
                    let mut end = start + ch.len_utf8();
                    while let Some((idx, next)) = chars.peek().copied() {
                        if next.is_whitespace()
                            || next == '|'
                            || next == ';'
                            || next == '}'
                            || next == '#'
                            || next == '"'
                            || next == '$'
                        {
                            break;
                        }
                        chars.next();
                        text.push(next);
                        end = idx + next.len_utf8();
                    }
                    tokens
                        .push(Token { kind: command_text_kind(text), span: Span::new(start, end) });
                    continue;
                }
            }
        }

        match ch {
            ' ' | '\t' | '\r' => {}
            '\n' => {
                at_line_start = true;
                tokens.push(Token {
                    kind: TokenKind::Newline,
                    span: Span::new(start, chars.peek().map_or(source.len(), |(index, _)| *index)),
                });
            }
            '#' => {
                while let Some((_, next)) = chars.peek().copied() {
                    if next == '\n' {
                        break;
                    }
                    chars.next();
                }
            }
            '"' => {
                let (out, end) = lex_string_body(source, &mut chars, start)?;
                tokens.push(Token { kind: TokenKind::Str(out), span: Span::new(start, end) });
                at_line_start = false;
            }
            '/' => {
                if matches!(chars.peek(), Some((_, '/'))) {
                    while let Some((_, next)) = chars.peek().copied() {
                        if next == '\n' {
                            break;
                        }
                        chars.next();
                    }
                } else if regex_literal_context(tokens.last().map(|t| &t.kind)) {
                    let (body, end) = lex_slash_body(&mut chars, start)?;
                    tokens
                        .push(Token { kind: TokenKind::Regex(body), span: Span::new(start, end) });
                    at_line_start = false;
                } else if matches!(chars.peek(), Some((_, '='))) {
                    chars.next();
                    push(
                        &mut tokens,
                        TokenKind::SlashEq,
                        start,
                        chars.peek().map_or(source.len(), |(index, _)| *index),
                    );
                    at_line_start = false;
                } else {
                    push(
                        &mut tokens,
                        TokenKind::Slash,
                        start,
                        chars.peek().map_or(source.len(), |(index, _)| *index),
                    );
                    at_line_start = false;
                }
            }
            '0'..='9' => {
                let mut text = String::from(ch);
                let mut end = start + 1;
                while let Some((idx, next)) = chars.peek().copied() {
                    let decimal_point = next == '.'
                        && !text.contains('.')
                        && text.chars().all(|ch| ch.is_ascii_digit())
                        && chars.clone().nth(1).is_some_and(|(_, ch)| ch.is_ascii_digit());
                    if next.is_ascii_alphanumeric() || decimal_point {
                        chars.next();
                        text.push(next);
                        end = idx + next.len_utf8();
                    } else {
                        break;
                    }
                }
                tokens.push(Token { kind: TokenKind::Number(text), span: Span::new(start, end) });
                at_line_start = false;
            }
            'r' if matches!(chars.peek(), Some((_, '"'))) => {
                // Raw string: no escapes, no interpolation.
                chars.next();
                let mut raw = String::new();
                let mut end = start + 2;
                let mut closed = false;
                for (idx, next) in chars.by_ref() {
                    end = idx + next.len_utf8();
                    if next == '"' {
                        closed = true;
                        break;
                    }
                    raw.push(next);
                }
                if !closed {
                    return Err(RillError::spanned(
                        "unterminated raw string",
                        Span::new(start, end),
                    ));
                }
                let parts = if raw.is_empty() { Vec::new() } else { vec![RawPart::Text(raw)] };
                tokens.push(Token { kind: TokenKind::Str(parts), span: Span::new(start, end) });
                at_line_start = false;
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let mut text = String::from(ch);
                let mut end = start + 1;
                while let Some((idx, next)) = chars.peek().copied() {
                    if next.is_ascii_alphanumeric() || next == '_' {
                        chars.next();
                        text.push(next);
                        end = idx + next.len_utf8();
                    } else {
                        break;
                    }
                }
                let after_dot =
                    matches!(tokens.last().map(|token| &token.kind), Some(TokenKind::Dot));
                let is_run = text == "run" && !after_dot;
                if text == "s"
                    && matches!(tokens.last().map(|t| &t.kind), Some(TokenKind::Match))
                    && matches!(chars.peek(), Some((_, '/')))
                {
                    let (pattern, replacement, all, subst_end) =
                        lex_substitution(&mut chars, start)?;
                    tokens.push(Token {
                        kind: TokenKind::Substitution { pattern, replacement, all },
                        span: Span::new(start, subst_end),
                    });
                    at_line_start = false;
                    continue;
                }

                let kind = if after_dot {
                    TokenKind::Ident(text)
                } else {
                    match text.as_str() {
                        "let" => TokenKind::Let,
                        "use" => TokenKind::Use,
                        "args" => TokenKind::Args,
                        "input" => TokenKind::Input,
                        "with" => TokenKind::With,
                        "fn" => TokenKind::Fn,
                        "for" => TokenKind::For,
                        "in" => TokenKind::In,
                        "if" => TokenKind::If,
                        "else" => TokenKind::Else,
                        "return" => TokenKind::Return,
                        "continue" => TokenKind::Continue,
                        "break" => TokenKind::Break,
                        "while" => TokenKind::While,
                        "var" => TokenKind::Var,
                        "emit" => TokenKind::Emit,
                        "on" => TokenKind::On,
                        "run" => TokenKind::Run,
                        "match" => TokenKind::MatchKw,
                        "true" => TokenKind::True,
                        "false" => TokenKind::False,
                        "null" => TokenKind::Null,
                        "Bool" | "Int" | "Float" | "Str" | "Bytes" | "List" | "Map"
                        | "Duration" | "Path" | "Regex" | "ProcessResult" | "ProcessHandle"
                        | "Error" | "Fn" | "Option" | "Result" => TokenKind::TypeIdent(text),
                        _ => TokenKind::Ident(text),
                    }
                };
                tokens.push(Token { kind, span: Span::new(start, end) });
                if is_run {
                    command_mode = true;
                }
                at_line_start = false;
            }
            '|' => {
                let kind = if matches!(chars.peek(), Some((_, '|'))) {
                    chars.next();
                    TokenKind::OrOr
                } else if matches!(chars.peek(), Some((_, '>'))) {
                    chars.next();
                    TokenKind::ValuePipe
                } else {
                    TokenKind::Pipe
                };
                tokens.push(Token {
                    kind,
                    span: Span::new(start, chars.peek().map_or(source.len(), |(index, _)| *index)),
                });
                at_line_start = false;
            }
            '&' => {
                if matches!(chars.peek(), Some((_, '&'))) {
                    chars.next();
                } else {
                    return Err(RillError::spanned("unexpected '&'", Span::new(start, start + 1)));
                }
                tokens.push(Token {
                    kind: TokenKind::AndAnd,
                    span: Span::new(start, chars.peek().map_or(source.len(), |(index, _)| *index)),
                });
                at_line_start = false;
            }
            '=' => {
                let kind = if matches!(chars.peek(), Some((_, '='))) {
                    chars.next();
                    TokenKind::EqEq
                } else if matches!(chars.peek(), Some((_, '~'))) {
                    chars.next();
                    TokenKind::Match
                } else if matches!(chars.peek(), Some((_, '>'))) {
                    chars.next();
                    TokenKind::FatArrow
                } else {
                    TokenKind::Eq
                };
                tokens.push(Token {
                    kind,
                    span: Span::new(start, chars.peek().map_or(source.len(), |(index, _)| *index)),
                });
                at_line_start = false;
            }
            '!' => {
                let kind = if matches!(chars.peek(), Some((_, '='))) {
                    chars.next();
                    TokenKind::Ne
                } else {
                    TokenKind::Bang
                };
                tokens.push(Token {
                    kind,
                    span: Span::new(start, chars.peek().map_or(source.len(), |(index, _)| *index)),
                });
                at_line_start = false;
            }
            '>' => {
                let kind = if matches!(chars.peek(), Some((_, '='))) {
                    chars.next();
                    TokenKind::Ge
                } else {
                    TokenKind::Gt
                };
                tokens.push(Token {
                    kind,
                    span: Span::new(start, chars.peek().map_or(source.len(), |(index, _)| *index)),
                });
                at_line_start = false;
            }
            '<' => {
                let kind = if matches!(chars.peek(), Some((_, '='))) {
                    chars.next();
                    TokenKind::Le
                } else {
                    TokenKind::Lt
                };
                tokens.push(Token {
                    kind,
                    span: Span::new(start, chars.peek().map_or(source.len(), |(index, _)| *index)),
                });
                at_line_start = false;
            }
            '+' => {
                let kind = if matches!(chars.peek(), Some((_, '='))) {
                    chars.next();
                    TokenKind::PlusEq
                } else {
                    TokenKind::Plus
                };
                push(
                    &mut tokens,
                    kind,
                    start,
                    chars.peek().map_or(source.len(), |(index, _)| *index),
                );
                at_line_start = false;
            }
            '-' => {
                let kind = if matches!(chars.peek(), Some((_, '='))) {
                    chars.next();
                    TokenKind::MinusEq
                } else {
                    TokenKind::Minus
                };
                push(
                    &mut tokens,
                    kind,
                    start,
                    chars.peek().map_or(source.len(), |(index, _)| *index),
                );
                at_line_start = false;
            }
            '*' => {
                let kind = if matches!(chars.peek(), Some((_, '*'))) {
                    chars.next();
                    TokenKind::StarStar
                } else if matches!(chars.peek(), Some((_, '='))) {
                    chars.next();
                    TokenKind::StarEq
                } else {
                    TokenKind::Star
                };
                push(
                    &mut tokens,
                    kind,
                    start,
                    chars.peek().map_or(source.len(), |(index, _)| *index),
                );
                at_line_start = false;
            }
            '$' => push(
                &mut tokens,
                TokenKind::Dollar,
                start,
                chars.peek().map_or(source.len(), |(index, _)| *index),
            ),
            '?' => {
                let kind = if matches!(chars.peek(), Some((_, '?'))) {
                    chars.next();
                    TokenKind::QuestionQuestion
                } else {
                    TokenKind::Question
                };
                push(
                    &mut tokens,
                    kind,
                    start,
                    chars.peek().map_or(source.len(), |(index, _)| *index),
                );
            }
            '%' => {
                let kind = if matches!(chars.peek(), Some((_, '='))) {
                    chars.next();
                    TokenKind::PercentEq
                } else {
                    TokenKind::Percent
                };
                push(
                    &mut tokens,
                    kind,
                    start,
                    chars.peek().map_or(source.len(), |(index, _)| *index),
                );
                at_line_start = false;
            }
            '.' => {
                if chars.peek().map(|(_, c)| *c) == Some('.') {
                    let (end, _) = chars.next().unwrap();
                    if chars.peek().map(|(_, c)| *c) == Some('=') {
                        chars.next();
                        tokens.push(Token {
                            kind: TokenKind::DotDotEq,
                            span: Span::new(start, end + 2),
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::DotDot,
                            span: Span::new(start, end + 1),
                        });
                    }
                } else {
                    push(
                        &mut tokens,
                        TokenKind::Dot,
                        start,
                        chars.peek().map_or(source.len(), |(index, _)| *index),
                    );
                }
            }
            ',' => push(
                &mut tokens,
                TokenKind::Comma,
                start,
                chars.peek().map_or(source.len(), |(index, _)| *index),
            ),
            ':' => push(
                &mut tokens,
                TokenKind::Colon,
                start,
                chars.peek().map_or(source.len(), |(index, _)| *index),
            ),
            '(' => push(
                &mut tokens,
                TokenKind::LParen,
                start,
                chars.peek().map_or(source.len(), |(index, _)| *index),
            ),
            ')' => push(
                &mut tokens,
                TokenKind::RParen,
                start,
                chars.peek().map_or(source.len(), |(index, _)| *index),
            ),
            '{' => push(
                &mut tokens,
                TokenKind::LBrace,
                start,
                chars.peek().map_or(source.len(), |(index, _)| *index),
            ),
            '}' => push(
                &mut tokens,
                TokenKind::RBrace,
                start,
                chars.peek().map_or(source.len(), |(index, _)| *index),
            ),
            '[' => push(
                &mut tokens,
                TokenKind::LBracket,
                start,
                chars.peek().map_or(source.len(), |(index, _)| *index),
            ),
            ']' => push(
                &mut tokens,
                TokenKind::RBracket,
                start,
                chars.peek().map_or(source.len(), |(index, _)| *index),
            ),
            ';' => push(
                &mut tokens,
                TokenKind::Semi,
                start,
                chars.peek().map_or(source.len(), |(index, _)| *index),
            ),
            other => {
                if at_line_start {
                    return Err(RillError::spanned(
                        format!("unexpected character '{other}'"),
                        Span::new(start, start + other.len_utf8()),
                    ));
                }
                push(
                    &mut tokens,
                    TokenKind::Word(other.to_string()),
                    start,
                    chars.peek().map_or(source.len(), |(index, _)| *index),
                );
            }
        }
    }

    tokens.push(Token { kind: TokenKind::Eof, span: Span::new(source.len(), source.len()) });
    Ok(tokens)
}

fn push(tokens: &mut Vec<Token>, kind: TokenKind, start: usize, end: usize) {
    tokens.push(Token { kind, span: Span::new(start, end.max(start + 1)) });
}

/// Lex a double-quoted string whose opening quote sits at `start`; `chars`
/// is positioned just after it.  Returns the template parts and the byte
/// offset just past the closing quote.
fn lex_string_body<I>(
    source: &str,
    chars: &mut std::iter::Peekable<I>,
    start: usize,
) -> Result<(Vec<RawPart>, usize), RillError>
where
    I: Iterator<Item = (usize, char)>,
{
    let body_start = start + 1;
    let (parts, consumed) = lex_template(&source[body_start..], body_start, true)?;
    let end = body_start + consumed + 1;
    while chars.peek().is_some_and(|(idx, _)| *idx < end) {
        chars.next();
    }
    Ok((parts, end))
}

/// Lex a string template.  `text` starts right after the opening quote (or is
/// the default of `${X:-...}`) and `base` is its absolute byte offset.  With
/// `quoted`, an unescaped `"` ends the template and the returned length
/// excludes it; otherwise the whole text is consumed.
fn lex_template(text: &str, base: usize, quoted: bool) -> Result<(Vec<RawPart>, usize), RillError> {
    let mut parts = Vec::new();
    let mut buf = String::new();
    let mut chars = text.char_indices().peekable();
    fn flush(buf: &mut String, parts: &mut Vec<RawPart>) {
        if !buf.is_empty() {
            parts.push(RawPart::Text(std::mem::take(buf)));
        }
    }
    while let Some((idx, ch)) = chars.next() {
        match ch {
            '"' if quoted => {
                flush(&mut buf, &mut parts);
                return Ok((parts, idx));
            }
            '\\' => {
                let Some((esc_idx, esc)) = chars.next() else {
                    return Err(RillError::spanned(
                        "unterminated string escape",
                        Span::new(base + idx, base + idx + 1),
                    ));
                };
                match esc {
                    'n' => buf.push('\n'),
                    't' => buf.push('\t'),
                    'r' => buf.push('\r'),
                    '0' => buf.push('\0'),
                    'e' => buf.push('\x1b'),
                    '"' | '\\' | '{' | '}' | '$' => buf.push(esc),
                    'u' => {
                        let unicode_span = |end: usize| Span::new(base + idx, base + end);
                        if !matches!(chars.next(), Some((_, '{'))) {
                            return Err(RillError::spanned(
                                "unicode escapes look like `\\u{1F600}`",
                                unicode_span(esc_idx + 1),
                            ));
                        }
                        let mut hex = String::new();
                        let mut end = esc_idx + 2;
                        loop {
                            match chars.next() {
                                Some((j, '}')) => {
                                    end = j + 1;
                                    break;
                                }
                                Some((j, digit)) if digit.is_ascii_hexdigit() && hex.len() < 6 => {
                                    hex.push(digit);
                                    end = j + 1;
                                }
                                _ => {
                                    return Err(RillError::spanned(
                                        "unicode escapes look like `\\u{1F600}`",
                                        unicode_span(end),
                                    ))
                                }
                            }
                        }
                        let scalar = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
                        match scalar {
                            Some(c) => buf.push(c),
                            None => {
                                return Err(RillError::spanned(
                                    format!("`\\u{{{hex}}}` is not a valid character"),
                                    unicode_span(end),
                                ))
                            }
                        }
                    }
                    other => {
                        return Err(RillError::spanned(
                            format!("unknown string escape `\\{other}`"),
                            Span::new(base + idx, base + esc_idx + other.len_utf8()),
                        )
                        .with_help(
                            "valid escapes: \\n \\t \\r \\0 \\e \\\\ \\\" \\{ \\} \\$ \\u{...}",
                        ))
                    }
                }
            }
            '{' => {
                flush(&mut buf, &mut parts);
                let expr_start = idx + 1;
                let mut depth = 1usize;
                let mut close = None;
                while let Some((j, c)) = chars.next() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(j);
                                break;
                            }
                        }
                        // Skip a nested string literal, braces and all.
                        '"' => {
                            while let Some((_, c2)) = chars.next() {
                                match c2 {
                                    '\\' => {
                                        chars.next();
                                    }
                                    '"' => break,
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
                let Some(close) = close else {
                    return Err(RillError::spanned(
                        "unterminated interpolation; expected `}`",
                        Span::new(base + idx, base + text.len()),
                    )
                    .with_help("write `\\{` for a literal brace"));
                };
                let source = &text[expr_start..close];
                if source.trim().is_empty() {
                    // `{}` has nothing to interpolate, so it is literal text
                    // (handy for JSON such as "{}").
                    buf.push_str(&text[idx..=close]);
                    continue;
                }
                parts.push(RawPart::Expr { source: source.to_string(), offset: base + expr_start });
            }
            '$' => match chars.peek().copied() {
                Some((_, '{')) => {
                    chars.next();
                    let inner_start = idx + 2;
                    let mut depth = 1usize;
                    let mut close = None;
                    for (j, c) in chars.by_ref() {
                        match c {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    close = Some(j);
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    let Some(close) = close else {
                        return Err(RillError::spanned(
                            "unclosed `${`",
                            Span::new(base + idx, base + text.len()),
                        ));
                    };
                    let inner = &text[inner_start..close];
                    let (name, default) = match inner.find(":-") {
                        Some(at) => (&inner[..at], Some((&inner[at + 2..], inner_start + at + 2))),
                        None => (inner, None),
                    };
                    if name.is_empty()
                        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    {
                        return Err(RillError::spanned(
                            format!("invalid variable name `${{{name}}}`"),
                            Span::new(base + idx, base + close + 1),
                        )
                        .with_help("write `\\$` for a literal dollar sign"));
                    }
                    flush(&mut buf, &mut parts);
                    match default {
                        None => parts.push(RawPart::Var(name.to_string())),
                        Some((default, default_offset)) => {
                            let (default_parts, _) =
                                lex_template(default, base + default_offset, false)?;
                            parts.push(RawPart::VarDefault(name.to_string(), default_parts));
                        }
                    }
                }
                Some((_, c)) if c.is_ascii_alphabetic() || c == '_' => {
                    let mut name = String::new();
                    while let Some((_, c)) = chars.peek().copied() {
                        if c.is_ascii_alphanumeric() || c == '_' {
                            name.push(c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    flush(&mut buf, &mut parts);
                    parts.push(RawPart::Var(name));
                }
                _ => buf.push('$'),
            },
            other => buf.push(other),
        }
    }
    if quoted {
        return Err(RillError::spanned(
            "unterminated string",
            Span::new(base.saturating_sub(1), base + text.len()),
        ));
    }
    flush(&mut buf, &mut parts);
    Ok((parts, text.len()))
}

/// Parse `NAME}` or `NAME:-default}` after `${` has been consumed.
/// Returns `(name, default, end_byte_offset)`.
fn lex_braced_var<I>(
    chars: &mut std::iter::Peekable<I>,
    start: usize,
) -> Result<(String, Option<String>, usize), RillError>
where
    I: Iterator<Item = (usize, char)>,
{
    let mut name = String::new();
    let mut end = start + 2; // past '${'
    loop {
        match chars.next() {
            None => return Err(RillError::spanned("unclosed '${'", Span::new(start, end))),
            Some((idx, '}')) => {
                end = idx + 1;
                return Ok((name, None, end));
            }
            Some((idx, ':')) => {
                end = idx + 1;
                if matches!(chars.peek(), Some((_, '-'))) {
                    chars.next(); // consume '-'
                    let (default, end2) = lex_braced_default(chars, start)?;
                    return Ok((name, Some(default), end2));
                }
                name.push(':');
            }
            Some((idx, ch)) => {
                end = idx + ch.len_utf8();
                name.push(ch);
            }
        }
    }
}

/// Collect the default text up to the matching `}`, tracking brace depth.
fn lex_braced_default<I>(
    chars: &mut std::iter::Peekable<I>,
    outer_start: usize,
) -> Result<(String, usize), RillError>
where
    I: Iterator<Item = (usize, char)>,
{
    let mut result = String::new();
    let mut depth = 1usize;
    let mut end = outer_start;
    loop {
        match chars.next() {
            None => return Err(RillError::spanned("unclosed '${'", Span::new(outer_start, end))),
            Some((idx, '{')) => {
                depth += 1;
                end = idx + 1;
                result.push('{');
            }
            Some((idx, '}')) => {
                end = idx + 1;
                depth -= 1;
                if depth == 0 {
                    return Ok((result, end));
                }
                result.push('}');
            }
            Some((idx, ch)) => {
                end = idx + ch.len_utf8();
                result.push(ch);
            }
        }
    }
}

fn lex_slash_body<I>(
    chars: &mut std::iter::Peekable<I>,
    start: usize,
) -> Result<(String, usize), RillError>
where
    I: Iterator<Item = (usize, char)>,
{
    let mut body = String::new();
    let mut end = start + 1;
    let mut escaped = false;
    let mut closed = false;
    for (idx, next) in chars.by_ref() {
        end = idx + next.len_utf8();
        if escaped {
            body.push(next);
            escaped = false;
            continue;
        }
        match next {
            '\\' => {
                body.push(next);
                escaped = true;
            }
            '/' => {
                closed = true;
                break;
            }
            other => body.push(other),
        }
    }
    if !closed {
        return Err(RillError::spanned("unterminated regex literal", Span::new(start, end)));
    }
    Ok((body, end))
}

fn lex_substitution<I>(
    chars: &mut std::iter::Peekable<I>,
    start: usize,
) -> Result<(String, String, bool, usize), RillError>
where
    I: Iterator<Item = (usize, char)>,
{
    let Some((pattern_slash_idx, '/')) = chars.next() else {
        return Err(RillError::spanned(
            "expected / after substitution operator",
            Span::new(start, start + 1),
        ));
    };
    let (pattern, _) = lex_slash_body(chars, pattern_slash_idx)?;
    let (replacement, mut end) = lex_slash_body(chars, start)?;
    let mut all = false;
    while let Some((idx, flag)) = chars.peek().copied() {
        if !flag.is_ascii_alphabetic() {
            break;
        }
        chars.next();
        end = idx + flag.len_utf8();
        if flag == 'g' {
            all = true;
        }
    }
    Ok((pattern, replacement, all, end))
}

fn regex_literal_context(previous: Option<&TokenKind>) -> bool {
    matches!(
        previous,
        Some(
            TokenKind::Match
                | TokenKind::LParen
                | TokenKind::LBrace
                | TokenKind::LBracket
                | TokenKind::Comma
                | TokenKind::FatArrow
                | TokenKind::Pipe
                | TokenKind::Newline
                | TokenKind::Semi
        ) | None
    )
}

fn command_text_kind(text: String) -> TokenKind {
    match text.as_str() {
        "err>" => TokenKind::CommandRedirect(RedirectKind::Stderr),
        "all>" => TokenKind::CommandRedirect(RedirectKind::All),
        _ => TokenKind::CommandText(text),
    }
}

struct Parser {
    tokens: VecDeque<Token>,
    last_end: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens: VecDeque::from(tokens), last_end: 0 }
    }

    fn parse_program(&mut self) -> Result<Program, RillError> {
        let mut stmts = Vec::new();
        self.skip_separators();
        while !self.at(TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
            self.skip_separators();
        }
        Ok(Program { stmts })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, RillError> {
        let start = self.peek().span.start;
        let stmt = self.parse_stmt_inner()?;
        Ok(Stmt::Located { stmt: Box::new(stmt), span: Span::new(start, self.last_end) })
    }

    fn parse_stmt_inner(&mut self) -> Result<Stmt, RillError> {
        match self.peek_kind() {
            TokenKind::Let => self.parse_let(false),
            TokenKind::Var => self.parse_let(true),
            TokenKind::Use => self.parse_use(),
            TokenKind::Args if matches!(self.peek_n_kind(1), TokenKind::LBrace) => {
                self.parse_args_decl()
            }
            TokenKind::Input if self.at_input_declaration() => self.parse_input_decl(),
            TokenKind::With => self.parse_with_env(),
            TokenKind::On => self.parse_on(),
            TokenKind::Fn => self.parse_fn(),
            TokenKind::For => Ok(Stmt::Expr(self.parse_for_expr(ForMode::Sequential, None)?)),
            TokenKind::Emit => {
                self.next();
                Ok(Stmt::Emit(self.parse_expr(0)?))
            }
            TokenKind::If => Ok(Stmt::Expr(self.parse_if()?)),
            TokenKind::Return => {
                self.next();
                if self.at_separator() {
                    Ok(Stmt::Return(None))
                } else {
                    Ok(Stmt::Return(Some(self.parse_expr(0)?)))
                }
            }
            TokenKind::Continue => {
                self.next();
                Ok(Stmt::Continue)
            }
            TokenKind::Break => {
                self.next();
                if self.at_separator() || self.at(TokenKind::RBrace) || self.at(TokenKind::Eof) {
                    Ok(Stmt::Break(None))
                } else {
                    Ok(Stmt::Break(Some(self.parse_expr(0)?)))
                }
            }
            TokenKind::While => {
                self.next();
                let cond = self.parse_expr(0)?;
                let body = self.parse_block()?;
                Ok(Stmt::While { cond: Box::new(cond), body })
            }
            TokenKind::Ident(name) if name == "env" && self.is_env_assignment() => {
                self.parse_env_assignment()
            }
            TokenKind::Ident(_) if self.is_assignment() => self.parse_assignment(),
            _ => Ok(Stmt::Expr(self.parse_expr(0)?)),
        }
    }

    fn at_input_declaration(&self) -> bool {
        matches!(self.peek_n_kind(1), TokenKind::Ident(name) if name == "lines")
    }

    fn parse_input_decl(&mut self) -> Result<Stmt, RillError> {
        self.expect(TokenKind::Input)?;
        let kind = self.expect_ident()?;
        if kind != "lines" {
            return Err(RillError::spanned("expected 'lines' after input", self.peek().span));
        }
        Ok(Stmt::InputLines)
    }

    fn parse_use(&mut self) -> Result<Stmt, RillError> {
        self.expect(TokenKind::Use)?;
        let mut path = String::new();
        loop {
            let token = self.next();
            match token.kind {
                TokenKind::Ident(name) | TokenKind::TypeIdent(name) | TokenKind::Word(name) => {
                    path.push_str(&name)
                }
                TokenKind::Dot => path.push('.'),
                TokenKind::DotDot => path.push_str(".."),
                TokenKind::Slash => path.push('/'),
                other => {
                    return Err(RillError::spanned(
                        format!("expected module name, found {}", other.describe()),
                        token.span,
                    ))
                }
            }
            if self.at_separator() || self.at(TokenKind::Eof) {
                break;
            }
        }
        Ok(Stmt::Use(path))
    }

    fn parse_args_decl(&mut self) -> Result<Stmt, RillError> {
        self.expect(TokenKind::Args)?;
        Ok(Stmt::Args(self.parse_arg_group("root".to_string())?))
    }

    fn parse_arg_group(&mut self, group_name: String) -> Result<ArgGroup, RillError> {
        self.expect(TokenKind::LBrace)?;
        self.skip_separators();
        let mut fields = Vec::new();
        let mut subcommands = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let name = self.expect_ident()?;
            if self.at(TokenKind::LBrace) {
                subcommands.push(self.parse_arg_group(name)?);
                self.skip_separators();
                continue;
            }
            self.expect(TokenKind::Colon)?;
            let type_name = self.parse_type_name()?;
            let default =
                if self.consume(TokenKind::Eq) { Some(self.parse_expr(0)?) } else { None };
            let short = if self.at_ident("short") {
                self.next();
                self.expect(TokenKind::Eq)?;
                Some(Self::static_string(self.parse_expr(0)?)?)
            } else {
                None
            };
            let description = if self.at_ident("description") {
                self.next();
                self.expect(TokenKind::Eq)?;
                Some(Self::static_string(self.parse_expr(0)?)?)
            } else {
                None
            };
            fields.push(ArgDecl { name, type_name, default, short, description });
            self.skip_separators();
        }
        self.expect(TokenKind::RBrace)?;
        Ok(ArgGroup { name: group_name, fields, subcommands })
    }

    fn at_ident(&self, expected: &str) -> bool {
        matches!(self.peek_kind(), TokenKind::Ident(name) if name == expected)
    }

    fn static_string(expr: Expr) -> Result<String, RillError> {
        let span = expr.span();
        match expr.unspanned() {
            Expr::Literal(Literal::Str(value)) => Ok(value.clone()),
            other => {
                let error = RillError::new(if matches!(other, Expr::Interpolated(_)) {
                    "interpolation is not allowed here; use a plain string literal".to_string()
                } else {
                    format!("expected a string literal, found {}", describe_expr(other))
                });
                Err(match span {
                    Some(span) => error.with_span(span),
                    None => error,
                })
            }
        }
    }

    fn is_env_assignment(&self) -> bool {
        matches!(
            (self.peek_n_kind(1), self.peek_n_kind(2), self.peek_n_kind(3)),
            (TokenKind::Dot, TokenKind::Ident(_), TokenKind::Eq)
        )
    }

    /// Whether the statement starting at the current identifier is an
    /// assignment to a place: `name`, `name.field`, `name[index]`, chained.
    fn is_assignment(&self) -> bool {
        let is_assign_op = |kind: &TokenKind| {
            matches!(
                kind,
                TokenKind::Eq
                    | TokenKind::PlusEq
                    | TokenKind::MinusEq
                    | TokenKind::StarEq
                    | TokenKind::SlashEq
                    | TokenKind::PercentEq
            )
        };
        let mut i = 1;
        loop {
            match self.peek_n_kind(i) {
                kind if is_assign_op(&kind) => return true,
                TokenKind::Dot => match self.peek_n_kind(i + 1) {
                    TokenKind::Ident(_) => i += 2,
                    _ => return false,
                },
                TokenKind::LBracket => {
                    let mut depth = 0usize;
                    i += 1;
                    loop {
                        match self.peek_n_kind(i) {
                            TokenKind::LBracket => depth += 1,
                            TokenKind::RBracket => {
                                if depth == 0 {
                                    i += 1;
                                    break;
                                }
                                depth -= 1;
                            }
                            TokenKind::Eof | TokenKind::Newline | TokenKind::Semi => return false,
                            _ => {}
                        }
                        i += 1;
                    }
                }
                _ => return false,
            }
        }
    }

    fn parse_assign_op(&mut self) -> Result<AssignOp, RillError> {
        let token = self.next();
        Ok(match token.kind {
            TokenKind::Eq => AssignOp::Assign,
            TokenKind::PlusEq => AssignOp::Add,
            TokenKind::MinusEq => AssignOp::Sub,
            TokenKind::StarEq => AssignOp::Mul,
            TokenKind::SlashEq => AssignOp::Div,
            TokenKind::PercentEq => AssignOp::Rem,
            other => {
                return Err(RillError::spanned(
                    format!("expected assignment operator, found {}", other.describe()),
                    token.span,
                ))
            }
        })
    }

    fn parse_assignment(&mut self) -> Result<Stmt, RillError> {
        let start = self.peek().span.start;
        let name = self.expect_ident()?;
        let mut target = Expr::Ident(name);
        loop {
            if self.consume(TokenKind::Dot) {
                let name = self.expect_ident()?;
                target = Expr::Field { base: Box::new(target), name };
            } else if self.consume(TokenKind::LBracket) {
                let index = self.parse_expr(0)?;
                self.expect(TokenKind::RBracket)?;
                target = Expr::Index { base: Box::new(target), index: Box::new(index) };
            } else {
                break;
            }
        }
        let target =
            Expr::Located { expr: Box::new(target), span: Span::new(start, self.last_end) };
        let op = self.parse_assign_op()?;
        self.skip_newlines();
        let expr = self.parse_expr(0)?;
        Ok(Stmt::Assign { target, op, expr })
    }

    fn parse_env_assignment(&mut self) -> Result<Stmt, RillError> {
        self.expect_ident()?;
        self.expect(TokenKind::Dot)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::Eq)?;
        self.skip_newlines();
        Ok(Stmt::AssignEnv { name, expr: self.parse_expr(0)? })
    }

    fn parse_with_env(&mut self) -> Result<Stmt, RillError> {
        self.expect(TokenKind::With)?;
        let target = self.expect_ident()?;
        if target != "env" {
            return Err(RillError::spanned("with currently supports only env", self.peek().span));
        }
        self.expect(TokenKind::LBrace)?;
        self.skip_separators();
        let mut bindings = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let name = self.expect_ident()?;
            self.expect(TokenKind::Eq)?;
            self.skip_newlines();
            bindings.push((name, self.parse_expr(0)?));
            self.skip_separators();
        }
        self.expect(TokenKind::RBrace)?;
        let body = self.parse_block()?;
        Ok(Stmt::WithEnv { bindings, body })
    }

    fn parse_let(&mut self, mutable: bool) -> Result<Stmt, RillError> {
        self.next(); // consume let or var
        let pattern = self.parse_binding_pattern()?;
        let type_name =
            if self.consume(TokenKind::Colon) { Some(self.parse_type_name()?) } else { None };
        self.expect(TokenKind::Eq)?;
        self.skip_newlines();
        let expr = self.parse_expr(0)?;
        Ok(Stmt::Let { pattern, type_name, expr, mutable })
    }

    fn parse_binding_pattern(&mut self) -> Result<LetPattern, RillError> {
        Ok(match self.peek_kind() {
            TokenKind::LBracket => {
                self.next();
                self.skip_newlines();
                let mut names = Vec::new();
                while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                    names.push(self.expect_ident()?);
                    self.skip_newlines();
                    if !self.consume(TokenKind::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
                self.expect(TokenKind::RBracket)?;
                LetPattern::List(names)
            }
            TokenKind::LParen => {
                let start = self.next().span;
                self.skip_newlines();
                let mut names = Vec::new();
                while !self.at(TokenKind::RParen) && !self.at(TokenKind::Eof) {
                    names.push(self.expect_ident()?);
                    self.skip_newlines();
                    if !self.consume(TokenKind::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
                self.expect(TokenKind::RParen)?;
                if names.len() < 2 {
                    return Err(RillError::spanned(
                        "tuple destructuring needs at least 2 bindings",
                        start,
                    ));
                }
                LetPattern::Tuple(names)
            }
            TokenKind::LBrace => {
                self.next();
                self.skip_newlines();
                let mut pairs = Vec::new();
                while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                    let key = self.expect_ident()?;
                    let alias = if self.consume(TokenKind::Colon) {
                        Some(self.expect_ident()?)
                    } else {
                        None
                    };
                    pairs.push((key, alias));
                    self.skip_newlines();
                    if !self.consume(TokenKind::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
                self.expect(TokenKind::RBrace)?;
                LetPattern::Map(pairs)
            }
            _ => LetPattern::Ident(self.expect_ident()?),
        })
    }

    fn parse_fn(&mut self) -> Result<Stmt, RillError> {
        self.expect(TokenKind::Fn)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::LParen)?;
        self.skip_newlines();
        let mut params = Vec::new();
        if !self.at(TokenKind::RParen) {
            loop {
                let name_token = self.peek().clone();
                let name = self.expect_ident()?;
                let type_name = if self.consume(TokenKind::Colon) {
                    Some(self.parse_type_name()?)
                } else {
                    None
                };
                let default =
                    if self.consume(TokenKind::Eq) { Some(self.parse_expr(0)?) } else { None };
                if default.is_none() && params.iter().any(|param: &Param| param.default.is_some()) {
                    return Err(RillError::spanned(
                        format!(
                            "parameter '{name}' without a default follows a parameter with one"
                        ),
                        name_token.span,
                    )
                    .with_help("move parameters with defaults to the end of the list"));
                }
                params.push(Param { name, type_name, default });
                self.skip_newlines();
                if !self.consume(TokenKind::Comma) {
                    break;
                }
                self.skip_newlines();
            }
        }
        self.expect(TokenKind::RParen)?;
        let return_type = if self.consume(TokenKind::Minus) {
            self.expect(TokenKind::Gt)?;
            Some(self.parse_type_name()?)
        } else {
            None
        };
        let body = self.parse_block()?;
        let propagation = initial_propagation(&body);
        Ok(Stmt::Fn { name, function: Function { params, return_type, body, propagation } })
    }

    fn parse_for_expr(
        &mut self,
        mode: ForMode,
        workers: Option<Box<Expr>>,
    ) -> Result<Expr, RillError> {
        self.expect(TokenKind::For)?;
        let destructuring = self.consume(TokenKind::Let);
        let pattern = self.parse_binding_pattern()?;
        let iter = if self.consume(TokenKind::In) {
            self.parse_expr(0)?
        } else if !destructuring && matches!(pattern, LetPattern::Ident(_)) {
            Expr::Field {
                base: Box::new(Expr::Ident("input".to_string())),
                name: "lines".to_string(),
            }
        } else {
            return Err(RillError::spanned(
                "destructuring for loops require 'in' followed by an expression",
                self.peek().span,
            ));
        };
        let body = self.parse_block()?;
        Ok(Expr::For { mode, workers, pattern, iter: Box::new(iter), body })
    }

    fn parse_if(&mut self) -> Result<Expr, RillError> {
        self.expect(TokenKind::If)?;
        self.parse_if_after_keyword()
    }

    /// The rest of an `if` once the keyword has been consumed.
    fn parse_if_after_keyword(&mut self) -> Result<Expr, RillError> {
        let (cond, pattern) = if self.consume(TokenKind::Let) {
            let pattern = self.parse_binding_pattern()?;
            self.expect(TokenKind::Eq)?;
            (self.parse_expr(0)?, Some(pattern))
        } else {
            (self.parse_expr(0)?, None)
        };
        let then_body = self.parse_block()?;
        let else_body = if self.consume(TokenKind::Else) {
            if self.at(TokenKind::If) {
                let start = self.peek().span.start;
                let nested = self.parse_if()?;
                let span = Span::new(start, self.last_end);
                vec![Stmt::Located {
                    stmt: Box::new(Stmt::Expr(Expr::Located { expr: Box::new(nested), span })),
                    span,
                }]
            } else {
                self.parse_block()?
            }
        } else {
            Vec::new()
        };
        Ok(Expr::If { cond: Box::new(cond), pattern, then_body, else_body })
    }

    fn parse_on(&mut self) -> Result<Stmt, RillError> {
        self.expect(TokenKind::On)?;
        let host = self.parse_expr(0)?;
        let body = self.parse_block()?;
        Ok(Stmt::On { host: Box::new(host), body })
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, RillError> {
        let open = self.expect(TokenKind::LBrace)?;
        self.skip_separators();
        let mut stmts = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            stmts.push(self.parse_stmt()?);
            self.skip_separators();
        }
        if self.at(TokenKind::Eof) {
            return Err(RillError::spanned("expected `}`, found end of file", self.peek().span)
                .with_label(open.span, "this block is never closed")
                .with_help("add a closing `}`"));
        }
        self.expect(TokenKind::RBrace)?;
        Ok(stmts)
    }

    fn parse_expr(&mut self, min_bp: u8) -> Result<Expr, RillError> {
        let start = self.peek().span.start;
        let left = self.parse_prefix()?;
        self.parse_infix(left, min_bp, start)
    }

    /// Continue parsing operators after an already-parsed left operand.
    fn parse_infix(&mut self, mut left: Expr, min_bp: u8, start: usize) -> Result<Expr, RillError> {
        loop {
            if self.at(TokenKind::Newline)
                && matches!(
                    self.peek_n_kind(1),
                    TokenKind::Pipe
                        | TokenKind::ValuePipe
                        | TokenKind::Dot
                        | TokenKind::QuestionQuestion
                )
            {
                self.next();
            }
            if self.at_separator()
                || self.at(TokenKind::RParen)
                || self.at(TokenKind::RBracket)
                || self.at(TokenKind::RBrace)
                || self.at(TokenKind::Comma)
                || self.at(TokenKind::FatArrow)
            {
                break;
            }

            if self.consume(TokenKind::Dot) {
                let name = self.expect_ident()?;
                left = Expr::Field { base: Box::new(left), name };
                continue;
            }

            if self.at(TokenKind::LParen) {
                self.next();
                self.skip_newlines();
                let mut args = Vec::new();
                if !self.at(TokenKind::RParen) {
                    loop {
                        args.push(self.parse_call_arg()?);
                        self.skip_newlines();
                        if !self.consume(TokenKind::Comma) {
                            break;
                        }
                        self.skip_newlines();
                    }
                }
                self.expect(TokenKind::RParen)?;
                left = Expr::Call { callee: Box::new(left), args };
                continue;
            }

            if self.consume(TokenKind::Question) {
                left = Expr::Try(Box::new(left));
                continue;
            }

            let Some((op, lbp, rbp)) = self.infix_binding_power() else {
                break;
            };
            if lbp < min_bp {
                break;
            }
            self.next();
            if op == ParsedOp::Binary(BinaryOp::NotIn) {
                self.next(); // the `in` after `not`
            }

            if op == ParsedOp::Match {
                let next = self.next();
                match next.kind {
                    TokenKind::Regex(pattern) => {
                        left = Expr::RegexMatch { value: Box::new(left), pattern };
                    }
                    TokenKind::Substitution { pattern, replacement, all } => {
                        left = Expr::RegexSubstitution {
                            value: Box::new(left),
                            pattern,
                            replacement,
                            all,
                        };
                    }
                    other => {
                        return Err(RillError::spanned(
                            format!(
                                "expected regex literal or substitution after `=~`, found {}",
                                other.describe()
                            ),
                            next.span,
                        ))
                    }
                };
                continue;
            }

            if op == ParsedOp::Index {
                // `[index]`, or a slice: `[a..b]`, `[..b]`, `[a..]`, `[a..=b]`, `[..]`.
                let index_start = self.peek().span.start;
                let at_range =
                    |parser: &Self| parser.at(TokenKind::DotDot) || parser.at(TokenKind::DotDotEq);
                let bound = if at_range(self) { None } else { Some(self.parse_expr(12)?) };
                if at_range(self) {
                    let inclusive = matches!(self.next().kind, TokenKind::DotDotEq);
                    let end = if self.at(TokenKind::RBracket) {
                        None
                    } else {
                        Some(Box::new(self.parse_expr(0)?))
                    };
                    self.expect(TokenKind::RBracket)?;
                    left = Expr::Slice {
                        base: Box::new(left),
                        start: bound.map(Box::new),
                        end,
                        inclusive,
                    };
                    continue;
                }
                let bound = bound.expect("a non-range index has a bound");
                let index = self.parse_infix(bound, 0, index_start)?;
                self.expect(TokenKind::RBracket)?;
                left = Expr::Index { base: Box::new(left), index: Box::new(index) };
                continue;
            }
            let right = self.parse_expr(rbp)?;
            left = match op {
                ParsedOp::Pipe(kind) => {
                    Expr::Pipe { kind, left: Box::new(left), right: Box::new(right) }
                }
                ParsedOp::Binary(op) => {
                    Expr::Binary { op, left: Box::new(left), right: Box::new(right) }
                }
                ParsedOp::Match | ParsedOp::Index => unreachable!(),
            };
        }
        Ok(Expr::Located { expr: Box::new(left), span: Span::new(start, self.last_end) })
    }

    fn parse_prefix(&mut self) -> Result<Expr, RillError> {
        let token = self.next();
        match token.kind {
            TokenKind::Args => Ok(Expr::Ident("args".to_string())),
            TokenKind::Input => Ok(Expr::Ident("input".to_string())),
            TokenKind::Number(text) => Ok(Expr::Literal(parse_number(&text, token.span)?)),
            TokenKind::Str(parts) => self.build_string(parts),
            TokenKind::Regex(text) => Ok(Expr::Literal(Literal::Regex(text))),
            TokenKind::True => Ok(Expr::Literal(Literal::Bool(true))),
            TokenKind::False => Ok(Expr::Literal(Literal::Bool(false))),
            TokenKind::Null => Ok(Expr::Literal(Literal::Null)),
            TokenKind::If => self.parse_if_after_keyword(),
            TokenKind::OrOr => {
                let body = if self.at(TokenKind::LBrace) {
                    self.parse_block()?
                } else {
                    vec![Stmt::Expr(self.parse_expr(0)?)]
                };
                let propagation = initial_propagation(&body);
                Ok(Expr::Closure { params: Vec::new(), body, propagation })
            }
            TokenKind::Ident(ref name) if name == "loop" && self.at(TokenKind::LBrace) => {
                let body = self.parse_block()?;
                Ok(Expr::Loop { body })
            }
            TokenKind::Bang => {
                let operand = self.parse_expr(17)?;
                Ok(Expr::Unary { op: UnaryOp::Not, operand: Box::new(operand) })
            }
            TokenKind::Minus => {
                let operand = self.parse_expr(17)?;
                Ok(Expr::Unary { op: UnaryOp::Neg, operand: Box::new(operand) })
            }
            TokenKind::Ident(ref name) if name == "not" => {
                let operand = self.parse_expr(17)?;
                Ok(Expr::Unary { op: UnaryOp::Not, operand: Box::new(operand) })
            }
            TokenKind::Ident(ref name) if name == "parallel" || name == "race" => {
                let mode = if name == "parallel" { ForMode::Parallel } else { ForMode::Race };
                let workers = if self.consume(TokenKind::LParen) {
                    let workers = self.parse_expr(0)?;
                    self.expect(TokenKind::RParen)?;
                    Some(Box::new(workers))
                } else {
                    None
                };
                if workers.is_some() || self.at(TokenKind::For) {
                    self.parse_for_expr(mode, workers)
                } else {
                    let arg = self.parse_expr(6)?;
                    Ok(Expr::Call { callee: Box::new(Expr::Ident(name.clone())), args: vec![arg] })
                }
            }
            TokenKind::Ident(name) => {
                if self.starts_bare_call(&name) {
                    let arg = self.parse_expr(6)?;
                    Ok(Expr::Call { callee: Box::new(Expr::Ident(name)), args: vec![arg] })
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            TokenKind::TypeIdent(name) => Ok(Expr::Ident(name)),
            TokenKind::For => {
                // `for` in expression position — `For` token already consumed by self.next().
                let destructuring = self.consume(TokenKind::Let);
                let pattern = self.parse_binding_pattern()?;
                let iter = if self.consume(TokenKind::In) {
                    self.parse_expr(0)?
                } else if !destructuring && matches!(pattern, LetPattern::Ident(_)) {
                    Expr::Field {
                        base: Box::new(Expr::Ident("input".to_string())),
                        name: "lines".to_string(),
                    }
                } else {
                    return Err(RillError::spanned(
                        "destructuring for loops require 'in' followed by an expression",
                        self.peek().span,
                    ));
                };
                let body = self.parse_block()?;
                Ok(Expr::For {
                    mode: ForMode::Sequential,
                    workers: None,
                    pattern,
                    iter: Box::new(iter),
                    body,
                })
            }
            TokenKind::MatchKw => self.parse_match_expr(),
            TokenKind::Run => self.parse_command(token.span),
            TokenKind::Pipe => {
                let mut params = Vec::new();
                if !self.at(TokenKind::Pipe) {
                    loop {
                        params.push(self.expect_ident()?);
                        if !self.consume(TokenKind::Comma) {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::Pipe)?;
                let body = if self.at(TokenKind::LBrace) {
                    self.parse_block()?
                } else {
                    vec![Stmt::Expr(self.parse_expr(0)?)]
                };
                let propagation = initial_propagation(&body);
                Ok(Expr::Closure { params, body, propagation })
            }
            TokenKind::Dot => {
                if let TokenKind::Ident(name) = self.peek_kind() {
                    self.next();
                    Ok(Expr::ImplicitField(name))
                } else {
                    Ok(Expr::Implicit)
                }
            }
            TokenKind::LParen => {
                self.skip_newlines();
                if self.at(TokenKind::RParen) {
                    return Err(RillError::spanned("empty tuples are not supported", token.span));
                }
                let first = self.parse_expr(0)?;
                self.skip_newlines();
                if !self.consume(TokenKind::Comma) {
                    self.expect(TokenKind::RParen)?;
                    return Ok(first);
                }
                self.skip_newlines();
                if self.at(TokenKind::RParen) {
                    return Err(RillError::spanned(
                        "tuple literals need at least 2 elements",
                        token.span,
                    ));
                }
                let mut values = vec![first];
                loop {
                    values.push(self.parse_expr(0)?);
                    self.skip_newlines();
                    if !self.consume(TokenKind::Comma) {
                        break;
                    }
                    self.skip_newlines();
                    if self.at(TokenKind::RParen) {
                        break;
                    }
                }
                self.expect(TokenKind::RParen)?;
                Ok(Expr::Tuple(values))
            }
            TokenKind::LBracket => {
                let mut values = Vec::new();
                self.skip_newlines();
                if !self.at(TokenKind::RBracket) {
                    loop {
                        values.push(self.parse_expr(0)?);
                        self.skip_newlines();
                        if !self.consume(TokenKind::Comma) {
                            break;
                        }
                        self.skip_newlines();
                        if self.at(TokenKind::RBracket) {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RBracket)?;
                Ok(Expr::List(values))
            }
            TokenKind::LBrace if !self.brace_starts_map() => {
                // A block expression: `{ stmts; value }`, as in a match arm.
                // Hand the brace back so parse_block sees the opening token.
                self.tokens.push_front(Token { kind: TokenKind::LBrace, span: token.span });
                let body = self.parse_block()?;
                Ok(Expr::Block(body))
            }
            TokenKind::LBrace => {
                let mut pairs = Vec::new();
                self.skip_separators();
                if !self.at(TokenKind::RBrace) {
                    loop {
                        let key = match self.peek_kind() {
                            TokenKind::Str(_) => {
                                let token = self.next();
                                let TokenKind::Str(parts) = token.kind else { unreachable!() };
                                match self.build_string(parts)? {
                                    Expr::Literal(Literal::Str(text)) => text,
                                    _ => {
                                        return Err(RillError::spanned(
                                            "map keys cannot be interpolated",
                                            token.span,
                                        ))
                                    }
                                }
                            }
                            _ => self.expect_ident()?,
                        };
                        self.expect(TokenKind::Colon)?;
                        let value = self.parse_expr(0)?;
                        pairs.push((key, value));
                        self.skip_newlines();
                        if !self.consume(TokenKind::Comma) {
                            break;
                        }
                        self.skip_separators();
                        if self.at(TokenKind::RBrace) {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RBrace)?;
                Ok(Expr::Map(pairs))
            }
            other => Err(RillError::spanned(
                format!("expected expression, found {}", other.describe()),
                token.span,
            )),
        }
    }

    fn parse_call_arg(&mut self) -> Result<Expr, RillError> {
        if let TokenKind::Ident(name) = self.peek_kind() {
            if matches!(self.peek_n_kind(1), TokenKind::Eq) {
                self.next();
                self.next();
                let value = self.parse_expr(0)?;
                return Ok(Expr::NamedArg { name, value: Box::new(value) });
            }
        }
        self.parse_expr(0)
    }

    fn parse_match_expr(&mut self) -> Result<Expr, RillError> {
        let value = self.parse_expr(0)?;
        self.skip_newlines();
        self.expect(TokenKind::LBrace)?;
        self.skip_separators();
        let mut arms = Vec::new();
        while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
            let pattern = self.parse_pattern()?;
            let guard = if self.consume(TokenKind::If) { Some(self.parse_expr(0)?) } else { None };
            self.expect(TokenKind::FatArrow)?;
            let expr = self.parse_expr(0)?;
            arms.push(MatchArm { pattern, guard, expr });
            self.consume(TokenKind::Comma);
            self.skip_separators();
        }
        self.expect(TokenKind::RBrace)?;
        Ok(Expr::Match { value: Box::new(value), arms })
    }

    /// A match pattern, including `a | b` alternatives.
    fn parse_pattern(&mut self) -> Result<Pattern, RillError> {
        let first = self.parse_pattern_primary()?;
        if !self.at(TokenKind::Pipe) {
            return Ok(first);
        }
        let mut alternatives = vec![first];
        while self.consume(TokenKind::Pipe) {
            alternatives.push(self.parse_pattern_primary()?);
        }
        Ok(Pattern::Or(alternatives))
    }

    fn parse_pattern_primary(&mut self) -> Result<Pattern, RillError> {
        let token = self.next();
        Ok(match token.kind {
            TokenKind::Regex(pattern) => Pattern::Regex(pattern),
            TokenKind::Number(text) => Pattern::Literal(parse_number(&text, token.span)?),
            TokenKind::Minus => {
                let number = self.next();
                let TokenKind::Number(text) = number.kind else {
                    return Err(RillError::spanned(
                        format!("expected a number after `-` in pattern, found {}", number.kind.describe()),
                        number.span,
                    ));
                };
                Pattern::Literal(match parse_number(&text, number.span)? {
                    Literal::Int(value) => Literal::Int(-value),
                    Literal::Float(value) => Literal::Float(-value),
                    _ => {
                        return Err(RillError::spanned(
                            "only numbers can be negated in a pattern",
                            number.span,
                        ))
                    }
                })
            }
            TokenKind::Str(parts) => match self.build_string(parts)? {
                Expr::Literal(Literal::Str(text)) => Pattern::Literal(Literal::Str(text)),
                _ => {
                    return Err(RillError::spanned(
                        "interpolation is not allowed in a match pattern",
                        token.span,
                    )
                    .with_help("bind the value with a name and compare it in a guard"))
                }
            },
            TokenKind::True => Pattern::Literal(Literal::Bool(true)),
            TokenKind::False => Pattern::Literal(Literal::Bool(false)),
            TokenKind::Null => Pattern::Literal(Literal::Null),
            TokenKind::Ident(name) if name == "_" => Pattern::Wildcard,
            TokenKind::Ident(name) if name == "none" => Pattern::None,
            TokenKind::Ident(name) if matches!(name.as_str(), "ok" | "err" | "some") => {
                let inner = if self.consume(TokenKind::LParen) {
                    let inner = self.parse_pattern()?;
                    self.expect(TokenKind::RParen)?;
                    inner
                } else {
                    Pattern::Wildcard
                };
                match name.as_str() {
                    "ok" => Pattern::Ok(Box::new(inner)),
                    "err" => Pattern::Err(Box::new(inner)),
                    _ => Pattern::Some(Box::new(inner)),
                }
            }
            TokenKind::Ident(name) => Pattern::Bind(name),
            TokenKind::LParen => {
                let mut items = Vec::new();
                loop {
                    self.skip_newlines();
                    items.push(self.parse_pattern()?);
                    self.skip_newlines();
                    if !self.consume(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RParen)?;
                if items.len() == 1 {
                    items.pop().expect("one item")
                } else {
                    Pattern::Tuple(items)
                }
            }
            TokenKind::LBracket => {
                let mut items = Vec::new();
                let mut rest = false;
                loop {
                    self.skip_newlines();
                    if self.at(TokenKind::RBracket) {
                        break;
                    }
                    if self.consume(TokenKind::DotDot) {
                        rest = true;
                        self.skip_newlines();
                        break;
                    }
                    items.push(self.parse_pattern()?);
                    self.skip_newlines();
                    if !self.consume(TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::RBracket)?;
                Pattern::List { items, rest }
            }
            other => {
                return Err(RillError::spanned(
                    format!("expected a pattern in match arm, found {}", other.describe()),
                    token.span,
                )
                .with_help("patterns are literals, names, `_`, /regex/, (a, b), [a, ..], some(p), ok(p), err(p), or none"))
            }
        })
    }

    fn parse_command(&mut self, span: Span) -> Result<Expr, RillError> {
        let check_exit = matches!(self.peek_kind(), TokenKind::CommandText(t) if t == "!");
        if check_exit {
            self.next(); // consume the `!`
        }
        let mut argv = Vec::new();
        let mut redirects = Vec::new();
        let mut current: Vec<CommandPart> = Vec::new();
        let mut last_end: Option<usize> = None;
        while !self.at_separator()
            && !self.at(TokenKind::Pipe)
            && !self.at(TokenKind::ValuePipe)
            && !self.at(TokenKind::LBrace)
            && !self.at(TokenKind::RBrace)
            && !self.at(TokenKind::Eof)
        {
            let token = self.next();
            if let TokenKind::CommandRedirect(kind) = token.kind {
                if !current.is_empty() {
                    argv.push(parts_to_arg(std::mem::take(&mut current)));
                }
                last_end = None;
                let target = self.parse_command_redirect_target(kind, token.span)?;
                redirects.push(CommandRedirect { kind, target });
                continue;
            }
            if last_end.is_some_and(|end| end < token.span.start) && !current.is_empty() {
                argv.push(parts_to_arg(std::mem::take(&mut current)));
            }
            let end = token.span.end;
            let part = match token.kind {
                TokenKind::CommandText(v) => CommandPart::Text(v),
                TokenKind::CommandString(parts) => match self.build_string(parts)? {
                    Expr::Literal(Literal::Str(text)) => CommandPart::Text(text),
                    expr => CommandPart::Expr(expr),
                },
                TokenKind::CommandVar(v) => CommandPart::Var(v),
                TokenKind::CommandSplat(_) => {
                    // $*var must stand alone as an arg, not be fused with adjacent parts.
                    if !current.is_empty() {
                        argv.push(parts_to_arg(std::mem::take(&mut current)));
                    }
                    let TokenKind::CommandSplat(v) = token.kind else { unreachable!() };
                    argv.push(CommandArg::Splat(v));
                    last_end = None;
                    continue;
                }
                TokenKind::CommandVarDefault(name, default) => {
                    CommandPart::VarDefault(name, self.build_parts(default)?)
                }
                other => {
                    return Err(RillError::spanned(
                        format!("unexpected {} in command", other.describe()),
                        token.span,
                    ))
                }
            };
            current.push(part);
            last_end = Some(end);
        }
        if !current.is_empty() {
            argv.push(parts_to_arg(current));
        }
        let config = if self.consume(TokenKind::LBrace) {
            self.skip_separators();
            let mut entries = Vec::new();
            while !self.at(TokenKind::RBrace) && !self.at(TokenKind::Eof) {
                let name = self.expect_ident()?;
                self.expect(TokenKind::Colon)?;
                let value = self.parse_expr(0)?;
                entries.push((name, value));
                self.skip_separators();
                self.consume(TokenKind::Comma);
                self.skip_separators();
            }
            self.expect(TokenKind::RBrace)?;
            entries
        } else {
            Vec::new()
        };
        Ok(Expr::Command(CommandExpr { argv, redirects, config, check_exit, span }))
    }

    fn parse_command_redirect_target(
        &mut self,
        kind: RedirectKind,
        span: Span,
    ) -> Result<CommandArg, RillError> {
        if self.at_separator()
            || self.at(TokenKind::Pipe)
            || self.at(TokenKind::ValuePipe)
            || self.at(TokenKind::RBrace)
            || self.at(TokenKind::Eof)
        {
            return Err(RillError::spanned(
                format!(
                    "missing target for {} redirection",
                    TokenKind::CommandRedirect(kind).describe()
                ),
                span,
            ));
        }
        let token = self.next();
        match token.kind {
            TokenKind::CommandText(v) => Ok(parse_command_text_arg(v)),
            TokenKind::CommandString(parts) => match self.build_string(parts)? {
                Expr::Literal(Literal::Str(text)) => Ok(CommandArg::String(text)),
                expr => Ok(CommandArg::Expr(expr)),
            },
            TokenKind::CommandVar(v) => Ok(CommandArg::Var(v)),
            other => Err(RillError::spanned(
                format!("unexpected {} after redirection", other.describe()),
                token.span,
            )),
        }
    }

    fn infix_binding_power(&self) -> Option<(ParsedOp, u8, u8)> {
        match self.peek_kind() {
            TokenKind::Pipe => Some((ParsedOp::Pipe(PipeKind::Byte), 1, 2)),
            TokenKind::ValuePipe => Some((ParsedOp::Pipe(PipeKind::Value), 1, 2)),
            TokenKind::OrOr => Some((ParsedOp::Binary(BinaryOp::Or), 3, 4)),
            TokenKind::Ident(ref name) if name == "or" => {
                Some((ParsedOp::Binary(BinaryOp::Or), 3, 4))
            }
            TokenKind::AndAnd => Some((ParsedOp::Binary(BinaryOp::And), 5, 6)),
            TokenKind::Ident(ref name) if name == "and" => {
                Some((ParsedOp::Binary(BinaryOp::And), 5, 6))
            }
            TokenKind::Match => Some((ParsedOp::Match, 7, 8)),
            TokenKind::EqEq => Some((ParsedOp::Binary(BinaryOp::Eq), 7, 8)),
            TokenKind::Ne => Some((ParsedOp::Binary(BinaryOp::Ne), 7, 8)),
            TokenKind::Gt => Some((ParsedOp::Binary(BinaryOp::Gt), 7, 8)),
            TokenKind::Ge => Some((ParsedOp::Binary(BinaryOp::Ge), 7, 8)),
            TokenKind::Lt => Some((ParsedOp::Binary(BinaryOp::Lt), 7, 8)),
            TokenKind::Le => Some((ParsedOp::Binary(BinaryOp::Le), 7, 8)),
            TokenKind::In => Some((ParsedOp::Binary(BinaryOp::In), 7, 8)),
            TokenKind::Ident(ref name)
                if name == "not" && matches!(self.peek_n_kind(1), TokenKind::In) =>
            {
                Some((ParsedOp::Binary(BinaryOp::NotIn), 7, 8))
            }
            // Right-associative: the right side is parsed at the same power.
            TokenKind::QuestionQuestion => Some((ParsedOp::Binary(BinaryOp::Coalesce), 9, 9)),
            TokenKind::DotDot => Some((ParsedOp::Binary(BinaryOp::Range), 11, 12)),
            TokenKind::DotDotEq => Some((ParsedOp::Binary(BinaryOp::RangeInclusive), 11, 12)),
            TokenKind::Plus => Some((ParsedOp::Binary(BinaryOp::Add), 13, 14)),
            TokenKind::Minus => Some((ParsedOp::Binary(BinaryOp::Sub), 13, 14)),
            TokenKind::Star => Some((ParsedOp::Binary(BinaryOp::Mul), 15, 16)),
            TokenKind::Slash => Some((ParsedOp::Binary(BinaryOp::Div), 15, 16)),
            TokenKind::Percent => Some((ParsedOp::Binary(BinaryOp::Rem), 15, 16)),
            // Binds tighter than prefix minus (`-2 ** 2 == -4`), right-associative.
            TokenKind::StarStar => Some((ParsedOp::Binary(BinaryOp::Pow), 18, 17)),
            TokenKind::LBracket => Some((ParsedOp::Index, 19, 20)),
            _ => None,
        }
    }

    /// Turn lexed template parts into an expression: a plain literal when
    /// there is nothing to interpolate, otherwise `Expr::Interpolated`.
    fn build_string(&self, parts: Vec<RawPart>) -> Result<Expr, RillError> {
        if parts.iter().all(|part| matches!(part, RawPart::Text(_))) {
            let text = parts
                .into_iter()
                .map(|part| match part {
                    RawPart::Text(text) => text,
                    _ => unreachable!(),
                })
                .collect::<String>();
            return Ok(Expr::Literal(Literal::Str(text)));
        }
        Ok(Expr::Interpolated(self.build_parts(parts)?))
    }

    fn build_parts(&self, parts: Vec<RawPart>) -> Result<Vec<StrPart>, RillError> {
        parts
            .into_iter()
            .map(|part| {
                Ok(match part {
                    RawPart::Text(text) => StrPart::Text(text),
                    RawPart::Var(name) => StrPart::Var(name),
                    RawPart::VarDefault(name, default) => {
                        StrPart::VarDefault(name, self.build_parts(default)?)
                    }
                    RawPart::Expr { source, offset } => {
                        StrPart::Expr(parse_embedded_expr(&source, offset)?)
                    }
                })
            })
            .collect()
    }

    /// Whether the token after an identifier starts the argument of a bare
    /// call such as `print "x"`.  Whitespace matters for the ambiguous cases:
    /// `print [1, 2]` is a call but `xs[0]` is indexing, `print -1` is a call
    /// but `x - 1` and `x-1` are subtraction.
    fn starts_bare_call(&self, callee: &str) -> bool {
        let next = self.peek();
        let spaced = next.span.start > self.last_end;
        match &next.kind {
            TokenKind::Str(_)
            | TokenKind::Number(_)
            | TokenKind::Ident(_)
            | TokenKind::TypeIdent(_)
            | TokenKind::Dollar
            | TokenKind::Pipe
            | TokenKind::OrOr
            | TokenKind::Bang
            | TokenKind::Run
            | TokenKind::Args
            | TokenKind::Input
            | TokenKind::True
            | TokenKind::False
            | TokenKind::Null
            | TokenKind::MatchKw
            | TokenKind::If
            | TokenKind::For => true,
            TokenKind::LBracket => spaced,
            TokenKind::Minus => {
                let operand_adjacent =
                    self.tokens.get(1).is_some_and(|operand| operand.span.start == next.span.end);
                spaced && operand_adjacent
            }
            // `print {a: 1}`: only for output builtins, so `if ok {` stays a block.
            TokenKind::LBrace => {
                spaced && matches!(callee, "print" | "prin" | "warn" | "pretty_print" | "die")
            }
            _ => false,
        }
    }

    fn skip_separators(&mut self) {
        while self.at_separator() {
            self.next();
        }
    }

    fn skip_newlines(&mut self) {
        while self.at(TokenKind::Newline) {
            self.next();
        }
    }

    fn at_separator(&self) -> bool {
        self.at(TokenKind::Newline) || self.at(TokenKind::Semi)
    }

    fn consume(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.next();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind) -> Result<Token, RillError> {
        if self.at(kind.clone()) {
            Ok(self.next())
        } else {
            let token = self.peek().clone();
            Err(RillError::spanned(
                format!("expected {}, found {}", kind.describe(), token.kind.describe()),
                token.span,
            ))
        }
    }

    fn expect_ident(&mut self) -> Result<String, RillError> {
        let token = self.next();
        match token.kind {
            TokenKind::Ident(name) => Ok(name),
            other => Err(RillError::spanned(
                format!("expected identifier, found {}", other.describe()),
                token.span,
            )),
        }
    }

    fn parse_type_name(&mut self) -> Result<TypeName, RillError> {
        let token = self.next();
        let name = match token.kind {
            TokenKind::TypeIdent(name) | TokenKind::Ident(name) => name,
            other => {
                return Err(RillError::spanned(
                    format!("expected type name, found {}", other.describe()),
                    token.span,
                ))
            }
        };
        let type_name = match name.as_str() {
            "Null" => TypeName::Null,
            "Bool" => TypeName::Bool,
            "Int" => TypeName::Int,
            "Float" => TypeName::Float,
            "Str" => TypeName::Str,
            "Bytes" => TypeName::Bytes,
            "Map" => {
                if self.consume(TokenKind::LBracket) {
                    let key_token = self.peek().clone();
                    let key = self.parse_type_name()?;
                    if key != TypeName::Str {
                        return Err(RillError::spanned("Map keys are always Str", key_token.span)
                            .with_help("write `Map[Str, V]`"));
                    }
                    self.expect(TokenKind::Comma)?;
                    let value = self.parse_type_name()?;
                    self.expect(TokenKind::RBracket)?;
                    TypeName::Map(Some(Box::new(value)))
                } else {
                    TypeName::Map(None)
                }
            }
            "Duration" => TypeName::Duration,
            "Path" => TypeName::Path,
            "Regex" => TypeName::Regex,
            "Error" => TypeName::Error,
            "ProcessResult" => TypeName::ProcessResult,
            "ProcessHandle" => TypeName::ProcessHandle,
            "Fn" => {
                let close = if self.at(TokenKind::LBracket) {
                    Some(TokenKind::RBracket)
                } else if self.at(TokenKind::LParen) {
                    Some(TokenKind::RParen)
                } else {
                    None
                };
                let params = if let Some(close) = close {
                    self.next();
                    let mut types = Vec::new();
                    while !self.at(close.clone()) && !self.at(TokenKind::Eof) {
                        types.push(self.parse_type_name()?);
                        if !self.consume(TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(close)?;
                    Some(types)
                } else {
                    None
                };
                let return_type = if self.consume(TokenKind::Minus) {
                    self.expect(TokenKind::Gt)?;
                    Some(Box::new(self.parse_type_name()?))
                } else {
                    None
                };
                TypeName::Function { params, return_type }
            }
            "List" => {
                if self.consume(TokenKind::LBracket) {
                    let inner = self.parse_type_name()?;
                    self.expect(TokenKind::RBracket)?;
                    TypeName::List(Box::new(inner))
                } else {
                    TypeName::ListAny
                }
            }
            "Tuple" => {
                self.expect(TokenKind::LBracket)?;
                let mut elements = Vec::new();
                self.skip_newlines();
                while !self.at(TokenKind::RBracket) && !self.at(TokenKind::Eof) {
                    elements.push(self.parse_type_name()?);
                    self.skip_newlines();
                    if !self.consume(TokenKind::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
                self.expect(TokenKind::RBracket)?;
                if elements.len() < 2 {
                    return Err(RillError::spanned(
                        "Tuple needs at least 2 element types",
                        token.span,
                    ));
                }
                TypeName::Tuple(elements)
            }
            "Option" => {
                self.expect(TokenKind::LBracket)?;
                let inner = self.parse_type_name()?;
                self.expect(TokenKind::RBracket)?;
                TypeName::Option(Box::new(inner))
            }
            "Result" => {
                self.expect(TokenKind::LBracket)?;
                let inner = self.parse_type_name()?;
                let error = if self.consume(TokenKind::Comma) {
                    Some(Box::new(self.parse_type_name()?))
                } else {
                    None
                };
                self.expect(TokenKind::RBracket)?;
                TypeName::Result(Box::new(inner), error)
            }
            other => return Err(RillError::spanned(format!("unknown type '{other}'"), token.span)),
        };
        if self.consume(TokenKind::LBracket) {
            return Err(RillError::spanned(
                "generic arguments are only supported on List, Tuple, Option, and Result",
                token.span,
            ));
        }
        // `T?` is shorthand for `Option[T]`.
        if self.consume(TokenKind::Question) {
            return Ok(TypeName::Option(Box::new(type_name)));
        }
        Ok(type_name)
    }

    fn at(&self, kind: TokenKind) -> bool {
        std::mem::discriminant(&self.peek_kind()) == std::mem::discriminant(&kind)
    }

    fn peek_kind(&self) -> TokenKind {
        self.peek().kind.clone()
    }

    /// After consuming `{`: whether what follows is a map literal (`}`, or a
    /// key followed by `:`) rather than a block of statements.
    fn brace_starts_map(&self) -> bool {
        let mut n = 0;
        while matches!(self.peek_n_kind(n), TokenKind::Newline | TokenKind::Semi) {
            n += 1;
        }
        match self.peek_n_kind(n) {
            TokenKind::RBrace => true,
            TokenKind::Ident(_) | TokenKind::Str(_) => {
                matches!(self.peek_n_kind(n + 1), TokenKind::Colon)
            }
            _ => false,
        }
    }

    fn peek_n_kind(&self, n: usize) -> TokenKind {
        self.tokens.get(n).map(|token| token.kind.clone()).unwrap_or(TokenKind::Eof)
    }

    fn peek(&self) -> &Token {
        self.tokens.front().expect("parser always has EOF token")
    }

    fn next(&mut self) -> Token {
        let token = self.tokens.pop_front().expect("parser always has EOF token");
        self.last_end = token.span.end;
        token
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedOp {
    Binary(BinaryOp),
    Pipe(PipeKind),
    Match,
    Index,
}

/// `Result` when a body can propagate a failure with `?` or `run!` (not
/// counting nested functions and closures, which propagate on their own).
fn initial_propagation(body: &[Stmt]) -> PropagationCell {
    PropagationCell::new(if stmts_propagate(body) {
        Propagation::Result
    } else {
        Propagation::None
    })
}

fn stmts_propagate(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|stmt| match stmt.unspanned() {
        Stmt::Located { stmt, .. } => stmts_propagate(std::slice::from_ref(stmt)),
        Stmt::Let { expr, .. }
        | Stmt::Expr(expr)
        | Stmt::Emit(expr)
        | Stmt::AssignEnv { expr, .. }
        | Stmt::Return(Some(expr)) => expr_propagates(expr),
        Stmt::Assign { target, expr, .. } => expr_propagates(target) || expr_propagates(expr),
        Stmt::While { cond, body } => expr_propagates(cond) || stmts_propagate(body),
        Stmt::WithEnv { bindings, body } => {
            bindings.iter().any(|(_, expr)| expr_propagates(expr)) || stmts_propagate(body)
        }
        Stmt::On { host, body } => expr_propagates(host) || stmts_propagate(body),
        Stmt::Fn { .. }
        | Stmt::Use(_)
        | Stmt::Args(_)
        | Stmt::InputLines
        | Stmt::Return(None)
        | Stmt::Continue
        | Stmt::Break(None) => false,
        Stmt::Break(Some(expr)) => expr_propagates(expr),
    })
}

fn expr_propagates(expr: &Expr) -> bool {
    match expr.unspanned() {
        Expr::Try(_) => true,
        Expr::Command(command) => {
            command.check_exit
                || command.argv.iter().any(command_arg_propagates)
                || command.redirects.iter().any(|redirect| command_arg_propagates(&redirect.target))
                || command.config.iter().any(|(_, value)| expr_propagates(value))
        }
        Expr::List(items) | Expr::Tuple(items) => items.iter().any(expr_propagates),
        Expr::Map(pairs) => pairs.iter().any(|(_, value)| expr_propagates(value)),
        Expr::Call { callee, args } => expr_propagates(callee) || args.iter().any(expr_propagates),
        Expr::Field { base, .. } => expr_propagates(base),
        Expr::Index { base, index } => expr_propagates(base) || expr_propagates(index),
        Expr::Slice { base, start, end, .. } => {
            expr_propagates(base)
                || start.as_deref().is_some_and(expr_propagates)
                || end.as_deref().is_some_and(expr_propagates)
        }
        Expr::Binary { left, right, .. } | Expr::Pipe { left, right, .. } => {
            expr_propagates(left) || expr_propagates(right)
        }
        Expr::RegexMatch { value, .. }
        | Expr::RegexSubstitution { value, .. }
        | Expr::NamedArg { value, .. }
        | Expr::Unary { operand: value, .. } => expr_propagates(value),
        Expr::Match { value, arms } => {
            expr_propagates(value)
                || arms.iter().any(|arm| {
                    expr_propagates(&arm.expr) || arm.guard.as_ref().is_some_and(expr_propagates)
                })
        }
        Expr::For { iter, body, workers, .. } => {
            expr_propagates(iter)
                || stmts_propagate(body)
                || workers.as_deref().is_some_and(expr_propagates)
        }
        Expr::Block(body) | Expr::Loop { body } => stmts_propagate(body),
        Expr::If { cond, then_body, else_body, .. } => {
            expr_propagates(cond) || stmts_propagate(then_body) || stmts_propagate(else_body)
        }
        Expr::Located { expr, .. } => expr_propagates(expr),
        Expr::Interpolated(parts) => parts_propagate(parts),
        Expr::Closure { .. }
        | Expr::Literal(_)
        | Expr::Ident(_)
        | Expr::Implicit
        | Expr::ImplicitField(_) => false,
    }
}

fn command_arg_propagates(arg: &CommandArg) -> bool {
    match arg {
        CommandArg::Expr(expr) => expr_propagates(expr),
        CommandArg::Parts(parts) => parts.iter().any(|part| match part {
            CommandPart::Expr(expr) => expr_propagates(expr),
            CommandPart::VarDefault(_, default) => parts_propagate(default),
            _ => false,
        }),
        _ => false,
    }
}

fn parts_propagate(parts: &[StrPart]) -> bool {
    parts.iter().any(|part| match part {
        StrPart::Expr(expr) => expr_propagates(expr),
        StrPart::VarDefault(_, default) => parts_propagate(default),
        _ => false,
    })
}

fn describe_expr(expr: &Expr) -> &'static str {
    match expr.unspanned() {
        Expr::Literal(Literal::Str(_)) => "a string literal",
        Expr::Interpolated(_) => "an interpolated string",
        Expr::If { .. } => "an if expression",
        Expr::Slice { .. } => "a slice",
        Expr::Loop { .. } => "a loop",
        Expr::Literal(Literal::Int(_)) => "an integer literal",
        Expr::Literal(Literal::Float(_)) => "a float literal",
        Expr::Literal(Literal::Bool(_)) => "a boolean literal",
        Expr::Literal(Literal::Null) => "`null`",
        Expr::Literal(Literal::Duration(_)) => "a duration literal",
        Expr::Literal(Literal::Regex(_)) => "a regex literal",
        Expr::Ident(_) => "an identifier",
        Expr::List(_) => "a list",
        Expr::Tuple(_) => "a tuple",
        Expr::Map(_) => "a map",
        Expr::Call { .. } => "a call",
        Expr::Closure { .. } => "a closure",
        Expr::Command(_) => "a command",
        _ => "an expression",
    }
}

fn parse_number(text: &str, span: Span) -> Result<Literal, RillError> {
    for (suffix, multiplier) in [
        ("ns", 1.0),
        ("us", 1_000.0),
        ("ms", 1_000_000.0),
        ("s", 1_000_000_000.0),
        ("m", 60_000_000_000.0),
        ("h", 3_600_000_000_000.0),
    ] {
        if let Some(raw) = text.strip_suffix(suffix) {
            let nanos = raw.parse::<f64>().map_err(|_| {
                RillError::spanned(format!("invalid duration literal '{text}'"), span)
            })? * multiplier;
            if !nanos.is_finite() || nanos < 0.0 || nanos > u64::MAX as f64 {
                return Err(RillError::spanned(format!("invalid duration literal '{text}'"), span));
            }
            return Ok(Literal::Duration(std::time::Duration::from_nanos(nanos as u64)));
        }
    }
    for (suffix, multiplier) in [
        ("KiB", 1024_i64),
        ("MiB", 1024_i64.pow(2)),
        ("GiB", 1024_i64.pow(3)),
        ("KB", 1000),
        ("MB", 1000_i64.pow(2)),
        ("GB", 1000_i64.pow(3)),
        ("B", 1),
    ] {
        if let Some(raw) = text.strip_suffix(suffix) {
            let value = raw.parse::<i64>().map_err(|_| {
                RillError::spanned(format!("invalid byte-size literal '{text}'"), span)
            })?;
            return value.checked_mul(multiplier).map(Literal::Int).ok_or_else(|| {
                RillError::spanned(format!("byte-size literal '{text}' is too large"), span)
            });
        }
    }
    if text.contains('.') {
        Ok(Literal::Float(
            text.parse::<f64>()
                .map_err(|_| RillError::spanned(format!("invalid float literal '{text}'"), span))?,
        ))
    } else {
        Ok(Literal::Int(
            text.parse::<i64>()
                .map_err(|_| RillError::spanned(format!("invalid int literal '{text}'"), span))?,
        ))
    }
}

fn parts_to_arg(parts: Vec<CommandPart>) -> CommandArg {
    match parts.as_slice() {
        [CommandPart::Text(text)] => CommandArg::Word(text.clone()),
        [CommandPart::Var(name)] => CommandArg::Var(name.clone()),
        // VarDefault and multi-part args both go through Parts.
        _ => CommandArg::Parts(parts),
    }
}

fn parse_command_text_arg(text: String) -> CommandArg {
    parts_to_arg(vec![CommandPart::Text(text)])
}

/// Parse the expression inside a `{...}` interpolation.  `offset` is the
/// absolute byte offset of `source`, so every span lands in the real file.
fn parse_embedded_expr(source: &str, offset: usize) -> Result<Expr, RillError> {
    let mut tokens = lex(source).map_err(|error| error.shift_spans(offset))?;
    for token in &mut tokens {
        token.span = Span::new(token.span.start + offset, token.span.end + offset);
    }
    let mut parser = Parser::new(tokens);
    parser.skip_separators();
    let expr = parser.parse_expr(0)?;
    parser.skip_separators();
    if !parser.at(TokenKind::Eof) {
        let token = parser.peek();
        return Err(RillError::spanned(
            format!("unexpected {} in interpolation", token.kind.describe()),
            token.span,
        ));
    }
    Ok(expr)
}
