use rill::types::Value;

fn run(source: &str) -> Value {
    rill::run_source(source, Vec::new()).expect(source)
}

#[test]
fn regex_match_uses_real_regex_semantics() {
    assert_eq!(
        run(r#""user=bob status=200" =~ /user=\w+ status=\d+/"#),
        Value::Bool(true)
    );
    assert_eq!(
        run(r#""user=bob status=ok" =~ /user=\w+ status=\d+/"#),
        Value::Bool(false)
    );
}

#[test]
fn named_captures_bind_inside_if_body() {
    let value = run(r#"
let line = "user=bob status=200"
if line =~ /user=(?<user>\w+) status=(?<status>\d+)/ {
    user + ":" + status
} else {
    "no"
}
"#);
    assert_eq!(value, Value::Str("bob:200".to_string()));
}

#[test]
fn named_captures_interpolate_with_dollar_syntax() {
    let value = run(r#"
let line = "2: eth0    inet 192.0.2.254/32"
let vip = "192.0.2.254"
let host = "controller-1"
if line =~ /^\d+:\s+(?<iface>\S+)\s+.*\s(?<addr>\d+\.\d+\.\d+\.\d+)\/\d+/ {
    if addr == vip {
        "VIP $vip is on $host:$iface"
    }
}
"#);
    assert_eq!(
        value,
        Value::Str("VIP 192.0.2.254 is on controller-1:eth0".to_string())
    );
}

#[test]
fn named_captures_do_not_leak_outside_if_body() {
    let err = rill::run_source(
        r#"
let line = "user=bob status=200"
if line =~ /user=(?<user>\w+)/ {
    print user
}
print user
"#,
        Vec::new(),
    )
    .expect_err("capture should not leak");

    assert!(err.to_string().contains("unknown identifier 'user'"));
}

#[test]
fn access_log_style_capture_example() {
    let value = run(r#"
let line = "10.0.0.1 - - \"GET /index.html HTTP/1.1\""
if line =~ /(?<ip>\S+) .* "(?<method>\w+) (?<path>\S+)/ {
    ip + " " + method + " " + path
} else {
    "no"
}
"#);
    assert_eq!(value, Value::Str("10.0.0.1 GET /index.html".to_string()));
}

#[test]
fn invalid_regex_is_a_typecheck_error() {
    let program = rill::parser::parse(r#""abc" =~ /[/"#).expect("parse");
    let err = rill::typeck::check(&program).expect_err("regex should fail");
    assert!(err.to_string().contains("invalid regex"));
}

#[test]
fn terse_substitution_replaces_first_match_by_default() {
    let value = run(r#""foo foo" =~ s/foo/bar/"#);
    assert_eq!(value, Value::Str("bar foo".to_string()));
}

#[test]
fn terse_substitution_g_flag_replaces_all_matches() {
    let value = run(r#""foo foo" =~ s/foo/bar/g"#);
    assert_eq!(value, Value::Str("bar bar".to_string()));
}

#[test]
fn method_replace_accepts_regex_literal_argument() {
    let value = run(r#""foo foo".replace(/foo/, "bar", all=true)"#);
    assert_eq!(value, Value::Str("bar bar".to_string()));
}

#[test]
fn match_expression_regex_arms_bind_captures() {
    let value = run(r#"
let line = "10.0.0.1 - - \"GET /index.html HTTP/1.1\""
match line {
    /(?<ip>\S+) .* "(?<method>\w+) (?<path>\S+)/ => ip + " " + method + " " + path
    _ => "no"
}
"#);
    assert_eq!(value, Value::Str("10.0.0.1 GET /index.html".to_string()));
}

#[test]
fn match_expression_uses_wildcard_fallback() {
    let value = run(r#"
match "abc" {
    /nomatch/ => "bad"
    _ => "fallback"
}
"#);
    assert_eq!(value, Value::Str("fallback".to_string()));
}
