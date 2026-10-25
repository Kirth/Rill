//! The public receiver-based standard-library API, shared by evaluation and checking.
use crate::ast::Expr;
use crate::diagnostic::RillError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Unknown,
    Time,
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
    SortBy,
    Unique,
    All,
    Count,
    Sum,
    IndexBy,
    OstackServers,
    OstackVolumes,
    OstackPorts,
    OstackNetworks,
    OstackSubnets,
    OstackImages,
    OstackFlavors,
    OstackHypervisors,
    OstackProjects,
    OstackUsers,
    OstackLoadbalancers,
    OstackAmphorae,
    OstackList,
    OstackInventory,
    OstackAttachedVolumes,
    OstackAttachedServers,
    OstackProjectOf,
    OstackFloatingIps,
    OstackSecurityGroups,
    OstackRouters,
    OstackServer,
    OstackNetwork,
    OstackPort,
    OstackHypervisor,
    TimeParseTime,
    TimeYear,
    TimeMonth,
    TimeDay,
    TimeHour,
    TimeMinute,
    TimeSecond,
    TimeWeekday,
    TimeUnix,
    TimeDate,
    TimeIso,
    TimeFormat,
    TimeAge,
    XmlParse,
    XmlSelect,
    XmlSelectOne,
    XmlTo,
    OstackRelCreatedServers,
    OstackRelProjectMemberships,
    OstackKeypairs,
    OstackServerGroups,
    OstackAggregates,
    OstackAvailabilityZones,
    OstackComputeServices,
    OstackNetworkAgents,
    OstackSnapshots,
    OstackBackups,
    OstackPools,
    OstackRoleAssignments,
    OstackRelFlavor,
    OstackRelImage,
    OstackRelActions,
    OstackRelComputeService,
    OstackRelQuota,
    OstackRelVolume,
    OstackRelMembers,
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
    Step,
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
            Self::SortBy => "sort_by",
            Self::Unique => "unique",
            Self::All => "all",
            Self::Count => "count",
            Self::Sum => "sum",
            Self::IndexBy => "index_by",
            Self::OstackServers => "servers",
            Self::OstackVolumes => "volumes",
            Self::OstackPorts => "ports",
            Self::OstackNetworks => "networks",
            Self::OstackSubnets => "subnets",
            Self::OstackImages => "images",
            Self::OstackFlavors => "flavors",
            Self::OstackHypervisors => "hypervisors",
            Self::OstackProjects => "projects",
            Self::OstackUsers => "users",
            Self::OstackLoadbalancers => "loadbalancers",
            Self::OstackAmphorae => "amphorae",
            Self::OstackList => "list",
            Self::OstackInventory => "inventory",
            Self::OstackAttachedVolumes => "attached_volumes",
            Self::OstackAttachedServers => "attached_servers",
            Self::OstackProjectOf => "project",
            Self::OstackFloatingIps => "floating_ips",
            Self::OstackSecurityGroups => "security_groups",
            Self::OstackRouters => "routers",
            Self::OstackServer => "server",
            Self::OstackNetwork => "network",
            Self::OstackPort => "port",
            Self::OstackHypervisor => "hypervisor",
            Self::TimeParseTime => "parse_time",
            Self::TimeYear => "year",
            Self::TimeMonth => "month",
            Self::TimeDay => "day",
            Self::TimeHour => "hour",
            Self::TimeMinute => "minute",
            Self::TimeSecond => "second",
            Self::TimeWeekday => "weekday",
            Self::TimeUnix => "unix",
            Self::TimeDate => "date",
            Self::TimeIso => "iso",
            Self::TimeFormat => "format",
            Self::TimeAge => "age",
            Self::XmlParse => "parse_xml",
            Self::XmlSelect => "select",
            Self::XmlSelectOne => "select_one",
            Self::XmlTo => "to_xml",
            Self::OstackRelCreatedServers => "created_servers",
            Self::OstackRelProjectMemberships => "project_memberships",
            Self::OstackKeypairs => "keypairs",
            Self::OstackServerGroups => "server_groups",
            Self::OstackAggregates => "aggregates",
            Self::OstackAvailabilityZones => "availability_zones",
            Self::OstackComputeServices => "compute_services",
            Self::OstackNetworkAgents => "network_agents",
            Self::OstackSnapshots => "snapshots",
            Self::OstackBackups => "backups",
            Self::OstackPools => "pools",
            Self::OstackRoleAssignments => "role_assignments",
            Self::OstackRelFlavor => "flavor",
            Self::OstackRelImage => "image",
            Self::OstackRelActions => "actions",
            Self::OstackRelComputeService => "compute_service",
            Self::OstackRelQuota => "quota",
            Self::OstackRelVolume => "volume",
            Self::OstackRelMembers => "members",
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
            Self::Step => "step",
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
    AndThen,
    MapErr,
    UnwrapOrElse,
    OkOr,
    Expect,
    DurationSecs,
    DurationMillis,
    DurationMicros,
    DurationNanos,
    DurationSecsFloat,
    HumanSize,
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
    /// A Str key for maps, an Int index for lists.
    KeyOrIndex,
    Bool,
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
            Argument::NamedBool(_) | Argument::Bool => kind == Kind::Bool,
            Argument::KeyOrIndex if receiver == Kind::List => kind == Kind::Int,
            Argument::KeyOrIndex => kind == Kind::Str,
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
            Argument::NamedBool(_) | Argument::Bool => Some("Bool"),
            Argument::KeyOrIndex => Some("Str key or Int index"),
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
    /// `List[Tuple[Str, V]]` from a Map.
    MapEntries,
    /// `List[V]` from a Map.
    MapValues,
    /// `map`: `List[R]` for lists and maps, `Option[R]`, or `Result[R, E]`.
    Mapped,
    /// `map_err`: `Result[T, R]`.
    MappedErr,
    /// `and_then`: whatever the callback returns.
    AndThen,
    /// `ok_or(e)`: `Result[T, E]`.
    OkOrResult,
    /// `get`: `Option[T]` for lists, unknown (nullable) for maps.
    GetResult,
    /// `List[Tuple[T, U]]` from `xs.zip(ys)`.
    ZipPairs,
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
    /// `index_by`: `Map[Str, item]` from a list.
    MapOfItem,
    /// `List[Map]`: records from an API.
    ListMap,
    Time,
    Duration,
    /// `Option[Map]`.
    OptionMap,
    /// `Option[Unknown]`.
    OptionUnknown,
    /// `Map[Str, List[Map]]`: collections of records by name.
    MapListMap,
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
    Show,
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
    builtin!("show", 1, 2, [Argument::Any, Argument::Any], Return::Null, Show),
    builtin!("warn", 0, usize::MAX, [Argument::Any], Return::Null, Warn),
    builtin!("die", 0, 1, [Argument::Any], Return::Unknown, Die),
    builtin!("set_env", 2, 2, [Argument::Any, Argument::Any], Return::Null, SetEnv),
    builtin!("path", 1, 1, [Argument::Any], Return::Path, Path),
    builtin!("file", 1, 1, [Argument::Any], Return::Path, File),
    builtin!("sh", 1, 1, [Argument::Any], Return::ProcessResult, Shell),
    builtin!("docker", 1, 1, [Argument::Any], Return::ListStr, Docker),
    builtin!("exit", 0, 1, [Argument::Int], Return::Unknown, Exit),
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
        [Map, Null, List],
        1,
        [Argument::KeyOrIndex],
        Return::GetResult,
        Target::Builtin(BuiltinMethod::Get)
    ),
    method!(
        "map",
        [Map, List, Option, Result],
        1,
        [Argument::Function],
        Return::Mapped,
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
        Return::ZipPairs,
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
        "sort_by",
        [List],
        1,
        [Argument::Function],
        Return::Receiver,
        Target::Builtin(BuiltinMethod::SortBy)
    ),
    method!("unique", [List], 0, [], Return::Receiver, Target::Builtin(BuiltinMethod::Unique)),
    method!(
        "all",
        [List],
        1,
        [Argument::Function],
        Return::Bool,
        Target::Builtin(BuiltinMethod::All)
    ),
    method!(
        "count",
        [List],
        1,
        [Argument::Function],
        Return::Int,
        Target::Builtin(BuiltinMethod::Count)
    ),
    method!("sum", [List], 0, [], Return::ReceiverItem, Target::Builtin(BuiltinMethod::Sum)),
    method!(
        "index_by",
        [List],
        1,
        [Argument::Function],
        Return::MapOfItem,
        Target::Builtin(BuiltinMethod::IndexBy)
    ),
    // Scoped OpenStack queries on an `ostack.project(..)` / `ostack.all_projects()` handle.
    method!(
        "servers",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackServers)
    ),
    method!(
        "volumes",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackVolumes)
    ),
    method!("ports", [Map], 0, [], Return::ListMap, Target::Builtin(BuiltinMethod::OstackPorts)),
    method!(
        "networks",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackNetworks)
    ),
    method!(
        "subnets",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackSubnets)
    ),
    method!("images", [Map], 0, [], Return::ListMap, Target::Builtin(BuiltinMethod::OstackImages)),
    method!(
        "flavors",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackFlavors)
    ),
    method!(
        "hypervisors",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackHypervisors)
    ),
    method!(
        "projects",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackProjects)
    ),
    method!("users", [Map], 0, [], Return::ListMap, Target::Builtin(BuiltinMethod::OstackUsers)),
    method!(
        "loadbalancers",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackLoadbalancers)
    ),
    method!(
        "amphorae",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackAmphorae)
    ),
    method!(
        "floating_ips",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackFloatingIps)
    ),
    method!(
        "security_groups",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackSecurityGroups)
    ),
    method!(
        "routers",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackRouters)
    ),
    method!(
        "server",
        [Map],
        0,
        [],
        Return::OptionMap,
        Target::Builtin(BuiltinMethod::OstackServer)
    ),
    method!(
        "network",
        [Map],
        0,
        [],
        Return::OptionMap,
        Target::Builtin(BuiltinMethod::OstackNetwork)
    ),
    method!("port", [Map], 0, [], Return::OptionMap, Target::Builtin(BuiltinMethod::OstackPort)),
    method!(
        "hypervisor",
        [Map],
        0,
        [],
        Return::OptionMap,
        Target::Builtin(BuiltinMethod::OstackHypervisor)
    ),
    Method {
        name: "parse_time",
        receivers: &[Kind::Str],
        min_args: 0,
        max_args: 1,
        arguments: &[Argument::Str],
        returns: Return::Time,
        target: Target::Builtin(BuiltinMethod::TimeParseTime),
    },
    method!("year", [Time], 0, [], Return::Int, Target::Builtin(BuiltinMethod::TimeYear)),
    method!("month", [Time], 0, [], Return::Int, Target::Builtin(BuiltinMethod::TimeMonth)),
    method!("day", [Time], 0, [], Return::Int, Target::Builtin(BuiltinMethod::TimeDay)),
    method!("hour", [Time], 0, [], Return::Int, Target::Builtin(BuiltinMethod::TimeHour)),
    method!("minute", [Time], 0, [], Return::Int, Target::Builtin(BuiltinMethod::TimeMinute)),
    method!("second", [Time], 0, [], Return::Int, Target::Builtin(BuiltinMethod::TimeSecond)),
    method!("weekday", [Time], 0, [], Return::Str, Target::Builtin(BuiltinMethod::TimeWeekday)),
    method!("unix", [Time], 0, [], Return::Int, Target::Builtin(BuiltinMethod::TimeUnix)),
    method!("date", [Time], 0, [], Return::Str, Target::Builtin(BuiltinMethod::TimeDate)),
    method!("iso", [Time], 0, [], Return::Str, Target::Builtin(BuiltinMethod::TimeIso)),
    method!(
        "format",
        [Time],
        1,
        [Argument::Str],
        Return::Str,
        Target::Builtin(BuiltinMethod::TimeFormat)
    ),
    method!("age", [Time], 0, [], Return::Duration, Target::Builtin(BuiltinMethod::TimeAge)),
    method!("parse_xml", [Str], 0, [], Return::Map, Target::Builtin(BuiltinMethod::XmlParse)),
    method!(
        "select",
        [Map, List],
        1,
        [Argument::Str],
        Return::ListUnknown,
        Target::Builtin(BuiltinMethod::XmlSelect)
    ),
    method!(
        "select_one",
        [Map, List],
        1,
        [Argument::Str],
        Return::OptionUnknown,
        Target::Builtin(BuiltinMethod::XmlSelectOne)
    ),
    method!("to_xml", [Map], 0, [], Return::Str, Target::Builtin(BuiltinMethod::XmlTo)),
    method!(
        "created_servers",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackRelCreatedServers)
    ),
    method!(
        "project_memberships",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackRelProjectMemberships)
    ),
    method!(
        "keypairs",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackKeypairs)
    ),
    method!(
        "server_groups",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackServerGroups)
    ),
    method!(
        "aggregates",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackAggregates)
    ),
    method!(
        "availability_zones",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackAvailabilityZones)
    ),
    method!(
        "compute_services",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackComputeServices)
    ),
    method!(
        "network_agents",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackNetworkAgents)
    ),
    method!(
        "snapshots",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackSnapshots)
    ),
    method!(
        "backups",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackBackups)
    ),
    method!("pools", [Map], 0, [], Return::ListMap, Target::Builtin(BuiltinMethod::OstackPools)),
    method!(
        "role_assignments",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackRoleAssignments)
    ),
    method!(
        "flavor",
        [Map],
        0,
        [],
        Return::OptionMap,
        Target::Builtin(BuiltinMethod::OstackRelFlavor)
    ),
    method!(
        "image",
        [Map],
        0,
        [],
        Return::OptionMap,
        Target::Builtin(BuiltinMethod::OstackRelImage)
    ),
    method!(
        "actions",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackRelActions)
    ),
    method!(
        "compute_service",
        [Map],
        0,
        [],
        Return::OptionMap,
        Target::Builtin(BuiltinMethod::OstackRelComputeService)
    ),
    method!("quota", [Map], 0, [], Return::ListMap, Target::Builtin(BuiltinMethod::OstackRelQuota)),
    method!(
        "volume",
        [Map],
        0,
        [],
        Return::OptionMap,
        Target::Builtin(BuiltinMethod::OstackRelVolume)
    ),
    method!(
        "members",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackRelMembers)
    ),
    method!(
        "list",
        [Map],
        1,
        [Argument::Str],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackList)
    ),
    Method {
        name: "inventory",
        receivers: &[Kind::Map],
        min_args: 0,
        max_args: 1,
        arguments: &[Argument::List],
        returns: Return::MapListMap,
        target: Target::Builtin(BuiltinMethod::OstackInventory),
    },
    // Relations on an OpenStack record (a Map with an `id`).
    method!(
        "attached_volumes",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackAttachedVolumes)
    ),
    method!(
        "attached_servers",
        [Map],
        0,
        [],
        Return::ListMap,
        Target::Builtin(BuiltinMethod::OstackAttachedServers)
    ),
    method!(
        "project",
        [Map],
        0,
        [],
        Return::OptionMap,
        Target::Builtin(BuiltinMethod::OstackProjectOf)
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
    method!(
        "first",
        [List],
        0,
        [],
        Return::OptionalReceiverItem,
        Target::Builtin(BuiltinMethod::First)
    ),
    method!(
        "last",
        [List],
        0,
        [],
        Return::OptionalReceiverItem,
        Target::Builtin(BuiltinMethod::Last)
    ),
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
        "step",
        [List],
        1,
        [Argument::Int],
        Return::Receiver,
        Target::Builtin(BuiltinMethod::Step)
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
    method!("values", [Map], 0, [], Return::MapValues, Target::Builtin(BuiltinMethod::Values)),
    method!("entries", [Map], 0, [], Return::MapEntries, Target::Builtin(BuiltinMethod::Entries)),
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
    method!("secs", [Duration], 0, [], Return::Int, Target::Structural(Structural::DurationSecs)),
    method!("ms", [Duration], 0, [], Return::Int, Target::Structural(Structural::DurationMillis)),
    method!("us", [Duration], 0, [], Return::Int, Target::Structural(Structural::DurationMicros)),
    method!("ns", [Duration], 0, [], Return::Int, Target::Structural(Structural::DurationNanos)),
    method!(
        "as_secs_float",
        [Duration],
        0,
        [],
        Return::Float,
        Target::Structural(Structural::DurationSecsFloat)
    ),
    method!("human_size", [Int], 0, [], Return::Str, Target::Structural(Structural::HumanSize)),
    method!("is_ok", [Result], 0, [], Return::Bool, Target::Structural(Structural::IsOk)),
    method!("is_err", [Result], 0, [], Return::Bool, Target::Structural(Structural::IsErr)),
    method!("ok", [Result], 0, [], Return::ResultOkOption, Target::Structural(Structural::Ok)),
    method!("err", [Result], 0, [], Return::ResultErrOption, Target::Structural(Structural::Err)),
    method!("is_some", [Option], 0, [], Return::Bool, Target::Structural(Structural::IsSome)),
    method!(
        "and_then",
        [Option, Result],
        1,
        [Argument::Function],
        Return::AndThen,
        Target::Structural(Structural::AndThen)
    ),
    method!(
        "map_err",
        [Result],
        1,
        [Argument::Function],
        Return::MappedErr,
        Target::Structural(Structural::MapErr)
    ),
    method!(
        "unwrap_or_else",
        [Option, Result],
        1,
        [Argument::Function],
        Return::Unwrapped,
        Target::Structural(Structural::UnwrapOrElse)
    ),
    method!(
        "ok_or",
        [Option],
        1,
        [Argument::Any],
        Return::OkOrResult,
        Target::Structural(Structural::OkOr)
    ),
    method!(
        "expect",
        [Option, Result],
        1,
        [Argument::Str],
        Return::Unwrapped,
        Target::Structural(Structural::Expect)
    ),
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

/// How many arguments the callback of `method` takes for a receiver of `kind`.
/// Shared by the checker and the evaluator so they never disagree.
pub fn callback_arity(method: &str, receiver: Kind) -> usize {
    match (method, receiver) {
        ("map" | "parallel_map", Kind::Map) => 2,
        ("unwrap_or_else", Kind::Option) => 0,
        _ => 1,
    }
}

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

/// A namespace function with no receiver (`ostack.servers()`), unlike the
/// `Target::Module` entries in `METHODS`, which are methods reachable through
/// a namespace with the receiver as first argument.
#[derive(Debug)]
pub struct NamespaceFunction {
    pub module: &'static str,
    pub name: &'static str,
    /// Required positional arguments; `positional.len()` is the maximum.
    pub min_args: usize,
    pub positional: &'static [Argument],
    /// Accepted `name=value` arguments.
    pub named: &'static [(&'static str, Argument)],
    pub returns: Return,
    pub summary: &'static str,
}

