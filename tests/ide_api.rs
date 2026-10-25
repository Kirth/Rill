//! The embedding API an IDE needs: tolerant tokenizing, captured output,
//! cancellation, cloneable checker state, true snapshots and I/O policy.

use rill::eval::{CancellationToken, Evaluator, IoPolicy, OutStream};
use rill::parser::{tokenize, HighlightKind};
use rill::typeck::Session;
use rill::types::Value;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn eval(evaluator: &mut Evaluator, source: &str) -> Result<Value, rill::diagnostic::RillError> {
    let program = rill::parser::parse(source).expect("parses");
    evaluator.eval_program(&program)
}

fn capture(evaluator: &mut Evaluator) -> Arc<Mutex<Vec<(OutStream, String)>>> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    evaluator.set_output(Arc::new(move |stream, text: &str| {
        sink.lock().unwrap().push((stream, text.to_string()));
    }));
    captured
}

#[test]
fn tokenize_keeps_tokens_before_a_lexing_error() {
    let source = "let x = 1 + \"never closed";
    let (tokens, error) = tokenize(source);
    assert!(error.is_some(), "an unterminated string is a lexing error");
    let kinds: Vec<_> = tokens.iter().map(|token| token.kind).collect();
    assert_eq!(
        kinds,
        [
            HighlightKind::Keyword,
            HighlightKind::Ident,
            HighlightKind::Operator,
            HighlightKind::Number,
            HighlightKind::Operator
        ]
    );
    assert_eq!(&source[tokens[0].span.start..tokens[0].span.end], "let");
}

#[test]
fn tokenize_classifies_commands_and_interpolations() {
    let source = "let r = run ls -l $dir\nprint \"{r.out} files\"";
    let (tokens, error) = tokenize(source);
    assert!(error.is_none(), "{error:?}");
    let text = |token: &rill::parser::HighlightToken| &source[token.span.start..token.span.end];
    let run = tokens.iter().find(|token| text(token) == "run").unwrap();
    assert_eq!(run.kind, HighlightKind::Keyword);
    let ls = tokens.iter().find(|token| text(token) == "ls").unwrap();
    assert_eq!(ls.kind, HighlightKind::CommandWord);
    let dir = tokens.iter().find(|token| text(token) == "$dir").unwrap();
    assert_eq!(dir.kind, HighlightKind::CommandVar);
    // The interpolated expression is lexed too, with spans in the outer source.
    let r = tokens.iter().filter(|token| text(token) == "r").collect::<Vec<_>>();
    assert_eq!(r.len(), 2, "`r` appears as a binding and inside the template");
    assert!(r.iter().all(|token| token.kind == HighlightKind::Ident));
    let out = tokens.iter().find(|token| text(token) == "out").unwrap();
    assert_eq!(out.kind, HighlightKind::Ident);
    assert!(tokens.iter().any(|token| token.kind == HighlightKind::Str));
}

#[test]
fn output_sink_receives_print_prin_and_warn() {
    let mut evaluator = Evaluator::new(Vec::new());
    let captured = capture(&mut evaluator);
    eval(&mut evaluator, "print \"a\"\nprin \"b\"\nwarn \"c\"\npretty_print([1])").unwrap();
    let captured = captured.lock().unwrap();
    assert_eq!(captured[0], (OutStream::Stdout, "a\n".to_string()));
    assert_eq!(captured[1], (OutStream::Stdout, "b".to_string()));
    assert_eq!(captured[2], (OutStream::Stderr, "c\n".to_string()));
    assert_eq!(captured[3].0, OutStream::Stdout);
    assert!(captured[3].1.contains('1') && captured[3].1.ends_with('\n'));
}

#[test]
fn show_hands_values_to_a_host_or_prints_a_text_table() {
    let mut evaluator = Evaluator::new(Vec::new());
    let captured = capture(&mut evaluator);
    eval(&mut evaluator, "show([{id: 1, name: \"x\"}, {id: 22, extra: true}], \"two\")").unwrap();
    let text = captured.lock().unwrap()[0].1.clone();
    assert_eq!(text, "two\nid  name  extra\n1   x\n22        true\n");
    captured.lock().unwrap().clear();
    eval(&mut evaluator, "show {a: [1, 2]}").unwrap();
    assert!(captured.lock().unwrap()[0].1.starts_with('{'));

    type Shown = Arc<Mutex<Vec<(Option<String>, Value)>>>;
    let shown: Shown = Arc::default();
    let sink = shown.clone();
    evaluator.set_show(Arc::new(move |title, value| {
        sink.lock().unwrap().push((title.map(str::to_string), value.clone()));
    }));
    captured.lock().unwrap().clear();
    eval(&mut evaluator, "show([1, 2], \"list\")\nshow 3").unwrap();
    let shown = shown.lock().unwrap();
    assert_eq!(shown[0].0.as_deref(), Some("list"));
    assert_eq!(shown[1], (None, Value::Int(3)));
    assert!(captured.lock().unwrap().is_empty(), "a host sink replaces the text");
}

