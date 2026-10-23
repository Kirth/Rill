//! AST snapshots for the examples in the design notes.  A change here means
//! the parser's output shape changed; review it with `cargo insta review`.

fn parsed(source: &str) -> rill::ast::Program {
    let program = rill::parser::parse(source).unwrap_or_else(|err| panic!("{err}"));
    rill::typeck::check(&program).unwrap_or_else(|err| panic!("{err}"));
    program
}

#[test]
fn core_script() {
    insta::assert_debug_snapshot!(parsed(
        r#"
let hosts = ["server1", "server2"]

for host in hosts {
    let result = run ssh $host "uptime"

    if result.ok {
        print "$host: {result.out.trim()}"
    } else {
        warn "$host failed: {result.err}"
    }
}
"#
    ));
}

#[test]
fn byte_and_value_pipelines() {
    insta::assert_debug_snapshot!(parsed(
        r#"run cat /var/log/foo | run grep ERROR | .lines() |> .take(20)"#
    ));
}

#[test]
fn regex_examples() {
    insta::assert_debug_snapshot!(parsed(
        r#"
let line = "user=bob status=200"
if line =~ /user=(?<user>\w+) status=(?<status>\d+)/ {
    print "{user}: {status}"
}

let x = "foo foo" =~ s/foo/bar/g
let y = x.replace(/bar/, "baz", all=true)
print(y)
"#
    ));
}

#[test]
fn map_literal_and_field_access() {
    insta::assert_debug_snapshot!(parsed(
        r#"
let server = {
    name: "compute-01",
    ip: "10.0.0.12",
    tags: ["gpu", "prod"],
}

print server.ip
"#
    ));
}

#[test]
fn redirections() {
    let program = rill::parser::parse(
        r#"
run foo > "output.txt"
run foo err> "errors.txt"
run foo all> "everything.txt"
run foo < "input.txt"
"#,
    )
    .expect("parse redirection examples");
    assert_eq!(program.stmts.len(), 4);
    insta::assert_debug_snapshot!(program);
}

#[test]
fn operators_and_precedence() {
    insta::assert_debug_snapshot!(parsed(
        r#"
let v = -2 ** 2 + 7 % 3 * 2
let w = some(1) ?? 0 .. 10
let inside = 2 in [1, 2] and not (3 not in [1])
print(v, w, inside)
"#
    ));
}

#[test]
fn patterns_and_closures() {
    insta::assert_debug_snapshot!(parsed(
        r#"
fn classify(x, scale = 2) -> Str {
    match x {
        1 | 2 => "small",
        (a, b) if a > b => "pair",
        [first, ..] => "list {first}",
        some(v) => "some",
        _ => "other",
    }
}
let f = || classify(1)
print(f())
"#
    ));
}