impl NamespaceFunction {
    /// Validate a call's argument kinds (`Unknown` always passes).
    pub fn validate(&self, positional: &[Kind], named: &[(&str, Kind)]) -> Result<(), RillError> {
        let max = self.positional.len();
        if positional.len() < self.min_args || positional.len() > max {
            let expected = if self.min_args == max {
                max.to_string()
            } else {
                format!("{} to {max}", self.min_args)
            };
            return Err(RillError::new(format!(
                "{}.{} needs {expected} args, got {}",
                self.module,
                self.name,
                positional.len()
            )));
        }
        for (index, (argument, kind)) in self.positional.iter().zip(positional).enumerate() {
            if !argument.accepts(Kind::Unknown, *kind) {
                return Err(RillError::new(format!(
                    "invalid {}.{} argument {}: expected {}, got {kind:?}",
                    self.module,
                    self.name,
                    index + 1,
                    argument.expected_name().unwrap_or("a value")
                )));
            }
        }
        for (name, kind) in named {
            let Some((_, argument)) = self.named.iter().find(|(known, _)| known == name) else {
                return Err(RillError::new(format!(
                    "unknown {}.{} argument '{name}'",
                    self.module, self.name
                ))
                .with_optional_help(crate::suggest::did_you_mean(
                    name,
                    self.named.iter().map(|(known, _)| *known),
                )));
            };
            if !argument.accepts(Kind::Unknown, *kind) {
                return Err(RillError::new(format!(
                    "{}.{} argument {name}= expects {}, got {kind:?}",
                    self.module,
                    self.name,
                    argument.expected_name().unwrap_or("a value")
                )));
            }
        }
        Ok(())
    }
}

