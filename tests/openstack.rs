//! The `ostack.*` namespace against JSON fixtures (no network).
#![cfg(feature = "openstack")]

use rill::types::Value;
use std::collections::BTreeMap;

fn fixture_dir() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/openstack").to_string()
}

fn env() -> BTreeMap<String, String> {
    let mut env = rill::eval::process_environment();
    env.insert("RILL_OPENSTACK_FIXTURE".to_string(), fixture_dir());
    env
}

fn run(source: &str) -> Value {
    rill::run_source_with_environment(source, Vec::new(), None, env()).expect(source)
}

fn run_err(source: &str) -> String {
    rill::run_source_with_environment(source, Vec::new(), None, env())
        .expect_err(source)
        .to_string()
}

fn string(value: &Value) -> String {
    match value {
        Value::Str(text) => text.clone(),
        other => panic!("expected Str, got {other:?}"),
    }
}

#[test]
fn lists_full_records_and_types_them_for_the_checker() {
    assert_eq!(run("ostack.servers().len()"), Value::Int(3));
    assert_eq!(run("ostack.list(\"volumes\").len()"), Value::Int(2));
    // Full records: the trimmed CLI columns would not have `attachments`.
    let value =
        run(r#"ostack.volumes().filter(.status == "in-use").map(.attachments[0].server_id)"#);
    assert_eq!(value, Value::list(vec![Value::Str("11111111-1111-4111-8111-111111111111".into())]));
    // The checker knows the shape, so a typed binding and map methods pass.
    let program = rill::parser::parse(
        "let xs: List[Map] = ostack.servers()\nlet one: Option[Map] = ostack.find(\"x\")\nxs.map(.keys())",
    )
    .unwrap();
    rill::typeck::check(&program).expect("typed");
}

#[test]
fn named_arguments_scope_the_query() {
    let names = run(r#"ostack.servers(project="staging").map(.name)"#);
    assert_eq!(names, Value::list(vec![Value::Str("web-1".into())]));
    let names = run(r#"ostack.servers(project="p1", all_projects=true).map(.name)"#);
    assert_eq!(names, Value::list(vec![Value::Str("web-1".into()), Value::Str("db-1".into())]));
    let error = run_err(r#"ostack.servers(project="nope")"#);
    assert!(error.contains("no project named 'nope'"), "{error}");
    let error = run_err(r#"ostack.servers(bogus=true)"#);
    assert!(error.contains("unknown ostack.servers argument 'bogus'"), "{error}");
}

#[test]
fn show_resolves_ids_and_names_and_reports_ambiguity() {
    assert_eq!(
        run(r#"ostack.show("server", "db-1").id"#),
        Value::Str("22222222-2222-4222-8222-222222222222".into())
    );
    assert_eq!(
        run(r#"ostack.show("servers", "22222222-2222-4222-8222-222222222222").name"#),
        Value::Str("db-1".into())
    );
    assert_eq!(
        run(r#"ostack.server("db-1").id"#),
        Value::Str("22222222-2222-4222-8222-222222222222".into())
    );
    assert_eq!(
        run(r#"ostack.volume("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").name"#),
        Value::Str("web-1-data".into())
    );
    assert_eq!(run(r#"ostack.hypervisor("lab-compute-2").id"#), Value::Int(2));
    let error = run_err(r#"ostack.server("web-1")"#);
    assert!(error.contains("ambiguous server 'web-1'"), "{error}");
    let error = run_err(r#"ostack.show("servers", "web-1")"#);
    assert!(error.contains("ambiguous server 'web-1'"), "{error}");
    assert!(error.contains("11111111-1111-4111-8111-111111111111"), "{error}");
    let error = run_err(r#"ostack.show("servers", "ghost")"#);
    assert!(error.contains("no server found matching 'ghost'"), "{error}");
    let error = run_err(r#"ostack.list("trunks")"#);
    assert!(error.contains("unknown kind 'trunks'"), "{error}");
}

#[test]
fn inventory_find_and_references_cross_resources() {
    let keys = run(r#"ostack.inventory(["servers", "ports"]).keys()"#);
    assert_eq!(keys, Value::list(vec![Value::Str("servers".into()), Value::Str("ports".into())]));

    let hit = run(r#"ostack.find("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")"#);
    let Value::Option(Some(hit)) = hit else { panic!("{hit:?}") };
    let Value::Map(hit) = *hit else { panic!() };
    assert_eq!(string(&hit["kind"]), "volume");
    let Value::Map(record) = &hit["record"] else { panic!() };
    assert_eq!(string(&record["name"]), "web-1-data");
    assert_eq!(run(r#"ostack.find("nope")"#), Value::Option(None));

    let refs = run(
        r#"ostack.references("11111111-1111-4111-8111-111111111111").map(|r| "{r.kind}:{r.field}")"#,
    );
    assert_eq!(
        refs,
        Value::list(vec![
            Value::Str("volume:attachments[0].server_id".into()),
            Value::Str("port:device_id".into()),
        ])
    );
    let refs =
        run(r#"ostack.references("11111111-1111-4111-8111-111111111111", kinds=["ports"]).len()"#);
    assert_eq!(refs, Value::Int(1));
}

#[test]
fn raw_get_info_and_kinds() {
    assert_eq!(
        run(r#"ostack.get("compute", "/os-services").services[0].binary"#),
        Value::Str("nova-compute".into())
    );
    assert_eq!(run("ostack.info().project_name"), Value::Str("demo".into()));
    assert_eq!(run("ostack.kinds().len()"), Value::Int(25));
    assert!(run("ostack.kinds().contains(\"servers\")") == Value::Bool(true));
}

#[test]
fn results_are_cached_until_fresh_or_refresh() {
    // A private fixture directory so the cache key is this test's own.
    let dir = std::env::temp_dir().join(format!("rill-ostack-cache-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("flavors.json");
    std::fs::write(&file, r#"[{"id": "f1", "name": "one"}]"#).unwrap();
    let mut env = rill::eval::process_environment();
    env.insert("RILL_OPENSTACK_FIXTURE".to_string(), dir.display().to_string());
    let run = |source: &str| {
        rill::run_source_with_environment(source, Vec::new(), None, env.clone()).expect(source)
    };

    assert_eq!(run("ostack.flavors().len()"), Value::Int(1));
    std::fs::write(&file, r#"[{"id": "f1", "name": "one"}, {"id": "f2", "name": "two"}]"#).unwrap();
    assert_eq!(run("ostack.flavors().len()"), Value::Int(1), "served from the cache");
    assert_eq!(run("ostack.flavors(fresh=true).len()"), Value::Int(2));
    std::fs::write(&file, r#"[]"#).unwrap();
    assert_eq!(run("ostack.flavors().len()"), Value::Int(2), "fresh refilled the cache");
    assert_eq!(run("ostack.refresh()\nostack.flavors().len()"), Value::Int(0));
}

#[test]
fn a_cancelled_evaluator_stops_before_calling_out() {
    let mut evaluator = rill::eval::Evaluator::with_environment(Vec::new(), None, env());
    let token = rill::eval::CancellationToken::new();
    token.cancel();
    evaluator.set_cancellation(token);
    let program = rill::parser::parse("ostack.servers()").unwrap();
    let error = evaluator.eval_program(&program).expect_err("cancelled");
    assert!(error.to_string().contains("cancel"), "{error}");
}

#[test]
fn bare_references_and_missing_credentials_are_explained() {
    let error = run_err("ostack.servers");
    assert!(error.contains("must be called: ostack.servers()"), "{error}");
    let mut env = rill::eval::process_environment();
    env.remove("RILL_OPENSTACK_FIXTURE");
    for key in ["OS_AUTH_URL", "OS_USERNAME", "OS_APPLICATION_CREDENTIAL_ID"] {
        env.remove(key);
    }
    let error = rill::run_source_with_environment("ostack.servers()", Vec::new(), None, env)
        .expect_err("no credentials")
        .to_string();
    assert!(error.contains("OS_AUTH_URL"), "{error}");
}

#[test]
fn project_handles_scope_their_queries() {
    let handle = run(r#"ostack.project("staging")"#);
    let Value::Map(handle) = handle else { panic!("{handle:?}") };
    assert_eq!(handle.get("@ostack"), Some(&Value::Str("project".into())));
    assert_eq!(handle.get("id"), Some(&Value::Str("p2".into())));
    assert_eq!(handle.get("name"), Some(&Value::Str("staging".into())));

    let names = run(r#"ostack.project("staging").servers().map(.name)"#);
    assert_eq!(names, Value::list(vec![Value::Str("web-1".into())]));
    assert_eq!(run(r#"ostack.project("p1").volumes().len()"#), Value::Int(2));
    assert_eq!(run(r#"ostack.project("demo").list("ports").len()"#), Value::Int(3));
    assert_eq!(run(r#"ostack.all_projects().servers().len()"#), Value::Int(3));
    let inv = run(r#"ostack.project("demo").inventory(["servers", "ports"]).servers.len()"#);
    assert_eq!(inv, Value::Int(2));
    // Handle methods take no named arguments; the namespace form does.
    assert_eq!(run(r#"ostack.servers(project="demo", fresh=true).len()"#), Value::Int(2));

    let error = run_err(r#"ostack.project("nope")"#);
    assert!(error.contains("no project named 'nope'"), "{error}");
    let error = run_err(r#"({a: 1}).servers()"#);
    assert!(error.contains("a handle from ostack.project"), "{error}");
}

#[test]
fn record_relations_follow_attachments_ports_and_projects() {
    let names = run(
        r#"ostack.server("11111111-1111-4111-8111-111111111111").attached_volumes().map(.name)"#,
    );
    assert_eq!(names, Value::list(vec![Value::Str("web-1-data".into())]));
    assert_eq!(run(r#"ostack.server("db-1").attached_volumes().len()"#), Value::Int(0));
    let ips = run(r#"ostack.server("db-1").ports().map(.fixed_ips[0].ip_address)"#);
    assert_eq!(ips, Value::list(vec![Value::Str("10.0.0.12".into())]));
    let servers = run(r#"ostack.volume("web-1-data").attached_servers().map(.name)"#);
    assert_eq!(servers, Value::list(vec![Value::Str("web-1".into())]));
    assert_eq!(run(r#"ostack.server("db-1").project().unwrap().name"#), Value::Str("demo".into()));
    assert_eq!(run(r#"ostack.flavor("f1").project()"#), Value::Option(None));
    // Through a project handle and a loop, as written at the keyboard.
    let value = run(r#"
var names = []
for s in ostack.project("demo").servers() { names += s.attached_volumes().map(.name) }
names"#);
    assert_eq!(value, Value::list(vec![Value::Str("web-1-data".into())]));
    let error = run_err(r#"({a: 1}).attached_volumes()"#);
    assert!(error.contains("needs an OpenStack record"), "{error}");
}

fn strs(items: &[&str]) -> Value {
    Value::list(items.iter().map(|s| Value::Str(s.to_string())).collect())
}

#[test]
fn new_network_kinds_list_and_show() {
    assert_eq!(run("ostack.floating_ips().len()"), Value::Int(1));
    assert_eq!(run("ostack.security_groups().map(.name)"), strs(&["default", "web"]));
    assert_eq!(run("ostack.routers()[0].name"), Value::Str("demo-router".into()));
    assert_eq!(
        run(r#"ostack.floating_ip("203.0.113.7").port_id"#),
        Value::Str("cccccccc-cccc-4ccc-8ccc-cccccccccccc".into())
    );
    assert_eq!(
        run(r#"ostack.security_group("web").description"#),
        Value::Str("http and https".into())
    );
    assert_eq!(run(r#"ostack.router("demo-router").status"#), Value::Str("ACTIVE".into()));
    assert_eq!(run(r#"ostack.list("floating-ips").len()"#), Value::Int(1));
}

#[test]
fn search_answers_who_owns_an_address() {
    // A fixed IP: the port is the exact hit, owned by web-1 in project demo.
    let hit = run(r#"let h = ostack.search("10.0.0.11")[0]
"{h.kind} {h.field} {h.match} {h.record.name} {h.project.name}""#);
    assert_eq!(hit, Value::Str("server networks[0].addr exact web-1 demo".into()));
    let kinds = run(r#"ostack.search("10.0.0.11").map(|h| "{h.kind}:{h.match}")"#);
    assert_eq!(kinds, strs(&["server:exact", "floating_ip:exact", "port:exact"]));
    let owner = run(r#"ostack.search("10.0.0.11").find(.kind == "port").unwrap().owner.name"#);
    assert_eq!(owner, Value::Str("web-1".into()));
    // A floating IP points through its port to the server.
    let owner = run(r#"ostack.search("203.0.113.7")[0].owner.name"#);
    assert_eq!(owner, Value::Str("web-1".into()));
    // A MAC, case-insensitively.
    assert_eq!(
        run(r#"ostack.search("FA:16:3E:00:00:12")[0].owner.name"#),
        Value::Str("db-1".into())
    );
    // A fragment: partial matches after exact ones, one hit per record.
    let names = run(r#"ostack.search("compute-1").map(|h| "{h.kind}:{h.match}")"#);
    assert_eq!(names, strs(&["server:partial", "server:partial", "hypervisor:partial"]));
    let only = run(r#"ostack.search("web", kinds=["security_groups"]).map(.record.name)"#);
    assert_eq!(only, strs(&["web"]));
    assert_eq!(run(r#"ostack.search("nothing-like-this").len()"#), Value::Int(0));
}

#[test]
fn find_accepts_addresses_and_names() {
    assert_eq!(
        run(r#"ostack.find("203.0.113.7").unwrap().kind"#),
        Value::Str("floating_ip".into())
    );
    assert_eq!(
        run(r#"ostack.find("db-1").unwrap().record.id"#),
        Value::Str("22222222-2222-4222-8222-222222222222".into())
    );
    assert_eq!(run(r#"ostack.find("fa:16:3e:00:00:01").unwrap().kind"#), Value::Str("port".into()));
    // A fragment alone is not a find.
    assert_eq!(run(r#"ostack.find("compute")"#), Value::Option(None));
}

#[test]
fn relations_between_network_records() {
    let web = r#"ostack.server("11111111-1111-4111-8111-111111111111")"#;
    assert_eq!(run(&format!("{web}.security_groups().map(.name)")), strs(&["default", "web"]));
    assert_eq!(
        run(&format!("{web}.floating_ips().map(.floating_ip_address)")),
        strs(&["203.0.113.7"])
    );
    assert_eq!(run(&format!("{web}.hypervisor().unwrap().id")), Value::Int(1));

    let port = r#"ostack.port("cccccccc-cccc-4ccc-8ccc-cccccccccccc")"#;
    assert_eq!(run(&format!("{port}.server().unwrap().name")), Value::Str("web-1".into()));
    assert_eq!(run(&format!("{port}.network().unwrap().name")), Value::Str("internal".into()));
    assert_eq!(run(&format!("{port}.subnets().map(.cidr)")), strs(&["10.0.0.0/24"]));
    assert_eq!(run(&format!("{port}.security_groups().len()")), Value::Int(2));
    assert_eq!(run(&format!("{port}.floating_ips().len()")), Value::Int(1));
    // A router interface port's device is the router.
    assert_eq!(
        run(r#"ostack.port("c3c3c3c3-c3c3-4c3c-8c3c-c3c3c3c3c3c3").server().unwrap().name"#),
        Value::Str("demo-router".into())
    );

    let net = r#"ostack.network("internal")"#;
    assert_eq!(run(&format!("{net}.ports().len()")), Value::Int(3));
    assert_eq!(run(&format!("{net}.subnets().map(.name)")), strs(&["internal-v4"]));
    assert_eq!(run(&format!("{net}.routers().map(.name)")), strs(&["demo-router"]));

    let subnet = r#"ostack.subnet("internal-v4")"#;
    assert_eq!(run(&format!("{subnet}.routers().map(.name)")), strs(&["demo-router"]));
    assert_eq!(run(&format!("{subnet}.network().unwrap().name")), Value::Str("internal".into()));
    assert_eq!(run(&format!("{subnet}.ports().len()")), Value::Int(3));

    assert_eq!(run(r#"ostack.router("demo-router").subnets().map(.name)"#), strs(&["internal-v4"]));
    assert_eq!(
        run(r#"ostack.floating_ip("203.0.113.7").server().unwrap().name"#),
        Value::Str("web-1".into())
    );
    assert_eq!(run(r#"ostack.security_group("web").servers().map(.name)"#), strs(&["web-1"]));
    assert_eq!(run(r#"ostack.security_group("default").ports().len()"#), Value::Int(2));
}

#[test]
fn hypervisors_list_their_servers_and_match_by_fragment() {
    let names = run(r#"ostack.hypervisor("compute-1").servers().map(.name)"#);
    assert_eq!(names, strs(&["web-1", "web-1"]));
    assert_eq!(
        run(r#"ostack.hypervisor("lab-compute-2").servers().map(.name)"#),
        strs(&["db-1"])
    );
    let error = run_err(r#"ostack.hypervisor("compute")"#);
    assert!(error.contains("matches 2 hypervisors"), "{error}");
    // By id it is a single show; by numeric id too.
    assert_eq!(
        run(r#"ostack.hypervisor("2").hypervisor_hostname"#),
        Value::Str("lab-compute-2.maas".into())
    );
    // A server name that does not exist is still "not found".
    let error = run_err(r#"ostack.server("ghost-9")"#);
    assert!(error.contains("no server found matching 'ghost-9'"), "{error}");
}

#[test]
fn project_records_scope_and_wrong_relations_explain_themselves() {
    // record.project() is a handle, so the project's listings are scoped.
    let names = run(r#"ostack.server("db-1").project().unwrap().servers().map(.name)"#);
    assert_eq!(names, strs(&["web-1", "db-1"]));
    // A plain project record from the listing scopes too.
    let count = run(r#"ostack.projects().find(.name == "staging").unwrap().servers().len()"#);
    assert_eq!(count, Value::Int(1));
    let error = run_err(r#"ostack.subnet("internal-v4").attached_volumes()"#);
    assert!(error.contains("a subnet has no .attached_volumes()"), "{error}");
    assert!(error.contains(".routers()"), "{error}");
}

/// A private copy of the fixtures, so its request log and cache are this
/// test's own.
fn private_fixtures(tag: &str) -> (std::path::PathBuf, BTreeMap<String, String>) {
    let source = std::path::PathBuf::from(fixture_dir());
    let dir = std::env::temp_dir().join(format!("rill-ostack-{tag}-{}", std::process::id()));
    fn copy(from: &std::path::Path, to: &std::path::Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    copy(&source, &dir);
    let mut env = rill::eval::process_environment();
    env.insert("RILL_OPENSTACK_FIXTURE".to_string(), dir.display().to_string());
    (dir, env)
}

#[test]
fn relations_ask_for_filtered_listings() {
    let (dir, env) = private_fixtures("filters");
    let run = |source: &str| {
        rill::run_source_with_environment(source, Vec::new(), None, env.clone()).expect(source)
    };
    assert_eq!(
        run(r#"ostack.server("22222222-2222-4222-8222-222222222222").ports().len()"#),
        Value::Int(1)
    );
    assert_eq!(
        run(r#"ostack.subnet("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee").routers().len()"#),
        Value::Int(1)
    );
    let log = rill::openstack::fixture_requests(&dir);
    assert!(
        log.contains(&"list ports device_id=22222222-2222-4222-8222-222222222222".to_string()),
        "{log:?}"
    );
    assert!(
        log.iter().any(|l| l.starts_with("list ports fixed_ips=subnet_id=eeeeeeee")
            && l.contains("device_owner=network:router_interface")),
        "{log:?}"
    );
    assert!(!log.contains(&"list ports ".to_string()), "no unfiltered port listing: {log:?}");
    assert!(!log.iter().any(|l| l == "list servers "), "{log:?}");

    // An exact address is a handful of filtered requests, not an inventory.
    let before = rill::openstack::fixture_requests(&dir).len();
    assert_eq!(run(r#"ostack.search("10.0.0.12")[0].record.name"#), Value::Str("db-1".into()));
    let new: Vec<String> = rill::openstack::fixture_requests(&dir)[before..].to_vec();
    assert!(new.iter().any(|l| l.starts_with("list servers ip=")), "{new:?}");
    assert!(new.iter().any(|l| l == "list ports fixed_ips=ip_address=10.0.0.12"), "{new:?}");
    assert!(!new.iter().any(|l| l == "list volumes " || l == "list networks "), "{new:?}");
}

#[test]
fn admin_kinds_list_show_and_relate() {
    assert_eq!(run("ostack.keypairs()[0].id"), Value::Str("kirth".into()));
    assert_eq!(run(r#"ostack.keypair("kirth").fingerprint"#), Value::Str("aa:bb:cc".into()));
    assert_eq!(run("ostack.server_groups()[0].servers().map(.name)"), strs(&["web-1"]));
    assert_eq!(
        run(r#"ostack.server("11111111-1111-4111-8111-111111111111").server_groups().map(.name)"#),
        strs(&["web-anti"])
    );
    assert_eq!(
        run(r#"ostack.aggregate("gpu").hypervisors().map(.id)"#),
        Value::list(vec![Value::Int(1)])
    );
    assert_eq!(run(r#"ostack.hypervisor("compute-1").aggregates().map(.name)"#), strs(&["gpu"]));
    assert_eq!(run(r#"ostack.availability_zone("zone-1").hosts.len()"#), Value::Int(2));
    assert_eq!(
        run(r#"ostack.hypervisor("compute-2").compute_service().unwrap().status"#),
        Value::Str("disabled".into())
    );
    assert_eq!(
        run(r#"ostack.hypervisor("compute-1").network_agents().map(.agent_type)"#),
        strs(&["Open vSwitch agent"])
    );
    assert_eq!(run(r#"ostack.network_agents().count(.alive == false)"#), Value::Int(1));
    let vol = r#"ostack.volume("web-1-data")"#;
    assert_eq!(run(&format!("{vol}.snapshots().map(.name)")), strs(&["data-snap"]));
    assert_eq!(run(&format!("{vol}.backups().map(.name)")), strs(&["data-nightly"]));
    assert_eq!(
        run(r#"ostack.snapshot("data-snap").volume().unwrap().name"#),
        Value::Str("web-1-data".into())
    );
    assert_eq!(run(r#"ostack.backup("data-nightly").volume().unwrap().size"#), Value::Int(50));
    assert_eq!(run(r#"ostack.pool("web-pool").members().map(.address)"#), strs(&["10.0.0.11"]));
    assert_eq!(run(r#"ostack.pool("web-pool").loadbalancers().map(.name)"#), strs(&["web-lb"]));
    assert_eq!(run(r#"ostack.loadbalancers()[0].pools().map(.name)"#), strs(&["web-pool"]));
    assert_eq!(
        run(r#"ostack.users()[0].role_assignments().map(.role)"#),
        strs(&["member", "admin"])
    );
    assert_eq!(run(r#"ostack.project("staging").role_assignments().map(.role)"#), strs(&["admin"]));
}

#[test]
fn structured_servers_follow_flavor_and_image() {
    let web = r#"ostack.server("11111111-1111-4111-8111-111111111111")"#;
    assert_eq!(run(&format!("{web}.flavor().unwrap().vcpus")), Value::Int(1));
    assert_eq!(run(&format!("{web}.image().unwrap().name")), Value::Str("ubuntu-24.04".into()));
    assert_eq!(run(&format!("{web}.networks[0].mac")), Value::Str("fa:16:3e:00:00:11".into()));
    assert_eq!(
        run(r#"ostack.server("db-1").flavor().unwrap().name"#),
        Value::Str("m1.large".into())
    );
}

#[test]
fn quotas_and_instance_actions() {
    let rows = run(
        r#"ostack.project("demo").quota().map(|r| "{r.service}.{r.resource}={r.in_use}/{r.limit}")"#,
    );
    assert_eq!(
        rows,
        strs(&["compute.cores=5/20", "compute.instances=2/10", "volume.gigabytes=60/1000"])
    );
    assert_eq!(run(r#"ostack.quota(project="staging").len()"#), Value::Int(1));
    assert_eq!(run(r#"ostack.quota().len()"#), Value::Int(4));

    let web = r#"ostack.server("11111111-1111-4111-8111-111111111111")"#;
    assert_eq!(
        run(&format!("{web}.actions().map(.action)")),
        strs(&["reboot", "resize", "create"])
    );
    let events = run(
        r#"ostack.actions("11111111-1111-4111-8111-111111111111", events=true, limit=2).map(|a| a.events[0].event)"#,
    );
    assert!(matches!(&events, Value::List(items) if items.len() == 2), "{events:?}");
    assert_eq!(
        run(
            r#"ostack.actions("11111111-1111-4111-8111-111111111111", events=true)[0].events[0].event"#
        ),
        Value::Str("compute_reboot_instance".into())
    );
}

#[test]
fn users_know_what_they_created_and_where_they_belong() {
    // Created: web-1 in demo and web-1 in staging were booted by kirth.
    let created = run(r#"ostack.user("kirth").created_servers().map(.id)"#);
    assert_eq!(
        created,
        strs(&["11111111-1111-4111-8111-111111111111", "33333333-3333-4333-8333-333333333333"])
    );
    // Memberships are project handles, so their listings are scoped.
    assert_eq!(
        run(r#"ostack.user("kirth").project_memberships().map(.name)"#),
        strs(&["demo", "staging"])
    );
    let reachable = run(r#"ostack.user("kirth").project_memberships().flat_map(.servers()).len()"#);
    assert_eq!(reachable, Value::Int(3));
    let error = run_err(r#"ostack.user("kirth").servers()"#);
    assert!(error.contains("a user has no .servers()"), "{error}");
    assert!(error.contains(".created_servers()"), "{error}");
}
