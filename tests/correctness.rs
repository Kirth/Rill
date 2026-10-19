use rill::types::Value;
use std::num::NonZeroUsize;

fn run(source: &str) -> Value {
    rill::run_source(source, vec![]).expect(source)
}

fn run_parallel(source: &str, parallelism: usize) -> Result<Value, rill::diagnostic::RillError> {
    let program = rill::parser::parse(source)?;
    rill::typeck::check(&program)?;
    let executor = rill::eval::ParallelExecutor::new(
        NonZeroUsize::new(parallelism).expect("nonzero parallelism"),
    )?;
    rill::eval::Evaluator::with_executor(vec![], None, executor).eval_program(&program)
}

#[test]
fn list_addition_concatenates_without_mutating_or_flattening() {
    assert_eq!(run("[1, 2] + [2, 3]"), run("[1, 2, 2, 3]"));
    assert_eq!(run("[] + [1] + []"), run("[1]"));
    assert_eq!(run("[] + []"), run("[]"));
    assert_eq!(run("[[1]] + [[2]]"), run("[[1], [2]]"));
    assert_eq!(run(r#"[1] + ["two"]"#), run(r#"[1, "two"]"#));
    assert_eq!(
        run("let a = [1]; let b = [2]; var c = a + b; c += [3]; (a, b, c)"),
        run("([1], [2], [1, 2, 3])")
    );
    assert_eq!(
        run(r#"let data = {a: [1], b: [2]}; data.get("a") + data.get("b")"#),
        run("[1, 2]")
    );
    assert_eq!(run("var xs = [[1]]; xs[0] += [2]; xs"), run("[[1, 2]]"));
    assert_eq!(
        run("let xs: List[Int] = [] + [1, 2]; xs"),
        run("[1, 2]")
    );
    for source in ["[1] + 2", "1 + [2]", "[1] - [2]", "[1] > [2]",
        "let xs: List[Str] = [1] + [2]; xs"] {
        assert!(rill::compile(source).is_err(), "{source}");
    }
    assert!(rill::run_source(r#"{a: [1]}.get("a") + 2"#, vec![]).is_err());
}

#[test]
fn arithmetic_matches_numeric_types_and_reports_errors() {
    assert_eq!(run("1.5 + 2"), Value::Float(3.5));
    assert_eq!(run("5.0 / 2"), Value::Float(2.5));
    assert_eq!(run("5 / 2"), Value::Int(2));
    assert_eq!(run("3.5 * 2 - 1.0"), Value::Float(6.0));
    assert_eq!(
        run("9007199254740993 > 9007199254740992"),
        Value::Bool(true)
    );
    for source in [
        "1 / 0",
        "1.0 / 0.0",
        "9223372036854775807 + 1",
        "2 * 9223372036854775807",
        "1 + \"x\"",
    ] {
        let error = rill::run_source(source, vec![]).expect_err(source);
        assert!(error.span.is_some(), "{error}");
    }
}

#[test]
fn propagation_stops_arguments_and_collection_elements() {
    assert_eq!(
        run(r#"
fn fail() -> Result[Int] {
    [err("original")?, die("must not execute")]
    ok(9)
}
fail()
"#),
        Value::Result(Err(Box::new(Value::Str("original".into()))))
    );
    assert_eq!(
        run(r#"
fn consume(a, b) { a }
fn fail() -> Result[Int] {
    consume(err("original")?, die("must not execute"))
}
fail()
"#),
        Value::Result(Err(Box::new(Value::Str("original".into()))))
    );
}

#[test]
fn callback_signatures_and_yaml_shapes_agree() {
    assert_eq!(
        run(r#"({a: 2}).map(|name, value| value + 1)"#),
        Value::List(vec![Value::Int(3)])
    );
    assert_eq!(
        run(r#""[1, 2]".parse_yaml() |> .map(|x| x + 1)"#),
        Value::List(vec![Value::Int(2), Value::Int(3)])
    );
    for source in [
        "({a: 1}).map(|x| x)",
        "[1] |> .map(|a, b| a)",
        "fn f(x: Int) { x } f(\"bad\")",
        "fn f(x: Int) { x } \"bad\" |> f",
        "for x in [1] { let f = |y| { break } }",
    ] {
        assert!(rill::run_source(source, vec![]).is_err(), "{source}");
    }
    assert_eq!(
        run("let f = |x| { if x { return 7 } 9 }; f(true)"),
        Value::Int(7)
    );
}

#[test]
fn durations_keep_fractional_precision() {
    assert_eq!(run("1.5s").to_string(), "1.5s");
}

#[test]
fn evaluation_scopes_are_restored_after_errors() {
    let mut evaluator = rill::eval::Evaluator::new(vec![]);
    let program = rill::parser::parse("let x = 1; if true { let x = 2; die(\"stop\") }").unwrap();
    assert!(evaluator.eval_program(&program).is_err());
    let program = rill::parser::parse("x").unwrap();
    assert_eq!(evaluator.eval_program(&program).unwrap(), Value::Int(1));
}

#[test]
fn bounded_parallel_map_preserves_order() {
    let start = std::time::Instant::now();
    let value = run_parallel(
        r#"[1, 2, 3, 4].parallel_map(|x| {
            if x == 1 { 50ms.sleep() }
            20ms.sleep()
            x * 2
        })"#,
        2,
    )
    .unwrap();
    assert_eq!(
        value,
        Value::List(vec![
            Value::Int(2),
            Value::Int(4),
            Value::Int(6),
            Value::Int(8),
        ])
    );
    assert!(start.elapsed() >= std::time::Duration::from_millis(40));
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn parallel_environment_updates_are_local_to_each_callback() {
    let host_value = std::env::var_os("RILL_PARALLEL_LOCAL_ENV");
    let value = run_parallel(
        r#"[1, 2].parallel_map(|item| {
            set_env("RILL_PARALLEL_LOCAL_ENV", item)
            ["printenv", "RILL_PARALLEL_LOCAL_ENV"].run().out.trim()
        })"#,
        2,
    )
    .unwrap();
    assert_eq!(
        value,
        Value::List(vec![Value::Str("1".into()), Value::Str("2".into())])
    );
    assert_eq!(std::env::var_os("RILL_PARALLEL_LOCAL_ENV"), host_value);
}

#[test]
fn ordered_parallel_for_and_filter_ignore_completion_order() {
    assert_eq!(
        run_parallel(
            r#"[
                parallel for i in [1, 2, 3] {
                    if i == 1 { 50ms.sleep() }
                    emit i
                    emit i * 10
                },
                [1, 2, 3, 4].parallel_filter(|i| {
                    if i == 1 { 50ms.sleep() }
                    i > 1
                })
            ]"#,
            2,
        )
        .unwrap(),
        Value::List(vec![
            Value::List(vec![
                Value::Int(1),
                Value::Int(10),
                Value::Int(2),
                Value::Int(20),
                Value::Int(3),
                Value::Int(30),
            ]),
            Value::List(vec![Value::Int(2), Value::Int(3), Value::Int(4)]),
        ])
    );
}

#[test]
fn nested_parallel_work_reuses_single_worker_pool() {
    assert_eq!(
        run_parallel(
            "[1, 2].parallel_map(|x| [x, x + 1].parallel_map(|y| y * 2))",
            1,
        )
        .unwrap(),
        Value::List(vec![
            Value::List(vec![Value::Int(2), Value::Int(4)]),
            Value::List(vec![Value::Int(4), Value::Int(6)]),
        ])
    );
}

#[test]
fn race_stops_at_first_emit_and_cancels_sleeping_losers() {
    let start = std::time::Instant::now();
    let value = run_parallel(
        r#"race for i in [0, 1] {
            if i == 0 { emit i }
            500ms.sleep()
            emit i
        }"#,
        2,
    )
    .unwrap();
    assert_eq!(value, Value::Int(0));
    assert!(start.elapsed() < std::time::Duration::from_millis(250));
    assert_eq!(
        run_parallel("race for i in [1] { emit i; die(\"after emit\") }", 2).unwrap(),
        Value::Int(1)
    );
}

#[test]
fn race_propagates_an_error_that_arrives_before_an_emit() {
    let error = run_parallel(
        r#"race for i in [0, 1] {
            if i == 0 { die("first failure") }
            200ms.sleep()
            emit i
        }"#,
        2,
    )
    .expect_err("error should win");
    assert!(error.message.contains("first failure"), "{error}");
}

#[test]
fn parallel_failure_cancels_other_callbacks() {
    let start = std::time::Instant::now();
    let error = run_parallel(
        r#"[0, 1].parallel_map(|i| {
            if i == 0 { 50ms.sleep(); die("parallel failure") }
            500ms.sleep()
            i
        })"#,
        2,
    )
    .expect_err("parallel callback should fail");
    assert!(error.message.contains("parallel failure"), "{error}");
    assert!(start.elapsed() < std::time::Duration::from_millis(250));
}

#[test]
fn race_propagates_nonlocal_control_flow() {
    assert_eq!(
        run_parallel(
            r#"fn choose() {
                race for i in [0, 1] {
                    if i == 0 { return 9 }
                    200ms.sleep()
                    emit i
                }
                0
            }
            choose()"#,
            2,
        )
        .unwrap(),
        Value::Int(9)
    );
    assert_eq!(
        run_parallel("race for i in [1] { break; emit i }", 1).unwrap(),
        Value::Null
    );
}

