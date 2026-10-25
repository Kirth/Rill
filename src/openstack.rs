//! The `ostack.*` namespace: OpenStack through Kirth's `ostack` crate.
//!
//! Calls are read-only.  Records are full (every field the API returned, with
//! ostack's normalisation of names), converted once to Rill values and kept in
//! a process-wide cache so a session that asks the same question twice, or an
//! IDE that replays cells, does not hit the cloud again.  Credentials come
//! from the `OS_*` variables of the evaluator's environment, so `with env {}`
//! applies; the token cache under `~/.cache/ostack` is shared with the CLI.
//!
//! `RILL_OPENSTACK_FIXTURE=<dir>` serves `<dir>/<collection>.json` files
//! instead of a cloud, for tests and demos.

use crate::blocking::env_secs;
use crate::diagnostic::RillError;
use crate::eval::CancellationToken;
use crate::eval_data::json_to_value;
use crate::methods::{ostack_record_kind, OSTACK_KINDS, OSTACK_MARKER};
use crate::types::{ErrorKind, Map, Value};
use ostack::{AuthConfig, OpenStackClient, ResourceKind};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

type Record = serde_json::Map<String, Json>;
type Query = Vec<(String, String)>;
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = anyhow::Result<T>> + Send + 'a>>;

/// One `ostack.name(...)` call from the evaluator.
pub struct Call<'a> {
    pub name: &'static str,
    pub positional: Vec<Value>,
    pub named: Vec<(String, Value)>,
    /// The evaluator's environment (`OS_*`, `RILL_OPENSTACK_*`).
    pub lookup_env: &'a dyn Fn(&str) -> Option<String>,
    pub cancellation: CancellationToken,
}

pub use crate::blocking::Error;

const DEFAULT_KINDS: &[&str] = &[
    "servers",
    "volumes",
    "ports",
    "networks",
    "subnets",
    "images",
    "flavors",
    "hypervisors",
    "projects",
    "floating_ips",
    "security_groups",
    "routers",
];

/// Entry point used by the evaluator.
pub fn call(call: Call<'_>) -> Result<Value, Error> {
    let Call { name, positional, named, lookup_env, cancellation } = call;
    let args = Args { positional, named };
    match name {
        "kinds" => {
            return Ok(Value::list(
                OSTACK_KINDS.iter().map(|k| Value::Str(k.to_string())).collect(),
            ))
        }
        "refresh" => {
            cache().lock().expect("ostack cache").clear();
            return Ok(Value::Null);
        }
        _ => {}
    }
    let backend = backend_for(lookup_env, &cancellation)?;
    {
        match name {
            "info" => {
                let b = backend.clone();
                let record = run(async move { b.info().await }, &cancellation)?;
                Ok(json_to_value(Json::Object(record)))
            }
            "list" => {
                let kind = kind_arg(args.positional.first())?;
                list(&backend, kind, &args, &cancellation)
            }
            "show" => {
                let kind = kind_arg(args.positional.first())?;
                let target = args.str_at(1, "id or name")?;
                let b = backend.clone();
                let record = run(async move { b.show(kind, target).await }, &cancellation)?;
                Ok(json_to_value(Json::Object(record)))
            }
            "get" => {
                let service = args.str_at(0, "service type")?;
                let path = args.str_at(1, "path")?;
                let query = match args.positional.get(2) {
                    Some(Value::Map(map)) => pairs(map),
                    _ => Vec::new(),
                };
                let b = backend.clone();
                let body =
                    run(async move { b.raw_get(service, path, query).await }, &cancellation)?;
                Ok(json_to_value(body))
            }
            "inventory" => {
                let kinds = match args.positional.first() {
                    Some(value) => kinds_from(value)?,
                    None => default_kinds(),
                };
                let map = inventory(&backend, &kinds, &args, &cancellation)?;
                Ok(Value::map(map))
            }
            "project" => {
                let wanted = args.str_at(0, "project name or id")?;
                let id = resolve_project(&backend, &wanted, &cancellation)?;
                let projects =
                    list_cached(&backend, ResourceKind::Project, Vec::new(), false, &cancellation)?;
                let record = match &projects {
                    Value::List(items) => items
                        .iter()
                        .find(|item| record_id(item).as_deref() == Some(id.as_str()))
                        .and_then(|item| match item {
                            Value::Map(map) => Some((**map).clone()),
                            _ => None,
                        }),
                    _ => None,
                };
                let mut handle = Map::new();
                handle.insert(OSTACK_MARKER.to_string(), Value::Str("project".to_string()));
                match record {
                    Some(record) => handle.extend(record),
                    None => {
                        handle.insert("id".to_string(), Value::Str(id));
                        handle.insert("name".to_string(), Value::Str(wanted));
                    }
                }
                Ok(Value::map(handle))
            }
            "attached_volumes" | "attached_servers" | "server_ports" | "project_of" => {
                let record = match args.positional.first() {
                    Some(Value::Map(map)) => map.clone(),
                    _ => return Err(RillError::new("ostack: a record map is needed").into()),
                };
                let (kind, method) = match name {
                    "attached_volumes" => ("server", "attached_volumes"),
                    "attached_servers" => ("volume", "attached_servers"),
                    "server_ports" => ("server", "ports"),
                    _ => (ostack_record_kind(&record).unwrap_or("record"), "project"),
                };
                relation(&backend, kind, method, &record, &cancellation)
            }
            "quota" => {
                let project = match args.str_named("project")? {
                    Some(project) => Some(resolve_project(&backend, &project, &cancellation)?),
                    None => None,
                };
                let usage = match args.named_value("usage") {
                    Some(Value::Bool(flag)) => *flag,
                    _ => true,
                };
                quota(&backend, project, usage, &cancellation)
            }
            "actions" => {
                let server = match args.positional.first() {
                    Some(Value::Map(map)) => map_id(map).unwrap_or_default(),
                    Some(Value::Str(text)) if looks_like_id(ResourceKind::Server, text) => {
                        text.clone()
                    }
                    Some(Value::Str(text)) => {
                        let found =
                            show_or_fragment(&backend, ResourceKind::Server, text, &cancellation)?;
                        as_map(&found).and_then(map_id).unwrap_or_default()
                    }
                    _ => {
                        return Err(RillError::new("ostack.actions needs a server or its id").into())
                    }
                };
                let events = args.bool_named("events")?;
                let limit = match args.named_value("limit") {
                    Some(Value::Int(n)) if *n > 0 => *n as usize,
                    _ if events => 20,
                    _ => 0,
                };
                actions(&backend, &server, events, limit, &cancellation)
            }
            "all_projects" => {
                let mut handle = Map::new();
                handle.insert(OSTACK_MARKER.to_string(), Value::Str("all".to_string()));
                Ok(Value::map(handle))
            }
            "find" => {
                let needle = args.str_at(0, "id, address or name")?;
                if is_ip(&needle) || is_mac(&needle) {
                    let hits =
                        address_search(&backend, &needle, &args, &default_kinds(), &cancellation)?;
                    let best = hits.into_iter().next().and_then(|hit| {
                        let map = as_map(&hit)?.clone();
                        let mut out = Map::new();
                        for key in ["kind", "field", "record"] {
                            out.insert(
                                key.to_string(),
                                map.get(key).cloned().unwrap_or(Value::Null),
                            );
                        }
                        Some(Value::map(out))
                    });
                    return Ok(Value::Option(best.map(Box::new)));
                }
                let map = inventory(&backend, &default_kinds(), &args, &cancellation)?;
                if let Some(hit) = find_record(&map, &needle) {
                    return Ok(Value::Option(Some(Box::new(hit))));
                }
                // Not an id: the best exact match on an address, MAC or name.
                let best = search(&map, &needle).into_iter().find(|hit| hit.exact).map(|hit| {
                    let mut out = Map::new();
                    out.insert("kind".to_string(), Value::Str(hit.kind.clone()));
                    out.insert("field".to_string(), Value::Str(hit.field.clone()));
                    out.insert("record".to_string(), hit.record.clone());
                    Value::map(out)
                });
                Ok(Value::Option(best.map(Box::new)))
            }
            "search" => {
                let needle = args.str_at(0, "text to look for")?;
                let kinds = match args.named_value("kinds") {
                    Some(value) => kinds_from(value)?,
                    None => default_kinds(),
                };
                if is_ip(needle.trim()) || is_mac(needle.trim()) {
                    let hits =
                        address_search(&backend, needle.trim(), &args, &kinds, &cancellation)?;
                    return Ok(Value::list(hits));
                }
                let mut wanted = default_kinds();
                for kind in &kinds {
                    if !wanted.iter().any(|k| k.name() == kind.name()) {
                        wanted.push(*kind);
                    }
                }
                let map = inventory(&backend, &wanted, &args, &cancellation)?;
                let only: Vec<&str> = kinds.iter().map(|k| k.singular()).collect();
                let hits = search(&map, &needle)
                    .into_iter()
                    .filter(|hit| only.contains(&hit.kind.as_str()))
                    .map(|hit| hit.into_value(&map))
                    .collect();
                Ok(Value::list(hits))
            }
            "references" => {
                let id = args.str_at(0, "id")?;
                let kinds = match args.named_value("kinds") {
                    Some(value) => kinds_from(value)?,
                    None => default_kinds(),
                };
                let map = inventory(&backend, &kinds, &args, &cancellation)?;
                Ok(Value::list(references(&map, &id)))
            }
            kind_name => {
                let kind = ResourceKind::from_name(kind_name).ok_or_else(|| {
                    RillError::new(format!("ostack has no function '{kind_name}'"))
                })?;
                if kind_name == kind.singular() {
                    // `ostack.server(x)`: one record, like show, then a unique
                    // name fragment when nothing matches exactly.
                    let target = args.str_at(0, "id or name")?;
                    return show_or_fragment(&backend, kind, &target, &cancellation);
                }
                list(&backend, kind, &args, &cancellation)
            }
        }
    }
}

