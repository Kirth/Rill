fn parses_design_note_sample(name: &str, source: &str) {
    let program = rill::parser::parse(source).unwrap_or_else(|err| panic!("{name}: {err}"));
    rill::typeck::check(&program).unwrap_or_else(|err| panic!("{name}: {err}"));
    assert!(!program.stmts.is_empty(), "{name}: parser produced no statements");
}

#[test]
fn design_note_core_script() {
    parses_design_note_sample(
        "core script",
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
"#,
    );
}

#[test]
fn design_note_byte_and_value_pipelines() {
    parses_design_note_sample(
        "byte and value pipelines",
        r#"run cat /var/log/foo | run grep ERROR | .lines() |> .take(20)"#,
    );
}

#[test]
fn design_note_regex_examples() {
    parses_design_note_sample(
        "regex examples",
        r#"
let line = "user=bob status=200"
if line =~ /user=(?<user>\w+) status=(?<status>\d+)/ {
    print "{user}: {status}"
}

let x = "foo foo" =~ s/foo/bar/g
let y = x.replace(/bar/, "baz", all=true)
"#,
    );
}

#[test]
fn design_note_args_shape_as_map_until_args_blocks_exist() {
    parses_design_note_sample(
        "map/object shape",
        r#"
let server = {
    name: "compute-01",
    ip: "10.0.0.12",
    tags: ["gpu", "prod"],
}

print server.ip
"#,
    );
}

#[test]
fn design_note_redirection_surface() {
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
}
