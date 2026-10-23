//! The interactive session: state persists across inputs, incomplete input
//! continues, errors do not end the session.

mod common;

use rill::repl::{Repl, Response};
use rill::types::Value;
use std::io::Write;
use std::process::Stdio;

#[test]
fn bindings_and_functions_persist_between_lines() {
    let mut repl = Repl::new(Vec::new());
    assert!(matches!(repl.feed("let x = 2"), Response::Silent));
    assert!(matches!(repl.feed("x * 21"), Response::Value(Value::Int(42))));
    assert!(matches!(repl.feed("fn f(a) {"), Response::Continue));
    assert!(repl.is_continuing());
    assert!(matches!(repl.feed("  a + 1"), Response::Continue));
    assert!(matches!(repl.feed("}"), Response::Silent));
    assert!(matches!(repl.feed("f(x)"), Response::Value(Value::Int(3))));
    assert!(matches!(repl.feed("var n = 0"), Response::Silent));
    assert!(matches!(repl.feed("n += 5"), Response::Silent));
    assert!(matches!(repl.feed("n"), Response::Value(Value::Int(5))));
}

#[test]
fn errors_are_reported_and_the_session_continues() {
    let mut repl = Repl::new(Vec::new());
    match repl.feed("nope + 1") {
        Response::Error(error) => {
            assert!(error.message.contains("unknown identifier 'nope'"), "{}", error.message);
            assert_eq!(error.source_name(), Some("<repl:1>"));
        }
        other => panic!("expected an error, got {other:?}"),
    }
    assert!(matches!(repl.feed("1 + 1"), Response::Value(Value::Int(2))));
    match repl.feed("let r: Result[Int, Error] = err(\"boom\")\nr") {
        Response::Error(error) => assert!(error.message.contains("boom")),
        other => panic!("expected the unhandled error, got {other:?}"),
    }
    assert!(matches!(repl.feed("[1,2].get(9).unwrap()"), Response::Error(_)));
    assert!(matches!(repl.feed("3"), Response::Value(Value::Int(3))));
}

#[test]
fn commands_report_types_names_and_quit() {
    let mut repl = Repl::new(Vec::new());
    repl.feed("let host = \"h\"");
    assert!(matches!(repl.feed(":type host"), Response::Text(text) if text == "Str"));
    assert!(
        matches!(repl.feed(":type [1].map(|x| x + 1)"), Response::Text(text) if text == "List[Int]")
    );
    assert!(matches!(repl.feed(":env"), Response::Text(text) if text.lines().any(|l| l == "host")));
    assert!(matches!(repl.feed(":help"), Response::Text(text) if text.contains(":type")));
    assert!(matches!(repl.feed(":nope"), Response::Text(text) if text.contains("unknown command")));
    assert!(matches!(repl.feed(""), Response::Silent));
    assert!(matches!(repl.feed(":quit"), Response::Quit));
}

#[test]
fn cli_repl_reads_stdin_until_quit() {
    let mut child = common::rill()
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"let x = 20\nx + 22\nzzz\nprint(\"after\")\n:quit\nprint(\"never\")\n")
        .unwrap();
    let output = common::bounded_output(child);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42\n"), "{stdout}");
    assert!(stdout.contains("after"), "{stdout}");
    assert!(!stdout.contains("never"), "{stdout}");
    assert!(!stdout.contains("rill>"), "no prompt when stdin is not a terminal: {stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown identifier 'zzz'"), "{stderr}");
}
