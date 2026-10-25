fn check_ok(source: &str) {
    let program = rill::parser::parse(source).expect("parse");
    rill::typeck::check(&program).expect(source);
}

fn check_err(source: &str) -> String {
    let program = rill::parser::parse(source).expect("parse");
    rill::typeck::check(&program).expect_err(source).to_string()
}

#[test]
fn accepts_basic_annotations() {
    check_ok(
        r#"
let name: Str = "rill"
let nums: List[Int] = [1, 2, 3]
fn id(x: Str) -> Str {
    return x
}
id(name)
"#,
    );
}

#[test]
fn accepts_bare_list_annotations() {
    check_ok(r#"fn is_vm_haunted(id: Str) -> Result[Option[Tuple[List, List]]] { none }"#);
    check_ok(r#"fn maybe(p: Path?) -> Str? { none }"#);
}

#[test]
fn checks_tuple_annotations_indexing_destructuring_and_mutation() {
    check_ok(
        r#"
let pair: Tuple[Int, Str] = (1, "one")
let number: Int = pair[0]
let text: Str = pair[-1]
let homogeneous: Tuple[Int, Int, Int] = (1, 2, 3)
fn select(values: Tuple[Int, Int, Int], index: Int) -> Int { values[index] }
let (first, _, third) = homogeneous
var mutable: Tuple[Int, Str] = pair
mutable[0] += 1
mutable[-1] = "two"
"#,
    );

    for source in [
        r#"let value: Tuple[Int, Str] = (1, 2)"#,
        r#"let value: Tuple[Int, Str, Bool] = (1, "two")"#,
        r#"let value = (1, "two"); let number: Int = value[1]"#,
        r#"let value = (1, "two"); value[0] = 2"#,
        r#"var value = (1, "two"); value[0] = "bad""#,
        r#"var value = (1, "two"); let index = 0; value[index] = 2"#,
        r#"let value = (1, "two"); value[2]"#,
        r#"let (a, b, c) = (1, 2)"#,
        r#"for value in (1, 2) { print(value) }"#,
        r#"(1, 2).len()"#,
    ] {
        check_err(source);
    }
}

#[test]
fn checks_environment_and_path_helpers() {
    check_ok(
        r#"let home: Str = env.HOME
let config: Path = path("~/.config/rill/config.toml")
set_env("RILL_CHECK", "yes")
config.exists()"#,
    );
}

#[test]
fn checks_environment_assignment_and_scope() {
    check_ok(
        r#"
env.DEBUG = "1"
with env {
    DEBUG = "2"
} {
    env.DEBUG
}
"#,
    );
    let err = check_err("env.DEBUG = 1");
    assert!(err.contains("expected Str, got Int"));
}

#[test]
fn on_and_with_env_blocks_are_expressions_typed_by_their_body() {
    check_ok(
        r#"
let user: Str = on "host" { run! whoami }
let status: ProcessResult = on ["docker", "exec", "c"] { run uptime }
let inner: Str = with env { DEBUG = "1" } { env.DEBUG }
let nested: Str = on "host" { with env { A = "1" } { run! id -un } }
let unit = on "host" { run! whoami; }
"#,
    );
    let err = check_err(r#"let n: Int = on "host" { run! whoami }"#);
    assert!(err.contains("Str"), "{err}");
    let err = check_err("let x = on 42 { run true }");
    assert!(err.contains("on needs Str or List"), "{err}");
    let err = check_err(r#"let x: Int = with env { A = "1" } { env.A }"#);
    assert!(err.contains("Str"), "{err}");
}

#[test]
fn checks_argument_and_input_declarations() {
    check_ok(
        r#"
args {
    verbose: Bool = false short="v"
    port: Int = 8080 short="p"
host: Str
}
input lines
for line { print line }
"#,
    );
}

#[test]
fn checks_nested_argument_declarations() {
    check_ok(
        r#"
args {
    command {
        start { service: Str }
    }
}
args.command.start.service
"#,
    );
}

#[test]
fn checks_standard_library_modules() {
    check_ok("use json\n\"{}\".parse_json()");
    let err = check_err("use imaginary");
    assert!(err.contains("unknown module 'imaginary'"));
}

#[test]
fn checks_concrete_standard_library_apis() {
    check_ok(
        r#"use path
use regex
let p: Path = path("/tmp").join("file")
"rill".is_match("ri.*")"#,
    );
}

#[test]
fn rejects_unknown_identifier() {
    let err = check_err("print missing");
    assert!(err.contains("unknown identifier 'missing'"));
}

#[test]
fn rejects_annotation_mismatch() {
    let err = check_err(r#"let n: Int = "nope""#);
    assert!(err.contains("expected Int, got Str"));
}

#[test]
fn rejects_non_bool_if_condition() {
    let err = check_err(
        r#"
if 1 {
    print "bad"
}
"#,
    );
    assert!(err.contains("if condition must be Bool"));
}

#[test]
fn rejects_bad_numeric_operator_operands() {
    let err = check_err(r#""x" - 1"#);
    assert!(err.contains("numeric operator needs numeric operands"));
}

#[test]
fn rejects_unknown_fields() {
    let err = check_err(r#""hello".ok"#);
    assert!(err.contains("Str has no field 'ok'"));
}

#[test]
fn rejects_bad_byte_pipe_endpoint() {
    let err = check_err(r#"["a"] | trim"#);
    assert!(err.contains("right side of | must be a command or byte consumer"));
}

#[test]
fn rejects_unknown_command_var() {
    let err = check_err(r#"run printf $missing"#);
    assert!(err.contains("unknown command variable '$missing'"));
}

#[test]
fn rejects_function_arity_mismatch() {
    let err = check_err(
        r#"
fn id(x: Str) -> Str {
    return x
}
id()
"#,
    );
    assert!(err.contains("id needs 1 args, got 0"));
}

#[test]
fn accepts_exit_builtin() {
    check_ok("exit(0)");
}

#[test]
fn checks_async_process_builtins() {
    check_ok(
        r#"
let handle: ProcessHandle = ["printf", "ok"].spawn()
let result: ProcessResult = handle.await()
[["printf", "one"], ["printf", "two"]].parallel()
"#,
    );
    let err = check_err("[\"printf\", \"bad\"].await()");
    assert!(err.contains(".await() is not supported on List"));
}

#[test]
fn rejects_non_int_exit_code() {
    let err = check_err(r#"exit("bad")"#);
    assert!(err.contains("expected Int, got Str"));
}

#[test]
fn checks_option_result_and_try_types() {
    check_ok(
        r#"
fn load() -> Result[Str] {
    let text = ok("ready")?
    text
}

let present: Bool = some(1).is_some()
let value: Int = some(1).unwrap()
let success: Option[Int] = ok(1).ok()
let failure: Option[Str] = err("failed").err()
load()
"#,
    );
    let err = check_err("some(1).is_some(2)");
    assert!(err.contains("needs 0 arguments, got 1"));
    let err = check_err(r#"1?"#);
    assert!(err.contains("? needs Option or Result"));
}

#[test]
fn tracks_explicit_result_error_types() {
    check_ok(r#"let result: Result[Int, Str] = err("failed")"#);
    check_ok(r#"fn failed() -> Result[Int, Error] { err("failed") }"#);
    let err = check_err(r#"let result: Result[Int, Int] = err("failed")"#);
    assert!(err.contains("expected Int, got Str"));
}

#[test]
fn accepts_implicit_callback_arguments() {
    check_ok(r#"[{score: 8}] |> .filter(.score > 5)"#);
}

#[test]
fn infers_function_return_types_for_callbacks() {
    check_ok(
        r#"
fn increment(value: Int) -> Int {
    return value + 1
}
[1, 2] |> .map(increment)
"#,
    );
}

#[test]
fn rejects_multi_argument_map_callback() {
    let err = check_err(r#"[1, 2] |> .map(|left, right| left + right)"#);
    assert!(err.contains("map callback needs 1 arg, got 2"));
}

#[test]
fn checks_declared_returns_inside_control_flow() {
    let err = check_err(
        r#"
fn bad(value: Int) -> Int {
    if true {
        return "not an integer"
    }
    return value
}
"#,
    );
    assert!(err.contains("expected Int, got Str"));
}

#[test]
fn parallel_bodies_cannot_assign_outer_vars() {
    for source in [
        "var n = 0\nparallel for x in [1, 2] { n += x }",
        "var n = 0\nrace for x in [1, 2] { n = x }",
        "var n = 0\n[1, 2].parallel_map(|x| { n += x })",
        "var xs = [0]\n[1, 2].parallel_filter(|x| { xs[0] = x\ntrue })",
    ] {
        let message = check_err(source);
        assert!(message.contains("outer variable"), "{source}: {message}");
    }
    check_ok("parallel for x in [1, 2] { var n = 0\nn += x\nemit n }");
    check_ok("var n = 0\nfor x in [1, 2] { n += x }");
}

fn check_help(source: &str) -> String {
    let program = rill::parser::parse(source).expect("parse");
    let error = rill::typeck::check(&program).expect_err(source);
    error.help_text().unwrap_or_default().to_string()
}

#[test]
fn unknown_names_come_with_suggestions() {
    assert_eq!(check_help("let host = 1\nhots"), "did you mean `host`?");
    assert_eq!(check_help("let host = 1\nhots()"), "did you mean `host`?");
    assert_eq!(check_help("\"a\".lnes()"), "did you mean `.lines()`?");
    assert_eq!(check_help("path(\"x\").read()"), "`.read()` was renamed; use `.read_text()`");
    assert_eq!(check_help("use jsn"), "did you mean `json`?");
    assert_eq!(check_help("json.parze(\"1\")"), "did you mean `parse`?");
    assert_eq!(check_help("let r = run true\nr.oute"), "did you mean `out`?");
    assert_eq!(check_help("let r = run true\nr.zzz"), "");
    assert!(check_err("lines(\"a\")").contains("was removed; use receiver.lines(...)"));
}

#[test]
fn rejects_reassignment_with_a_different_type() {
    let err = check_err("var x = 1\nx = \"s\"");
    assert!(err.contains("cannot assign Str to 'x', which has type Int"), "{err}");
    check_ok("var t = 0\nt += 1.5\nlet f: Float = t");
    check_ok("var xs = []\nxs += [1]\nlet n: List[Int] = xs");
    check_ok("var v = null\nv = 1\nlet n: Int = v");
}

#[test]
fn rejects_assignment_to_undeclared_variable_with_suggestion() {
    let program = rill::parser::parse("let total = 1\ntotl = 5").unwrap();
    let error = rill::typeck::check(&program).unwrap_err();
    assert!(error.to_string().contains("assignment to undeclared variable 'totl'"), "{error}");
    assert_eq!(error.help_text(), Some("did you mean `total`?"));
    assert!(error.span.is_some(), "statement errors carry spans");
}

#[test]
fn rejects_non_bool_while_condition() {
    let err = check_err("while 5 { }");
    assert!(err.contains("while condition must be Bool, got Int"), "{err}");
    check_ok("var i = 0\nwhile i < 3 { i += 1 }");
}

#[test]
fn rejects_function_that_may_not_return() {
    let err = check_err("fn f(x: Int) -> Int { if x > 0 { return 1 } }");
    assert!(err.contains("can finish without a value"), "{err}");
    check_ok("fn g(x: Int) -> Int { if x > 0 { return 1 } else { return 2 } }");
    check_ok("fn h() -> Int { match 1 { _ => 1 } }");
    check_ok("fn k(x: Int) -> Int { if x > 0 { return 1 }\nx * 2 }");
}

#[test]
fn infers_result_for_functions_using_try_and_rejects_map_on_it() {
    let source = r#"
fn load(cmd: Str) {
    let out = run! $*cmd
    out.trim()
}
load("echo hi").trim()
"#;
    let program = rill::parser::parse(source).unwrap();
    let error = rill::typeck::check(&program).unwrap_err();
    assert!(error.to_string().contains(".trim() is not supported on Result"), "{error}");
    assert!(error.help_text().unwrap_or_default().contains("unwrap it first"));
    check_ok(
        r#"
fn load(cmd: Str) {
    let out = run! $*cmd
    out.trim()
}
fn main() -> Result[Str, Error] {
    let text = load("echo hi")?
    ok(text)
}
let first = |xs| { let x = xs.find(. != "")?
x.upper() }
let s: Option[Str] = first(["", "a"])
"#,
    );
}

#[test]
fn map_annotations_infer_value_types() {
    check_ok(
        r#"
let m: Map[Str, Int] = {a: 1}
let n: Int = m.a
let i: Int = m["a"]
for (k, v) in m { let x: Int = v }
let vs: List[Int] = m.values()
let es: List[Tuple[Str, Int]] = m.entries()
let doubled: List[Int] = m.map(|k, v| v * 2)
let inferred = {x: 1, y: 2}
let y: Int = inferred.y
"#,
    );
    assert!(rill::parser::parse("let bad: Map[Int, Int] = {}").is_err());
    assert!(check_err("let m: Map[Str, Int] = {a: \"x\"}").contains("expected Int, got Str"));
    assert!(
        check_err("let m: Map[Str, Int] = {a: 1}\nm.a.upper()").contains("not supported on Int")
    );
    check_ok("let m: Map[Str, Int] = {}\nvar n = m\nn = {b: 2}");
}

#[test]
fn common_type_merges_option_and_result() {
    check_ok(
        r#"
let flag = true
let o = if flag { some(1) } else { none }
let i: Int = o ?? 0
let r = if flag { ok(1) } else { err("e") }
let v: Int = r.unwrap_or(0)
let z: Int = if flag { 1 } else { 2 }
"#,
    );
}

#[test]
fn parameters_and_loop_variables_are_immutable() {
    let error = check_err("fn f(x: Int) -> Int { x = 2\n x }");
    assert!(error.contains("cannot assign to immutable variable 'x'"), "{error}");
    let error = check_err("for x in [1] { x = 2 }");
    assert!(error.contains("cannot assign to immutable variable 'x'"), "{error}");
    check_ok("fn f(x: Int) -> Int { var y = x\n y += 1\n y }");
}

#[test]
fn for_expressions_infer_their_element_type_from_emit() {
    check_ok("let fs = for i in 0..3 { emit || i }\nfs.map(|f| f())");
    check_ok("let r = race for x in [1] { emit x }\nprint(r + 1)");
    let error = check_err("let names = for x in [1, 2] { emit \"n{x}\" }\nnames.map(|n| n + 1)");
    assert!(error.contains("Str and Int"), "{error}");
    check_ok("let mixed = for x in [1] { if x > 0 { emit 1 } else { emit \"s\" } }\nmixed.len()");
}

#[test]
fn ostack_namespace_functions_are_typed_without_the_feature() {
    check_ok(
        r#"
let servers: List[Map] = ostack.servers()
let one: Option[Map] = ostack.find("id")
let inv: Map[Str, List[Map]] = ostack.inventory(["servers"])
let who: Map = ostack.show("server", "web-1")
let same: Map = ostack.server("web-1")
let vol: Map = ostack.volume("data")
let attached: List[Map] = ostack.server("web-1").attached_volumes()
let owner: Option[Map] = vol.project()
let hits: List[Map] = ostack.search("10.0.0.11", all_projects=true, kinds=["ports"])
let fips: List[Map] = ostack.floating_ips()
let dev: Option[Map] = ostack.port("p").server()
let net: Option[Map] = ostack.port("p").network()
let on_host: List[Map] = ostack.hypervisor("compute-17").servers()
let routers: List[Map] = ostack.subnet("s").routers()
let quota: List[Map] = ostack.quota(project="demo", usage=true)
let acts: List[Map] = ostack.actions("web-1", events=true, limit=5)
let flavor: Option[Map] = ostack.server("x").flavor()
let snaps: List[Map] = ostack.volume("v").snapshots()
let zones: List[Map] = ostack.availability_zones()
let members: List[Map] = ostack.pool("p").members()
let created: List[Map] = ostack.user("u").created_servers()
let memberships: List[Map] = ostack.user("u").project_memberships()
let names: List[Str] = ostack.kinds()
let active = ostack.servers(all_projects=true, project="demo").filter(.status == "ACTIVE")
let f = ostack.servers
let scoped: List[Map] = ostack.project("demo").servers()
let everywhere: Map[Str, List[Map]] = ostack.all_projects().inventory(["servers"])
"#,
    );
    let err = check_err(r#"ostack.show("servers")"#);
    assert!(err.contains("ostack.show needs 2 args, got 1"), "{err}");
    let err = check_err("ostack.servers(bogus=true)");
    assert!(err.contains("unknown ostack.servers argument 'bogus'"), "{err}");
    let err = check_err("ostack.servers(fresh=1)");
    assert!(err.contains("fresh= expects Bool"), "{err}");
    let err = check_err("ostack.server()");
    assert!(err.contains("ostack.server needs 1 args, got 0"), "{err}");
    let err = check_err("ostack.servr()");
    assert!(err.contains("ostack has no function 'servr'"), "{err}");
}