struct Args {
    positional: Vec<Value>,
    named: Vec<(String, Value)>,
}

impl Args {
    fn str_at(&self, index: usize, what: &str) -> Result<String, RillError> {
        match self.positional.get(index) {
            Some(Value::Str(text)) => Ok(text.clone()),
            Some(other) => Err(RillError::new(format!(
                "ostack: argument {} ({what}) must be a Str, got {}",
                index + 1,
                other.type_name()
            ))),
            None => Err(RillError::new(format!("ostack: missing argument {} ({what})", index + 1))),
        }
    }

    fn named_value(&self, name: &str) -> Option<&Value> {
        self.named.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    fn bool_named(&self, name: &str) -> Result<bool, RillError> {
        match self.named_value(name) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(RillError::new(format!(
                "ostack: {name}= must be a Bool, got {}",
                other.type_name()
            ))),
        }
    }

    fn str_named(&self, name: &str) -> Result<Option<String>, RillError> {
        match self.named_value(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Str(text)) => Ok(Some(text.clone())),
            Some(other) => Err(RillError::new(format!(
                "ostack: {name}= must be a Str, got {}",
                other.type_name()
            ))),
        }
    }
}

fn default_kinds() -> Vec<ResourceKind> {
    DEFAULT_KINDS.iter().filter_map(|name| ResourceKind::from_name(name)).collect()
}

fn kind_arg(value: Option<&Value>) -> Result<ResourceKind, RillError> {
    let Some(Value::Str(name)) = value else {
        return Err(RillError::new("ostack: the kind must be a Str such as \"servers\""));
    };
    ResourceKind::from_name(name).ok_or_else(|| {
        RillError::new(format!("ostack: unknown kind '{name}'")).with_help(
            match crate::suggest::did_you_mean(name, OSTACK_KINDS.iter().copied()) {
                Some(hint) => hint,
                None => format!("kinds: {}", OSTACK_KINDS.join(", ")),
            },
        )
    })
}

fn kinds_from(value: &Value) -> Result<Vec<ResourceKind>, RillError> {
    let Value::List(items) = value else {
        return Err(RillError::new("ostack: kinds must be a List of names"));
    };
    items.iter().map(|item| kind_arg(Some(item))).collect()
}

fn pairs(map: &Map) -> Vec<(String, String)> {
    map.iter().map(|(k, v)| (k.clone(), v.as_string_lossy())).collect()
}

// ---------------------------------------------------------------- runtime

/// Run `future` on the shared runtime (see [`crate::blocking::run`]).
fn run<T: Send + 'static>(
    future: impl Future<Output = anyhow::Result<T>> + Send + 'static,
    cancellation: &CancellationToken,
) -> Result<T, Error> {
    crate::blocking::run(
        async move { future.await.map_err(rill_error) },
        cancellation,
        env_secs("RILL_OPENSTACK_TIMEOUT_SECS", 60),
        |timeout| {
            format!("ostack: no answer within {}s (RILL_OPENSTACK_TIMEOUT_SECS)", timeout.as_secs())
        },
    )
}

fn rill_error(error: anyhow::Error) -> RillError {
    if let Some(config) = error.downcast_ref::<ostack::ConfigError>() {
        return RillError::new(format!("ostack: {config}")).with_help(
            "set OS_AUTH_URL plus OS_USERNAME/OS_PASSWORD/OS_PROJECT_NAME or \
             OS_APPLICATION_CREDENTIAL_ID/OS_APPLICATION_CREDENTIAL_SECRET",
        );
    }
    if let Some(ambiguous) = error.downcast_ref::<ostack::openstack::AmbiguousLookup>() {
        return RillError::new(format!("ostack: {ambiguous}"))
            .with_help("pass an id instead of a name");
    }
    RillError::new(format!("ostack: {error:#}")).with_kind(ErrorKind::Io)
}

// ---------------------------------------------------------------- backends

trait Backend: Send + Sync {
    fn identity(&self) -> String;
    fn info(&self) -> BoxFuture<'_, Record>;
    fn list_full(
        &self,
        kind: ResourceKind,
        query: Vec<(String, String)>,
    ) -> BoxFuture<'_, Vec<Record>>;
    fn show(&self, kind: ResourceKind, id_or_name: String) -> BoxFuture<'_, Record>;
    fn raw_get(
        &self,
        service: String,
        path: String,
        query: Vec<(String, String)>,
    ) -> BoxFuture<'_, Json>;
    /// Quota rows `{service, scope, resource, limit, in_use, reserved}` for a
    /// project (the token's project when `None`).
    fn quota(&self, project: Option<String>, usage: bool) -> BoxFuture<'_, Vec<Record>>;
}

/// Query keys that scope a listing rather than filter it.
fn is_scope_param(key: &str) -> bool {
    matches!(key, "all_tenants" | "all_projects" | "project_id" | "tenant_id" | "include_names")
}

