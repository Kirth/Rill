//! Record types: `type Name { ... }` and the built-in ostack records.

use rill::ast::{Stmt, TypeName};

fn check_ok(source: &str) {
    let program = rill::parser::parse(source).expect("parse");
    rill::typeck::check(&program).expect(source);
}

fn check_err(source: &str) -> rill::diagnostic::RillError {
    let program = rill::parser::parse(source).expect("parse");
    rill::typeck::check(&program).expect_err(source)
}

fn warning_messages(source: &str) -> Vec<String> {
    let program = rill::parser::parse(source).expect(source);
    rill::typeck::check_with_warnings(&program)
        .expect(source)
        .into_iter()
        .map(|warning| warning.message)
        .collect()
}

#[test]
fn parses_fields_with_optional_marks_quoted_names_and_docs() {
    let program = rill::parser::parse(
        r#"
type Host {
    # the host's name
    name: Str
    port?: Int, tags: List[Str]   # free-form
    "OS-EXT:zone"?: Str
}
"#,
    )
    .expect("parse");
    let Stmt::TypeDecl { name, fields } = program.stmts[0].unspanned() else {
        panic!("not a type declaration: {:?}", program.stmts[0]);
    };
    assert_eq!(name, "Host");
    let summary: Vec<(&str, bool, Option<&str>)> =
        fields.iter().map(|f| (f.name.as_str(), f.optional, f.doc.as_deref())).collect();
    assert_eq!(
        summary,
        vec![
            ("name", false, Some("the host's name")),
            ("port", true, None),
            ("tags", false, Some("free-form")),
            ("OS-EXT:zone", true, None),
        ]
    );
    assert_eq!(fields[2].type_name, TypeName::List(Box::new(TypeName::Str)));
}

#[test]
fn type_stays_usable_as_a_name() {
    check_ok("let type = 1\nprint(type)\nlet m = {type: \"kvm\"}\nprint(m.type)\n");
}

#[test]
fn rejects_lowercase_and_builtin_type_names() {
    assert!(rill::parser::parse("type host { name: Str }").is_err());
    assert!(rill::parser::parse("type Str { name: Str }").is_err());
    assert!(check_err("type OSServer { id: Str }").message.contains("built-in"));
}

#[test]
fn checks_fields_of_declared_records() {
    check_ok(
        r#"
type Member { user: Str, role: Str, since?: Time }
fn label(m: Member) -> Str { "{m.user} ({m.role})" }
let m: Member = {user: "kirth", role: "admin"}
print(label(m))
"#,
    );
    let error = check_err("type M { n: Int }\nfn f(m: M) -> Str { m.n }\n");
    assert!(error.message.contains("expected Str, got Int"), "{}", error.message);
}

#[test]
fn map_literals_must_have_the_required_fields() {
    let error = check_err("type M { a: Str, b: Int }\nlet m: M = {a: \"x\"}\n");
    assert_eq!(error.message, "M needs field 'b'");
    let error = check_err("type M { a: Str, b: Int }\nfn f(m: M) { m }\nf({a: \"x\", b: \"y\"})\n");
    assert_eq!(error.message, "M.b is Int, got Str");
    check_ok("type M { a: Str, b?: Int }\nlet m: M = {a: \"x\", extra: true}\nprint(m)\n");
}

#[test]
fn records_are_nominal() {
    let error = check_err("fn h(p: OSProject) { p.id }\nh(ostack.server(\"web-1\"))\n");
    assert_eq!(error.message, "type mismatch: expected OSProject, got OSServer");
    let error = check_err(
        "type A { id: Str }\ntype B { id: Str }\nfn f(a: A) { a }\nlet b: B = {id: \"x\"}\nf(b)\n",
    );
    assert_eq!(error.message, "type mismatch: expected A, got B");
}

#[test]
fn a_string_is_not_a_record() {
    let error = check_err("fn h(p: OSProject) { p.id }\nh(\"demo\")\n");
    assert_eq!(error.message, "type mismatch: expected OSProject, got Str");
    assert!(error.help_text().unwrap_or_default().contains("ostack.project("));
}

#[test]
fn untyped_maps_pass_as_records() {
    check_ok(
        r#"
fn h(p: OSProject) -> Str { p.name }
let raw = json.parse("{}")
let m: Map = {id: "x", name: "y"}
print(h(raw), h(m))
let as_map: Map = ostack.project("demo")
print(as_map)
"#,
    );
}

