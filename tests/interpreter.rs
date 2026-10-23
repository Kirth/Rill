use rill::types::Value;

fn run(source: &str) -> Value {
    rill::run_source(source, Vec::new()).expect(source)
}

/// Run `source` with the host environment plus `overrides`, without touching
/// the test process's own environment.
fn run_with_env(source: &str, overrides: &[(&str, &str)]) -> Value {
    let mut environment = rill::eval::process_environment();
    for (name, value) in overrides {
        environment.insert(name.to_string(), value.to_string());
    }
    rill::run_source_with_environment(source, Vec::new(), None, environment).expect(source)
}

#[test]
fn cli_args_and_input_lines_are_available_to_scripts() {
    let value = rill::run_source_with_input(
        r#"[args, input.lines]"#,
        vec!["first.txt".to_string(), "second.txt".to_string()],
        Some("alpha\nbeta\n".to_string()),
    )
    .expect("run");
    assert_eq!(
        value,
        Value::List(vec![
            Value::List(vec![
                Value::Str("first.txt".to_string()),
                Value::Str("second.txt".to_string()),
            ]),
            Value::List(vec![Value::Str("alpha".to_string()), Value::Str("beta".to_string()),]),
        ])
    );
}

#[test]
fn compilation_recursively_tracks_direct_input_references() {
    let program = rill::compile("print(input.lines)").expect("compile");
    assert!(program.requirements.needs_input);

    let program = rill::compile("let value = 1; print(value)").expect("compile");
    assert!(!program.requirements.needs_input);
}

#[test]
fn json_module_parses_and_stringifies_values() {
    let value = run(r#"use json
"[1, 2]".parse_json()"#);
    assert_eq!(value, Value::List(vec![Value::Int(1), Value::Int(2)]));
    let value = run(r#"use json
({name: "rill"}).to_json()"#);
    assert_eq!(value, Value::Str("{\"name\":\"rill\"}".to_string()));
}

#[test]
fn concrete_stdlib_modules_are_callable() {
    let value = run(r#"use path
use regex
use csv
use process
[path("/tmp").join("rill"), "rill".is_match("ri.*"), "a,b\nc,d".parse_csv(), ["printf", "ok"].run().out]"#);
    let Value::List(values) = value else {
        panic!("expected list");
    };
    assert_eq!(values[0], Value::Path(std::path::PathBuf::from("/tmp/rill")));
    assert_eq!(values[1], Value::Bool(true));
    assert_eq!(values[3], Value::Str("ok".to_string()));
}

#[test]
fn local_modules_resolve_relative_to_the_importer_and_typecheck_exports() {
    let scratch = Scratch::new();
    std::fs::create_dir(scratch.0.join("nested")).unwrap();
    std::fs::write(scratch.0.join("shared.ril"), "fn increment(value: Int) -> Int { value + 1 }\n")
        .unwrap();
    std::fs::write(
        scratch.0.join("nested/helper.ril"),
        "use ../shared.ril\nfn answer(value: Int) -> Int { increment(value) }\n",
    )
    .unwrap();
    let root = scratch.0.join("root.ril");
    let source = "use ./nested/helper.ril\nanswer(41)\n";
    std::fs::write(&root, source).unwrap();

    let mut session = rill::CompilationSession::new();
    let program = session
        .compile(rill::SourceUnit {
            name: root.display().to_string(),
            source: source.to_string(),
            base_dir: scratch.0.clone(),
        })
        .expect("compile module graph");
    assert_eq!(rill::run_compiled(&program, vec![], None).unwrap(), Value::Int(42));
}

#[test]
fn repeated_module_imports_execute_once_and_cycles_fail_during_compilation() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("once.ril"), "set_env(\"RILL_MODULE_ONCE\", \"x\")\n").unwrap();
    let root = scratch.0.join("root.ril");
    let source = "use ./once.ril\nuse ./once.ril\nenv.RILL_MODULE_ONCE\n";
    std::fs::write(&root, source).unwrap();
    let mut session = rill::CompilationSession::new();
    let program = session
        .compile(rill::SourceUnit {
            name: root.display().to_string(),
            source: source.to_string(),
            base_dir: scratch.0.clone(),
        })
        .unwrap();
    assert_eq!(rill::run_compiled(&program, vec![], None).unwrap(), Value::Str("x".into()));

    std::fs::write(scratch.0.join("a.ril"), "use ./b.ril\n").unwrap();
    std::fs::write(scratch.0.join("b.ril"), "use ./a.ril\n").unwrap();
    let cycle_source = "use ./a.ril\n";
    let cycle_root = scratch.0.join("cycle_root.ril");
    std::fs::write(&cycle_root, cycle_source).unwrap();
    let error = rill::CompilationSession::new()
        .compile(rill::SourceUnit {
            name: cycle_root.display().to_string(),
            source: cycle_source.to_string(),
            base_dir: scratch.0.clone(),
        })
        .expect_err("cycle should fail");
    assert!(error.to_string().contains("cyclic module import"));
}

#[test]
fn imported_symbol_type_errors_are_reported_at_compile_time() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("typed.ril"), "fn takes(value: Int) -> Int { value }\n").unwrap();
    let source = "use ./typed.ril\ntakes(\"wrong\")\n";
    let mut session = rill::CompilationSession::new();
    let error = session
        .compile(rill::SourceUnit {
            name: "<module-type-test>".to_string(),
            source: source.to_string(),
            base_dir: scratch.0.clone(),
        })
        .expect_err("imported argument type should fail");
    assert!(error.to_string().contains("expected Int"));
}

