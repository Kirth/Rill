use crate::diagnostic::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub params: Vec<Param>,
    pub return_type: Option<TypeName>,
    pub body: Vec<Stmt>,
    /// How an unannotated body that uses `?` or `run!` wraps its result.
    pub propagation: PropagationCell,
}

/// What an unannotated function or closure returns when its body can
/// propagate a failure with `?` or `run!`.  The parser sets `Result` when the
/// body contains either; the checker refines it to `Option` when every `?`
/// applies to an Option; a declared return type disables it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Propagation {
    None,
    Result,
    Option,
}

/// Shared, thread-safe cell so the checker can record its decision on the
/// AST the evaluator later reads.
#[derive(Debug)]
pub struct PropagationCell(std::sync::atomic::AtomicU8);

impl PropagationCell {
    pub fn new(propagation: Propagation) -> Self {
        let cell = Self(std::sync::atomic::AtomicU8::new(0));
        cell.set(propagation);
        cell
    }

    pub fn get(&self) -> Propagation {
        match self.0.load(std::sync::atomic::Ordering::Relaxed) {
            1 => Propagation::Result,
            2 => Propagation::Option,
            _ => Propagation::None,
        }
    }

    pub fn set(&self, propagation: Propagation) {
        let raw = match propagation {
            Propagation::None => 0,
            Propagation::Result => 1,
            Propagation::Option => 2,
        };
        self.0.store(raw, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Clone for PropagationCell {
    fn clone(&self) -> Self {
        Self::new(self.get())
    }
}

impl PartialEq for PropagationCell {
    fn eq(&self, other: &Self) -> bool {
        self.get() == other.get()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub type_name: Option<TypeName>,
    /// Default value, evaluated in the callee's scope when the argument is omitted.
    pub default: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArgDecl {
    pub name: String,
    pub type_name: TypeName,
    pub default: Option<Expr>,
    pub short: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArgGroup {
    pub name: String,
    pub fields: Vec<ArgDecl>,
    pub subcommands: Vec<ArgGroup>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeName {
    Null,
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    /// A list whose element type is not statically known.
    ListAny,
    List(Box<TypeName>),
    Tuple(Vec<TypeName>),
    /// `Map` or `Map[Str, V]`; keys are always strings.
    Map(Option<Box<TypeName>>),
    Duration,
    Path,
    Regex,
    Error,
    ProcessResult,
    ProcessHandle,
    Function {
        params: Option<Vec<TypeName>>,
        return_type: Option<Box<TypeName>>,
    },
    Option(Box<TypeName>),
    Result(Box<TypeName>, Option<Box<TypeName>>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum LetPattern {
    Ident(String),
    List(Vec<String>),
    Tuple(Vec<String>),
    Map(Vec<(String, Option<String>)>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// A statement with its source span; every statement the parser produces
    /// is wrapped in one.  Consumers match on `stmt.unspanned()`.
    Located {
        stmt: Box<Stmt>,
        span: Span,
    },
    Use(String),
    Let {
        pattern: LetPattern,
        type_name: Option<TypeName>,
        expr: Expr,
        mutable: bool,
    },
    AssignEnv {
        name: String,
        expr: Expr,
    },
    Args(ArgGroup),
    InputLines,
    Fn {
        name: String,
        function: Function,
    },
    WithEnv {
        bindings: Vec<(String, Expr)>,
        body: Vec<Stmt>,
    },
    On {
        host: Box<Expr>,
        body: Vec<Stmt>,
    },
    Emit(Expr),
    Return(Option<Expr>),
    Continue,
    /// `break` or `break value` (the value is only meaningful inside `loop`).
    Break(Option<Expr>),
    /// Assignment to a place: a variable, or a field/index chain rooted at one
    /// (`x = 1`, `m.a += 2`, `xs[0].name = "n"`).
    Assign {
        target: Expr,
        op: AssignOp,
        expr: Expr,
    },
    While {
        cond: Box<Expr>,
        body: Vec<Stmt>,
    },
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ForMode {
    Sequential,
    Parallel,
    Race,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Located {
        expr: Box<Expr>,
        span: Span,
    },
    Literal(Literal),
    Ident(String),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    Map(Vec<(String, Expr)>),
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Field {
        base: Box<Expr>,
        name: String,
    },
    Binary {
        op: BinaryOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Pipe {
        kind: PipeKind,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    Command(CommandExpr),
    RegexMatch {
        value: Box<Expr>,
        pattern: String,
    },
    RegexSubstitution {
        value: Box<Expr>,
        pattern: String,
        replacement: String,
        all: bool,
    },
    Match {
        value: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    NamedArg {
        name: String,
        value: Box<Expr>,
    },
    For {
        mode: ForMode,
        workers: Option<Box<Expr>>,
        pattern: LetPattern,
        iter: Box<Expr>,
        body: Vec<Stmt>,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    /// `xs[start..end]`, `xs[..end]`, `xs[start..]`, `xs[start..=end]`.
    Slice {
        base: Box<Expr>,
        start: Option<Box<Expr>>,
        end: Option<Box<Expr>>,
        inclusive: bool,
    },
    /// `if`/`else if`/`else`, usable as a statement or as a value.
    If {
        cond: Box<Expr>,
        pattern: Option<LetPattern>,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    },
    /// `loop { ... }`: runs until `break`, whose value is the loop's value.
    Loop {
        body: Vec<Stmt>,
    },
    Block(Vec<Stmt>),
    Closure {
        params: Vec<String>,
        body: Vec<Stmt>,
        propagation: PropagationCell,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Implicit,
    ImplicitField(String),
    Try(Box<Expr>),
    /// A string literal with `{expr}`, `$name`, or `${name:-default}` parts,
    /// resolved at parse time.
    Interpolated(Vec<StrPart>),
}

/// One piece of an interpolated string.
#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Text(String),
    /// `{expr}`: any expression.
    Expr(Expr),
    /// `$name` or `${name}`: a Rill binding, falling back to the environment.
    Var(String),
    /// `${name:-default}`: the default is itself a template.
    VarDefault(String, Vec<StrPart>),
}

/// Literal syntax is deliberately independent from runtime values.  In
/// particular, handles, streams, functions, and process results cannot be
/// smuggled into an AST through this type.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Duration(std::time::Duration),
    Str(String),
    Regex(String),
}

impl Stmt {
    pub fn unspanned(&self) -> &Stmt {
        match self {
            Self::Located { stmt, .. } => stmt.unspanned(),
            other => other,
        }
    }

    pub fn span(&self) -> Option<Span> {
        match self {
            Self::Located { span, .. } => Some(*span),
            _ => None,
        }
    }
}

impl Expr {
    pub fn unspanned(&self) -> &Expr {
        match self {
            Self::Located { expr, .. } => expr.unspanned(),
            other => other,
        }
    }

    pub fn span(&self) -> Option<crate::diagnostic::Span> {
        match self {
            Self::Located { span, .. } => Some(*span),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    /// `pattern if guard => ...`
    pub guard: Option<Expr>,
    pub expr: Expr,
}

/// A `match` arm pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// `_`
    Wildcard,
    /// A name that binds the matched value.
    Bind(String),
    /// `1`, `-2.5`, `"text"`, `true`, `null`
    Literal(Literal),
    /// `/regex/` with named captures bound as `Str`.
    Regex(String),
    /// `(a, _)`
    Tuple(Vec<Pattern>),
    /// `[a, b]`, or `[first, ..]` when `rest` is set.
    List {
        items: Vec<Pattern>,
        rest: bool,
    },
    Some(Box<Pattern>),
    None,
    Ok(Box<Pattern>),
    Err(Box<Pattern>),
    /// `a | b`
    Or(Vec<Pattern>),
}

impl Pattern {
    /// Whether the pattern matches every value of its type.
    pub fn is_irrefutable(&self) -> bool {
        match self {
            Pattern::Wildcard | Pattern::Bind(_) => true,
            Pattern::Tuple(items) => items.iter().all(Pattern::is_irrefutable),
            Pattern::List { items, rest } => *rest && items.is_empty(),
            Pattern::Or(alternatives) => alternatives.iter().any(Pattern::is_irrefutable),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandExpr {
    pub argv: Vec<CommandArg>,
    pub redirects: Vec<CommandRedirect>,
    pub config: Vec<(String, Expr)>,
    pub check_exit: bool,
    pub span: Span,
}

impl CommandExpr {
    pub fn validate_config(&self) -> Result<(), crate::diagnostic::RillError> {
        let mut seen = std::collections::BTreeSet::new();
        let mut claim = |name: &str| {
            if seen.insert(name.to_string()) {
                Ok(())
            } else {
                Err(crate::diagnostic::RillError::spanned(
                    format!("duplicate or conflicting command setting '{name}'"),
                    self.span,
                ))
            }
        };
        for redirect in &self.redirects {
            match redirect.kind {
                RedirectKind::Stdin => claim("stdin")?,
                RedirectKind::Stdout => claim("stdout")?,
                RedirectKind::Stderr => claim("stderr")?,
                RedirectKind::All => {
                    claim("stdout")?;
                    claim("stderr")?;
                }
            }
        }
        for (name, _) in &self.config {
            claim(name)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandRedirect {
    pub kind: RedirectKind,
    pub target: CommandArg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectKind {
    Stdout,
    Stderr,
    All,
    Stdin,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandArg {
    Word(String),
    String(String),
    Var(String),
    Splat(String),
    Parts(Vec<CommandPart>),
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandPart {
    Text(String),
    Var(String),
    /// `${name:-default}`; the default is a string template.
    VarDefault(String, Vec<StrPart>),
    /// A quoted argument fragment with interpolation.
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Add,
    Sub,
    Mul,
    Div,
    /// `%`: remainder with the sign of the dividend, like `/` truncates.
    Rem,
    /// `**`: exponentiation, right-associative.
    Pow,
    /// `a..b`: half-open integer range.
    Range,
    /// `a..=b`: inclusive integer range.
    RangeInclusive,
    And,
    Or,
    /// `x in xs`: membership in a List, Map (keys), or Str (substring).
    In,
    NotIn,
    /// `x ?? y`: `y` when `x` is `null` or `none`; unwraps `some`.
    Coalesce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeKind {
    Byte,
    Value,
}
