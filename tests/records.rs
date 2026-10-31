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
    assert_eq!(error.message, "M.b: type mismatch: expected Int, got Str");
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
fn unknown_fields_are_errors_with_a_suggestion() {
    let error = check_err("fn h(p: OSProject) { p.descripton }\n");
    assert_eq!(error.message, "OSProject has no field 'descripton'");
    assert!(error.help_text().unwrap_or_default().contains("description"));
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
        ("ostack.server(\"x\").flavor_id", "Option[Str]"),
        ("ostack.server(\"x\")[\"OS-EXT-SRV-ATTR:host\"]", "Option[Str]"),
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
    let error = check_err("print(ostack.servers().map(|s| s.flavr))\n");
    assert_eq!(error.message, "OSServer has no field 'flavr'");
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

fn run(source: &str) -> rill::types::Value {
    rill::run_source(source, Vec::new()).expect(source)
}

fn runtime_error(source: &str) -> String {
    check_ok(source);
    rill::run_source(source, Vec::new()).expect_err(source).message
}

#[test]
fn computed_fields_and_every_construction_context_are_checked() {
    for expression in ["{n: s}", "[{n: s}][0]", "if true { {n: s} } else { {n: 1} }"] {
        let source =
            format!("type Count {{ n: Int }}\nlet s = \"bad\"\nlet c: Count = {expression}\n");
        // An indexed untyped collection is validated at the boundary at runtime.
        if expression.starts_with('[') {
            assert!(runtime_error(&source).contains("expected Int"));
        } else {
            assert!(check_err(&source).message.contains("expected Int"));
        }
    }
    for body in [
        "let c: List[Count] = [{n: s}]",
        "fn f() -> Count { {n: s} }",
        "fn f() -> Count { return {n: s} }",
        "fn f(c: Count = {n: s}) { c }",
        "fn f(c: Count) { c }\nf(c={n: s})",
        "var c: Count = {n: 1}\nc = {}",
        "var c: Count = {n: 1}\nc.n = s",
        "var c: Count = {n: 1}\nc[\"n\"] = s",
    ] {
        let source = format!("type Count {{ n: Int }}\nlet s = \"bad\"\n{body}\n");
        check_err(&source);
    }
}

#[test]
fn contextual_checks_keep_branch_and_match_bindings_in_scope() {
    assert_eq!(
        run(r#"
type Count { n: Int }
let c: Count = if true { let n = 3; {n: n} } else { {n: 4} }
let d: Count = match some(5) { some(n) => {n: n}, none => {n: 0} }
c.n + d.n
"#),
        rill::types::Value::Int(8)
    );
    check_err("type Count { n: Int }\nlet c: Count = match some(\"bad\") { some(n) => {n: n}, none => {n: 0} }");
}

#[test]
fn dynamic_records_are_validated_recursively_at_boundaries() {
    let error = runtime_error(
        r#"
type Count { n: Int }
type Batch { items: List[Count] }
let raw = json.parse("\{\"items\": [\{\"n\": \"bad\"}]}")
let batch: Batch = raw
batch
"#,
    );
    assert!(error.contains("items[0].n"), "{error}");
    assert!(error.contains("expected Int"), "{error}");
    for body in [
        "let raw = {}\nfn f(c: Count) { die(\"body ran\") }\nf(raw)",
        "let raw = {}\nfn f(c: Count = raw) { die(\"body ran\") }\nf()",
        "let raw = {}\nfn f() -> Count { raw }\nf()",
        "let raw = [{}]\nfn f(cs: List[Count]) { die(\"body ran\") }\nf(raw)",
        "let raw = {}\nvar c: Count = {n: 1}\nc = raw",
        "let raw = {}\nlet c: OSProject = raw",
    ] {
        let source = format!("type Count {{ n: Int }}\n{body}\n");
        let error = runtime_error(&source);
        assert!(error.contains("required field"), "{error}");
        assert!(!error.contains("body ran"), "{error}");
    }
}

#[test]
fn optional_fields_have_option_reads_and_plain_stored_values() {
    use rill::types::Value;
    assert_eq!(
        run(r#"
type Count { n?: Int }
let absent: Count = {}
let present: Count = {n: 3}
(absent.n, present.n, absent["n"], present["n"], present.get("n"), present.to_json())
"#),
        Value::tuple(vec![
            Value::Option(None),
            Value::Option(Some(Box::new(Value::Int(3)))),
            Value::Option(None),
            Value::Option(Some(Box::new(Value::Int(3)))),
            Value::Int(3),
            Value::Str("{\"n\":3}".into())
        ])
    );
    check_err("type Count { n?: Int }\nlet c: Count = {}\nlet n: Int = c.n");
    check_err("type Count { n?: Int }\nlet c: Count = {n: null}");
    assert_eq!(
        run("type Count { n?: Int }\nvar c: Count = {}\nc.n = 3\nc[\"n\"] += 2\nc.n.unwrap()"),
        Value::Int(5)
    );
    assert_eq!(run("type Count { n?: Int }\nlet cs: List[Count] = [{n: 3}, {}]\ncs.parallel_map(.n.unwrap_or(0)).sum()"), Value::Int(3));
    assert_eq!(
        run("type Count { n?: Int }\nfn f(c: Count) -> Option[Int] { c.n? }\nf({n: 3})"),
        Value::Option(Some(Box::new(Value::Int(3))))
    );
}

#[test]
fn dynamic_key_mutation_checks_the_record_before_committing() {
    let error = runtime_error(
        r#"
type Count { n: Int }
var c: Count = {n: 1}
let key = "n"
c[key] = "bad"
"#,
    );
    assert!(error.contains("value.n: expected Int"), "{error}");
    assert_eq!(
        run("type Count { n: Int }\nvar c: Count = {n: 1}\nlet key = \"n\"\nc[key] = 4\nc.n"),
        rill::types::Value::Int(4)
    );
}

#[test]
fn record_views_respect_homogeneous_maps_including_extra_fields() {
    check_err("type Label { name: Str }\nlet c: Label = {name: \"x\"}\nlet m: Map[Str, Int] = c");
    assert_eq!(
        run("type Count { n: Int }\nlet c: Count = {n: 4}\nlet m: Map[Str, Int] = c\nm.n"),
        rill::types::Value::Int(4)
    );
    let error = runtime_error(
        "type Count { n: Int }\nlet c: Count = {n: 4, extra: \"bad\"}\nlet m: Map[Str, Int] = c",
    );
    assert!(error.contains("value.extra"), "{error}");
}

#[test]
fn ordinary_fields_never_infer_an_openstack_identity() {
    assert_eq!(
        run(r#"
type Contact { id: Str, email: Str }
fn email(c: Contact) -> Str { c.email }
let c: Contact = {id: "a", email: "a@example.com"}
email(c)
"#),
        rill::types::Value::Str("a@example.com".into())
    );
    assert_eq!(run("type A { id: Str }\ntype B { id: Str }\nlet a: A = {id: \"x\"}\nlet m: Map = a\nlet b: B = m\nb.id"), rill::types::Value::Str("x".into()));
}

#[test]
fn conflicting_imported_record_names_are_rejected_and_repeat_imports_work() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rill-record-import-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.ril"), "type Item { n: Int }\nfn number(i: Item) -> Int { i.n }\n")
        .unwrap();
    std::fs::write(dir.join("b.ril"), "type Item { n: Str }\nlet item: Item = {n: \"bad\"}\n")
        .unwrap();
    std::fs::write(
        dir.join("wrapper.ril"),
        "use ./a.ril\nfn wrapped(i: Item) -> Int { number(i) }\n",
    )
    .unwrap();
    let compile = |source: &str| {
        rill::CompilationSession::new().compile(rill::SourceUnit {
            name: "<record imports>".into(),
            source: source.into(),
            base_dir: dir.clone(),
        })
    };
    for source in [
        "use ./a.ril\nuse ./b.ril\nnumber(item)",
        "use ./wrapper.ril\nuse ./b.ril\nwrapped(item)",
        "use ./a.ril\ntype Item { n: Int }\n",
    ] {
        assert!(compile(source).unwrap_err().message.contains("conflict"), "{source}");
    }
    let program = compile("use ./a.ril\nuse ./a.ril\nuse ./wrapper.ril\nwrapped({n: 4})").unwrap();
    assert_eq!(rill::run_compiled(&program, vec![], None).unwrap(), rill::types::Value::Int(4));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_mutation_leaves_the_existing_value_unchanged() {
    let mut checker = rill::typeck::Session::new();
    let mut evaluator = rill::eval::Evaluator::new(vec![]);
    let program =
        rill::parser::parse("type Count { n: Int }\nvar c: Count = {n: 1}\nlet key = \"n\"")
            .unwrap();
    checker.check(&program).unwrap();
    evaluator.eval_program(&program).unwrap();
    let invalid = rill::parser::parse("c[key] = \"bad\"").unwrap();
    checker.check(&invalid).unwrap();
    assert!(evaluator.eval_program(&invalid).unwrap_err().message.contains("expected Int"));
    let read = rill::parser::parse("c.n").unwrap();
    checker.check(&read).unwrap();
    assert_eq!(evaluator.eval_program(&read).unwrap(), rill::types::Value::Int(1));
}

#[test]
fn nested_options_and_flat_map_preserve_record_types() {
    use rill::types::Value;
    assert_eq!(run("type Count { n?: Option[Int] }\nlet absent: Count = {}\nlet present: Count = {n: none}\n(absent.n, present.n)"), Value::tuple(vec![Value::Option(None), Value::Option(Some(Box::new(Value::Option(None))))]));
    assert_eq!(run("type Count { n: Int }\nlet c: Count = {n: 3}\nlet cs: List[Count] = [[c]].flat_map(|xs| xs)\ncs[0].n"), Value::Int(3));
    check_err("var n: Int = 1\nn = 1.5");
    assert_eq!(run("var n = 1\nn = 1.5\nn"), Value::Float(1.5));
}

#[test]
fn contextual_record_checks_preserve_implicit_return_wrapping() {
    use rill::types::Value;
    assert_eq!(run("type Count { n: Int }\nfn f(flag: Bool) -> Option[Count] { if flag { {n: 3} } else { none } }\nf(true).unwrap().n"), Value::Int(3));
    assert_eq!(run("type Count { n: Int }\nfn f(flag: Bool) -> Result[Count] { if flag { {n: 3} } else { err(\"bad\") } }\nf(true).unwrap().n"), Value::Int(3));
    assert_eq!(
        run("fn f(x: Any) -> Option[Int] { x }\nf(some(3))"),
        Value::Option(Some(Box::new(Value::Int(3))))
    );
    check_err("type Count { n: Int }\nfn f(flag: Bool) -> Option[Count] { if flag { {n: \"bad\"} } else { none } }");
    check_err("let x: Option[Int] = if true { 3 } else { none }");
}
