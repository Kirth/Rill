//! Record types: maps with a declared shape (`type Name { field: T }`).
//! At runtime a record is a plain Map; the declaration informs the checker,
//! completion and hover.  The ostack kinds come as built-in records,
//! declared in Rill in `ostack.ril`.

use crate::ast::{FieldDecl, Stmt, TypeName};
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// The built-in record declarations, in Rill.
pub const OSTACK_PRELUDE: &str = include_str!("ostack.ril");

/// Each ostack kind as `(singular, plural, record type)`.
pub const OSTACK_RECORDS: &[(&str, &str, &str)] = &[
    ("server", "servers", "OSServer"),
    ("volume", "volumes", "OSVolume"),
    ("port", "ports", "OSPort"),
    ("network", "networks", "OSNetwork"),
    ("subnet", "subnets", "OSSubnet"),
    ("image", "images", "OSImage"),
    ("flavor", "flavors", "OSFlavor"),
    ("hypervisor", "hypervisors", "OSHypervisor"),
    ("project", "projects", "OSProject"),
    ("user", "users", "OSUser"),
    ("loadbalancer", "loadbalancers", "OSLoadbalancer"),
    ("amphora", "amphorae", "OSAmphora"),
    ("floating_ip", "floating_ips", "OSFloatingIp"),
    ("security_group", "security_groups", "OSSecurityGroup"),
    ("router", "routers", "OSRouter"),
    ("keypair", "keypairs", "OSKeypair"),
    ("server_group", "server_groups", "OSServerGroup"),
    ("aggregate", "aggregates", "OSAggregate"),
    ("availability_zone", "availability_zones", "OSAvailabilityZone"),
    ("compute_service", "compute_services", "OSComputeService"),
    ("network_agent", "network_agents", "OSNetworkAgent"),
    ("snapshot", "snapshots", "OSSnapshot"),
    ("backup", "backups", "OSBackup"),
    ("pool", "pools", "OSPool"),
    ("member", "members", "OSMember"),
    ("role_assignment", "role_assignments", "OSRoleAssignment"),
];

/// A declared record type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
}

impl RecordDecl {
    pub fn field(&self, name: &str) -> Option<&FieldDecl> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// The ostack kind this record describes (`"server"` for `OSServer`).
    pub fn ostack_kind(&self) -> Option<&'static str> {
        ostack_kind_of(&self.name)
    }
}

/// A record type as hosts show it: fields with their types and docs, and
/// for an ostack record the relations it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordInfo {
    pub name: String,
    pub fields: Vec<FieldInfo>,
    /// `(method, return type)`, e.g. `("ports", "List[OSPort]")`.
    pub relations: Vec<(String, String)>,
    pub ostack_kind: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub name: String,
    pub ty: String,
    pub optional: bool,
    pub doc: Option<String>,
}

impl RecordInfo {
    pub fn field(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|field| field.name == name)
    }
}

/// The built-in records, by name.
pub fn builtins() -> &'static BTreeMap<String, RecordDecl> {
    static BUILTINS: OnceLock<BTreeMap<String, RecordDecl>> = OnceLock::new();
    BUILTINS.get_or_init(|| {
        let program = crate::parser::parse(OSTACK_PRELUDE).expect("ostack.ril parses");
        declarations(&program.stmts).into_iter().map(|decl| (decl.name.clone(), decl)).collect()
    })
}

pub fn builtin(name: &str) -> Option<&'static RecordDecl> {
    builtins().get(name)
}

/// The record declarations among `stmts` (top level only).
pub fn declarations(stmts: &[Stmt]) -> Vec<RecordDecl> {
    stmts
        .iter()
        .filter_map(|stmt| match stmt.unspanned() {
            Stmt::TypeDecl { name, fields } => {
                Some(RecordDecl { name: name.clone(), fields: fields.clone() })
            }
            _ => None,
        })
        .collect()
}

/// The record type of an ostack kind, singular or plural (`"servers"` and
/// `"server"` are both `OSServer`).
pub fn ostack_record_name(kind: &str) -> Option<&'static str> {
    OSTACK_RECORDS
        .iter()
        .find(|(singular, plural, _)| *singular == kind || *plural == kind)
        .map(|(_, _, name)| *name)
}

/// The ostack kind of a built-in record name.
pub fn ostack_kind_of(name: &str) -> Option<&'static str> {
    OSTACK_RECORDS.iter().find(|(_, _, record)| *record == name).map(|(kind, _, _)| *kind)
}

/// What a relation or scoped listing on an ostack record returns: the
/// record it yields and whether it yields a list (`server.ports()`) or an
/// Option of one (`port.network()`).  `None` for methods that return
/// something else (`quota`, `actions`, `inventory`).
pub fn ostack_relation(method: &str) -> Option<(&'static str, bool)> {
    let many = |name: &str| ostack_record_name(name).map(|record| (record, true));
    match method {
        "attached_volumes" => many("volumes"),
        "attached_servers" | "created_servers" => many("servers"),
        "project_memberships" => many("projects"),
        _ => OSTACK_RECORDS.iter().find_map(|(singular, plural, record)| {
            if *plural == method {
                Some((*record, true))
            } else if *singular == method {
                Some((*record, false))
            } else {
                None
            }
        }),
    }
}

/// The names of the types a field can be declared with, for completion.
pub fn type_name_text(name: &TypeName) -> String {
    match name {
        TypeName::Null => "Null".into(),
        TypeName::Any => "Any".into(),
        TypeName::Bool => "Bool".into(),
        TypeName::Int => "Int".into(),
        TypeName::Float => "Float".into(),
        TypeName::Str => "Str".into(),
        TypeName::Bytes => "Bytes".into(),
        TypeName::ListAny => "List".into(),
        TypeName::List(inner) => format!("List[{}]", type_name_text(inner)),
        TypeName::Tuple(items) => {
            let items: Vec<String> = items.iter().map(type_name_text).collect();
            format!("Tuple[{}]", items.join(", "))
        }
        TypeName::Map(None) => "Map".into(),
        TypeName::Map(Some(value)) => format!("Map[Str, {}]", type_name_text(value)),
        TypeName::Duration => "Duration".into(),
        TypeName::Time => "Time".into(),
        TypeName::Db => "Db".into(),
        TypeName::Path => "Path".into(),
        TypeName::Regex => "Regex".into(),
        TypeName::Error => "Error".into(),
        TypeName::ProcessResult => "ProcessResult".into(),
        TypeName::ProcessHandle => "ProcessHandle".into(),
        TypeName::Function { .. } => "Fn".into(),
        TypeName::Option(inner) => format!("Option[{}]", type_name_text(inner)),
        TypeName::Result(ok, None) => format!("Result[{}]", type_name_text(ok)),
        TypeName::Result(ok, Some(err)) => {
            format!("Result[{}, {}]", type_name_text(ok), type_name_text(err))
        }
        TypeName::Named(name) => name.clone(),
    }
}