#[test]
fn data_format_url_and_hash_stdlibs_work() {
    let value = run(r#"use toml
use yaml
use url
use hashes
["count = 2".parse_toml().count, "name: rill".parse_yaml().name, "https://example.com/a?b=c".parse_url().host, "hello".sha256()]"#);
    let Value::List(values) = value else {
        panic!("expected list");
    };
    assert_eq!(values[0], Value::Int(2));
    assert_eq!(values[1], Value::Str("rill".to_string()));
    assert_eq!(values[2], Value::Str("example.com".to_string()));
    assert_eq!(
        values[3],
        Value::Str("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_string())
    );
}

#[test]
fn declarative_arguments_parse_options_positionals_and_defaults() {
    let value = rill::run_source_with_input(
        r#"
args {
    verbose: Bool = false short="v"
    port: Int = 8080 short="p"
host: Str
}
[args.verbose, args.port, args.host]
"#,
        vec!["-v".to_string(), "server1".to_string()],
        None,
    )
    .expect("run");
    assert_eq!(
        value,
        Value::List(vec![Value::Bool(true), Value::Int(8080), Value::Str("server1".to_string())])
    );
}

#[test]
fn declarative_arguments_parse_subcommands_and_descriptions() {
    let value = rill::run_source_with_input(
        r#"
args {
    command {
        start {
            service: Str description="service to start"
        }
    }
}
args.command.start.service
"#,
        vec!["start".to_string(), "api".to_string()],
        None,
    )
    .expect("run");
    assert_eq!(value, Value::Str("api".to_string()));
}

#[test]
fn declarative_subcommand_options_use_the_active_schema() {
    let program = rill::compile(
        r#"
args {
    command {
        start {
            port: Int
            deploy {
                force: Bool = false
            }
        }
    }
}
args.command.start.port
"#,
    )
    .expect("compile");
    let value = rill::run_compiled(
        &program,
        vec!["start".to_string(), "--port".to_string(), "42".to_string()],
        None,
    )
    .expect("run");
    assert_eq!(value, Value::Int(42));
}

#[test]
fn environment_is_available_as_a_map() {
    let value = run(r#"
set_env("RILL_TEST_ENV", "ready")
env.RILL_TEST_ENV
"#);
    assert_eq!(value, Value::Str("ready".to_string()));
}

#[test]
fn get_reads_map_values_and_returns_null_for_missing_values() {
    let value = run(r#"let values = {present: "yes"}
[values.get("present"), values.get("missing"), null.get("missing")]"#);
    assert_eq!(value, Value::List(vec![Value::Str("yes".to_string()), Value::Null, Value::Null,]));
}

#[test]
fn environment_assignment_and_scoped_override_restore_values() {
    let host_value = std::env::var_os("RILL_SCOPED_TEST_ENV");
    let value = run(r#"
env.RILL_SCOPED_TEST_ENV = "outer"
with env {
    RILL_SCOPED_TEST_ENV = "inner"
} {
    print env.RILL_SCOPED_TEST_ENV
}
env.RILL_SCOPED_TEST_ENV
"#);
    assert_eq!(value, Value::Str("outer".to_string()));
    assert_eq!(std::env::var_os("RILL_SCOPED_TEST_ENV"), host_value);
}

#[test]
fn evaluator_environment_is_passed_to_children_without_mutating_host() {
    let host_value = std::env::var_os("RILL_EVALUATOR_LOCAL_TEST");
    let value = run(r#"
set_env("RILL_EVALUATOR_LOCAL_TEST", "local")
let child = ["printenv", "RILL_EVALUATOR_LOCAL_TEST"].run().out.trim()
let scoped = run printenv "RILL_EVALUATOR_LOCAL_TEST" {
    env: { RILL_EVALUATOR_LOCAL_TEST: "override" }
}
[env.RILL_EVALUATOR_LOCAL_TEST, child, scoped.out.trim()]
"#);
    assert_eq!(
        value,
        Value::List(vec![
            Value::Str("local".into()),
            Value::Str("local".into()),
            Value::Str("override".into()),
        ])
    );
    assert_eq!(std::env::var_os("RILL_EVALUATOR_LOCAL_TEST"), host_value);
}

#[test]
fn path_reads_absolute_path() {
    let path = std::env::temp_dir().join("rill-path-test.txt");
    std::fs::write(&path, "hello").expect("write test file");
    let value = run(&format!(r#"path("{}").read_text()"#, path.display()));
    assert_eq!(value, Value::Result(Ok(Box::new(Value::Str("hello".to_string())))));
    let _ = std::fs::remove_file(path);
}

// ── Path expansion: tilde, $VAR, ${VAR}, ${VAR:-default} ──────────────────────

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let stamp =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("rill-interp-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn tilde_prefix_in_path_expands_to_home() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("tilde.txt"), "tilde").unwrap();

    let value = run_with_env(
        r#"path("~/tilde.txt").read_text()"#,
        &[("HOME", scratch.0.to_str().unwrap())],
    );

    assert_eq!(value, Value::Result(Ok(Box::new(Value::Str("tilde".to_string())))));
}

#[test]
fn dollar_env_var_in_path_expands() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("env.txt"), "env").unwrap();

    let value = run_with_env(
        r#"path("$RILL_TEST_PATHDIR/env.txt").read_text()"#,
        &[("RILL_TEST_PATHDIR", scratch.0.to_str().unwrap())],
    );

    assert_eq!(value, Value::Result(Ok(Box::new(Value::Str("env".to_string())))));
}

#[test]
fn braced_dollar_var_in_path_expands() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("braced.txt"), "braced").unwrap();
    let value = run_with_env(
        r#"path("${RILL_TEST_BDIR}/braced.txt").read_text()"#,
        &[("RILL_TEST_BDIR", scratch.0.to_str().unwrap())],
    );

    assert_eq!(value, Value::Result(Ok(Box::new(Value::Str("braced".to_string())))));
}

#[test]
fn braced_dollar_with_default_uses_default_when_unset() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("dflt.txt"), "default").unwrap();
    let dir = scratch.0.to_str().unwrap().to_string();

    // The variable is not set in the host environment.
    let value = run(&format!(r#"path("${{RILL_NONEXISTENT_9f3a:-{dir}}}/dflt.txt").read_text()"#));
    assert_eq!(value, Value::Result(Ok(Box::new(Value::Str("default".to_string())))));
}

#[test]
fn braced_dollar_with_default_uses_var_when_set() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("set.txt"), "set").unwrap();
    let dir = scratch.0.to_str().unwrap().to_string();

    let value = run_with_env(
        r#"path("${RILL_TEST_SET_VAR:-/wrong}/set.txt").read_text()"#,
        &[("RILL_TEST_SET_VAR", &dir)],
    );

    assert_eq!(value, Value::Result(Ok(Box::new(Value::Str("set".to_string())))));
}

#[test]
fn yaml_read_expands_tilde_in_path() {
    let scratch = Scratch::new();
    let yaml_path = scratch.0.join("data.yml");
    std::fs::write(&yaml_path, "key: value\n").unwrap();

    let value =
        run_with_env(r#"yaml.read("~/data.yml")"#, &[("HOME", scratch.0.to_str().unwrap())]);

    let Value::Result(Ok(map_val)) = value else {
        panic!("expected Ok result, got {value:?}");
    };
    let Value::Map(map) = *map_val else {
        panic!("expected Map");
    };
    assert_eq!(map.get("key"), Some(&Value::Str("value".to_string())));
}

#[test]
fn path_join_expands_child() {
    // child starts with a Rill variable
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("joined.txt"), "joined").unwrap();
    let dir = scratch.0.to_str().unwrap().to_string();

    let value = run_with_env(
        r#"use path
path("/").join("$RILL_TEST_JOINDIR/joined.txt").read_text()"#,
        &[("RILL_TEST_JOINDIR", &dir)],
    );

    assert_eq!(value, Value::Result(Ok(Box::new(Value::Str("joined".to_string())))));
}

#[test]
fn dollar_var_in_path_is_rill_binding_before_env() {
    // A Rill let binding shadows the process env for path resolution.
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("binding.txt"), "binding").unwrap();
    let dir = scratch.0.to_str().unwrap();

    // Process env points somewhere wrong.
    let value = run_with_env(
        &format!(
            r#"let RILL_SHADOW_VAR = "{dir}"
path("$RILL_SHADOW_VAR/binding.txt").read_text()"#
        ),
        &[("RILL_SHADOW_VAR", "/dev/null")],
    );

    assert_eq!(value, Value::Result(Ok(Box::new(Value::Str("binding".to_string())))));
}

#[test]
fn string_interpolation_supports_braced_var() {
    let value = run(r#"let x = "world"
"hello ${x}""#);
    assert_eq!(value, Value::Str("hello world".to_string()));
}

#[test]
fn string_interpolation_preserves_option_display() {
    let value = run(r#"let i = none
"interpolated ${i}""#);
    assert_eq!(value, Value::Str("interpolated none".to_string()));

    let value = run(r#"let i = some("value")
"interpolated ${i}""#);
    assert_eq!(value, Value::Str("interpolated some(value)".to_string()));
}

#[test]
fn string_interpolation_supports_braced_var_with_default() {
    let value = run(r#""${absent_var_xyz:-fallback}""#);
    assert_eq!(value, Value::Str("fallback".to_string()));
}

#[test]
fn command_supports_braced_var_default() {
    // ${VAR:-default} in a command arg; the variable is not set in the host environment.
    let Value::ProcessResult(result) = run(r#"run printf "${RILL_CMD_DFLT:-hello}""#) else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.stdout_text_lossy(), "hello");
}

#[test]
fn command_braced_var_uses_rill_binding_when_set() {
    let Value::ProcessResult(result) = run(r#"let MY_MSG = "from-rill"
run printf "${MY_MSG:-wrong}""#)
    else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.stdout_text_lossy(), "from-rill");
}

#[test]
fn command_word_leading_tilde_expands() {
    // A bare word starting with `~/` should expand to $HOME/...
    let Value::ProcessResult(result) =
        run_with_env(r#"run printf "%s" ~/foo"#, &[("HOME", "/tmp")])
    else {
        panic!("expected ProcessResult");
    };

    assert_eq!(result.stdout_text_lossy(), "/tmp/foo");
}

#[test]
fn run_returns_process_result() {
    let value = run(r#"run printf "hi""#);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.status, 0);
    assert_eq!(result.stdout_text_lossy(), "hi");
}

#[test]
fn explicit_shell_escape_runs_shell_syntax() {
    let value = run(r#"sh "printf 'hello' | tr a-z A-Z""#);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.stdout_text_lossy(), "HELLO");
}

#[test]
fn duration_and_byte_size_literals_have_distinct_values() {
    let value = run(r#"[1.5s, 20MiB, 2h]"#);
    let Value::List(values) = value else {
        panic!("expected list");
    };
    assert_eq!(values[0], Value::Duration(std::time::Duration::from_millis(1500)));
    assert_eq!(values[1], Value::Int(20 * 1024 * 1024));
    assert_eq!(values[2], Value::Duration(std::time::Duration::from_secs(7200)));
}

#[test]
fn result_and_option_values_support_try_propagation() {
    let value = run(r#"
fn successful() -> Result[Int] {
    let value = ok(4)?
    value + 1
}
successful()
"#);
    assert_eq!(value, Value::Result(Ok(Box::new(Value::Int(5)))));

    let value = run(r#"
fn failed() -> Result[Int] {
    let value = err("nope")?
    value
}
failed()
"#);
    assert_eq!(value, Value::Result(Err(Box::new(Value::Str("nope".to_string())))));

    let value = run(r#"
fn optional() -> Option[Int] {
    let value = none()?
    value
}
optional()
"#);
    assert_eq!(value, Value::Option(None));
}

#[test]
fn declared_error_type_coerces_string_errors() {
    let value = run(r#"
fn failed() -> Result[Int, Error] {
    err("bad input")
}
failed()
"#);
    let Value::Result(Err(error)) = value else {
        panic!("expected error result");
    };
    assert!(matches!(*error, Value::Error(_)));
}

#[test]
fn command_var_is_single_argv_argument() {
    let value = run(r#"
let file = "some dumb filename.txt"
run printf $file
"#);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.stdout_text_lossy(), "some dumb filename.txt");
}

#[test]
fn command_parts_concatenate_var_and_suffix() {
    let value = run(r#"
let server = "host"
run printf $server:/srv/app
"#);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.stdout_text_lossy(), "host:/srv/app");
}

#[test]
fn byte_pipe_to_lines() {
    let value = run(r#"run printf "a\nb\n" | .lines()"#);
    assert_eq!(value, Value::List(vec![Value::Str("a".to_string()), Value::Str("b".to_string())]));
}

#[test]
fn chained_byte_pipeline_connects_processes_directly() {
    let value = run(r#"run printf "a\nb\n" | run tr a-z A-Z | .lines()"#);
    assert_eq!(value, Value::List(vec![Value::Str("A".to_string()), Value::Str("B".to_string())]));
}

#[test]
fn chained_byte_pipeline_streams_between_processes() {
    let value = run(r#"run printf "a\nb\n" | run tr a-z A-Z | .lines()"#);
    assert_eq!(value, Value::List(vec![Value::Str("A".to_string()), Value::Str("B".to_string())]));
}

#[test]
fn value_pipe_into_take() {
    let value = run(r#"["a", "b", "c"] |> .take(2)"#);
    assert_eq!(value, Value::List(vec![Value::Str("a".to_string()), Value::Str("b".to_string())]));
}

#[test]
fn closure_can_be_called_directly() {
    let value = run(r#"
let add_suffix = |value| value + "!"
add_suffix("hello")
"#);
    assert_eq!(value, Value::Str("hello!".to_string()));
}

#[test]
fn closure_captures_lexical_values() {
    let value = run(r#"
let suffix = "!"
let add_suffix = |value| value + suffix
let suffix = "?"
add_suffix("hello")
"#);
    assert_eq!(value, Value::Str("hello!".to_string()));
}

#[test]
fn closures_work_as_map_and_filter_callbacks() {
    let value = run(r#"
[1, 2, 3] |> .map(|value| value + 1) |> .filter(|value| value > 2)
"#);
    assert_eq!(value, Value::List(vec![Value::Int(3), Value::Int(4)]));
}

#[test]
fn implicit_callback_arguments_work_for_fields_and_values() {
    let value = run(r#"
let records = [{name: "a", score: 4}, {name: "b", score: 8}]
records |> .filter(.score > 5) |> .map(.name)
"#);
    assert_eq!(value, Value::List(vec![Value::Str("b".to_string())]));
}

#[test]
fn multi_parameter_and_block_closures_work() {
    let value = run(r#"
let combine = |left, right| {
    let joined = left + right
    joined + "!"
}
combine("a", "b")
"#);
    assert_eq!(value, Value::Str("ab!".to_string()));
}

#[test]
fn for_loop_and_interpolation() {
    let value = run(r#"
let hosts = ["a", "b"]
for host in hosts {
    print "$host"
}
"#);
    assert_eq!(value, Value::List(vec![]));
}

#[test]
fn for_expr_emits_mapped_values() {
    let value = run(r#"
let x = for i in [1, 2, 3] { emit i * 2 }
x
"#);
    assert_eq!(value, Value::List(vec![Value::Int(2), Value::Int(4), Value::Int(6)]));
}

#[test]
fn for_expr_no_emit_returns_empty_list() {
    let value = run(r#"
let x = for i in [1, 2, 3] { print i }
x
"#);
    assert_eq!(value, Value::List(vec![]));
}

#[test]
fn for_expr_emit_inside_if() {
    let value = run(r#"
let x = for i in [1, 2, 3] {
    if i > 1 { emit i }
}
x
"#);
    assert_eq!(value, Value::List(vec![Value::Int(2), Value::Int(3)]));
}

#[test]
fn for_stmt_still_works_as_statement() {
    // for used as statement with no assignment still works for side effects
    let value = run(r#"
for i in [1, 2] { print i }
"#);
    // for as statement evaluates to the list from the for expression; since it's
    // the last expression in the program, that's what run() returns
    assert_eq!(value, Value::List(vec![]));
}

#[test]
fn for_expr_break_stops_collection() {
    let value = run(r#"
let x = for i in [1, 2, 3, 4, 5] {
    if i > 3 { break }
    emit i
}
x
"#);
    assert_eq!(value, Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
}

#[test]
fn range_literal_produces_list() {
    let value = run("0..5");
    assert_eq!(
        value,
        Value::List(vec![
            Value::Int(0),
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
        ])
    );
}

#[test]
fn for_expr_over_range() {
    let value = run(r#"
let x = for i in 0..4 { emit i }
x
"#);
    assert_eq!(
        value,
        Value::List(vec![Value::Int(0), Value::Int(1), Value::Int(2), Value::Int(3),])
    );
}

#[test]
fn parallel_for_runs_all_iterations() {
    let value = run(r#"
let x = parallel for i in [1, 2, 3] { emit i * 2 }
x
"#);
    // Results collected in spawn order (deterministic).
    assert_eq!(value, Value::List(vec![Value::Int(2), Value::Int(4), Value::Int(6)]));
}

#[test]
fn bounded_parallel_and_race_for_accept_worker_counts() {
    let value = run(r#"
let mapped = parallel(1) for i in [1, 2, 3] { emit i * 2 }
let winner = race(1) for i in [7] { emit i }
(mapped, winner)
"#);
    assert_eq!(
        value,
        Value::Tuple(vec![
            Value::List(vec![Value::Int(2), Value::Int(4), Value::Int(6)]),
            Value::Int(7),
        ])
    );
}

#[test]
fn bounded_parallel_for_rejects_non_positive_worker_counts() {
    for source in ["parallel(0) for i in [1] { emit i }", "parallel(-1) for i in [1] { emit i }"] {
        assert!(rill::run_source(source, vec![]).is_err(), "{source}");
    }
}

#[test]
fn parallel_for_empty_list() {
    let value = run("parallel for i in [] { emit i }");
    assert_eq!(value, Value::List(vec![]));
}

#[test]
fn parallel_for_over_range() {
    let value = run(r#"
let x = parallel for i in 0..4 { emit i }
x
"#);
    assert_eq!(
        value,
        Value::List(vec![Value::Int(0), Value::Int(1), Value::Int(2), Value::Int(3),])
    );
}

#[test]
fn parallel_for_no_emit_returns_empty_list() {
    let value = run(r#"
let x = parallel for i in [1, 2, 3] { print i }
x
"#);
    assert_eq!(value, Value::List(vec![]));
}

#[test]
fn race_for_returns_first_emit_single_item() {
    // With a single item the result is that item (not a list).
    let value = run("race for i in [42] { emit i }");
    assert_eq!(value, Value::Int(42));
}

#[test]
fn race_for_empty_list_returns_null() {
    let value = run("race for i in [] { emit i }");
    assert_eq!(value, Value::Null);
}

#[test]
fn race_for_no_emit_returns_null() {
    let value = run(r#"race for i in [1, 2, 3] { print i }"#);
    assert_eq!(value, Value::Null);
}

#[test]
fn for_as_expression_in_let() {
    let value = run(r#"
let doubled = for i in 0..5 { emit i * 2 }
doubled
"#);
    assert_eq!(
        value,
        Value::List(vec![
            Value::Int(0),
            Value::Int(2),
            Value::Int(4),
            Value::Int(6),
            Value::Int(8),
        ])
    );
}

#[test]
fn boolean_and_operator() {
    assert_eq!(run("true && true"), Value::Bool(true));
    assert_eq!(run("true && false"), Value::Bool(false));
    assert_eq!(run("false && true"), Value::Bool(false));
    assert_eq!(run("false && false"), Value::Bool(false));
}

#[test]
fn boolean_or_operator() {
    assert_eq!(run("true || true"), Value::Bool(true));
    assert_eq!(run("true || false"), Value::Bool(true));
    assert_eq!(run("false || true"), Value::Bool(true));
    assert_eq!(run("false || false"), Value::Bool(false));
}

#[test]
fn boolean_not_operator() {
    assert_eq!(run("!true"), Value::Bool(false));
    assert_eq!(run("!false"), Value::Bool(true));
    assert_eq!(run("not true"), Value::Bool(false));
    assert_eq!(run("not false"), Value::Bool(true));
}

#[test]
fn boolean_keyword_aliases() {
    assert_eq!(run("true and false"), Value::Bool(false));
    assert_eq!(run("false or true"), Value::Bool(true));
    assert_eq!(run("not (true and false)"), Value::Bool(true));
}

#[test]
fn boolean_short_circuit_and() {
    // Right side is not evaluated when left is false.
    // "1/0" would panic at runtime if evaluated.
    assert_eq!(run("false && (1 == 0)"), Value::Bool(false));
}

#[test]
fn boolean_short_circuit_or() {
    assert_eq!(run("true || (1 == 0)"), Value::Bool(true));
}

#[test]
fn boolean_operators_in_conditions() {
    assert_eq!(
        run(r#"
let x = 5
if x > 3 && x < 10 { "yes" } else { "no" }
"#),
        Value::Str("yes".to_string())
    );
}

#[test]
fn mutation_plain_assign() {
    assert_eq!(run("var x = 1\nx = 2\nx"), Value::Int(2));
}

#[test]
fn mutation_add_assign_int() {
    assert_eq!(run("var x = 0\nx += 5\nx"), Value::Int(5));
}

#[test]
fn mutation_add_assign_string() {
    assert_eq!(
        run(r#"var x = "a"
x += "b"
x"#),
        Value::Str("ab".to_string())
    );
}

#[test]
fn mutation_sub_assign() {
    assert_eq!(run("var x = 10\nx -= 3\nx"), Value::Int(7));
}

#[test]
fn mutation_mul_assign() {
    assert_eq!(run("var x = 4\nx *= 3\nx"), Value::Int(12));
}

#[test]
fn mutation_div_assign() {
    assert_eq!(run("var x = 12\nx /= 4\nx"), Value::Int(3));
}

#[test]
fn mutation_undeclared_variable_errors() {
    let result = rill::run_source("y = 5", Vec::new());
    assert!(result.is_err());
}

#[test]
fn mutation_in_loop() {
    assert_eq!(
        run(r#"
var sum = 0
for i in [1, 2, 3, 4] {
    sum += i
}
sum
"#),
        Value::Int(10)
    );
}

#[test]
fn closure_single_expr_still_works() {
    assert_eq!(run("let f = |x| x * 2\nf(4)"), Value::Int(8));
}

#[test]
fn closure_multi_stmt_block() {
    assert_eq!(
        run(r#"
let f = |x| {
    let y = x * 2
    y + 1
}
f(3)
"#),
        Value::Int(7)
    );
}

#[test]
fn closure_explicit_return() {
    assert_eq!(
        run(r#"
let g = |x| {
    if x > 0 { return x }
    0
}
g(5)
"#),
        Value::Int(5)
    );
    assert_eq!(
        run(r#"
let g = |x| {
    if x > 0 { return x }
    0
}
g(-1)
"#),
        Value::Int(0)
    );
}

#[test]
fn closure_mutation_in_body() {
    assert_eq!(
        run(r#"
let f = |n| {
    let acc = 0
    let i = 0
    let acc = n * 3
    acc
}
f(4)
"#),
        Value::Int(12)
    );
}

#[test]
fn destructure_list_basic() {
    assert_eq!(run("let [a, b] = [1, 2]\na"), Value::Int(1));
    assert_eq!(run("let [a, b] = [1, 2]\nb"), Value::Int(2));
}

#[test]
fn destructure_list_wildcard() {
    assert_eq!(run("let [x, _, z] = [1, 2, 3]\nz"), Value::Int(3));
}

#[test]
fn destructure_list_short() {
    assert_eq!(run("let [a, b] = [1]\nb"), Value::Null);
}

#[test]
fn destructure_map_basic() {
    assert_eq!(
        run(r#"let { name } = { name: "alice" }
name"#),
        Value::Str("alice".to_string())
    );
}

#[test]
fn destructure_map_alias() {
    assert_eq!(
        run(r#"let { x: renamed } = { x: 42 }
renamed"#),
        Value::Int(42)
    );
}

#[test]
fn destructure_map_missing_key() {
    assert_eq!(
        run(r#"let { missing } = { other: 1 }
missing"#),
        Value::Null
    );
}

#[test]
fn destructure_list_wrong_type_errors() {
    assert!(rill::run_source("let [a] = 42", Vec::new()).is_err());
}

#[test]
fn destructure_map_wrong_type_errors() {
    assert!(rill::run_source("let { a } = [1, 2]", Vec::new()).is_err());
}

#[test]
fn parallel_map_list() {
    // Order is preserved (handles joined in spawn order)
    assert_eq!(
        run("[1, 2, 3].parallel_map(|x| x * 10)"),
        Value::List(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
    );
}

#[test]
fn parallel_map_on_map() {
    // Maps iterate in insertion order: keys a, b
    let result = run(r#"{ a: 10, b: 20 }.parallel_map(|k, v| v * 2)"#);
    assert_eq!(result, Value::List(vec![Value::Int(20), Value::Int(40)]));
}

#[test]
fn flatten_nested_lists() {
    assert_eq!(
        run("[[1, 2], [3, 4]].flatten()"),
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)])
    );
}

#[test]
fn flatten_mixed_keeps_non_lists() {
    assert_eq!(
        run("[1, [2, 3], 4].flatten()"),
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)])
    );
}

#[test]
fn zip_two_lists() {
    assert_eq!(
        run("[1, 2, 3].zip([\"a\", \"b\", \"c\"])"),
        Value::List(vec![
            Value::Tuple(vec![Value::Int(1), Value::Str("a".into())]),
            Value::Tuple(vec![Value::Int(2), Value::Str("b".into())]),
            Value::Tuple(vec![Value::Int(3), Value::Str("c".into())]),
        ])
    );
}

#[test]
fn zip_stops_at_shorter() {
    assert_eq!(
        run("[1, 2, 3].zip([\"a\", \"b\"])"),
        Value::List(vec![
            Value::Tuple(vec![Value::Int(1), Value::Str("a".into())]),
            Value::Tuple(vec![Value::Int(2), Value::Str("b".into())]),
        ])
    );
}

#[test]
fn enumerate_list() {
    assert_eq!(
        run("[\"a\", \"b\", \"c\"].enumerate()"),
        Value::List(vec![
            Value::Tuple(vec![Value::Int(0), Value::Str("a".into())]),
            Value::Tuple(vec![Value::Int(1), Value::Str("b".into())]),
            Value::Tuple(vec![Value::Int(2), Value::Str("c".into())]),
        ])
    );
}

#[test]
fn destructuring_for_list_items() {
    assert_eq!(
        run(r#"for let (i, value) in ["a", "b"].enumerate() { emit (i, value) }"#),
        Value::List(vec![
            Value::Tuple(vec![Value::Int(0), Value::Str("a".into())]),
            Value::Tuple(vec![Value::Int(1), Value::Str("b".into())]),
        ])
    );
}

#[test]
fn destructuring_if_binds_then_branch_and_uses_else_on_mismatch() {
    assert_eq!(
        run(r#"if let [name, count] = ["alice", 3] { (name, count) } else { "no" }"#),
        Value::Tuple(vec![Value::Str("alice".into()), Value::Int(3)])
    );
    assert_eq!(
        run(r#"if let [name, count] = { name: "alice" } { name } else { "no" }"#),
        Value::Str("no".into())
    );
}

#[test]
fn group_by_key() {
    let result = run("[1, 2, 3, 4].group_by(|n| { if n > 2 { \"big\" } else { \"small\" } })");
    let Value::Map(map) = result else { panic!("expected Map") };
    assert_eq!(map.get("small"), Some(&Value::List(vec![Value::Int(1), Value::Int(2)])));
    assert_eq!(map.get("big"), Some(&Value::List(vec![Value::Int(3), Value::Int(4)])));
}

#[test]
fn parallel_filter_list() {
    assert_eq!(
        run("[1, 2, 3, 4, 5].parallel_filter(|x| x > 2)"),
        Value::List(vec![Value::Int(3), Value::Int(4), Value::Int(5)])
    );
}

#[test]
fn tuples_are_fixed_heterogeneous_values() {
    assert_eq!(
        run(r#"(1, "two", true)"#),
        Value::Tuple(vec![Value::Int(1), Value::Str("two".into()), Value::Bool(true),])
    );
    assert_eq!(run("(1, 2) == (1, 2)"), Value::Bool(true));
    assert_eq!(run("(1, 2) == [1, 2]"), Value::Bool(false));
    assert_eq!(run(r#"type((1, "two"))"#), Value::Str("Tuple".into()));
}

#[test]
fn tuples_support_typed_indexing_and_exact_destructuring() {
    assert_eq!(
        run(r#"
let quad: Tuple[Int, Str, Bool, Float] = (7, "skip", true, 2.5)
let (number, _, flag, decimal) = quad
[quad[0], quad[-1], number, flag, decimal]
"#),
        Value::List(vec![
            Value::Int(7),
            Value::Float(2.5),
            Value::Int(7),
            Value::Bool(true),
            Value::Float(2.5),
        ])
    );

    let program = rill::parser::parse("let (a, b, c) = (1, 2)").unwrap();
    let error = rill::eval::Evaluator::new(vec![])
        .eval_program(&program)
        .expect_err("unchecked tuple destructuring should enforce arity");
    assert!(error.message.contains("needs 2 bindings, got 3"));
}

#[test]
fn mutable_tuples_support_position_preserving_assignment() {
    assert_eq!(
        run(r#"
var values: Tuple[Int, Str] = (1, "a")
values[0] += 2
values[-1] = "b"
values
"#),
        Value::Tuple(vec![Value::Int(3), Value::Str("b".into())])
    );
}

#[test]
fn tuple_serialization_uses_arrays_without_changing_parsed_array_types() {
    assert_eq!(run(r#"(1, "two").to_json()"#), Value::Str(r#"[1,"two"]"#.into()));
    assert_eq!(
        run(r#"(1, "two").to_yaml().parse_yaml()"#),
        Value::List(vec![Value::Int(1), Value::Str("two".into())])
    );
    assert_eq!(
        run(r#"({pair: (1, 2)}).to_toml().parse_toml().pair"#),
        Value::List(vec![Value::Int(1), Value::Int(2)])
    );
}

#[test]
fn map_literals_preserve_insertion_order() {
    assert_eq!(
        run(r#"{zeta: 1, alpha: 2, mid: 3}.keys()"#),
        Value::List(vec![
            Value::Str("zeta".into()),
            Value::Str("alpha".into()),
            Value::Str("mid".into())
        ])
    );
    assert_eq!(run(r#"Str({zeta: 1, alpha: 2})"#), Value::Str("{zeta: 1, alpha: 2}".into()));
}

#[test]
fn json_and_yaml_round_trips_preserve_key_order() {
    let source = r#"
        let m = {zeta: 1, alpha: 2}
        [
            m.to_json(),
            m.to_json().parse_json().to_json(),
            m.to_yaml().parse_yaml().keys().join(","),
            m.to_toml().parse_toml().keys().join(","),
        ]
    "#;
    assert_eq!(
        run(source),
        Value::List(vec![
            Value::Str(r#"{"zeta":1,"alpha":2}"#.into()),
            Value::Str(r#"{"zeta":1,"alpha":2}"#.into()),
            Value::Str("zeta,alpha".into()),
            Value::Str("zeta,alpha".into()),
        ])
    );
}

#[test]
fn map_equality_ignores_order_and_so_does_difference() {
    assert_eq!(run(r#"{a: 1, b: 2} == {b: 2, a: 1}"#), Value::Bool(true));
    assert_eq!(run(r#"[{a: 1, b: 2}].difference([{b: 2, a: 1}])"#), Value::List(vec![]));
    assert_eq!(
        run(r#"[3, 1, 3, 2].group_by(|x| Str(x)).keys()"#),
        Value::List(vec![Value::Str("3".into()), Value::Str("1".into()), Value::Str("2".into())])
    );
}

// ── Scoping model: let by value, var by shared cell, lexical hoisted fns ──────

#[test]
fn closures_share_mutable_var_bindings() {
    assert_eq!(run("var n = 0\n[1, 2, 3].map(|x| { n += x })\nn"), Value::Int(6));
}

#[test]
fn named_functions_share_mutable_var_bindings() {
    assert_eq!(run("var n = 0\nfn bump() { n += 1 }\nbump()\nbump()\nn"), Value::Int(2));
}

#[test]
fn var_shadowing_after_capture_creates_new_cell() {
    assert_eq!(run("var v = 1\nlet f = |x| v + x\nvar v = 2\nf(0)"), Value::Int(1));
}

#[test]
fn named_functions_are_lexically_scoped_not_dynamic() {
    let error = rill::run_source("fn f() { x }\nfn g() { let x = 1\nf() }\ng()", vec![])
        .expect_err("dynamic lookup must fail");
    assert!(error.to_string().contains("unknown identifier 'x'"), "{error}");
}

#[test]
fn functions_can_call_functions_declared_later_and_recurse() {
    let source = r#"
        fn is_even(n) { if n == 0 { true } else { is_odd(n - 1) } }
        fn is_odd(n) { if n == 0 { false } else { is_even(n - 1) } }
        [is_even(10), is_odd(7), is_even(3)]
    "#;
    assert_eq!(
        run(source),
        Value::List(vec![Value::Bool(true), Value::Bool(true), Value::Bool(false)])
    );
}

#[test]
fn function_used_before_definition_statement_errors() {
    let error = rill::run_source("fn a() { b() }\na()\nfn b() { 1 }", vec![])
        .expect_err("calling a hoisted but undefined function must fail");
    assert!(error.to_string().contains("used before its definition"), "{error}");
}

#[test]
fn with_env_applies_to_called_functions_and_is_restored() {
    let source = r#"
        fn show() { env.get("RILL_WITH_ENV_TEST") }
        fn with_env_probe() {
            var seen = null
            with env { RILL_WITH_ENV_TEST = "x" } { seen = show() }
            seen
        }
        let inside = with_env_probe()
        [inside, show()]
    "#;
    assert_eq!(run(source), Value::List(vec![Value::Str("x".into()), Value::Null]));
}

#[test]
fn set_env_is_evaluator_local_and_visible_to_children() {
    let source = r#"
        set_env("RILL_EVAL_LOCAL", "42")
        let result = run printenv RILL_EVAL_LOCAL
        result.out.trim()
    "#;
    assert_eq!(run(source), Value::Str("42".into()));
    assert!(std::env::var_os("RILL_EVAL_LOCAL").is_none(), "host environment must not change");
}

// ── Unannotated functions that propagate with `?` or `run!` ──────────────────

#[test]
fn unannotated_propagating_functions_return_consistent_wrappers() {
    let source = r#"
        fn shout(cmd: Str) {
            let out = run! $*cmd
            out.trim().upper()
        }
        fn first_nonempty(xs) {
            let x = xs.find(. != "")?
            x
        }
        let f = |x| { let v = ok(x)?
            v + 1 }
        [shout("printf hi"), shout("false").is_err(), first_nonempty(["", "a"]), first_nonempty([""]), f(1)]
    "#;
    assert_eq!(
        run(source),
        Value::List(vec![
            Value::Result(Ok(Box::new(Value::Str("HI".into())))),
            Value::Bool(true),
            Value::Option(Some(Box::new(Value::Str("a".into())))),
            Value::Option(None),
            Value::Result(Ok(Box::new(Value::Int(2)))),
        ])
    );
}