#[test]
fn cancellation_stops_a_running_command() {
    let mut evaluator = Evaluator::new(Vec::new());
    let token = CancellationToken::new();
    evaluator.set_cancellation(token.clone());
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        token.cancel();
    });
    let started = Instant::now();
    let result = eval(&mut evaluator, "run sleep 10");
    canceller.join().unwrap();
    assert!(result.is_err(), "a cancelled run is an error");
    assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
}

#[test]
fn session_clones_are_independent() {
    let mut session = Session::new();
    let program = rill::parser::parse("let x = 1").unwrap();
    session.check(&program).unwrap();
    let mut copy = session.clone();
    let program = rill::parser::parse("let y = x + 1").unwrap();
    copy.check(&program).unwrap();
    assert!(copy.names().contains(&"y".to_string()));
    assert!(!session.names().contains(&"y".to_string()));
}

#[test]
fn kind_of_reports_the_receiver_kind() {
    let mut session = Session::new();
    session.check(&rill::parser::parse("let xs = [1, 2]").unwrap()).unwrap();
    let program = rill::parser::parse("xs").unwrap();
    let rill::ast::Stmt::Expr(expr) = program.stmts[0].unspanned().clone() else {
        panic!("expected an expression");
    };
    assert_eq!(session.kind_of(&expr).unwrap(), rill::methods::Kind::List);
    let types = session.binding_types();
    let xs = types.iter().find(|(name, _, _)| name == "xs").unwrap();
    assert_eq!(xs.1, "List[Int]");
    assert!(!xs.2, "let bindings are immutable");
}

#[test]
fn snapshot_isolates_var_cells_but_clone_shares_them() {
    let mut evaluator = Evaluator::new(Vec::new());
    eval(&mut evaluator, "var n = 1").unwrap();
    let shared = evaluator.clone();
    let snapshot = evaluator.snapshot();
    eval(&mut evaluator, "n += 1").unwrap();
    assert_eq!(evaluator.binding("n"), Some(Value::Int(2)));
    assert_eq!(shared.binding("n"), Some(Value::Int(2)), "clone shares the cell");
    assert_eq!(snapshot.binding("n"), Some(Value::Int(1)), "snapshot keeps its own copy");
    let names: Vec<_> = snapshot.bindings().into_iter().map(|(name, _)| name).collect();
    assert!(names.contains(&"n".to_string()));
}

#[test]
fn io_policy_nulls_child_stdin_and_captures_inherited_streams() {
    let mut evaluator = Evaluator::new(Vec::new());
    evaluator.set_io_policy(IoPolicy { null_stdin: true, capture_inherited: true });
    let captured = capture(&mut evaluator);
    // Without stdin `cat` would wait for the terminal; with /dev/null it exits at once.
    let started = Instant::now();
    let value = eval(&mut evaluator, "run cat").unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(matches!(value, Value::ProcessResult(_)));
    let value = eval(&mut evaluator, "let r = run echo hi { stdout: inherit }\nr.out").unwrap();
    assert_eq!(value, Value::Str(String::new()), "inherited stdout stays invisible to the script");
    let captured = captured.lock().unwrap();
    assert_eq!(captured.last(), Some(&(OutStream::Stdout, "hi\n".to_string())));
}

#[test]
fn incomplete_input_detection_is_public() {
    let source = "fn f() {\n";
    let error = rill::parser::parse(source).unwrap_err();
    assert!(rill::repl::is_incomplete(&error, source));
    let source = "let = 1";
    let error = rill::parser::parse(source).unwrap_err();
    assert!(!rill::repl::is_incomplete(&error, source));
}

#[test]
fn namespace_functions_include_the_ostack_table() {
    let names = rill::methods::namespace_functions("ostack");
    for expected in ["servers", "find", "inventory", "show", "refresh"] {
        assert!(names.contains(&expected), "{expected} in {names:?}");
    }
    let function = rill::methods::namespace_function("ostack", "show").expect("show");
    assert_eq!(function.min_args, 2);
    assert!(rill::methods::namespace_function("json", "parse").is_none(), "methods stay methods");
}

#[test]
fn a_host_defines_bindings_for_the_checker_and_the_evaluator() {
    let mut evaluator = Evaluator::new(Vec::new());
    evaluator.define("limit", Value::Int(3));
    let mut session = Session::new();
    session.define("limit", &Value::Int(3));
    let program = rill::parser::parse("[1, 2, 3, 4, 5].filter(|x| x <= limit)").expect("parses");
    session.check(&program).expect("limit is known to the checker");
    assert_eq!(
        evaluator.eval_program(&program).unwrap(),
        Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
    let wrong = rill::parser::parse("limit.trim()").expect("parses");
    assert!(session.check(&wrong).is_err(), "limit is typed as Int");
}
