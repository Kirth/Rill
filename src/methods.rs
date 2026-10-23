//! The public receiver-based standard-library API, shared by evaluation and checking.
use crate::ast::Expr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Unknown,
    Null,
    Str,
    Float,
    List,
    Tuple,
    Map,
    Path,
    Regex,
    Duration,
    ProcessHandle,
    Int,
    Bool,
    Function,
    Option,
    Result,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Builtin(BuiltinMethod),
    Module(&'static str, &'static str),
    Read(bool),
    Sleep,
    Match,
    Replace,
    Structural(Structural),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinMethod {
    Get,
    Map,
    ParallelMap,
    Filter,
    ParallelFilter,
    Flatten,
    Zip,
    Enumerate,
    GroupBy,
    Take,
    Reverse,
    Sort,
    FlatMap,
    Find,
    Any,
    First,
    Last,
    Slice,
    Skip,
    Difference,
    SymmetricDifference,
    Contains,
    Len,
    Trim,
    Lines,
    Split,
    Upper,
    Lower,
    StartsWith,
    EndsWith,
    Repeat,
    IndexOf,
    Keys,
    Values,
    Entries,
    ContainsKey,
    Merge,
    Glob,
    Join,
    Spawn,
    Parallel,
    Await,
}

impl BuiltinMethod {
    pub fn name(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Map => "map",
            Self::ParallelMap => "parallel_map",
            Self::Filter => "filter",
            Self::ParallelFilter => "parallel_filter",
            Self::Flatten => "flatten",
            Self::Zip => "zip",
            Self::Enumerate => "enumerate",
            Self::GroupBy => "group_by",
            Self::Take => "take",
            Self::Reverse => "reverse",
            Self::Sort => "sort",
            Self::FlatMap => "flat_map",
            Self::Find => "find",
            Self::Any => "any",
            Self::First => "first",
            Self::Last => "last",
            Self::Slice => "slice",
            Self::Skip => "skip",
            Self::Difference => "difference",
            Self::SymmetricDifference => "symmetric_difference",
            Self::Contains => "contains",
            Self::Len => "len",
            Self::Trim => "trim",
            Self::Lines => "lines",
            Self::Split => "split",
            Self::Upper => "upper",
            Self::Lower => "lower",
            Self::StartsWith => "starts_with",
            Self::EndsWith => "ends_with",
            Self::Repeat => "repeat",
            Self::IndexOf => "index_of",
            Self::Keys => "keys",
            Self::Values => "values",
            Self::Entries => "entries",
            Self::ContainsKey => "contains_key",
            Self::Merge => "merge",
            Self::Glob => "glob",
            Self::Join => "join",
            Self::Spawn => "spawn",
            Self::Parallel => "parallel",
            Self::Await => "await",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Structural {
    IsOk,
    IsErr,
    Ok,
    Err,
    IsSome,
    IsNone,
    Unwrap,
    UnwrapOr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Argument {
    Any,
    Str,
    Int,
    Function,
    List,
    Map,
    PathOrStr,
    JoinPart,
    MatchPattern,
    NamedBool(&'static str),
}

impl Argument {
    pub fn accepts(self, receiver: Kind, kind: Kind) -> bool {
        if kind == Kind::Unknown {
            return true;
        }
        match self {
            Argument::Any => true,
            Argument::Str => kind == Kind::Str,
            Argument::Int => kind == Kind::Int,
            Argument::Function => kind == Kind::Function,
            Argument::List => kind == Kind::List,
            Argument::Map => kind == Kind::Map,
            Argument::PathOrStr => matches!(kind, Kind::Path | Kind::Str),
            Argument::JoinPart if receiver == Kind::List => kind == Kind::Str,
            Argument::JoinPart => matches!(kind, Kind::Path | Kind::Str),
            Argument::MatchPattern if receiver == Kind::Regex => kind == Kind::Str,
            Argument::MatchPattern => matches!(kind, Kind::Str | Kind::Regex),
            Argument::NamedBool(_) => kind == Kind::Bool,
        }
    }

    pub fn expected_name(self) -> Option<&'static str> {
        match self {
            Argument::Any => None,
            Argument::Str => Some("Str"),
            Argument::Int => Some("Int"),
            Argument::Function => Some("Fn"),
            Argument::List => Some("List"),
            Argument::Map => Some("Map"),
            Argument::PathOrStr | Argument::JoinPart => Some("Str or Path"),
            Argument::MatchPattern => Some("Str or Regex"),
            Argument::NamedBool(_) => Some("Bool"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Return {
    Unknown,
    Null,
    Bool,
    Int,
    Float,
    Str,
    Map,
    Path,
    ProcessResult,
    ProcessHandle,
    Receiver,
    ReceiverItem,
    OptionalReceiverItem,
    Join,
    CallbackList,
    ListUnknown,
    ListTupleUnknown,
    ListStr,
    ListPath,
    ListProcessResult,
    ListListStr,
    ReadText,
    ReadYaml,
    ResultOkOption,
    ResultErrOption,
    Unwrapped,
    OptionArgument,
    OkResultArgument,
    ErrResultArgument,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinImplementation {
    Str,
    Bool,
    Int,
    Float,
    Type,
    Print,
    Prin,
    PrettyPrint,
    Warn,
    Die,
    SetEnv,
    Path,
    File,
    Shell,
    Docker,
    Exit,
    Some,
    None,
    Ok,
    Err,
}

pub struct Builtin {
    pub name: &'static str,
    pub min_args: usize,
    pub max_args: usize,
    pub arguments: &'static [Argument],
    pub returns: Return,
    pub implementation: BuiltinImplementation,
}

impl Builtin {
    pub fn validate(&self, arity: usize) -> Result<(), String> {
        if (self.min_args..=self.max_args).contains(&arity) {
            return Ok(());
        }
        let expected = if self.min_args == self.max_args {
            self.min_args.to_string()
        } else if self.max_args == usize::MAX {
            format!("at least {}", self.min_args)
        } else {
            format!("{} to {}", self.min_args, self.max_args)
        };
        Err(format!("{} needs {expected} args, got {arity}", self.name))
    }

    pub fn argument(&self, index: usize) -> Argument {
        self.arguments.get(index).copied().unwrap_or(Argument::Any)
    }
}

macro_rules! builtin {
    ($name:literal, $min:expr, $max:expr, [$($arg:expr),*], $returns:expr, $implementation:ident) => {
        Builtin { name: $name, min_args: $min, max_args: $max, arguments: &[$($arg),*],
            returns: $returns, implementation: BuiltinImplementation::$implementation }
    };
}

pub static BUILTINS: &[Builtin] = &[
    builtin!("Str", 1, 1, [Argument::Any], Return::Str, Str),
    builtin!("Bool", 1, 1, [Argument::Any], Return::Bool, Bool),
    builtin!("Int", 1, 1, [Argument::Any], Return::Int, Int),
    builtin!("Float", 1, 1, [Argument::Any], Return::Float, Float),
    builtin!("type", 1, 1, [Argument::Any], Return::Str, Type),
    builtin!("print", 0, usize::MAX, [Argument::Any], Return::Null, Print),
    builtin!("prin", 0, usize::MAX, [Argument::Any], Return::Null, Prin),
    builtin!("pretty_print", 1, 1, [Argument::Any], Return::Null, PrettyPrint),
    builtin!("warn", 0, usize::MAX, [Argument::Any], Return::Null, Warn),
    builtin!("die", 0, 1, [Argument::Any], Return::Null, Die),
    builtin!("set_env", 2, 2, [Argument::Any, Argument::Any], Return::Null, SetEnv),
    builtin!("path", 1, 1, [Argument::Any], Return::Path, Path),
    builtin!("file", 1, 1, [Argument::Any], Return::Path, File),
    builtin!("sh", 1, 1, [Argument::Any], Return::ProcessResult, Shell),
    builtin!("docker", 1, 1, [Argument::Any], Return::ListStr, Docker),
    builtin!("exit", 0, 1, [Argument::Int], Return::Null, Exit),
    builtin!("some", 1, 1, [Argument::Any], Return::OptionArgument, Some),
    builtin!("none", 0, 0, [], Return::OptionArgument, None),
    builtin!("ok", 1, 1, [Argument::Any], Return::OkResultArgument, Ok),
    builtin!("err", 1, 1, [Argument::Any], Return::ErrResultArgument, Err),
];

pub fn lookup_builtin(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|builtin| builtin.name == name)
}

pub struct Method {
    pub name: &'static str,
    pub receivers: &'static [Kind],
    pub min_args: usize,
    pub max_args: usize,
    pub arguments: &'static [Argument],
    pub returns: Return,
    pub target: Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverAdapter {
    Direct,
    Path,
    Duration,
    SwapFirstTwo,
}

#[derive(Clone, Copy)]
pub struct NamespaceCall {
    pub method: &'static Method,
    pub adapter: ReceiverAdapter,
}

struct NamespaceAlias {
    module: &'static str,
    name: &'static str,
    method: &'static str,
    adapter: ReceiverAdapter,
}

impl Method {
    pub fn accepts_argument(&self, receiver: Kind, index: usize, kind: Kind) -> bool {
        self.argument(index).accepts(receiver, kind)
    }
    pub fn accepts(&self, kind: Kind) -> bool {
        kind == Kind::Unknown || self.receivers.is_empty() || self.receivers.contains(&kind)
    }

    pub fn validate(&self, kind: Kind, arity: usize) -> Result<(), String> {
        if !self.accepts(kind) {
            return Err(format!(".{}() is not supported on {kind:?}", self.name));
        }
        if !(self.min_args..=self.max_args).contains(&arity) {
            return Err(format!(
                ".{}() needs {}{} arguments, got {arity}",
                self.name,
                self.min_args,
                if self.min_args == self.max_args {
                    String::new()
                } else {
                    format!(" to {}", self.max_args)
                }
            ));
        }
        Ok(())
    }

    pub fn argument(&self, index: usize) -> Argument {
        self.arguments.get(index).copied().unwrap_or(Argument::Any)
    }
}

macro_rules! method {
    ($name:literal, [$($kind:ident),*], $arity:literal, [$($arg:expr),*], $returns:expr, $target:expr) => {
        Method { name: $name, receivers: &[$(Kind::$kind),*], min_args: $arity,
            max_args: $arity, arguments: &[$($arg),*], returns: $returns, target: $target }
    };
}

pub static METHODS: &[Method] = &[
    method!(
        "get",
        [Map, Null],
        1,
        [Argument::Str],
        Return::Unknown,
        Target::Builtin(BuiltinMethod::Get)
    ),
    method!(
        "map",
        [Map, List],
        1,
        [Argument::Function],
        Return::CallbackList,
        Target::Builtin(BuiltinMethod::Map)
    ),
    method!(
        "parallel_map",
        [Map, List],
        1,
        [Argument::Function],
        Return::CallbackList,
        Target::Builtin(BuiltinMethod::ParallelMap)
    ),
    method!(
        "filter",
        [List],
        1,
        [Argument::Function],
        Return::Receiver,
        Target::Builtin(BuiltinMethod::Filter)
    ),
    method!(
        "parallel_filter",
        [List],
        1,
        [Argument::Function],
        Return::Receiver,
        Target::Builtin(BuiltinMethod::ParallelFilter)
    ),
    method!("flatten", [List], 0, [], Return::ListUnknown, Target::Builtin(BuiltinMethod::Flatten)),
    method!(
        "zip",
        [List],
        1,
        [Argument::List],
        Return::ListUnknown,
        Target::Builtin(BuiltinMethod::Zip)
    ),
    method!(
        "enumerate",
        [List],
        0,
        [],
        Return::ListTupleUnknown,
        Target::Builtin(BuiltinMethod::Enumerate)
    ),
    method!(
        "group_by",
        [List],
        1,
        [Argument::Function],
        Return::Map,
        Target::Builtin(BuiltinMethod::GroupBy)
    ),
    method!(
        "take",
        [List],
        1,
        [Argument::Int],
        Return::Receiver,
        Target::Builtin(BuiltinMethod::Take)
    ),
    method!("reverse", [List], 0, [], Return::Receiver, Target::Builtin(BuiltinMethod::Reverse)),
    method!("sort", [List], 0, [], Return::Receiver, Target::Builtin(BuiltinMethod::Sort)),
    method!(
        "flat_map",
        [List],
        1,
        [Argument::Function],
        Return::CallbackList,
        Target::Builtin(BuiltinMethod::FlatMap)
    ),
    method!(
        "find",
        [List],
        1,
        [Argument::Function],
        Return::OptionalReceiverItem,
        Target::Builtin(BuiltinMethod::Find)
    ),
    method!(
        "any",
        [List],
        1,
        [Argument::Function],
        Return::Bool,
        Target::Builtin(BuiltinMethod::Any)
    ),
    method!("first", [List], 0, [], Return::ReceiverItem, Target::Builtin(BuiltinMethod::First)),
    method!("last", [List], 0, [], Return::ReceiverItem, Target::Builtin(BuiltinMethod::Last)),
    method!(
        "slice",
        [List],
        2,
        [Argument::Int, Argument::Int],
        Return::ListUnknown,
        Target::Builtin(BuiltinMethod::Slice)
    ),
    method!(
        "skip",
        [List],
        1,
        [Argument::Int],
        Return::Receiver,
        Target::Builtin(BuiltinMethod::Skip)
    ),
    method!(
        "difference",
        [List],
        1,
        [Argument::List],
        Return::Receiver,
        Target::Builtin(BuiltinMethod::Difference)
    ),
    method!(
        "symmetric_difference",
        [List],
        1,
        [Argument::List],
        Return::Receiver,
        Target::Builtin(BuiltinMethod::SymmetricDifference)
    ),
    method!(
        "contains",
        [Str, List],
        1,
        [Argument::Any],
        Return::Bool,
        Target::Builtin(BuiltinMethod::Contains)
    ),
    method!("len", [Str, List, Map], 0, [], Return::Int, Target::Builtin(BuiltinMethod::Len)),
    method!("trim", [Str], 0, [], Return::Str, Target::Builtin(BuiltinMethod::Trim)),
    method!("lines", [Str], 0, [], Return::ListStr, Target::Builtin(BuiltinMethod::Lines)),
    method!(
        "split",
        [Str],
        1,
        [Argument::Str],
        Return::ListUnknown,
        Target::Builtin(BuiltinMethod::Split)
    ),
    method!("upper", [Str], 0, [], Return::Str, Target::Builtin(BuiltinMethod::Upper)),
    method!("lower", [Str], 0, [], Return::Str, Target::Builtin(BuiltinMethod::Lower)),
    method!(
        "starts_with",
        [Str],
        1,
        [Argument::Str],
        Return::Bool,
        Target::Builtin(BuiltinMethod::StartsWith)
    ),
    method!(
        "ends_with",
        [Str],
        1,
        [Argument::Str],
        Return::Bool,
        Target::Builtin(BuiltinMethod::EndsWith)
    ),
    method!(
        "repeat",
        [Str],
        1,
        [Argument::Int],
        Return::Str,
        Target::Builtin(BuiltinMethod::Repeat)
    ),
    method!(
        "index_of",
        [Str],
        1,
        [Argument::Str],
        Return::Int,
        Target::Builtin(BuiltinMethod::IndexOf)
    ),
    method!("keys", [Map], 0, [], Return::ListUnknown, Target::Builtin(BuiltinMethod::Keys)),
    method!("values", [Map], 0, [], Return::ListUnknown, Target::Builtin(BuiltinMethod::Values)),
    method!("entries", [Map], 0, [], Return::ListUnknown, Target::Builtin(BuiltinMethod::Entries)),
    method!(
        "contains_key",
        [Map],
        1,
        [Argument::Str],
        Return::Bool,
        Target::Builtin(BuiltinMethod::ContainsKey)
    ),
    method!("merge", [Map], 1, [Argument::Map], Return::Map, Target::Builtin(BuiltinMethod::Merge)),
    Method {
        name: "replace",
        receivers: &[Kind::Str],
        min_args: 2,
        max_args: 3,
        arguments: &[Argument::MatchPattern, Argument::Str, Argument::NamedBool("all")],
        returns: Return::Str,
        target: Target::Replace,
    },
    method!("is_match", [Str, Regex], 1, [Argument::MatchPattern], Return::Bool, Target::Match),
    method!("sha256", [Str], 0, [], Return::Str, Target::Module("hashes", "sha256")),
    method!("glob", [Str], 0, [], Return::ListPath, Target::Builtin(BuiltinMethod::Glob)),
    method!("parse_json", [Str], 0, [], Return::Unknown, Target::Module("json", "parse")),
    method!("parse_yaml", [Str], 0, [], Return::Unknown, Target::Module("yaml", "parse")),
    method!("parse_toml", [Str], 0, [], Return::Unknown, Target::Module("toml", "parse")),
    method!("parse_csv", [Str], 0, [], Return::ListListStr, Target::Module("csv", "parse")),
    method!("parse_url", [Str], 0, [], Return::Map, Target::Module("url", "parse")),
    method!("to_json", [], 0, [], Return::Str, Target::Module("json", "stringify")),
    method!("to_json_pretty", [], 0, [], Return::Str, Target::Module("json", "pretty")),
    method!("to_yaml", [], 0, [], Return::Str, Target::Module("yaml", "stringify")),
    method!("to_toml", [], 0, [], Return::Str, Target::Module("toml", "stringify")),
    method!(
        "join",
        [Path, List],
        1,
        [Argument::JoinPart],
        Return::Join,
        Target::Builtin(BuiltinMethod::Join)
    ),
    method!("exists", [Path], 0, [], Return::Bool, Target::Module("fs", "exists")),
    method!("read_text", [Path], 0, [], Return::ReadText, Target::Read(false)),
    method!("read_yaml", [Path], 0, [], Return::ReadYaml, Target::Read(true)),
    method!("read_lines", [Path], 0, [], Return::ListStr, Target::Module("fs", "read_lines")),
    method!("write", [Path], 1, [Argument::Str], Return::Null, Target::Module("fs", "write")),
    method!("append", [Path], 1, [Argument::Str], Return::Null, Target::Module("fs", "append")),
    method!("mkdir", [Path], 0, [], Return::Null, Target::Module("fs", "mkdir")),
    method!(
        "rename",
        [Path],
        1,
        [Argument::PathOrStr],
        Return::Null,
        Target::Module("fs", "rename")
    ),
    method!("stat", [Path], 0, [], Return::Map, Target::Module("fs", "stat")),
    method!("ls", [Path], 0, [], Return::ListUnknown, Target::Module("fs", "ls")),
    method!("parent", [Path], 0, [], Return::Path, Target::Module("path", "parent")),
    method!("basename", [Path], 0, [], Return::Str, Target::Module("path", "basename")),
    method!("stem", [Path], 0, [], Return::Str, Target::Module("path", "stem")),
    method!("extension", [Path], 0, [], Return::Str, Target::Module("path", "extension")),
    method!("is_file", [Path], 0, [], Return::Bool, Target::Module("fs", "is_file")),
    method!("is_dir", [Path], 0, [], Return::Bool, Target::Module("fs", "is_dir")),
    method!("canonicalize", [Path], 0, [], Return::Path, Target::Module("path", "canonicalize")),
    method!("archive_list", [Path], 0, [], Return::ListStr, Target::Module("archive", "list")),
    method!(
        "archive_extract",
        [Path],
        1,
        [Argument::PathOrStr],
        Return::Null,
        Target::Module("archive", "extract")
    ),
    method!("run", [List], 0, [], Return::ProcessResult, Target::Module("process", "run")),
    method!("spawn", [List], 0, [], Return::ProcessHandle, Target::Builtin(BuiltinMethod::Spawn)),
    method!(
        "parallel",
        [List],
        0,
        [],
        Return::ListProcessResult,
        Target::Builtin(BuiltinMethod::Parallel)
    ),
    method!(
        "await",
        [ProcessHandle],
        0,
        [],
        Return::ProcessResult,
        Target::Builtin(BuiltinMethod::Await)
    ),
    method!("sleep", [Duration], 0, [], Return::Null, Target::Sleep),
    method!("is_ok", [Result], 0, [], Return::Bool, Target::Structural(Structural::IsOk)),
    method!("is_err", [Result], 0, [], Return::Bool, Target::Structural(Structural::IsErr)),
    method!("ok", [Result], 0, [], Return::ResultOkOption, Target::Structural(Structural::Ok)),
    method!("err", [Result], 0, [], Return::ResultErrOption, Target::Structural(Structural::Err)),
    method!("is_some", [Option], 0, [], Return::Bool, Target::Structural(Structural::IsSome)),
    method!("is_none", [Option], 0, [], Return::Bool, Target::Structural(Structural::IsNone)),
    method!(
        "unwrap",
        [Result, Option],
        0,
        [],
        Return::Unwrapped,
        Target::Structural(Structural::Unwrap)
    ),
    method!(
        "unwrap_or",
        [Result, Option],
        1,
        [Argument::Any],
        Return::Unwrapped,
        Target::Structural(Structural::UnwrapOr)
    ),
];

pub fn lookup(name: &str) -> Option<&'static Method> {
    METHODS.iter().find(|method| method.name == name)
}

pub fn moved_function(name: &str) -> Option<&'static str> {
    match name {
        "read" => Some("read_text"),
        _ => METHODS.iter().find_map(|method| match method.target {
            Target::Builtin(old) if old.name() == name => Some(method.name),
            _ => None,
        }),
    }
}

static NAMESPACE_ALIASES: &[NamespaceAlias] = &[
    NamespaceAlias {
        module: "fs",
        name: "read",
        method: "read_text",
        adapter: ReceiverAdapter::Path,
    },
    NamespaceAlias {
        module: "yaml",
        name: "read",
        method: "read_yaml",
        adapter: ReceiverAdapter::Path,
    },
    NamespaceAlias {
        module: "time",
        name: "sleep",
        method: "sleep",
        adapter: ReceiverAdapter::Duration,
    },
    NamespaceAlias {
        module: "regex",
        name: "is_match",
        method: "is_match",
        adapter: ReceiverAdapter::SwapFirstTwo,
    },
    NamespaceAlias {
        module: "process",
        name: "exec",
        method: "run",
        adapter: ReceiverAdapter::Direct,
    },
    NamespaceAlias { module: "path", name: "join", method: "join", adapter: ReceiverAdapter::Path },
];

pub fn namespace_method(module: &str, name: &str) -> Option<NamespaceCall> {
    if let Some(alias) =
        NAMESPACE_ALIASES.iter().find(|alias| alias.module == module && alias.name == name)
    {
        return Some(NamespaceCall {
            method: lookup(alias.method).expect("namespace alias references registered method"),
            adapter: alias.adapter,
        });
    }
    METHODS.iter().find_map(|method| match method.target {
        Target::Module(target_module, target_name)
            if target_module == module && target_name == name =>
        {
            Some(NamespaceCall {
                method,
                adapter: if matches!(module, "fs" | "path" | "archive") {
                    ReceiverAdapter::Path
                } else {
                    ReceiverAdapter::Direct
                },
            })
        }
        _ => None,
    })
}

/// The standard namespaces that `use` accepts and that resolve as `name.func(...)`.
pub const NAMESPACES: &[&str] = &[
    "json", "toml", "yaml", "csv", "url", "path", "fs", "process", "time", "regex", "archive",
    "hashes",
];

pub fn is_namespace(name: &str) -> bool {
    NAMESPACES.contains(&name) || name == "Stream"
}

/// Names of the functions available on a namespace, for diagnostics.
pub fn namespace_functions(module: &str) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = NAMESPACE_ALIASES
        .iter()
        .filter(|alias| alias.module == module)
        .map(|alias| alias.name)
        .collect();
    names.extend(METHODS.iter().filter_map(|method| match method.target {
        Target::Module(target_module, name) if target_module == module => Some(name),
        _ => None,
    }));
    names.sort_unstable();
    names.dedup();
    names
}

/// Names of the methods a receiver of `kind` accepts, for diagnostics.
pub fn methods_for(kind: Kind) -> Vec<&'static str> {
    let mut names: Vec<&'static str> =
        METHODS.iter().filter(|method| method.accepts(kind)).map(|method| method.name).collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// Whether an expression starts with a receiver-relative method, e.g. .map(f).take(2).
pub fn is_method_pipeline(expr: &Expr) -> bool {
    match expr.unspanned() {
        Expr::Call { callee, .. } => match callee.unspanned() {
            Expr::ImplicitField(_) => true,
            Expr::Field { base, .. } => is_method_pipeline(base),
            _ => false,
        },
        Expr::Try(inner) => is_method_pipeline(inner),
        _ => false,
    }
}

pub fn is_lines_consumer(expr: &Expr) -> bool {
    matches!(expr.unspanned(), Expr::Call { callee, args }
        if args.is_empty() && matches!(callee.unspanned(), Expr::ImplicitField(name) if name == "lines"))
}