fn json_text(value: &Json) -> Option<String> {
    match value {
        Json::String(text) => Some(text.clone()),
        Json::Number(number) => Some(number.to_string()),
        Json::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Every IP address a server record lists, structured or flattened.
fn server_ips(record: &Record) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(Json::Array(networks)) = record.get("networks") {
        out.extend(networks.iter().filter_map(|n| n.get("addr").and_then(json_text)));
    }
    if let Some(Json::String(addresses)) = record.get("addresses") {
        let tokens = addresses.split(|c: char| !(c.is_ascii_hexdigit() || c == '.' || c == ':'));
        out.extend(
            tokens.filter(|t| t.contains('.') || t.matches(':').count() >= 2).map(str::to_string),
        );
    }
    out
}

/// Whether a record satisfies an OpenStack list query, the way the APIs
/// filter.  The fixture backend filters with it, and the live backend falls
/// back to it when an API rejects a filter.
fn record_matches(record: &Record, query: &[(String, String)]) -> bool {
    query.iter().all(|(key, wanted)| {
        let field_is = |field: &str| match record.get(field) {
            Some(Json::Array(items)) => items.iter().any(|item| {
                json_text(item).as_deref() == Some(wanted.as_str())
                    || item.get("id").and_then(json_text).as_deref() == Some(wanted.as_str())
            }),
            Some(value) => json_text(value).as_deref() == Some(wanted.as_str()),
            None => false,
        };
        match key.as_str() {
            "all_tenants" | "all_projects" | "limit" | "marker" | "include_names" | "effective" => {
                true
            }
            "project_id" | "tenant_id" => project_matches(record.get("project_id"), wanted),
            "host" => field_is("OS-EXT-SRV-ATTR:host") || field_is("host"),
            "node" => field_is("OS-EXT-SRV-ATTR:hypervisor_hostname"),
            "user.id" => field_is("user_id"),
            "loadbalancer_id" => field_is("loadbalancer_id") || field_is("loadbalancers"),
            "scope.project.id" => field_is("project_id"),
            "mac_address" => record
                .get("mac_address")
                .and_then(Json::as_str)
                .is_some_and(|mac| mac.eq_ignore_ascii_case(wanted)),
            "fixed_ips" => {
                let (sub, value) = wanted.split_once('=').unwrap_or(("ip_address", wanted));
                record.get("fixed_ips").and_then(Json::as_array).is_some_and(|ips| {
                    ips.iter().any(|ip| ip.get(sub).and_then(Json::as_str) == Some(value))
                })
            }
            "ip" => {
                let ip = wanted.trim_start_matches('^').trim_end_matches('$').replace('\\', "");
                server_ips(record).contains(&ip)
            }
            other => field_is(other),
        }
    })
}

/// The project rule: records without a project (images, flavors,
/// hypervisors) match any project filter.
fn project_matches(project: Option<&Json>, wanted: &str) -> bool {
    match project {
        None | Some(Json::Null) => true,
        Some(value) => json_text(value).is_some_and(|text| text == wanted),
    }
}

struct LiveBackend {
    client: OpenStackClient,
    identity: String,
}

impl Backend for LiveBackend {
    fn identity(&self) -> String {
        self.identity.clone()
    }

    fn info(&self) -> BoxFuture<'_, Record> {
        Box::pin(async move { Ok(self.client.info_record().await) })
    }

    fn list_full(
        &self,
        kind: ResourceKind,
        query: Vec<(String, String)>,
    ) -> BoxFuture<'_, Vec<Record>> {
        Box::pin(async move {
            match self.client.list_with_query_full(kind, &query).await {
                Ok(records) => Ok(records),
                // An API that does not know a filter: list within the same
                // scope and filter here instead.
                Err(error)
                    if format!("{error:#}").contains("400")
                        && query.iter().any(|(k, _)| !is_scope_param(k)) =>
                {
                    let scope: Vec<(String, String)> =
                        query.iter().filter(|(k, _)| is_scope_param(k)).cloned().collect();
                    let records = self.client.list_with_query_full(kind, &scope).await?;
                    Ok(records.into_iter().filter(|r| record_matches(r, &query)).collect())
                }
                Err(error) => Err(error),
            }
        })
    }

    fn show(&self, kind: ResourceKind, id_or_name: String) -> BoxFuture<'_, Record> {
        Box::pin(async move { self.client.show(kind, &id_or_name).await })
    }

    fn raw_get(
        &self,
        service: String,
        path: String,
        query: Vec<(String, String)>,
    ) -> BoxFuture<'_, Json> {
        Box::pin(async move { self.client.raw_get(&service, &path, &query).await })
    }

    fn quota(&self, project: Option<String>, usage: bool) -> BoxFuture<'_, Vec<Record>> {
        Box::pin(async move {
            let project = match project {
                Some(project) => project,
                None => self
                    .client
                    .info_record()
                    .await
                    .get("project_id")
                    .and_then(json_text)
                    .ok_or_else(|| anyhow::anyhow!("the token has no project"))?,
            };
            let services = [
                ostack::QuotaService::Compute,
                ostack::QuotaService::Volume,
                ostack::QuotaService::Network,
            ];
            let report = self
                .client
                .show_quotas(ostack::QuotaScope::Project(project), &services, usage)
                .await?;
            Ok(ostack::report_records(&report))
        })
    }
}

/// JSON files standing in for a cloud.
struct FixtureBackend {
    dir: PathBuf,
}

impl FixtureBackend {
    fn log(&self, entry: String) {
        fixture_logs()
            .lock()
            .expect("fixture log")
            .entry(self.dir.clone())
            .or_default()
            .push(entry);
    }

    fn records(&self, kind: ResourceKind) -> anyhow::Result<Vec<Record>> {
        self.records_file(kind.name())
    }

    fn records_file(&self, name: &str) -> anyhow::Result<Vec<Record>> {
        let path = self.dir.join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .map_err(|error| anyhow::anyhow!("fixture {}: {error}", path.display()))?;
        let value: Json = serde_json::from_str(&text)
            .map_err(|error| anyhow::anyhow!("fixture {}: {error}", path.display()))?;
        match value {
            Json::Array(items) => Ok(items
                .into_iter()
                .filter_map(|item| match item {
                    Json::Object(record) => Some(record),
                    _ => None,
                })
                .collect()),
            _ => Err(anyhow::anyhow!("fixture {} is not a JSON array", path.display())),
        }
    }
}

impl Backend for FixtureBackend {
    fn identity(&self) -> String {
        format!("fixture:{}", self.dir.display())
    }

    fn info(&self) -> BoxFuture<'_, Record> {
        Box::pin(async move {
            let path = self.dir.join("info.json");
            match std::fs::read_to_string(&path) {
                Ok(text) => Ok(serde_json::from_str(&text)?),
                Err(_) => {
                    let mut record = Record::new();
                    record.insert("auth_method".into(), Json::String("fixture".into()));
                    record.insert("fixture".into(), Json::String(self.dir.display().to_string()));
                    Ok(record)
                }
            }
        })
    }

    fn list_full(
        &self,
        kind: ResourceKind,
        query: Vec<(String, String)>,
    ) -> BoxFuture<'_, Vec<Record>> {
        Box::pin(async move {
            self.log(format!("list {} {}", kind.name(), query_key(&query)));
            let records = self.records(kind)?;
            Ok(records.into_iter().filter(|r| record_matches(r, &query)).collect())
        })
    }

    fn show(&self, kind: ResourceKind, id_or_name: String) -> BoxFuture<'_, Record> {
        Box::pin(async move {
            self.log(format!("show {} {id_or_name}", kind.name()));
            let records = self.records(kind)?;
            let field = |record: &Record, key: &str| match record.get(key) {
                Some(Json::String(text)) => Some(text.clone()),
                Some(Json::Number(number)) => Some(number.to_string()),
                _ => None,
            };
            if let Some(record) =
                records.iter().find(|r| field(r, "id").as_deref() == Some(&id_or_name))
            {
                return Ok(record.clone());
            }
            let name_key = if matches!(kind, ResourceKind::Hypervisor) {
                "hypervisor_hostname"
            } else {
                "name"
            };
            let matches: Vec<&Record> = records
                .iter()
                .filter(|r| field(r, name_key).as_deref() == Some(&id_or_name))
                .collect();
            match matches.as_slice() {
                [record] => Ok((*record).clone()),
                [] => Err(anyhow::anyhow!("no {} found matching '{id_or_name}'", kind.singular())),
                many => Err(anyhow::anyhow!(
                    "ambiguous {} '{id_or_name}': matches {}",
                    kind.singular(),
                    many.iter().filter_map(|r| field(r, "id")).collect::<Vec<_>>().join(", ")
                )),
            }
        })
    }

    fn raw_get(
        &self,
        service: String,
        path: String,
        _query: Vec<(String, String)>,
    ) -> BoxFuture<'_, Json> {
        Box::pin(async move {
            self.log(format!("get {service} {path}"));
            let file = self
                .dir
                .join("raw")
                .join(&service)
                .join(format!("{}.json", path.trim_matches('/').replace('/', "_")));
            let text = std::fs::read_to_string(&file)
                .map_err(|error| anyhow::anyhow!("fixture {}: {error}", file.display()))?;
            Ok(serde_json::from_str(&text)?)
        })
    }

    fn quota(&self, project: Option<String>, usage: bool) -> BoxFuture<'_, Vec<Record>> {
        Box::pin(async move {
            self.log(format!("quota {} {usage}", project.as_deref().unwrap_or("-")));
            let rows = self.records_file("quota")?;
            Ok(rows
                .into_iter()
                .filter(|row| {
                    project.as_deref().map_or(true, |p| {
                        row.get("scope").and_then(Json::as_str).map_or(true, |s| s == p)
                    })
                })
                .collect())
        })
    }
}