/// The OpenStack collections `ostack` knows, by their API names.
pub const OSTACK_KINDS: &[&str] = &[
    "servers",
    "volumes",
    "ports",
    "networks",
    "subnets",
    "images",
    "flavors",
    "hypervisors",
    "projects",
    "users",
    "loadbalancers",
    "amphorae",
    "floating_ips",
    "security_groups",
    "routers",
    "keypairs",
    "server_groups",
    "aggregates",
    "availability_zones",
    "compute_services",
    "network_agents",
    "snapshots",
    "backups",
    "pools",
    "role_assignments",
];

/// Methods that only make sense on an `ostack.project(..)` handle (a Map
/// with an `@ostack` marker); IDEs can hide them on other maps.
pub const OSTACK_SCOPED_METHODS: &[&str] = &[
    "servers",
    "volumes",
    "ports",
    "networks",
    "subnets",
    "images",
    "flavors",
    "hypervisors",
    "projects",
    "users",
    "loadbalancers",
    "amphorae",
    "floating_ips",
    "security_groups",
    "routers",
    "keypairs",
    "server_groups",
    "aggregates",
    "availability_zones",
    "compute_services",
    "network_agents",
    "snapshots",
    "backups",
    "pools",
    "role_assignments",
    "list",
    "inventory",
];