#[test]
fn winning_race_can_return_a_live_process_handle() {
    let value = run_parallel(
        r#"let handle = race for i in [1] {
            emit ["sh", "-c", "printf winner"].spawn()
        }
        handle.await().out"#,
        2,
    )
    .unwrap();
    assert_eq!(value, Value::Str("winner".into()));
}

#[test]
fn race_cancels_a_losing_synchronous_command() {
    let marker = std::env::temp_dir().join(format!(
        "rill-race-command-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source = format!(
        r#"race for i in [0, 1] {{
            if i == 0 {{ 50ms.sleep(); emit i }}
            sh "sleep 1; touch '{}'"
            emit i
        }}"#,
        marker.display()
    );
    let start = std::time::Instant::now();
    assert_eq!(run_parallel(&source, 2).unwrap(), Value::Int(0));
    assert!(start.elapsed() < std::time::Duration::from_millis(500));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(!marker.exists(), "losing command continued after race");
}

#[test]
fn race_cancels_a_process_handle_owned_by_a_losing_branch() {
    let marker = std::env::temp_dir().join(format!(
        "rill-race-handle-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source = format!(
        r#"race for i in [0, 1] {{
            if i == 0 {{ 50ms.sleep(); emit i }}
            if i == 1 {{
                let handle = ["sh", "-c", "sleep 1; touch '{}'"].spawn()
                2s.sleep()
            }}
        }}"#,
        marker.display()
    );
    assert_eq!(run_parallel(&source, 2).unwrap(), Value::Int(0));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(!marker.exists(), "losing branch-owned handle escaped cancellation");
}