fn fixture_logs() -> &'static Mutex<HashMap<PathBuf, Vec<String>>> {
    static LOGS: OnceLock<Mutex<HashMap<PathBuf, Vec<String>>>> = OnceLock::new();
    LOGS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The requests a fixture directory has served, oldest first (for tests
/// that check a relation asked for a filtered listing).
#[doc(hidden)]
pub fn fixture_requests(dir: &std::path::Path) -> Vec<String> {
    fixture_logs().lock().expect("fixture log").get(dir).cloned().unwrap_or_default()
}

fn clients() -> &'static Mutex<HashMap<String, Arc<LiveBackend>>> {
    static CLIENTS: OnceLock<Mutex<HashMap<String, Arc<LiveBackend>>>> = OnceLock::new();
    CLIENTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn backend_for(
    lookup: &dyn Fn(&str) -> Option<String>,
    cancellation: &CancellationToken,
) -> Result<Arc<dyn Backend>, Error> {
    let fixture = lookup("RILL_OPENSTACK_FIXTURE")
        .or_else(|| std::env::var("RILL_OPENSTACK_FIXTURE").ok())
        .filter(|dir| !dir.is_empty());
    if let Some(dir) = fixture {
        return Ok(Arc::new(FixtureBackend { dir: PathBuf::from(dir) }));
    }
    let config = AuthConfig::from_lookup(lookup).map_err(rill_error)?;
    let identity = config.identity_key();
    if let Some(client) = clients().lock().expect("ostack clients").get(&identity) {
        return Ok(client.clone());
    }
    let verify = lookup("OS_VERIFY_TLS").is_some_and(|v| v == "1" || v == "true");
    let no_auth_cache =
        lookup("RILL_OPENSTACK_NO_AUTH_CACHE").is_some_and(|v| v == "1" || v == "true");
    let client = run(
        async move {
            OpenStackClient::builder(config)
                .verify_tls(verify)
                .auth_cache_enabled(!no_auth_cache)
                .resource_cache_enabled(false)
                .build()
                .await
        },
        cancellation,
    )?;
    let backend = Arc::new(LiveBackend { client, identity: identity.clone() });
    clients().lock().expect("ostack clients").insert(identity, backend.clone());
    Ok(backend)
}

// ---------------------------------------------------------------- results

type CacheKey = (String, &'static str, String);

fn cache() -> &'static Mutex<HashMap<CacheKey, (Instant, Value)>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, (Instant, Value)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn ttl() -> Duration {
    env_secs("RILL_OPENSTACK_CACHE_TTL_SECS", 120)
}

fn query_key(query: &[(String, String)]) -> String {
    query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
}

fn cached(key: &CacheKey) -> Option<Value> {
    let cache = cache().lock().expect("ostack cache");
    cache.get(key).filter(|(at, _)| at.elapsed() < ttl()).map(|(_, value)| value.clone())
}

fn store(key: CacheKey, value: &Value) {
    cache().lock().expect("ostack cache").insert(key, (Instant::now(), value.clone()));
}

fn records_value(records: Vec<Record>) -> Value {
    Value::List(records.into_iter().map(|record| json_to_value(Json::Object(record))).collect())
}

/// The query a `list`-style call asks for: `all_projects`, `project`, `query`.
fn query_for(
    backend: &Arc<dyn Backend>,
    kind: ResourceKind,
    args: &Args,
    cancellation: &CancellationToken,
) -> Result<Vec<(String, String)>, Error> {
    let mut query = Vec::new();
    let widen = kind.all_projects_param();
    if args.bool_named("all_projects")? {
        if let Some((key, value)) = widen {
            query.push((key.to_string(), value.to_string()));
        }
    }
    if let Some(project) = args.str_named("project")? {
        let id = resolve_project(backend, &project, cancellation)?;
        if let Some((key, value)) = widen {
            if !query.iter().any(|(k, _)| k == key) {
                query.push((key.to_string(), value.to_string()));
            }
        }
        // Kinds whose API cannot filter by project are filtered here
        // (`@` keys never reach the API).
        let key = kind.project_filter_param().unwrap_or("@project_id");
        query.push((key.to_string(), id));
    }
    if let Some(Value::Map(map)) = args.named_value("query") {
        query.extend(pairs(map));
    }
    Ok(query)
}

fn resolve_project(
    backend: &Arc<dyn Backend>,
    project: &str,
    cancellation: &CancellationToken,
) -> Result<String, Error> {
    // A Keystone id needs no lookup (and normal users cannot list projects).
    if looks_like_id(ResourceKind::Project, project) {
        return Ok(project.to_string());
    }
    let projects = list_cached(backend, ResourceKind::Project, Vec::new(), false, cancellation)?;
    let Value::List(items) = &projects else { return Ok(project.to_string()) };
    let field = |item: &Value, key: &str| match item {
        Value::Map(map) => match map.get(key) {
            Some(Value::Str(text)) => Some(text.clone()),
            _ => None,
        },
        _ => None,
    };
    if items.iter().any(|item| field(item, "id").as_deref() == Some(project)) {
        return Ok(project.to_string());
    }
    let matches: Vec<String> = items
        .iter()
        .filter(|item| field(item, "name").as_deref() == Some(project))
        .filter_map(|item| field(item, "id"))
        .collect();
    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(RillError::new(format!("ostack: no project named '{project}'")).into()),
        many => Err(RillError::new(format!(
            "ostack: ambiguous project '{project}': matches {}",
            many.join(", ")
        ))
        .with_help("pass the project id instead")
        .into()),
    }
}

fn list(
    backend: &Arc<dyn Backend>,
    kind: ResourceKind,
    args: &Args,
    cancellation: &CancellationToken,
) -> Result<Value, Error> {
    let query = query_for(backend, kind, args, cancellation)?;
    let fresh = args.bool_named("fresh")?;
    list_cached(backend, kind, query, fresh, cancellation)
}

fn list_cached(
    backend: &Arc<dyn Backend>,
    kind: ResourceKind,
    query: Vec<(String, String)>,
    fresh: bool,
    cancellation: &CancellationToken,
) -> Result<Value, Error> {
    let key = (backend.identity(), kind.name(), query_key(&query));
    if !fresh {
        if let Some(value) = cached(&key) {
            return Ok(value);
        }
    }
    let (local, remote): (Query, Query) = query.into_iter().partition(|(k, _)| k.starts_with('@'));
    let b = backend.clone();
    let mut records = run(async move { b.list_full(kind, remote).await }, cancellation)?;
    if !local.is_empty() {
        let local: Vec<(String, String)> =
            local.into_iter().map(|(k, v)| (k.trim_start_matches('@').to_string(), v)).collect();
        records.retain(|record| record_matches(record, &local));
    }
    let value = records_value(records);
    store(key, &value);
    Ok(value)
}

/// Several collections, fetched concurrently unless cached.  The scope
/// arguments (`project=`, `all_projects=`, `query=`) apply to every kind
/// they make sense for.
fn inventory(
    backend: &Arc<dyn Backend>,
    kinds: &[ResourceKind],
    args: &Args,
    cancellation: &CancellationToken,
) -> Result<Map, Error> {
    let fresh = args.bool_named("fresh")?;
    let mut map = Map::new();
    let mut missing = Vec::new();
    for kind in kinds {
        let query = query_for(backend, *kind, args, cancellation)?;
        let key = (backend.identity(), kind.name(), query_key(&query));
        match if fresh { None } else { cached(&key) } {
            Some(value) => {
                map.insert(kind.name().to_string(), value);
            }
            None => missing.push((*kind, query)),
        }
    }
    if !missing.is_empty() {
        let b = backend.clone();
        let fetched = run(
            async move {
                let mut set = tokio::task::JoinSet::new();
                for (kind, query) in missing {
                    let b = b.clone();
                    set.spawn(async move { (kind, query.clone(), b.list_full(kind, query).await) });
                }
                let mut out = Vec::new();
                while let Some(joined) = set.join_next().await {
                    let (kind, query, records) = joined?;
                    out.push((kind, query, records?));
                }
                Ok(out)
            },
            cancellation,
        )?;
        for (kind, query, records) in fetched {
            let value = records_value(records);
            store((backend.identity(), kind.name(), query_key(&query)), &value);
            map.insert(kind.name().to_string(), value);
        }
        // Keep the requested order.
        let mut ordered = Map::new();
        for kind in kinds {
            if let Some(value) = map.shift_remove(kind.name()) {
                ordered.insert(kind.name().to_string(), value);
            }
        }
        map = ordered;
    }
    Ok(map)
}

/// Follow `method` from a record of `kind` (see `ostack_relations`): the
/// public entry for `server.ports()` and friends.
pub fn relation_call(
    kind: &str,
    method: &str,
    record: Map,
    lookup_env: &dyn Fn(&str) -> Option<String>,
    cancellation: CancellationToken,
) -> Result<Value, Error> {
    let backend = backend_for(lookup_env, &cancellation)?;
    relation(&backend, kind, method, &record, &cancellation)
}

/// Kinds that belong to a project, so related lookups stay within it.
fn project_scoped(kind: ResourceKind) -> bool {
    !matches!(
        kind,
        ResourceKind::Hypervisor
            | ResourceKind::Flavor
            | ResourceKind::Image
            | ResourceKind::Project
            | ResourceKind::User
    )
}