#[test]
fn unknown_fields_warn_with_a_suggestion() {
    let source = "fn h(p: OSProject) { p.descripton }\nprint(h(ostack.project(\"x\")))\n";
    assert_eq!(warning_messages(source), vec!["OSProject has no field 'descripton'"]);
    let program = rill::parser::parse(source).unwrap();
    let warning = rill::typeck::check_with_warnings(&program).unwrap().remove(0);
    assert_eq!(warning.code_name(), "rill::warn::unknown_field");
    assert!(warning.help_text().unwrap_or_default().contains("description"));
}

#[test]
fn unknown_type_names_are_errors() {
    let error = check_err("fn h(p: OSProjct) { p }\n");
    assert_eq!(error.message, "unknown type 'OSProjct'");
    assert!(error.help_text().unwrap_or_default().contains("OSProject"));
    assert!(check_err("let x: Nope = 1\n").message.contains("unknown type 'Nope'"));
}

#[test]
fn types_can_be_used_before_their_declaration() {
    check_ok("fn f(h: Host) -> Str { h.name }\ntype Host { name: Str }\nprint(f({name: \"x\"}))\n");
}

#[test]
fn types_are_declared_once_at_the_top_level() {
    assert!(check_err("type A { x: Int }\ntype A { x: Str }\n").message.contains("declared twice"));
    assert!(check_err("fn f() {\n type A { x: Int }\n}\n").message.contains("top level"));
}

#[test]
fn ostack_functions_return_records() {
    let cases = [
        ("ostack.servers()", "List[OSServer]"),
        ("ostack.server(\"x\")", "OSServer"),
        ("ostack.project(\"x\")", "OSProject"),
        ("ostack.show(\"volume\", \"x\")", "OSVolume"),
        ("ostack.list(\"floating_ips\")", "List[OSFloatingIp]"),
        ("ostack.project(\"x\").servers()", "List[OSServer]"),
        ("ostack.server(\"x\").ports()", "List[OSPort]"),
        ("ostack.server(\"x\").attached_volumes()", "List[OSVolume]"),
        ("ostack.port(\"x\").network()", "Option[OSNetwork]"),
        ("ostack.servers()[0]", "OSServer"),
        ("ostack.servers().filter(|s| s.status == \"ACTIVE\")", "List[OSServer]"),
        ("ostack.server(\"x\").flavor_id", "Str"),
        ("ostack.server(\"x\")[\"OS-EXT-SRV-ATTR:host\"]", "Str"),
    ];
    for (expr, expected) in cases {
        let error = check_err(&format!("let x: Bytes = {expr}\n"));
        assert_eq!(
            error.message,
            format!("type mismatch: expected Bytes, got {expected}"),
            "for {expr}"
        );
    }
}

#[test]
fn closures_over_record_lists_see_the_record() {
    let warnings = warning_messages("print(ostack.servers().map(|s| s.flavr))\n");
    assert_eq!(warnings, vec!["OSServer has no field 'flavr'"]);
}

#[test]
fn every_ostack_kind_has_a_record() {
    let builtins = rill::records::builtins();
    for kind in rill::methods::OSTACK_KINDS {
        let name = rill::records::ostack_record_name(kind)
            .unwrap_or_else(|| panic!("no record for {kind}"));
        let decl = builtins.get(name).unwrap_or_else(|| panic!("{name} is not declared"));
        assert!(decl.field("id").is_some(), "{name} has no id");
        assert_eq!(decl.ostack_kind().map(rill::records::ostack_record_name), Some(Some(name)));
    }
    // Every relation names a record that exists.
    for relations in rill::records::OSTACK_RECORDS
        .iter()
        .map(|(kind, _, _)| rill::methods::ostack_relations(kind))
    {
        for method in relations.iter() {
            if let Some((record, _)) = rill::records::ostack_relation(method) {
                assert!(builtins.contains_key(record), "{method} returns unknown {record}");
            }
        }
    }
}

#[test]
fn map_methods_work_on_records() {
    check_ok(
        r#"
fn summary(p: OSProject) -> Str {
    let keys: List[Str] = p.keys()
    let pairs = p.map(|key, value| "{key}={value}")
    let note = p.get("description").unwrap_or("-")
    "{keys.len()} {pairs.len()} {note}"
}
print(summary(ostack.project("x")))
"#,
    );
}