/// What kind of OpenStack record a map is, from the fields the APIs return
/// (`None` for anything else).  Shared by the evaluator, which dispatches
/// `record.ports()` on it, and by IDEs.
pub fn ostack_record_kind(map: &crate::types::Map) -> Option<&'static str> {
    let has = |key: &str| map.contains_key(key);
    if !has("id") {
        return None;
    }
    Some(if has("fingerprint") || has("public_key") {
        "keypair"
    } else if (has("policies") || has("policy")) && has("members") {
        "server_group"
    } else if has("role_id") {
        "role_assignment"
    } else if has("lb_algorithm") {
        "pool"
    } else if has("protocol_port") && has("address") {
        "member"
    } else if has("agent_type") {
        "network_agent"
    } else if has("binary") && has("host") && (has("zone") || has("state")) {
        "compute_service"
    } else if has("available") && has("hosts") {
        "availability_zone"
    } else if has("hosts") && has("availability_zone") && has("metadata") {
        "aggregate"
    } else if has("volume_id") && (has("container") || has("is_incremental")) {
        "backup"
    } else if has("volume_id") && !has("attachments") {
        "snapshot"
    } else if has("floating_ip_address") {
        "floating_ip"
    } else if has("security_group_rules") {
        "security_group"
    } else if has("external_gateway_info") || (has("routes") && !has("cidr")) {
        "router"
    } else if has("mac_address") || has("device_owner") {
        "port"
    } else if has("cidr") {
        "subnet"
    } else if has("hypervisor_hostname") {
        "hypervisor"
    } else if has("router:external") || has("router_external") || has("subnets") {
        "network"
    } else if has("volume_type") || has("attachments") {
        "volume"
    } else if has("addresses") || has("OS-EXT-SRV-ATTR:host") || (has("flavor") && has("image")) {
        "server"
    } else if has("vcpus") {
        "flavor"
    } else if has("visibility") || has("disk_format") {
        "image"
    } else if has("vip_address") {
        "loadbalancer"
    } else if has("lb_network_ip") {
        "amphora"
    } else if has("email") || has("default_project_id") {
        "user"
    } else if has("domain_id") && has("enabled") {
        "project"
    } else {
        return None;
    })
}

/// The methods a record of `kind` answers (`server.ports()`, …).  A project
/// record answers the scoped listings, like an `ostack.project(..)` handle.
pub fn ostack_relations(kind: &str) -> &'static [&'static str] {
    match kind {
        "server" => &[
            "attached_volumes",
            "ports",
            "security_groups",
            "floating_ips",
            "hypervisor",
            "flavor",
            "image",
            "server_groups",
            "actions",
            "project",
        ],
        "volume" => &["attached_servers", "snapshots", "backups", "project"],
        "snapshot" | "backup" => &["volume", "project"],
        "aggregate" => &["hypervisors"],
        "server_group" => &["servers", "project"],
        "pool" => &["members", "loadbalancers", "project"],
        "loadbalancer" => &["pools", "project"],
        "user" => &["role_assignments", "created_servers", "project_memberships"],
        // Not owned by a project, and nothing to follow.
        "keypair" | "availability_zone" | "compute_service" | "network_agent"
        | "role_assignment" => &[],
        "port" => &["server", "network", "subnets", "security_groups", "floating_ips", "project"],
        "network" => &["ports", "subnets", "routers", "project"],
        "subnet" => &["network", "ports", "routers", "project"],
        "router" => &["ports", "subnets", "project"],
        "floating_ip" => &["port", "server", "project"],
        "security_group" => &["ports", "servers", "project"],
        "hypervisor" => &["servers", "compute_service", "network_agents", "aggregates"],
        "project" => OSTACK_PROJECT_METHODS,
        _ => &["project"],
    }
}

/// What a project record or handle answers: its listings and its quota.
const OSTACK_PROJECT_METHODS: &[&str] = &[
    "servers",
    "volumes",
    "ports",
    "networks",
    "subnets",
    "images",
    "flavors",
    "hypervisors",
    "projects",
    "users",
    "loadbalancers",
    "amphorae",
    "floating_ips",
    "security_groups",
    "routers",
    "keypairs",
    "server_groups",
    "aggregates",
    "availability_zones",
    "compute_services",
    "network_agents",
    "snapshots",
    "backups",
    "pools",
    "role_assignments",
    "list",
    "inventory",
    "quota",
];

