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
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub type_name: Option<TypeName>,
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
    Map,
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
    Let { pattern: LetPattern, type_name: Option<TypeName>, expr: Expr, mutable: bool },
    AssignEnv { name: String, expr: Expr },
    Args(ArgGroup),
    InputLines,
    Fn { name: String, function: Function },
    If { cond: Expr, pattern: Option<LetPattern>, then_body: Vec<Stmt>, else_body: Vec<Stmt> },
    WithEnv { bindings: Vec<(String, Expr)>, body: Vec<Stmt> },
    On { host: Box<Expr>, body: Vec<Stmt> },
    Emit(Expr),
    Return(Option<Expr>),
    Continue,
    Break,
    Assign { name: String, op: AssignOp, expr: Expr },
    IndexAssign { name: String, index: Box<Expr>, op: AssignOp, expr: Box<Expr> },
    While { cond: Box<Expr>, body: Vec<Stmt> },
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Add,
    Sub,
    Mul,
    Div,
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
    Block(Vec<Stmt>),
    Closure {
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Implicit,
    ImplicitField(String),
    Try(Box<Expr>),
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
    pub pattern: MatchPattern,
    pub expr: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchPattern {
    Regex(String),
    Wildcard,
    Ok(Option<String>),
    Err(Option<String>),
    Some(Option<String>),
    None,
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
    /// `${name:-default}` with a default template string.
    VarDefault(String, String),
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
    Range,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeKind {
    Byte,
    Value,
}
