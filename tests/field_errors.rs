//! Field-access and `?` errors show the offending value, so a missing field in
//! unfamiliar data can be diagnosed without adding a print first.

fn error_help(source: &str) -> (String, String) {
    let error = rill::run_source(source, Vec::new()).expect_err(source);
    (error.to_string(), error.help_text().unwrap_or_default().to_string())
}

#[test]
fn missing_field_shows_the_record_and_the_get_hint() {
    let (message, help) = error_help(
        r#"let rows = "[\{\"id\": \"a\", \"created\": 1}, \{\"id\": \"b\"}]".parse_json()
rows.filter(.created > 0)"#,
    );
    assert!(message.contains("unknown field 'created'"), "{message}");
    assert!(help.contains("this record has no 'created'"), "{help}");
    assert!(help.contains(r#""id": "b""#), "{help}");
    assert!(help.contains(r#"use .get("created") when the field is optional"#), "{help}");
}

#[test]
fn missing_field_suggests_a_close_key() {
    let (_, help) = error_help(r#""\{\"name\": \"x\", \"created\": 1}".parse_json().creatd"#);
    assert!(help.starts_with("did you mean 'created'?"), "{help}");
}

#[test]
fn large_records_are_truncated_and_nested_values_collapsed() {
    let fields: Vec<String> = (0..20).map(|i| format!(r#"\"f{i}\": {i}"#)).collect();
    let source = format!(
        r#"let m = "\{{{}, \"nested\": \{{\"a\": 1, \"b\": 2}}, \"list\": [1, 2, 3]}}".parse_json()
m.missing"#,
        fields.join(", ")
    );
    let (_, help) = error_help(&source);
    assert!(help.contains("… 10 more fields"), "{help}");
    assert!(!help.contains("f19"), "{help}");

    let (_, help) = error_help(
        r#"let m = "\{\"nested\": \{\"a\": 1, \"b\": 2}, \"list\": [1, 2, 3]}".parse_json()
m.missing"#,
    );
    assert!(help.contains(r#""nested": {2 fields}"#), "{help}");
    assert!(help.contains(r#""list": [3 items]"#), "{help}");
}

#[test]
fn long_strings_are_cut() {
    let long = "x".repeat(200);
    let (_, help) = error_help(&format!(r#""\{{\"s\": \"{long}\"}}".parse_json().missing"#));
    assert!(help.contains(&format!("\"{}…\"", "x".repeat(60))), "{help}");
}

#[test]
fn field_access_on_a_scalar_shows_the_value() {
    let (message, help) =
        error_help(r#""[\"2026-10-15T14:29:21Z\"]".parse_json().map(.created_at)"#);
    assert!(message.contains("Str has no field 'created_at'"), "{message}");
    assert_eq!(help, r#"the value is "2026-10-15T14:29:21Z""#);
}

#[test]
fn try_on_a_plain_value_shows_it() {
    let (message, help) = error_help(r#""[\"x\"]".parse_json().map(|v| v?)"#);
    assert!(message.contains("? needs Option or Result, got Str"), "{message}");
    assert!(help.starts_with(r#"the value is "x""#), "{help}");
    assert!(help.contains("does not test whether a field exists"), "{help}");
}

#[test]
fn previews_never_contain_ansi() {
    let (_, help) = error_help(r#""\{\"a\": [1], \"b\": true}".parse_json().c"#);
    assert!(!help.contains('\x1b'), "{help:?}");
}
