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
    check_ok(r#"fn is_vm_haunted?(id: Str) -> Result[Option[Tuple[List, List]]] { none }"#);
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
