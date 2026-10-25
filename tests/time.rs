//! Time values: parsing, comparison, arithmetic, methods, the `time`
//! namespace and day/week durations.

use rill::types::Value;

fn run(source: &str) -> Value {
    rill::run_source(source, Vec::new()).expect(source)
}

fn run_err(source: &str) -> String {
    rill::run_source(source, Vec::new()).expect_err(source).to_string()
}

fn text(source: &str) -> String {
    run(source).as_string_lossy()
}

#[test]
fn parses_the_common_forms_into_utc() {
    assert_eq!(text(r#""2026-10-15T14:29:21Z".parse_time()"#), "2026-10-15T14:29:21Z");
    assert_eq!(text(r#""2026-10-15T16:29:21+02:00".parse_time()"#), "2026-10-15T14:29:21Z");
    // Nova and Keystone write timestamps without a zone: UTC.
    assert_eq!(text(r#""2026-10-15T14:29:21.000000".parse_time()"#), "2026-10-15T14:29:21Z");
    assert_eq!(text(r#""2026-10-15T14:29:21.250000Z".parse_time()"#), "2026-10-15T14:29:21.250Z");
    assert_eq!(text(r#""2026-10-15 14:29:21".parse_time()"#), "2026-10-15T14:29:21Z");
    assert_eq!(text(r#""2026-10-15".parse_time()"#), "2026-10-15T00:00:00Z");
    assert_eq!(text(r#""15/10/2026 14:29".parse_time("%d/%m/%Y %H:%M")"#), "2026-10-15T14:29:00Z");
    assert_eq!(text(r#"time.parse("2026-10-15")"#), "2026-10-15T00:00:00Z");
    assert_eq!(text("time.from_unix(0)"), "1970-01-01T00:00:00Z");
    assert_eq!(run(r#"type("2026-10-15".parse_time())"#), Value::Str("Time".into()));

    let error = run_err(r#""yesterday".parse_time()"#);
    assert!(error.contains("cannot parse 'yesterday' as a time"), "{error}");
}

#[test]
fn compares_and_does_arithmetic() {
    let t = r#"let a = "2026-10-15T14:00:00Z".parse_time()
let b = "2026-10-16T14:00:00Z".parse_time()
"#;
    assert_eq!(
        run(&format!("{t}[a < b, a <= a, b > a, a == a, a != b]")).to_string(),
        "[true, true, true, true, true]"
    );
    assert_eq!(run(&format!("{t}b - a == 1d")), Value::Bool(true));
    assert_eq!(text(&format!("{t}a + 36h")), "2026-10-17T02:00:00Z");
    assert_eq!(text(&format!("{t}b - 2w")), "2026-10-02T14:00:00Z");
    assert_eq!(run(&format!("{t}(b - a).secs()")), Value::Int(86_400));
    let error = run_err(&format!("{t}a - b"));
    assert!(error.contains("a Duration cannot be negative"), "{error}");
    assert_eq!(run("30d == 720h"), Value::Bool(true));
    assert_eq!(text("30d + 3h"), "30d3h");
}

#[test]
fn sorts_groups_and_answers_questions() {
    let rows = r#"let rows = [
  {name: "c", updated: "2026-10-20T00:00:00Z"},
  {name: "a", updated: "2026-01-01T00:00:00Z"},
  {name: "b", updated: "2026-05-05T00:00:00Z"},
]
"#;
    assert_eq!(
        run(&format!("{rows}rows.sort_by(.updated.parse_time()).map(.name)")).to_string(),
        "[a, b, c]"
    );
    let cutoff = r#"let cutoff = "2026-03-01".parse_time()
"#;
    assert_eq!(
        run(&format!("{rows}{cutoff}rows.count(.updated.parse_time() > cutoff)")),
        Value::Int(2)
    );
    assert_eq!(
        run(&format!("{rows}rows.map(.updated.parse_time()).unique().len()")),
        Value::Int(3)
    );
    let t = r#"let t = "2026-10-15T14:29:21Z".parse_time()
"#;
    assert_eq!(
        run(&format!("{t}[t.year(), t.month(), t.day(), t.hour(), t.minute(), t.second()]"))
            .to_string(),
        "[2026, 10, 15, 14, 29, 21]"
    );
    assert_eq!(text(&format!("{t}t.weekday()")), "Thursday");
    assert_eq!(text(&format!("{t}t.date()")), "2026-10-15");
    assert_eq!(text(&format!("{t}t.format(\"%H:%M on %d %b\")")), "14:29 on 15 Oct");
    assert_eq!(run(&format!("{t}t.unix()")), Value::Int(1_792_074_561));
    assert_eq!(text(&format!("{t}t.to_json()")), "\"2026-10-15T14:29:21Z\"");
    // The clock: now is later than a fixed past time, and age is positive.
    assert_eq!(
        run(&format!("{t}[time.now() > t, t.age() > 1s, time.now() - 1d < time.now()]"))
            .to_string(),
        "[true, true, true]"
    );
    assert_eq!(run(r#""2999-01-01".parse_time().age() == 0s"#), Value::Bool(true));
}

#[test]
fn the_checker_types_times() {
    let ok =
        |source: &str| rill::typeck::check(&rill::parser::parse(source).unwrap()).expect(source);
    let err = |source: &str| {
        rill::typeck::check(&rill::parser::parse(source).unwrap()).expect_err(source).to_string()
    };
    ok(r#"let t: Time = "2026-10-15".parse_time()
let earlier: Time = t - 1d
let gap: Duration = time.now() - t
let fresh: Bool = t.age() < 7d
let y: Int = t.year()
let s: Str = t.format("%Y")"#);
    let e = err(r#"let t = "2026-10-15".parse_time()
t < "2026""#);
    assert!(e.contains("comparison needs"), "{e}");
    let e = err(r#"let t = "2026-10-15".parse_time()
t + t"#);
    assert!(e.contains("not defined for Time and Time"), "{e}");
}