/// Every Map method that belongs to ostack (listings and relations).
pub const OSTACK_MAP_METHODS: &[&str] = &[
    "servers",
    "volumes",
    "ports",
    "networks",
    "subnets",
    "images",
    "flavors",
    "hypervisors",
    "projects",
    "users",
    "loadbalancers",
    "amphorae",
    "floating_ips",
    "security_groups",
    "routers",
    "list",
    "inventory",
    "attached_volumes",
    "attached_servers",
    "project",
    "server",
    "network",
    "port",
    "hypervisor",
    "created_servers",
    "project_memberships",
    "keypairs",
    "server_groups",
    "aggregates",
    "availability_zones",
    "compute_services",
    "network_agents",
    "snapshots",
    "backups",
    "pools",
    "role_assignments",
    "flavor",
    "image",
    "actions",
    "compute_service",
    "quota",
    "volume",
    "members",
];

/// The key marking a handle map made by `ostack.project` / `ostack.all_projects`.
pub const OSTACK_MARKER: &str = "@ostack";

const OSTACK_FIND_NAMED: &[(&str, Argument)] =
    &[("fresh", Argument::Bool), ("all_projects", Argument::Bool), ("project", Argument::Str)];

const OSTACK_SEARCH_NAMED: &[(&str, Argument)] = &[
    ("fresh", Argument::Bool),
    ("all_projects", Argument::Bool),
    ("project", Argument::Str),
    ("kinds", Argument::List),
];

const OSTACK_LIST_NAMED: &[(&str, Argument)] = &[
    ("fresh", Argument::Bool),
    ("all_projects", Argument::Bool),
    ("project", Argument::Str),
    ("query", Argument::Map),
];

macro_rules! ostack_fn {
    ($name:literal, $min:expr, [$($pos:expr),*], $named:expr, $returns:expr, $summary:literal) => {
        NamespaceFunction {
            module: "ostack",
            name: $name,
            min_args: $min,
            positional: &[$($pos),*],
            named: $named,
            returns: $returns,
            summary: $summary,
        }
    };
}

macro_rules! ostack_kind {
    ($name:literal, $summary:literal) => {
        ostack_fn!($name, 0, [], OSTACK_LIST_NAMED, Return::ListMap, $summary)
    };
}