/// A collection listed within the record's project (or across all projects
/// when the record does not say), from the cache when possible.
fn scoped_list(
    backend: &Arc<dyn Backend>,
    kind: ResourceKind,
    record: &Map,
    cancellation: &CancellationToken,
) -> Result<Vec<Value>, Error> {
    let named = if !project_scoped(kind) {
        Vec::new()
    } else {
        match record.get("project_id") {
            Some(Value::Str(project)) if !project.is_empty() => {
                vec![("project".to_string(), Value::Str(project.clone()))]
            }
            _ => vec![("all_projects".to_string(), Value::Bool(true))],
        }
    };
    let args = Args { positional: Vec::new(), named };
    let query = query_for(backend, kind, &args, cancellation)?;
    match list_cached(backend, kind, query, false, cancellation)? {
        Value::List(items) => Ok((*items).clone()),
        _ => Ok(Vec::new()),
    }
}

fn str_field<'a>(map: &'a Map, key: &str) -> Option<&'a str> {
    match map.get(key) {
        Some(Value::Str(text)) => Some(text.as_str()),
        _ => None,
    }
}

fn as_map(value: &Value) -> Option<&Map> {
    match value {
        Value::Map(map) => Some(map),
        _ => None,
    }
}

/// `[{key: id}, ...]` or `[id, ...]` under `field`, as strings.
fn id_list(record: &Map, field: &str, key: &str) -> Vec<String> {
    match record.get(field) {
        Some(Value::List(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::Str(text) => Some(text.clone()),
                Value::Map(map) => str_field(map, key).map(str::to_string),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn ids_of(items: &[Value]) -> Vec<String> {
    items.iter().filter_map(record_id).collect()
}

fn some_first(items: Vec<Value>) -> Value {
    Value::Option(items.into_iter().next().map(Box::new))
}

/// Whether a server runs on a hypervisor, by any of the host names they share.
fn runs_on(server: &Map, hypervisor: &Map) -> bool {
    let hyp_name = str_field(hypervisor, "hypervisor_hostname").unwrap_or("");
    let service_host = match hypervisor.get("service") {
        Some(Value::Map(service)) => str_field(service, "host").unwrap_or(""),
        _ => "",
    };
    let short = |name: &str| name.split('.').next().unwrap_or("").to_string();
    let server_hyp = str_field(server, "OS-EXT-SRV-ATTR:hypervisor_hostname").unwrap_or("");
    let server_host = str_field(server, "OS-EXT-SRV-ATTR:host").unwrap_or("");
    (!server_hyp.is_empty() && server_hyp == hyp_name)
        || (!server_host.is_empty()
            && (server_host == service_host || short(server_host) == short(hyp_name)))
}

/// A project record as a handle, so `.servers()` on it is scoped.
fn project_handle(project: &Value) -> Value {
    let mut handle = Map::new();
    handle.insert(OSTACK_MARKER.to_string(), Value::Str("project".to_string()));
    if let Some(map) = as_map(project) {
        handle.extend(map.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Value::map(handle)
}

fn list_of(items: Value) -> Vec<Value> {
    match items {
        Value::List(items) => (*items).clone(),
        _ => Vec::new(),
    }
}

/// Records of `kind` matching API filters, as the API filters them (cached
/// per query).  No project scoping: the API returns what the token may see.
fn filtered_list(
    backend: &Arc<dyn Backend>,
    kind: ResourceKind,
    filters: Vec<(&str, String)>,
    cancellation: &CancellationToken,
) -> Result<Vec<Value>, Error> {
    let query = filters.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    Ok(list_of(list_cached(backend, kind, query, false, cancellation)?))
}

/// Like `filtered_list`, widened to every project for kinds that need it
/// (Nova and Cinder hide other projects' records even from admins).
fn filtered_everywhere(
    backend: &Arc<dyn Backend>,
    kind: ResourceKind,
    mut filters: Vec<(&str, String)>,
    cancellation: &CancellationToken,
) -> Result<Vec<Value>, Error> {
    if let Some((key, value)) = kind.all_projects_param() {
        filters.push((key, value.to_string()));
    }
    filtered_list(backend, kind, filters, cancellation)
}

/// The records with these ids, fetched concurrently; ids that no longer
/// exist are skipped.
fn with_ids(
    backend: &Arc<dyn Backend>,
    kind: ResourceKind,
    ids: Vec<String>,
    cancellation: &CancellationToken,
) -> Result<Vec<Value>, Error> {
    let mut unique: Vec<String> = Vec::new();
    for id in ids {
        if !id.is_empty() && !unique.contains(&id) {
            unique.push(id);
        }
    }
    if unique.is_empty() {
        return Ok(Vec::new());
    }
    let b = backend.clone();
    let results = run(
        async move {
            let mut set = tokio::task::JoinSet::new();
            for (index, id) in unique.into_iter().enumerate() {
                let b = b.clone();
                set.spawn(async move { (index, b.show(kind, id).await) });
            }
            let mut out = Vec::new();
            while let Some(joined) = set.join_next().await {
                let (index, result) = joined?;
                out.push((index, result.map_err(|error| format!("{error:#}"))));
            }
            out.sort_by_key(|(index, _)| *index);
            Ok(out)
        },
        cancellation,
    )?;
    let mut records = Vec::new();
    for (_, result) in results {
        match result {
            Ok(record) => records.push(json_to_value(Json::Object(record))),
            Err(message) => {
                let lower = message.to_lowercase();
                if lower.contains("404")
                    || lower.contains("not found")
                    || lower.contains("found matching")
                {
                    continue;
                }
                return Err(RillError::new(format!("ostack: {message}"))
                    .with_kind(ErrorKind::Io)
                    .into());
            }
        }
    }
    Ok(records)
}

fn field_text(record: &Map, key: &str) -> String {
    match record.get(key) {
        Some(Value::Str(text)) => text.clone(),
        Some(Value::Int(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// The compute host a hypervisor record runs as (`service.host`, else the
/// short hostname).
fn service_host(hypervisor: &Map) -> String {
    match hypervisor.get("service") {
        Some(Value::Map(service)) if !field_text(service, "host").is_empty() => {
            field_text(service, "host")
        }
        _ => field_text(hypervisor, "hypervisor_hostname")
            .split('.')
            .next()
            .unwrap_or("")
            .to_string(),
    }
}

/// Instance actions of a server, newest first; with `events`, each action's
/// events too (one request per action, at most `limit` actions).
fn actions(
    backend: &Arc<dyn Backend>,
    server: &str,
    events: bool,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<Value, Error> {
    let b = backend.clone();
    let path = format!("/servers/{server}/os-instance-actions");
    let body =
        run(async move { b.raw_get("compute".to_string(), path, Vec::new()).await }, cancellation)?;
    let mut items: Vec<Record> = body
        .get("instanceActions")
        .and_then(Json::as_array)
        .map(|items| items.iter().filter_map(|i| i.as_object().cloned()).collect())
        .unwrap_or_default();
    let started = |r: &Record| r.get("start_time").and_then(Json::as_str).unwrap_or("").to_string();
    items.sort_by_key(|r| std::cmp::Reverse(started(r)));
    if limit > 0 {
        items.truncate(limit);
    }
    if events && !items.is_empty() {
        let requests: Vec<String> = items
            .iter()
            .map(|r| r.get("request_id").and_then(Json::as_str).unwrap_or("").to_string())
            .collect();
        let b = backend.clone();
        let server = server.to_string();
        let details = run(
            async move {
                let mut set = tokio::task::JoinSet::new();
                for (index, request) in requests.into_iter().enumerate() {
                    let b = b.clone();
                    let path = format!("/servers/{server}/os-instance-actions/{request}");
                    set.spawn(async move {
                        (index, b.raw_get("compute".to_string(), path, Vec::new()).await)
                    });
                }
                let mut out = Vec::new();
                while let Some(joined) = set.join_next().await {
                    let (index, body) = joined?;
                    out.push((index, body?));
                }
                Ok(out)
            },
            cancellation,
        )?;
        for (index, body) in details {
            let events = body
                .get("instanceAction")
                .and_then(|action| action.get("events"))
                .cloned()
                .unwrap_or(Json::Array(Vec::new()));
            items[index].insert("events".to_string(), events);
        }
    }
    Ok(records_value(items))
}

fn quota(
    backend: &Arc<dyn Backend>,
    project: Option<String>,
    usage: bool,
    cancellation: &CancellationToken,
) -> Result<Value, Error> {
    let b = backend.clone();
    let rows = run(async move { b.quota(project, usage).await }, cancellation)?;
    Ok(records_value(rows))
}

/// Follow a relation from a record of `kind`, asking the APIs for exactly
/// the related records (filters and single GETs, not whole collections).
fn relation(
    backend: &Arc<dyn Backend>,
    kind: &str,
    method: &str,
    record: &Map,
    cancellation: &CancellationToken,
) -> Result<Value, Error> {
    use ResourceKind as K;
    let id = map_id(record).ok_or_else(|| RillError::new("ostack: the record has no id"))?;
    let text = |key: &str| field_text(record, key);
    let filtered = |k: ResourceKind, filters: Vec<(&str, String)>| {
        filtered_list(backend, k, filters, cancellation)
    };
    let ids = |k: ResourceKind, list: Vec<String>| with_ids(backend, k, list, cancellation);
    let cached_all = |k: ResourceKind| -> Result<Vec<Value>, Error> {
        Ok(list_of(list_cached(backend, k, Vec::new(), false, cancellation)?))
    };
    let device_ids = |ports: &[Value]| -> Vec<String> {
        ports.iter().filter_map(as_map).map(|p| field_text(p, "device_id")).collect()
    };
    let value = match (kind, method) {
        (_, "project") => {
            let project =
                str_field(record, "project_id").or_else(|| str_field(record, "tenant_id"));
            let Some(project) = project else { return Ok(Value::Option(None)) };
            let projects =
                list_of(list_cached(backend, K::Project, Vec::new(), false, cancellation)?);
            let found = projects
                .iter()
                .find(|p| record_id(p).as_deref() == Some(project))
                .map(project_handle);
            Value::Option(found.map(Box::new))
        }
        ("server", "attached_volumes") => Value::list(ids(
            K::Volume,
            id_list(record, "os-extended-volumes:volumes_attached", "id"),
        )?),
        ("server", "ports") | ("router", "ports") => {
            Value::list(filtered(K::Port, vec![("device_id", id.clone())])?)
        }
        ("server", "security_groups") => {
            let ports = filtered(K::Port, vec![("device_id", id.clone())])?;
            let groups = ports
                .iter()
                .filter_map(as_map)
                .flat_map(|p| id_list(p, "security_groups", "id"))
                .collect();
            Value::list(ids(K::SecurityGroup, groups)?)
        }
        ("server", "floating_ips") => {
            let ports = filtered(K::Port, vec![("device_id", id.clone())])?;
            let mut fips = Vec::new();
            for port in ids_of(&ports) {
                fips.extend(filtered(K::FloatingIp, vec![("port_id", port)])?);
            }
            Value::list(fips)
        }
        ("server", "hypervisor") => some_first(
            cached_all(K::Hypervisor)?
                .into_iter()
                .filter(|h| as_map(h).is_some_and(|h| runs_on(record, h)))
                .collect(),
        ),
        ("server", "flavor") => {
            let flavor_id = text("flavor_id");
            if !flavor_id.is_empty() {
                some_first(ids(K::Flavor, vec![flavor_id])?)
            } else {
                // Newer Nova embeds only the name.
                let name = if text("flavor_name").is_empty() {
                    text("flavor")
                } else {
                    text("flavor_name")
                };
                some_first(
                    cached_all(K::Flavor)?
                        .into_iter()
                        .filter(|f| {
                            as_map(f).is_some_and(|f| {
                                field_text(f, "name") == name || field_text(f, "id") == name
                            })
                        })
                        .collect(),
                )
            }
        }
        ("server", "image") => some_first(ids(K::Image, vec![text("image_id")])?),
        ("server", "server_groups") => Value::list(
            scoped_list(backend, K::ServerGroup, record, cancellation)?
                .into_iter()
                .filter(|g| as_map(g).is_some_and(|g| id_list(g, "members", "id").contains(&id)))
                .collect(),
        ),
        ("server", "actions") => actions(backend, &id, false, 0, cancellation)?,
        ("volume", "attached_servers") => {
            Value::list(ids(K::Server, id_list(record, "attachments", "server_id"))?)
        }
        ("volume", "snapshots") => Value::list(filtered_everywhere(
            backend,
            K::Snapshot,
            vec![("volume_id", id.clone())],
            cancellation,
        )?),
        ("volume", "backups") => Value::list(filtered_everywhere(
            backend,
            K::Backup,
            vec![("volume_id", id.clone())],
            cancellation,
        )?),
        ("snapshot", "volume") | ("backup", "volume") => {
            some_first(ids(K::Volume, vec![text("volume_id")])?)
        }
        ("port", "server") => {
            let device = text("device_id");
            if text("device_owner").contains("router") {
                some_first(ids(K::Router, vec![device])?)
            } else {
                some_first(ids(K::Server, vec![device])?)
            }
        }
        ("port", "network") | ("subnet", "network") => {
            some_first(ids(K::Network, vec![text("network_id")])?)
        }
        ("port", "subnets") => {
            Value::list(ids(K::Subnet, id_list(record, "fixed_ips", "subnet_id"))?)
        }
        ("port", "security_groups") => {
            Value::list(ids(K::SecurityGroup, id_list(record, "security_groups", "id"))?)
        }
        ("port", "floating_ips") => {
            Value::list(filtered(K::FloatingIp, vec![("port_id", id.clone())])?)
        }
        ("network", "ports") => Value::list(filtered(K::Port, vec![("network_id", id.clone())])?),
        ("network", "subnets") => {
            Value::list(filtered(K::Subnet, vec![("network_id", id.clone())])?)
        }
        ("network", "routers") => {
            let interfaces = filtered(
                K::Port,
                vec![
                    ("network_id", id.clone()),
                    ("device_owner", "network:router_interface".to_string()),
                ],
            )?;
            let wanted = device_ids(&interfaces);
            let routers = cached_all(K::Router)?
                .into_iter()
                .filter(|r| {
                    as_map(r).is_some_and(|r| {
                        wanted.contains(&field_text(r, "id"))
                            || matches!(r.get("external_gateway_info"), Some(Value::Map(g)) if field_text(g, "network_id") == id)
                    })
                })
                .collect();
            Value::list(routers)
        }
        ("subnet", "ports") => {
            Value::list(filtered(K::Port, vec![("fixed_ips", format!("subnet_id={id}"))])?)
        }
        ("subnet", "routers") => {
            let interfaces = filtered(
                K::Port,
                vec![
                    ("fixed_ips", format!("subnet_id={id}")),
                    ("device_owner", "network:router_interface".to_string()),
                ],
            )?;
            Value::list(ids(K::Router, device_ids(&interfaces))?)
        }
        ("router", "subnets") => {
            let ports = filtered(K::Port, vec![("device_id", id.clone())])?;
            let subnets = ports
                .iter()
                .filter_map(as_map)
                .flat_map(|p| id_list(p, "fixed_ips", "subnet_id"))
                .collect();
            Value::list(ids(K::Subnet, subnets)?)
        }
        ("floating_ip", "port") => some_first(ids(K::Port, vec![text("port_id")])?),
        ("floating_ip", "server") => {
            let ports = ids(K::Port, vec![text("port_id")])?;
            some_first(ids(K::Server, device_ids(&ports))?)
        }
        ("security_group", "ports") => {
            Value::list(filtered(K::Port, vec![("security_groups", id.clone())])?)
        }
        ("security_group", "servers") => {
            let ports: Vec<Value> = filtered(K::Port, vec![("security_groups", id.clone())])?
                .into_iter()
                .filter(|p| {
                    as_map(p).is_some_and(|p| field_text(p, "device_owner").starts_with("compute:"))
                })
                .collect();
            Value::list(ids(K::Server, device_ids(&ports))?)
        }
        ("hypervisor", "servers") => {
            // Nova filters by compute host (admin); the local match guards.
            let host = service_host(record);
            let mut filters = vec![("all_tenants", "1".to_string())];
            if !host.is_empty() {
                filters.push(("host", host));
            } else if !text("hypervisor_hostname").is_empty() {
                filters.push(("node", text("hypervisor_hostname")));
            }
            Value::list(
                filtered(K::Server, filters)?
                    .into_iter()
                    .filter(|s| as_map(s).is_some_and(|m| runs_on(m, record)))
                    .collect(),
            )
        }
        ("hypervisor", "compute_service") => {
            some_first(filtered(K::ComputeService, vec![("host", service_host(record))])?)
        }
        ("hypervisor", "network_agents") => {
            Value::list(filtered(K::NetworkAgent, vec![("host", service_host(record))])?)
        }
        ("hypervisor", "aggregates") => {
            let host = service_host(record);
            Value::list(
                cached_all(K::Aggregate)?
                    .into_iter()
                    .filter(|a| as_map(a).is_some_and(|a| id_list(a, "hosts", "").contains(&host)))
                    .collect(),
            )
        }
        ("aggregate", "hypervisors") => {
            let hosts = id_list(record, "hosts", "");
            Value::list(
                cached_all(K::Hypervisor)?
                    .into_iter()
                    .filter(|h| as_map(h).is_some_and(|h| hosts.contains(&service_host(h))))
                    .collect(),
            )
        }
        ("server_group", "servers") => {
            Value::list(ids(K::Server, id_list(record, "members", "id"))?)
        }
        ("pool", "members") => {
            let b = backend.clone();
            let path = format!("/v2/lbaas/pools/{id}/members");
            let body = run(
                async move { b.raw_get("load-balancer".to_string(), path, Vec::new()).await },
                cancellation,
            )?;
            let members = body.get("members").cloned().unwrap_or(Json::Array(Vec::new()));
            Value::list(list_of(json_to_value(members)))
        }
        ("pool", "loadbalancers") => {
            Value::list(ids(K::Loadbalancer, id_list(record, "loadbalancers", "id"))?)
        }
        ("loadbalancer", "pools") => {
            Value::list(filtered(K::Pool, vec![("loadbalancer_id", id.clone())])?)
        }
        ("project", "quota") => quota(backend, Some(id.clone()), true, cancellation)?,
        ("user", "role_assignments") => {
            Value::list(filtered(K::RoleAssignment, vec![("user.id", id.clone())])?)
        }
        // Servers the user booted (Nova records the creator), in any project.
        ("user", "created_servers") => Value::list(filtered_everywhere(
            backend,
            K::Server,
            vec![("user_id", id.clone())],
            cancellation,
        )?),
        // Projects the user can reach, directly or through a group
        // (Keystone's effective assignments), as project handles.
        ("user", "project_memberships") => {
            let assignments = filtered(
                K::RoleAssignment,
                vec![("user.id", id.clone()), ("effective", String::new())],
            )?;
            let projects: Vec<String> = assignments
                .iter()
                .filter_map(as_map)
                .map(|a| field_text(a, "project_id"))
                .filter(|p| !p.is_empty())
                .collect();
            Value::list(ids(K::Project, projects)?.iter().map(project_handle).collect())
        }
        (kind, method) => {
            return Err(RillError::new(format!("ostack: a {kind} has no .{method}()")).into())
        }
    };
    Ok(value)
}

fn is_ip(text: &str) -> bool {
    text.parse::<std::net::IpAddr>().is_ok()
}

fn is_mac(text: &str) -> bool {
    let parts: Vec<&str> = text.split([':', '-']).collect();
    parts.len() == 6
        && parts.iter().all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Exact IP or MAC lookups asked of the APIs directly: a few filtered
/// requests instead of the whole inventory.  Hits as `search` returns them.
fn address_search(
    backend: &Arc<dyn Backend>,
    needle: &str,
    args: &Args,
    kinds: &[ResourceKind],
    cancellation: &CancellationToken,
) -> Result<Vec<Value>, Error> {
    use ResourceKind as K;
    let want = |k: ResourceKind| kinds.iter().any(|w| w.name() == k.name());
    let all = args.bool_named("all_projects")?;
    let filtered = |k: ResourceKind, filters: Vec<(&str, String)>| {
        filtered_list(backend, k, filters, cancellation)
    };
    let mut hits: Vec<(&'static str, String, Value)> = Vec::new();
    if is_mac(needle) {
        if want(K::Port) {
            for port in filtered(K::Port, vec![("mac_address", needle.to_lowercase())])? {
                hits.push(("port", "mac_address".to_string(), port));
            }
        }
    } else {
        if want(K::Server) {
            let mut filters = vec![("ip", format!("^{}$", needle.replace('.', "\\.")))];
            if all {
                filters.push(("all_tenants", "1".to_string()));
            }
            for server in filtered(K::Server, filters)? {
                let field = as_map(&server)
                    .and_then(|m| match m.get("networks") {
                        Some(Value::List(nets)) => nets.iter().position(|n| {
                            as_map(n).is_some_and(|n| field_text(n, "addr") == needle)
                        }),
                        _ => None,
                    })
                    .map_or("addresses".to_string(), |i| format!("networks[{i}].addr"));
                hits.push(("server", field, server));
            }
        }
        if want(K::FloatingIp) {
            for key in ["floating_ip_address", "fixed_ip_address"] {
                for fip in filtered(K::FloatingIp, vec![(key, needle.to_string())])? {
                    hits.push(("floating_ip", key.to_string(), fip));
                }
            }
        }
        if want(K::Port) {
            for port in filtered(K::Port, vec![("fixed_ips", format!("ip_address={needle}"))])? {
                let field = as_map(&port)
                    .and_then(|m| match m.get("fixed_ips") {
                        Some(Value::List(ips)) => ips.iter().position(|ip| {
                            as_map(ip).is_some_and(|ip| field_text(ip, "ip_address") == needle)
                        }),
                        _ => None,
                    })
                    .map_or("fixed_ips".to_string(), |i| format!("fixed_ips[{i}].ip_address"));
                hits.push(("port", field, port));
            }
        }
    }
    let mut seen = Vec::new();
    hits.retain(|(_, _, record)| {
        let id = record_id(record).unwrap_or_default();
        if seen.contains(&id) {
            false
        } else {
            seen.push(id);
            true
        }
    });
    hits.sort_by_key(|(kind, _, _)| kind_rank(kind));
    // Owners and projects, looked up the same targeted way.
    let projects = list_cached(backend, K::Project, Vec::new(), false, cancellation)
        .map(list_of)
        .unwrap_or_default();
    let mut out = Vec::new();
    for (kind, field, record) in hits {
        let map = as_map(&record).cloned().unwrap_or_default();
        let owner = match kind {
            "port" => relation(backend, "port", "server", &map, cancellation)?,
            "floating_ip" => relation(backend, "floating_ip", "server", &map, cancellation)?,
            _ => Value::Option(None),
        };
        let owner = match owner {
            Value::Option(Some(found)) => *found,
            _ => Value::Null,
        };
        let project = projects
            .iter()
            .find(|p| record_id(p).as_deref() == str_field(&map, "project_id"))
            .cloned()
            .unwrap_or(Value::Null);
        let mut hit = Map::new();
        hit.insert("kind".to_string(), Value::Str(kind.to_string()));
        hit.insert("field".to_string(), Value::Str(field));
        hit.insert("match".to_string(), Value::Str("exact".to_string()));
        hit.insert("record".to_string(), record);
        hit.insert("owner".to_string(), owner);
        hit.insert("project".to_string(), project);
        out.push(Value::map(hit));
    }
    Ok(out)
}

/// Whether `text` is an id rather than a name: a UUID, 32 hex digits, or a
/// number for kinds with numeric ids (Nova hypervisors).
fn looks_like_id(kind: ResourceKind, text: &str) -> bool {
    let hex = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_hexdigit());
    let uuid = text.len() == 36
        && text.split('-').map(str::len).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
        && text.split('-').all(hex);
    uuid || (text.len() == 32 && hex(text))
        || (matches!(kind, ResourceKind::Hypervisor)
            && !text.is_empty()
            && text.bytes().all(|b| b.is_ascii_digit()))
}

/// `ostack.server(x)`: one GET for an id; for a name, one cached listing
/// searched for an exact name and then a unique case-insensitive fragment.
fn show_or_fragment(
    backend: &Arc<dyn Backend>,
    kind: ResourceKind,
    target: &str,
    cancellation: &CancellationToken,
) -> Result<Value, Error> {
    if looks_like_id(kind, target) {
        let b = backend.clone();
        let wanted = target.to_string();
        let record = run(async move { b.show(kind, wanted).await }, cancellation)?;
        return Ok(json_to_value(Json::Object(record)));
    }
    let field = match kind {
        ResourceKind::Hypervisor => "hypervisor_hostname",
        ResourceKind::FloatingIp => "floating_ip_address",
        _ => "name",
    };
    let records = list_of(list_cached(backend, kind, Vec::new(), false, cancellation)?);
    // Ids that do not look like ids (flavor "m1.small", "42") still match.
    if let Some(record) = records.iter().find(|r| record_id(r).as_deref() == Some(target)) {
        return Ok(record.clone());
    }
    let value_of = |r: &Value| as_map(r).and_then(|m| str_field(m, field)).map(str::to_string);
    let exact: Vec<&Value> =
        records.iter().filter(|r| value_of(r).as_deref() == Some(target)).collect();
    let ids =
        |hits: &[&Value]| hits.iter().filter_map(|r| record_id(r)).collect::<Vec<_>>().join(", ");
    match exact.as_slice() {
        [one] => return Ok((*one).clone()),
        [] => {}
        many => {
            return Err(RillError::new(format!(
                "ostack: ambiguous {} '{target}': matches {}",
                kind.singular(),
                ids(many)
            ))
            .with_help("pass an id instead of a name")
            .into())
        }
    }
    let needle = target.to_lowercase();
    let partial: Vec<&Value> = records
        .iter()
        .filter(|r| value_of(r).is_some_and(|v| v.to_lowercase().contains(&needle)))
        .collect();
    match partial.as_slice() {
        [one] => Ok((*one).clone()),
        [] => {
            Err(RillError::new(format!("ostack: no {} found matching '{target}'", kind.singular()))
                .into())
        }
        many => Err(RillError::new(format!(
            "ostack: '{target}' matches {} {}: {}",
            many.len(),
            kind.name(),
            many.iter().filter_map(|r| value_of(r)).collect::<Vec<_>>().join(", ")
        ))
        .with_help("use a longer fragment, the full name, or the id")
        .into()),
    }
}

struct Hit {
    kind: String,
    field: String,
    exact: bool,
    record: Value,
}

impl Hit {
    /// `{kind, field, match, record, owner, project}`, with the owner (the
    /// server or router behind a port or floating IP) and the project
    /// looked up in the same inventory.
    fn into_value(self, inventory: &Map) -> Value {
        let all = |collection: &str| -> Vec<Value> {
            inventory.get(collection).cloned().map(list_of).unwrap_or_default()
        };
        let record = as_map(&self.record).map(|m| (*m).clone()).unwrap_or_default();
        let device = match self.kind.as_str() {
            "port" => str_field(&record, "device_id").map(str::to_string),
            "floating_ip" => str_field(&record, "port_id").and_then(|port| {
                all("ports")
                    .iter()
                    .filter_map(as_map)
                    .find(|p| map_id(p).as_deref() == Some(port))
                    .and_then(|p| str_field(p, "device_id").map(str::to_string))
            }),
            _ => None,
        };
        let owner = device
            .filter(|d| !d.is_empty())
            .and_then(|d| {
                let mut both = all("servers");
                both.extend(all("routers"));
                both.into_iter().find(|r| record_id(r).as_deref() == Some(d.as_str()))
            })
            .unwrap_or(Value::Null);
        let project = str_field(&record, "project_id")
            .and_then(|p| all("projects").into_iter().find(|r| record_id(r).as_deref() == Some(p)))
            .unwrap_or(Value::Null);
        let mut out = Map::new();
        out.insert("kind".to_string(), Value::Str(self.kind));
        out.insert("field".to_string(), Value::Str(self.field));
        let matched = if self.exact { "exact" } else { "partial" };
        out.insert("match".to_string(), Value::Str(matched.to_string()));
        out.insert("record".to_string(), self.record);
        out.insert("owner".to_string(), owner);
        out.insert("project".to_string(), project);
        Value::map(out)
    }
}

/// The searchable values of a record: (field path, value, compare folded).
fn searchable(record: &Map) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    if let Some(v) = map_id(record) {
        out.push(("id".to_string(), v, false));
    }
    for key in ["floating_ip_address", "fixed_ip_address", "host_ip", "mac_address"] {
        if let Some(v) = str_field(record, key) {
            out.push((key.to_string(), v.to_string(), true));
        }
    }
    if let Some(Value::List(ips)) = record.get("fixed_ips") {
        for (index, ip) in ips.iter().enumerate() {
            if let Some(address) = as_map(ip).and_then(|m| str_field(m, "ip_address")) {
                out.push((format!("fixed_ips[{index}].ip_address"), address.to_string(), true));
            }
        }
    }
    if let Some(addresses) = str_field(record, "addresses") {
        let tokens = addresses.split(|c: char| !(c.is_ascii_hexdigit() || c == '.' || c == ':'));
        for token in tokens {
            if token.contains('.') || token.matches(':').count() >= 2 {
                out.push(("addresses".to_string(), token.to_string(), true));
            }
        }
    }
    for key in [
        "name",
        "hypervisor_hostname",
        "OS-EXT-SRV-ATTR:host",
        "OS-EXT-SRV-ATTR:hypervisor_hostname",
        "cidr",
    ] {
        if let Some(v) = str_field(record, key) {
            if !v.is_empty() {
                out.push((key.to_string(), v.to_string(), true));
            }
        }
    }
    out
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "server" => 0,
        "floating_ip" => 1,
        "port" => 2,
        "router" => 3,
        "volume" => 4,
        _ => 5,
    }
}

/// Every record whose id, address, MAC or name matches `needle`: exact
/// matches first, then name or address fragments; one hit per record.
fn search(inventory: &Map, needle: &str) -> Vec<Hit> {
    let needle = needle.trim();
    let lower = needle.to_lowercase();
    if lower.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for (collection, value) in inventory {
        let Value::List(items) = value else { continue };
        let kind = item_name(collection);
        for record in items.iter() {
            let Some(map) = as_map(record) else { continue };
            let fields = searchable(map);
            let exact =
                fields.iter().find(
                    |(_, v, folded)| {
                        if *folded {
                            v.to_lowercase() == lower
                        } else {
                            v == needle
                        }
                    },
                );
            let found = match exact {
                Some((field, _, _)) => Some((field.clone(), true)),
                None => fields
                    .iter()
                    .find(|(field, v, _)| field != "id" && v.to_lowercase().contains(&lower))
                    .map(|(field, _, _)| (field.clone(), false)),
            };
            if let Some((field, exact)) = found {
                hits.push(Hit { kind: kind.clone(), field, exact, record: record.clone() });
            }
        }
    }
    hits.sort_by_key(|hit| (!hit.exact, kind_rank(&hit.kind)));
    hits
}

/// A record's id as text: Neutron, Cinder and Keystone use strings, Nova's
/// hypervisors (and some older APIs) use integers.
fn record_id(record: &Value) -> Option<String> {
    as_map(record).and_then(map_id)
}

fn map_id(map: &Map) -> Option<String> {
    match map.get("id") {
        Some(Value::Str(id)) => Some(id.clone()),
        Some(Value::Int(id)) => Some(id.to_string()),
        _ => None,
    }
}

fn item_name(collection: &str) -> String {
    ResourceKind::from_name(collection)
        .map_or_else(|| collection.to_string(), |k| k.singular().to_string())
}

fn find_record(inventory: &Map, id: &str) -> Option<Value> {
    for (collection, value) in inventory {
        let Value::List(items) = value else { continue };
        if let Some(record) = items.iter().find(|record| record_id(record).as_deref() == Some(id)) {
            let mut hit = Map::new();
            hit.insert("kind".to_string(), Value::Str(item_name(collection)));
            hit.insert("record".to_string(), record.clone());
            return Some(Value::map(hit));
        }
    }
    None
}

/// The dotted path of the first string equal to `needle` inside `value`.
fn path_of(value: &Value, needle: &str, path: &str) -> Option<String> {
    match value {
        Value::Str(text) if text == needle => Some(path.to_string()),
        Value::List(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, item)| path_of(item, needle, &format!("{path}[{index}]"))),
        Value::Map(map) => map.iter().find_map(|(key, item)| {
            let child = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
            path_of(item, needle, &child)
        }),
        Value::Tuple(items) => items
            .iter()
            .enumerate()
            .find_map(|(index, item)| path_of(item, needle, &format!("{path}[{index}]"))),
        _ => None,
    }
}

fn references(inventory: &Map, id: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for (collection, value) in inventory {
        let Value::List(items) = value else { continue };
        for record in items.iter() {
            if record_id(record).as_deref() == Some(id) {
                continue;
            }
            if let Some(field) = path_of(record, id, "") {
                let mut hit = Map::new();
                hit.insert("kind".to_string(), Value::Str(item_name(collection)));
                hit.insert("field".to_string(), Value::Str(field));
                hit.insert("record".to_string(), record.clone());
                out.push(Value::map(hit));
            }
        }
    }
    out
}
