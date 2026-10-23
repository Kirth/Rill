use rill::types::Value;

fn run(source: &str) -> Value {
    rill::run_source(source, Vec::new()).expect(source)
}

fn run_str(source: &str) -> String {
    match run(source) {
        Value::Str(text) => text,
        other => panic!("{source}: expected Str, got {other:?}"),
    }
}

fn error_of(source: &str) -> rill::diagnostic::RillError {
    rill::run_source(source, Vec::new()).expect_err(source)
}

const PRELUDE: &str = r#"
let a = 1
let b = 2
let xs = [10, 20]
let m = {k: "v", name: "ann"}
let f = |x| x * 3
let o = some(5)
"#;

fn with_prelude(expr: &str) -> String {
    run_str(&format!("{PRELUDE}\n{expr}"))
}

#[test]
fn interpolates_variables_fields_calls_and_expressions() {
    for (source, expected) in [
        (r#""$a""#, "1"),
        (r#""${a}""#, "1"),
        (r#""{a}""#, "1"),
        (r#""{a + b}""#, "3"),
        (r#""{xs[0]}""#, "10"),
        (r#""{m.name}""#, "ann"),
        (r#""{m.name.upper()}""#, "ANN"),
        (r#""{f(2)}""#, "6"),
        (r#""{m.get("k")}""#, "v"),
        (r#""{xs.map(|x| x * 2)}""#, "20\n40"),
        (r#""{ {a: 1}.a }""#, "1"),
        (r#""{o}""#, "some(5)"),
        (r#""{none}""#, "none"),
        (r#""${o}""#, "some(5)"),
        (r#""$a$b""#, "12"),
        (r#""$a.b""#, "1.b"),
    ] {
        assert_eq!(with_prelude(source), expected, "{source}");
    }
}

#[test]
fn escapes_and_literal_dollars_and_braces() {
    for (source, expected) in [
        (r#""\{literal\}""#, "{literal}"),
        (r#""\$HOME""#, "$HOME"),
        (r#""cost: $5""#, "cost: $5"),
        (r#""a$""#, "a$"),
        (r#""}""#, "}"),
        (r#""{}""#, "{}"),
        (r#""\u{41}\u{1F600}""#, "A😀"),
        (r#""x\0y""#, "x\0y"),
        (r#""\e[0m""#, "\x1b[0m"),
        (r#""tab\tnl\nq\"bs\\""#, "tab\tnl\nq\"bs\\"),
    ] {
        assert_eq!(run_str(source), expected, "{source}");
    }
}

#[test]
fn raw_strings_are_verbatim() {
    assert_eq!(run_str(r#"r"\n{x}$y""#), "\\n{x}$y");
    assert_eq!(run_str(r#"r"""#), "");
}

#[test]
fn braced_defaults_fall_back_to_templates() {
    let source = format!("{PRELUDE}\n[\"${{RILL_UNSET_Q:-def}}\", \"${{RILL_UNSET_Q:-{{a}}+$b}}\", \"${{a:-no}}\"].join(\"|\")");
    assert_eq!(run_str(&source), "def|1+2|1");
    let mut environment = rill::eval::process_environment();
    environment.insert("RILL_SET_Q".into(), "yes".into());
    environment.insert("RILL_EMPTY_Q".into(), String::new());
    let value = rill::run_source_with_environment(
        r#"["${RILL_SET_Q:-no}", "${RILL_EMPTY_Q:-empty}"].join("|")"#,
        vec![],
        None,
        environment,
    )
    .unwrap();
    assert_eq!(value, Value::Str("yes|empty".into()));
}

#[test]
fn interpolation_errors_point_into_the_string() {
    let error = error_of(
        r#"let host = 1
print "{hots}""#,
    );
    assert!(error.to_string().contains("unknown identifier 'hots'"), "{error}");
    assert_eq!(error.help_text(), Some("did you mean `host`?"));
    assert_eq!(error.location(), Some((2, 9)));

    let error = error_of(r#"print "{1 +}""#);
    assert!(error.to_string().contains("expected expression"), "{error}");
    assert!(error.span.is_some());

    let error = error_of(r#"print "oops {x""#);
    assert!(error.to_string().contains("unterminated interpolation"), "{error}");

    let error = error_of(r#"print "bad \q""#);
    assert!(error.to_string().contains("unknown string escape `\\q`"), "{error}");

    let error = error_of(r#"print "{print}""#);
    assert!(error.to_string().contains("cannot interpolate a function"), "{error}");
}

#[test]
fn command_quoted_strings_interpolate_once() {
    let source = r#"
let x = "X"
let r = run printf "%s|%s|%s" "{x}" "\$HOME" "${RILL_UNSET_Q:-d}"
r.out
"#;
    assert_eq!(run_str(source), "X|$HOME|d");
    // Escaped braces and dollars reach the command verbatim, and its output is
    // never interpolated again.
    let source = r#"
let r = run printf "%s" "\{nope\} \$nope"
r.out
"#;
    assert_eq!(run_str(source), "{nope} $nope");
}

#[test]
fn paths_expand_tilde_only_for_source_text() {
    let mut environment = rill::eval::process_environment();
    environment.insert("HOME".into(), "/home/test".into());
    let value = rill::run_source_with_environment(
        r#"
let p = "~/x"
[Str(path(p)), Str(path("~/x")), Str(path("$p")), Str(file("~/y")), Str(path("/a").join("~/b"))].join("|")
"#,
        vec![],
        None,
        environment,
    )
    .unwrap();
    assert_eq!(value, Value::Str("~/x|/home/test/x|/home/test/x|/home/test/y|/a/~/b".into()));
}

#[test]
fn input_use_is_detected_inside_interpolation() {
    let value =
        rill::run_source_with_input(r#""{input.lines.len()}""#, vec![], Some("a\nb\n".into()))
            .unwrap();
    assert_eq!(value, Value::Str("2".into()));
}