/// Zero-receiver namespace functions.  The `ostack` family talks to OpenStack
/// through the `ostack` crate when rill is built with the `openstack` feature;
/// the table is always present so scripts type-check everywhere.
pub static NAMESPACE_FUNCTIONS: &[NamespaceFunction] = &[
    ostack_fn!(
        "info",
        0,
        [],
        &[],
        Return::Map,
        "the authenticated session: user, project, catalog"
    ),
    ostack_fn!("kinds", 0, [], &[], Return::ListStr, "the collection names ostack knows"),
    ostack_fn!(
        "list",
        1,
        [Argument::Str],
        OSTACK_LIST_NAMED,
        Return::ListMap,
        "every record of a collection, e.g. ostack.list(\"servers\")"
    ),
    ostack_kind!("servers", "every server (full records)"),
    ostack_kind!("volumes", "every volume, with attachments"),
    ostack_kind!("ports", "every port, with device_id and fixed_ips"),
    ostack_kind!("networks", "every network"),
    ostack_kind!("subnets", "every subnet"),
    ostack_kind!("images", "every image"),
    ostack_kind!("flavors", "every flavor"),
    ostack_kind!("hypervisors", "every hypervisor"),
    ostack_kind!("projects", "every project"),
    ostack_kind!("users", "every user"),
    ostack_kind!("loadbalancers", "every load balancer"),
    ostack_kind!("amphorae", "every amphora"),
    ostack_kind!("floating_ips", "every floating IP"),
    ostack_kind!("security_groups", "every security group"),
    ostack_kind!("routers", "every router"),
    ostack_kind!("keypairs", "every keypair of the user"),
    ostack_kind!("server_groups", "every server group"),
    ostack_kind!("aggregates", "every host aggregate"),
    ostack_kind!("availability_zones", "every compute availability zone"),
    ostack_kind!("compute_services", "every Nova service (admin)"),
    ostack_kind!("network_agents", "every Neutron agent (admin)"),
    ostack_kind!("snapshots", "every volume snapshot"),
    ostack_kind!("backups", "every volume backup"),
    ostack_kind!("pools", "every load balancer pool"),
    ostack_kind!("role_assignments", "every role assignment"),
    ostack_fn!(
        "show",
        2,
        [Argument::Str, Argument::Str],
        &[],
        Return::Map,
        "one record by id or name, e.g. ostack.show(\"server\", \"web-1\")"
    ),
    ostack_fn!(
        "server",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one server by id or name (ostack.show(\"server\", ..))"
    ),
    ostack_fn!(
        "volume",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one volume by id or name (ostack.show(\"volume\", ..))"
    ),
    ostack_fn!(
        "port",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one port by id or name (ostack.show(\"port\", ..))"
    ),
    ostack_fn!(
        "network",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one network by id or name (ostack.show(\"network\", ..))"
    ),
    ostack_fn!(
        "subnet",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one subnet by id or name (ostack.show(\"subnet\", ..))"
    ),
    ostack_fn!(
        "image",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one image by id or name (ostack.show(\"image\", ..))"
    ),
    ostack_fn!(
        "flavor",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one flavor by id or name (ostack.show(\"flavor\", ..))"
    ),
    ostack_fn!(
        "hypervisor",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one hypervisor by id or name (ostack.show(\"hypervisor\", ..))"
    ),
    ostack_fn!(
        "user",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one user by id or name (ostack.show(\"user\", ..))"
    ),
    ostack_fn!(
        "loadbalancer",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one loadbalancer by id or name (ostack.show(\"loadbalancer\", ..))"
    ),
    ostack_fn!(
        "amphora",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one amphora by id or name (ostack.show(\"amphora\", ..))"
    ),
    ostack_fn!(
        "floating_ip",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one floating IP by id or address"
    ),
    ostack_fn!(
        "security_group",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one security group by id or name"
    ),
    ostack_fn!(
        "router",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one router by id or name"
    ),
    ostack_fn!(
        "keypair",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one keypair by id or name"
    ),
    ostack_fn!(
        "server_group",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one server group by id or name"
    ),
    ostack_fn!(
        "aggregate",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one aggregate by id or name"
    ),
    ostack_fn!(
        "availability_zone",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one availability zone by id or name"
    ),
    ostack_fn!(
        "compute_service",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one compute service by id or name"
    ),
    ostack_fn!(
        "network_agent",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one network agent by id or name"
    ),
    ostack_fn!(
        "snapshot",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one snapshot by id or name"
    ),
    ostack_fn!(
        "backup",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one backup by id or name"
    ),
    ostack_fn!(
        "pool",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one pool by id or name"
    ),
    ostack_fn!(
        "role_assignment",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "one role assignment by id or name"
    ),
    ostack_fn!(
        "get",
        2,
        [Argument::Str, Argument::Str, Argument::Map],
        &[],
        Return::Unknown,
        "a raw GET on a service: ostack.get(\"compute\", \"/os-services\")"
    ),
    ostack_fn!(
        "inventory",
        0,
        [Argument::List],
        OSTACK_LIST_NAMED,
        Return::MapListMap,
        "several collections fetched in parallel, by name"
    ),
    ostack_fn!(
        "project",
        1,
        [Argument::Str],
        &[],
        Return::Map,
        "a project by name or id; its .servers(), .volumes(), .list(kind), .inventory() are scoped to it"
    ),
    ostack_fn!(
        "all_projects",
        0,
        [],
        &[],
        Return::Map,
        "a handle whose .servers() and .volumes() span every project"
    ),
    ostack_fn!(
        "find",
        1,
        [Argument::Str],
        OSTACK_FIND_NAMED,
        Return::OptionMap,
        "the record with this id (or exactly this IP, MAC or name), as {kind, record}"
    ),
    ostack_fn!(
        "search",
        1,
        [Argument::Str],
        OSTACK_SEARCH_NAMED,
        Return::ListMap,
        "records matching an id, IP, MAC, name or fragment, as {kind, field, match, record, owner, project}"
    ),
    ostack_fn!(
        "references",
        1,
        [Argument::Str],
        &[("fresh", Argument::Bool), ("kinds", Argument::List)],
        Return::ListMap,
        "every record that mentions this id, as {kind, field, record}"
    ),
    ostack_fn!(
        "attached_volumes",
        1,
        [Argument::Map],
        &[],
        Return::ListMap,
        "the volumes attached to a server record (also server.attached_volumes())"
    ),
    ostack_fn!(
        "attached_servers",
        1,
        [Argument::Map],
        &[],
        Return::ListMap,
        "the servers a volume record is attached to (also volume.attached_servers())"
    ),
    ostack_fn!(
        "server_ports",
        1,
        [Argument::Map],
        &[],
        Return::ListMap,
        "the ports of a server record (also server.ports())"
    ),
    ostack_fn!(
        "project_of",
        1,
        [Argument::Map],
        &[],
        Return::OptionMap,
        "the project record a record belongs to (also record.project())"
    ),
    ostack_fn!(
        "quota",
        0,
        [],
        &[("project", Argument::Str), ("usage", Argument::Bool)],
        Return::ListMap,
        "quota rows {service, resource, limit, in_use, reserved} for a project (default: yours)"
    ),
    ostack_fn!(
        "actions",
        1,
        [Argument::Any],
        &[("events", Argument::Bool), ("limit", Argument::Int)],
        Return::ListMap,
        "a server's instance actions, newest first; events=true adds each action's events"
    ),
    ostack_fn!("refresh", 0, [], &[], Return::Null, "forget cached results"),
    NamespaceFunction {
        module: "time",
        name: "now",
        min_args: 0,
        positional: &[],
        named: &[],
        returns: Return::Time,
        summary: "the current time (UTC)",
    },
    NamespaceFunction {
        module: "time",
        name: "from_unix",
        min_args: 1,
        positional: &[Argument::Int],
        named: &[],
        returns: Return::Time,
        summary: "the time a Unix timestamp (seconds) stands for",
    },
];

/// A method's or builtin's parameter names and one-line description, for
/// signatures and IDE help.  Optional parameters read `name=default` or
/// `name?`; a variadic last parameter ends in `…`.
#[derive(Debug, Clone, Copy)]
pub struct Doc {
    pub name: &'static str,
    pub params: &'static [&'static str],
    pub summary: &'static str,
}

macro_rules! doc {
    ($name:literal, [$($param:literal),*], $summary:literal) => {
        Doc { name: $name, params: &[$($param),*], summary: $summary }
    };
}

pub static METHOD_DOCS: &[Doc] = &[
    doc!("get", ["key"], "a map's value for a Str key (null when absent), or a list's item at an Int index as an Option"),
    doc!("map", ["f"], "apply f to every item (maps: f(key, value)); on an Option or Result, to the value inside"),
    doc!("parallel_map", ["f"], "map on the worker pool, results in input order"),
    doc!("filter", ["predicate"], "the items for which the predicate is true"),
    doc!("parallel_filter", ["predicate"], "filter on the worker pool, order kept"),
    doc!("flatten", [], "one level of nested lists flattened into one list"),
    doc!("zip", ["other"], "pairs of items from both lists, as tuples, up to the shorter length"),
    doc!("enumerate", [], "(index, item) tuples"),
    doc!("group_by", ["key"], "a map from key(item), as text, to the list of items with that key, in first-seen order"),
    doc!("sort_by", ["key"], "the items sorted by key(item); stable, nulls first"),
    doc!("unique", [], "the items without repeats, keeping first occurrences"),
    doc!("all", ["predicate"], "whether the predicate holds for every item (true for an empty list)"),
    doc!("count", ["predicate"], "how many items satisfy the predicate"),
    doc!("sum", [], "the sum of the numbers (Int when all are Int, else Float; 0 for an empty list)"),
    doc!("index_by", ["key"], "a map from key(item), as text, to the item (later duplicates win)"),
    doc!("servers", [], "ostack: servers of a project handle, a hypervisor or a security group"),
    doc!("volumes", [], "ostack: volumes of a project handle"),
    doc!("ports", [], "ostack: ports of a project, server, router, network or subnet"),
    doc!("networks", [], "ostack: networks of a project handle"),
    doc!("subnets", [], "ostack: subnets of a project, network, port or router"),
    doc!("images", [], "ostack: images visible to a project handle"),
    doc!("flavors", [], "ostack: flavors visible to a project handle"),
    doc!("hypervisors", [], "ostack: hypervisors (on a project handle or an aggregate)"),
    doc!("projects", [], "ostack: projects visible to a handle"),
    doc!("users", [], "ostack: users visible to a handle"),
    doc!("loadbalancers", [], "ostack: load balancers of a project, or of a pool"),
    doc!("amphorae", [], "ostack: amphorae of a project handle"),
    doc!("floating_ips", [], "ostack: floating IPs of a project, server or port"),
    doc!("security_groups", [], "ostack: security groups of a project, server or port"),
    doc!("routers", [], "ostack: routers of a project, network or subnet"),
    doc!("server", [], "ostack: the server behind a port or floating IP (the router for router ports)"),
    doc!("network", [], "ostack: the network of a port or subnet"),
    doc!("port", [], "ostack: the port of a floating IP"),
    doc!("hypervisor", [], "ostack: the hypervisor a server runs on"),
    doc!("created_servers", [], "ostack: servers a user booted, in any project"),
    doc!("project_memberships", [], "ostack: projects a user can reach (directly or through groups), as handles"),
    doc!("keypairs", [], "ostack: keypairs visible to a project handle"),
    doc!("server_groups", [], "ostack: server groups of a project, or the groups a server belongs to"),
    doc!("aggregates", [], "ostack: host aggregates, or those containing a hypervisor"),
    doc!("availability_zones", [], "ostack: availability zones"),
    doc!("compute_services", [], "ostack: Nova services (admin)"),
    doc!("network_agents", [], "ostack: Neutron agents, or those on a hypervisor's host"),
    doc!("snapshots", [], "ostack: snapshots of a project or of a volume"),
    doc!("backups", [], "ostack: backups of a project or of a volume"),
    doc!("pools", [], "ostack: load balancer pools of a project or a load balancer"),
    doc!("role_assignments", [], "ostack: role assignments of a project or a user"),
    doc!("flavor", [], "ostack: a server's flavor record"),
    doc!("image", [], "ostack: a server's image record"),
    doc!("actions", [], "ostack: a server's instance actions, newest first"),
    doc!("compute_service", [], "ostack: the Nova compute service on a hypervisor's host"),
    doc!("quota", [], "ostack: a project's quota rows {service, resource, limit, in_use, reserved}"),
    doc!("volume", [], "ostack: the volume of a snapshot or backup"),
    doc!("members", [], "ostack: a load balancer pool's members"),
    doc!("list", ["kind"], "ostack: one collection (\"servers\", \"floating_ips\", …) within a project handle"),
    doc!("attached_volumes", [], "ostack: volumes attached to a server"),
    doc!("attached_servers", [], "ostack: servers a volume is attached to"),
    doc!("project", [], "ostack: the project a record belongs to, as a handle"),
    doc!("take", ["n"], "the first n items"),
    doc!("reverse", [], "the items in reverse order"),
    doc!("sort", [], "the items in natural order (numbers, else by text)"),
    doc!("flat_map", ["f"], "map, then flatten one level of the lists f returns"),
    doc!("find", ["predicate"], "the first item for which the predicate is true, as an Option"),
    doc!("any", ["predicate"], "whether the predicate holds for some item"),
    doc!("first", [], "the first item, as an Option"),
    doc!("last", [], "the last item, as an Option"),
    doc!("slice", ["start", "end"], "the items from start up to (not including) end; negatives count from the end"),
    doc!("skip", ["n"], "the items after the first n"),
    doc!("step", ["n"], "every n-th item, starting with the first"),
    doc!("difference", ["other"], "the items not in other"),
    doc!("symmetric_difference", ["other"], "the items in exactly one of the two lists"),
    doc!("contains", ["part"], "whether a string contains a substring, or a list an item"),
    doc!("len", [], "the number of characters, items or entries"),
    doc!("trim", [], "the string without leading and trailing whitespace"),
    doc!("lines", [], "the string's lines, without line endings"),
    doc!("split", ["separator"], "the pieces between occurrences of the separator"),
    doc!("upper", [], "the string in upper case"),
    doc!("lower", [], "the string in lower case"),
    doc!("starts_with", ["prefix"], "whether the string starts with prefix"),
    doc!("ends_with", ["suffix"], "whether the string ends with suffix"),
    doc!("repeat", ["n"], "the string repeated n times"),
    doc!("index_of", ["part"], "the character index of the first occurrence of part, or -1"),
    doc!("keys", [], "a map's keys, in insertion order"),
    doc!("values", [], "a map's values, in insertion order"),
    doc!("entries", [], "a map's (key, value) tuples"),
    doc!("contains_key", ["key"], "whether the map has the key"),
    doc!("merge", ["other"], "a map with other's entries added (other wins on conflicts)"),
    doc!("is_match", ["pattern"], "whether a string matches a regex (or a regex matches a string)"),
    doc!("sha256", [], "the SHA-256 digest of the string, as hex"),
    doc!("glob", [], "the paths matching the glob pattern"),
    doc!("parse_json", [], "the value encoded as JSON in the string"),
    doc!("parse_yaml", [], "the value encoded as YAML in the string"),
    doc!("parse_toml", [], "the value encoded as TOML in the string"),
    doc!("parse_csv", [], "the CSV rows as lists of strings"),
    doc!("parse_url", [], "a map of the URL's parts (scheme, host, port, path, query, …)"),
    doc!("to_json", [], "the value as compact JSON"),
    doc!("to_json_pretty", [], "the value as indented JSON"),
    doc!("to_yaml", [], "the value as YAML"),
    doc!("to_toml", [], "the value as TOML"),
    doc!("join", ["part"], "a path joined with part, or a list's strings joined with a separator"),
    doc!("exists", [], "whether the path exists"),
    doc!("read_text", [], "the file's contents as a Result"),
    doc!("read_yaml", [], "the file parsed as YAML, as a Result"),
    doc!("read_lines", [], "the file's lines"),
    doc!("write", ["text"], "replace the file's contents with text"),
    doc!("append", ["text"], "add text to the end of the file"),
    doc!("mkdir", [], "create the directory and its parents"),
    doc!("rename", ["to"], "move the file or directory"),
    doc!("stat", [], "a map {size, modified, is_file, is_dir, is_symlink}"),
    doc!("ls", [], "the directory's entries as paths"),
    doc!("parent", [], "the path's parent directory"),
    doc!("basename", [], "the last component of the path"),
    doc!("stem", [], "the file name without its extension"),
    doc!("extension", [], "the file extension without the dot"),
    doc!("is_file", [], "whether the path is a regular file"),
    doc!("is_dir", [], "whether the path is a directory"),
    doc!("canonicalize", [], "the absolute path with links resolved"),
    doc!("archive_list", [], "the names inside a tar archive"),
    doc!("archive_extract", ["to"], "unpack a tar archive into a directory"),
    doc!("run", [], "run an argv list as a process and wait for its result"),
    doc!("spawn", [], "start an argv list as a process; .await() gets its result"),
    doc!("parallel", [], "run a list of argv lists concurrently, results in order"),
    doc!("await", [], "wait for a spawned process and get its result"),
    doc!("sleep", [], "pause for the duration"),
    doc!("secs", [], "whole seconds of the duration"),
    doc!("ms", [], "whole milliseconds of the duration"),
    doc!("us", [], "whole microseconds of the duration"),
    doc!("ns", [], "whole nanoseconds of the duration"),
    doc!("as_secs_float", [], "the duration in seconds, as a Float"),
    doc!("human_size", [], "a byte count as text such as 1.5 GiB"),
    doc!("is_ok", [], "whether the Result is ok"),
    doc!("is_err", [], "whether the Result is an error"),
    doc!("ok", [], "the Result's ok value, as an Option"),
    doc!("err", [], "the Result's error, as an Option"),
    doc!("is_some", [], "whether the Option holds a value"),
    doc!("and_then", ["f"], "f applied to the value inside, flattening the Option or Result it returns"),
    doc!("map_err", ["f"], "the Result with f applied to its error"),
    doc!("unwrap_or_else", ["f"], "the value inside, or what f returns"),
    doc!("ok_or", ["error"], "the Option as a Result, with error for none"),
    doc!("expect", ["message"], "the value inside, or stop with message"),
    doc!("is_none", [], "whether the Option is empty"),
    doc!("unwrap", [], "the value inside, or stop with an error"),
    doc!("unwrap_or", ["default"], "the value inside, or default"),
    doc!("inventory", ["kinds?"], "ostack: several collections of a project handle, fetched in parallel"),
    doc!("parse_time", ["format?"], "the time in the string (RFC 3339/ISO 8601, a bare date, or a strftime format); raises when it does not parse"),
    doc!("year", [], "the year"),
    doc!("month", [], "the month, 1 to 12"),
    doc!("day", [], "the day of the month"),
    doc!("hour", [], "the hour, 0 to 23 (UTC)"),
    doc!("minute", [], "the minute"),
    doc!("second", [], "the second"),
    doc!("weekday", [], "the day of the week, such as Monday"),
    doc!("unix", [], "seconds since 1970-01-01T00:00:00Z"),
    doc!("date", [], "the date as YYYY-MM-DD"),
    doc!("iso", [], "the time as RFC 3339 (2026-10-15T14:29:21Z)"),
    doc!("format", ["fmt"], "the time formatted with strftime syntax (%Y-%m-%d %H:%M)"),
    doc!("age", [], "how long ago the time was (zero for a future time)"),
    doc!("parse_xml", [], "the root element of the XML in the string, as a record {tag, attrs, text, children}"),
    doc!("select", ["path"], "every match of an XPath-like path (a/b, //a, a[@k='v'], a[2], …/@attr, …/text()) under an XML element or list of elements"),
    doc!("select_one", ["path"], "the first match of an XPath-like path under an XML element, or none"),
    doc!("to_xml", [], "the XML element record as indented XML"),
    doc!("replace", ["pattern", "with", "all=false"], "the string with the first (or every, all=true) match of a string or regex replaced"),
];

pub static BUILTIN_DOCS: &[Doc] = &[
    doc!("Str", ["value"], "the value as text"),
    doc!("Bool", ["value"], "the value as a Bool"),
    doc!("Int", ["value"], "the value as an Int (parses strings)"),
    doc!("Float", ["value"], "the value as a Float (parses strings)"),
    doc!("type", ["value"], "the name of the value's type"),
    doc!("print", ["values…"], "write the values and a newline to stdout"),
    doc!("prin", ["values…"], "write the values without a newline"),
    doc!("pretty_print", ["value"], "write the value indented and coloured"),
    doc!(
        "show",
        ["value", "title?"],
        "show the value: records as a table (in eddy, a live one), the rest as a tree"
    ),
    doc!("warn", ["values…"], "write the values to stderr"),
    doc!("die", ["message?"], "stop the script with an error"),
    doc!("set_env", ["name", "value"], "set an environment variable for later commands"),
    doc!("path", ["text"], "a Path (literals expand ~)"),
    doc!("file", ["text"], "a Path to a file"),
    doc!("sh", ["command"], "run a shell command line through sh -c"),
    doc!("docker", ["container"], "the `docker exec` prefix for `on docker(name) { }`"),
    doc!("exit", ["code?"], "stop the script with an exit code (default 0)"),
    doc!("some", ["value"], "an Option holding the value"),
    doc!("none", [], "an empty Option"),
    doc!("ok", ["value"], "an ok Result"),
    doc!("err", ["error"], "an error Result"),
];

pub fn method_doc(name: &str) -> Option<&'static Doc> {
    METHOD_DOCS.iter().find(|doc| doc.name == name)
}

pub fn builtin_doc(name: &str) -> Option<&'static Doc> {
    BUILTIN_DOCS.iter().find(|doc| doc.name == name)
}

/// `replace(pattern, with, all=false)`.
pub fn signature(name: &str, params: &[&str]) -> String {
    format!("{name}({})", params.join(", "))
}

/// Parameter names of a namespace function, positional then `name=`.
pub fn namespace_params(function: &NamespaceFunction) -> Vec<String> {
    let positional: &[&str] = match function.name {
        "list" => &["kind"],
        "show" => &["kind", "id_or_name"],
        "get" => &["service", "path", "query?"],
        "inventory" => &["kinds?"],
        "find" => &["id_or_address"],
        "search" => &["text"],
        "references" => &["id"],
        "project" => &["name_or_id"],
        "attached_volumes" | "server_ports" => &["server"],
        "attached_servers" => &["volume"],
        "project_of" => &["record"],
        "actions" => &["server"],
        "from_unix" => &["seconds"],
        _ if function.positional.len() == 1 => &["id_or_name"],
        _ => &[],
    };
    let mut params: Vec<String> =
        positional.iter().take(function.positional.len()).map(|p| p.to_string()).collect();
    params.extend(function.named.iter().map(|(name, _)| format!("{name}=")));
    params
}

pub fn namespace_function(module: &str, name: &str) -> Option<&'static NamespaceFunction> {
    NAMESPACE_FUNCTIONS.iter().find(|function| function.module == module && function.name == name)
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
    NamespaceAlias {
        module: "time",
        name: "parse",
        method: "parse_time",
        adapter: ReceiverAdapter::Direct,
    },
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
    "hashes", "ostack",
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
    names.extend(
        NAMESPACE_FUNCTIONS
            .iter()
            .filter(|function| function.module == module)
            .map(|function| function.name),
    );
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
