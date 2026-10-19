use rill::types::{ErrorKind, Value};
use std::path::PathBuf;

fn run(source: &str) -> Value {
    rill::run_source(source, vec![]).expect(source)
}

fn check(source: &str) -> Result<(), rill::diagnostic::RillError> {
    rill::typeck::check(&rill::parser::parse(source).expect(source))
}

fn eval_without_check(source: &str) -> Result<Value, rill::diagnostic::RillError> {
    let program = rill::parser::parse(source).expect(source);
    rill::eval::Evaluator::new(vec![]).eval_program(&program)
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("rill-methods-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn list_searches_return_options_and_short_circuit() {
    assert_eq!(run("[1, 2, 3].find(. > 1)"), run("some(2)"));
    assert_eq!(run("[1, 2].find(. > 2)"), run("none"));
    assert_eq!(run("[null].find(|x| true)"), run("some(null)"));
    assert_eq!(run("[1, 2].any(. > 1)"), Value::Bool(true));
    assert_eq!(run("[1, 2].any(. > 2)"), Value::Bool(false));
    for method in ["find", "any"] {
        let empty = run(&format!("[].{method}(|x| {{ die(\"must not run\") }})"));
        assert_eq!(empty, if method == "find" { run("none") } else { Value::Bool(false) });
        let found = run(&format!(
            "[1, 2].{method}(|x| {{ if x == 2 {{ die(\"must short circuit\") }}; true }})"
        ));
        assert_eq!(found, if method == "find" { run("some(1)") } else { Value::Bool(true) });
        assert!(rill::run_source(&format!("[1].{method}(|x| {{ die(\"predicate failed\") }})"), vec![])
            .unwrap_err().message.contains("predicate failed"));
        for source in [format!("[1].{method}()"), format!("[1].{method}(1)"),
            format!("[1].{method}(|a, b| true)"), format!("null.{method}(|x| true)")] {
            assert!(check(&source).is_err(), "{source}");
        }
    }
    assert_eq!(run(r#"["disk: root"].find(.starts_with("disk:")).is_some()"#), Value::Bool(true));
    assert_eq!(run("fn positive(n: Int) -> Bool { n > 0 }; [0, 1] |> .any(positive)"), Value::Bool(true));
    assert_eq!(run("[1] |> .find(. > 0) |> .unwrap_or(0)"), Value::Int(1));
    assert_eq!(run("match [1].find(. > 0) { some(x) => x, none => 0 }"), Value::Int(1));
    assert_eq!(run("fn search() -> Option[Int] { let x = [1].find(. > 0)?; some(x) }; search()"), run("some(1)"));
    check("let x: Option[Int] = [1].find(. > 0)").unwrap();
    check("let x: Bool = [1].any(. > 0)").unwrap();
    assert!(check("let x: Option[Str] = [1].find(. > 0)").is_err());
    assert!(check("let x: Int = [1].find(. > 0)").is_err());
    assert!(check("let x: Int = [1].any(. > 0)").is_err());
}

#[test]
fn collections_chain_and_bind_implicit_receivers_to_each_item() {
    assert_eq!(
        run(r#"
let alloc = "maas_ips:\n  host: 10.0.0.1".parse_yaml()
[alloc.get("maas_ips").get("host"), alloc.get("absent").get("host")]
"#),
        Value::List(vec![Value::Str("10.0.0.1".into()), Value::Null])
    );
    assert_eq!(
        run(r#"
[" a ", " b ", " c "] |> .map(.trim()).take(2).reverse()
"#),
        Value::List(vec![Value::Str("b".into()), Value::Str("a".into())])
    );
    assert_eq!(
        run(r#"
({a: 2, b: 4}).map(|key, value| value + 1).filter(. > 3)
"#),
        Value::List(vec![Value::Int(5)])
    );
    check(r#"let values: List[Str] = [" a "].map(.trim())"#).unwrap();
    check(r#"let values: List[Int] = [1].map(|x| 2).reverse().take(1)"#).unwrap();
    assert!(check(r#"[1].map(.trim())"#).is_err());
}

#[test]
fn list_difference_preserves_order_duplicates_and_nested_values() {
    let value = run(r#"[
        [1, [2]], [3], [1, [2]], 4
    ].difference([[1, [2]], 9])"#);
    assert_eq!(
        value,
        Value::List(vec![Value::List(vec![Value::Int(3)]), Value::Int(4)])
    );
}

#[test]
fn list_symmetric_difference_matches_equality_for_mixed_values() {
    let value = run(r#"[1, 2, 2, 3].symmetric_difference([2, 4, 4])"#);
    assert_eq!(
        value,
        Value::List(vec![Value::Int(1), Value::Int(3), Value::Int(4), Value::Int(4)])
    );
}

#[test]
fn map_fields_and_method_names_can_coexist() {
    assert_eq!(run(r#"({custom: |x| x + 1}) |> .custom(2)"#), Value::Int(3));
    assert_eq!(
        run(r#"
let obj = {get: "data", trim: "field", custom: |x| x + 1}
[obj.get("get"), obj.trim, obj.custom(2)]
"#),
        Value::List(vec![
            Value::Str("data".into()),
            Value::Str("field".into()),
            Value::Int(3)
        ])
    );
    assert_eq!(
        run(r#"
let json = {parse: |text| text}
let path = path("/tmp")
[json.parse("ok"), path.join("child")]
"#),
        Value::List(vec![
            Value::Str("ok".into()),
            Value::Path(PathBuf::from("/tmp/child"))
        ])
    );
    assert_eq!(
        run(r#"({get: |x| x}).get("get")("ok")"#),
        Value::Str("ok".into())
    );
}

#[test]
fn serialization_regex_and_duration_methods_work() {
    assert_eq!(
        run(r#"let text = " hi "; "{text.trim()}""#),
        Value::Str("hi".into())
    );
    assert_eq!(run("1.to_json()"), Value::Str("1".into()));
    assert_eq!(run("0s.sleep()"), Value::Null);
    assert_eq!(
        run(r#"
let data = {name: "rill", count: 2}
[
    data.to_json().parse_json().name,
    data.to_json_pretty().parse_json().count,
    data.to_yaml().parse_yaml().name,
    data.to_toml().parse_toml().count,
    "a,b\nc,d".parse_csv(),
    "https://example.com/a".parse_url().host,
    /ri.*/.is_match("rill"),
    "rill".is_match(/ri.*/),
    "rill".replace("l", "L", all=true),
    (0s).sleep(),
]
"#),
        Value::List(vec![
            Value::Str("rill".into()),
            Value::Int(2),
            Value::Str("rill".into()),
            Value::Int(2),
            Value::List(vec![
                Value::List(vec![Value::Str("a".into()), Value::Str("b".into())]),
                Value::List(vec![Value::Str("c".into()), Value::Str("d".into())])
            ]),
            Value::Str("example.com".into()),
            Value::Bool(true),
            Value::Bool(true),
            Value::Str("riLL".into()),
            Value::Null,
        ])
    );
    assert_eq!(
        run(r#""hello".sha256()"#),
        Value::Str("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into())
    );
}

#[test]
fn receivers_arguments_and_callback_factories_are_evaluated_once_in_order() {
    assert_eq!(
        run(r#"
set_env("RILL_METHOD_ORDER", "")
fn receiver() {
    set_env("RILL_METHOD_ORDER", env.RILL_METHOD_ORDER + "r")
    "abc"
}
fn argument(value) {
    set_env("RILL_METHOD_ORDER", env.RILL_METHOD_ORDER + value)
    value
}
receiver().replace(argument("a"), argument("b"))
env.RILL_METHOD_ORDER
"#),
        Value::Str("rab".into())
    );
    assert_eq!(
        run(r#"
set_env("RILL_METHOD_FACTORY", "")
fn factory() {
    set_env("RILL_METHOD_FACTORY", env.RILL_METHOD_FACTORY + "x")
    return |value| value + 1
}
[1, 2, 3].map(factory())
env.RILL_METHOD_FACTORY
"#),
        Value::Str("x".into())
    );
    // A string returned by a process must not be interpolated a second time.
    assert_eq!(
        run(r#"["printf", "%b%s", "\\044", "missing"].run().out.trim()"#),
        Value::Str("$missing".into())
    );
}

#[test]
fn failure_propagates_before_later_method_arguments() {
    assert_eq!(
        run(r#"
fn later() -> Str { die("must not execute"); "" }
fn fail() -> Result[Str] {
    "abc".replace(err("stop")?, later())
}
fail()
"#),
        Value::Result(Err(Box::new(Value::Str("stop".into()))))
    );
    let mut evaluator = rill::eval::Evaluator::new(vec![]);
    let program =
        rill::parser::parse(r#"let it = "outer"; "x" |> .replace("x", die("stop"))"#).unwrap();
    assert!(evaluator.eval_program(&program).is_err());
    assert_eq!(
        evaluator
            .eval_program(&rill::parser::parse("it").unwrap())
            .unwrap(),
        Value::Str("outer".into())
    );
}

#[test]
fn file_reads_return_structured_results_and_support_propagation() {
    let scratch = Scratch::new();
    let filename = scratch.0.join("config.yml");
    let source = format!(
        r#"
fn load() -> Result[Str, Error] {{
    let p = path("{}")
    p.write("name: rill")
    let text = p.read_text()?
    let data = p.read_yaml()?
    text + " " + data.name
}}
load()
"#,
        filename.display()
    );
    assert_eq!(
        run(&source),
        Value::Result(Ok(Box::new(Value::Str("name: rill rill".into()))))
    );
    for (namespace, method) in [("yaml.read", "read_yaml"), ("fs.read", "read_text")] {
        let expected = run(&format!(r#"path("{}").{method}()"#, filename.display()));
        assert_eq!(
            run(&format!(r#"{namespace}("{}")"#, filename.display())),
            expected
        );
        assert_eq!(
            run(&format!(r#"{namespace}(path("{}"))"#, filename.display())),
            expected
        );
    }
    check(r#"let text: Result[Str, Error] = fs.read("file")"#).unwrap();
    check(r#"fn load() -> Result[Map, Error] { yaml.read("file") }"#).unwrap();
    assert_eq!(
        run(&format!(r#"path("{}").exists()"#, filename.display())),
        Value::Bool(true)
    );
    assert_eq!(
        run(&format!(r#""{}/*.yml".glob()"#, scratch.0.display())),
        Value::List(vec![Value::Path(filename.clone())])
    );
    std::fs::write(&filename, "broken: [").unwrap();
    for (path, method, expected_kind) in [
        (filename.clone(), "read_yaml", ErrorKind::Parse),
        (scratch.0.join("missing"), "read_yaml", ErrorKind::Io),
        (scratch.0.join("missing"), "read_text", ErrorKind::Io),
    ] {
        let source = format!(r#"path("{}").{method}()"#, path.display());
        let namespace = if method == "read_yaml" {
            "yaml.read"
        } else {
            "fs.read"
        };
        assert_eq!(
            run(&format!(r#"{namespace}("{}")"#, path.display())),
            run(&source)
        );
        let Value::Result(Err(error)) = run(&source) else {
            panic!("expected Err")
        };
        let Value::Error(error) = *error else {
            panic!("expected structured error")
        };
        assert_eq!(error.kind, expected_kind);
        assert_eq!(error.operation.as_deref(), Some(method));
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_rill"))
            .args(["-e", &source])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(if expected_kind == ErrorKind::Io {
                "I/O error"
            } else {
                "parse error"
            }),
            "{stderr}"
        );
    }
}

#[test]
fn archive_methods_list_and_extract() {
    let scratch = Scratch::new();
    let archive = scratch.0.join("test.tar");
    let mut builder = tar::Builder::new(std::fs::File::create(&archive).unwrap());
    let mut header = tar::Header::new_gnu();
    header.set_size(5);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "file.txt", &b"hello"[..])
        .unwrap();
    builder.finish().unwrap();
    assert_eq!(
        run(&format!(r#"path("{}").archive_list()"#, archive.display())),
        Value::List(vec![Value::Str("file.txt".into())])
    );
    let destination = scratch.0.join("extracted");
    assert_eq!(
        run(&format!(
            r#"path("{}").archive_extract(path("{}"))"#,
            archive.display(),
            destination.display()
        )),
        Value::Null
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("file.txt")).unwrap(),
        "hello"
    );
}

#[test]
fn method_pipelines_preserve_process_and_function_behavior() {
    assert_eq!(
        run(r#"run printf " a\nb\n" | run cat | .lines() |> .map(.trim()).reverse()"#),
        Value::List(vec![Value::Str("b".into()), Value::Str("a".into())])
    );
    assert_eq!(
        run(r#"["printf", "ok"].spawn().await().out"#),
        Value::Str("ok".into())
    );
    assert_eq!(
        run(r#"["sh", "-c", "exit 3"].run().ok"#),
        Value::Bool(false)
    );
    assert_eq!(
        run(r#"[["printf", "a"], ["printf", "b"]].parallel().map(.out)"#),
        Value::List(vec![Value::Str("a".into()), Value::Str("b".into())])
    );
    assert_eq!(
        run(r#"fn double(x: Int) -> Int { x * 2 }; 3 |> double"#),
        Value::Int(6)
    );
    assert_eq!(run(r#"let double = |x| x * 2; 3 |> double"#), Value::Int(6));
    assert_eq!(
        run(r#"fn map(x: Int, n: Int) -> Int { x + n }; 3 |> map(2)"#),
        Value::Int(5)
    );
    assert_eq!(
        run(r#"fn trim(x: Int) -> Int { x + 1 }; trim(2)"#),
        Value::Int(3)
    );
}

#[test]
fn invalid_methods_fail_statically_or_when_dynamic_data_is_checked_at_runtime() {
    for source in [
        r#""{}".parse_yaml().map(|x| x)"#,
        r#""{}".parse_yaml().map(.name)"#,
        r#""[]".parse_yaml().map(|a, b| a)"#,
    ] {
        check(source).unwrap();
        assert!(rill::run_source(source, vec![]).is_err(), "{source}");
    }
    for source in [
        "1.trim()",
        "[].get(0)",
        "{}.get(1)",
        "[].take(\"two\")",
        "[].map(2)",
        "[].map(|a, b| a)",
        "{}.map(|x| x)",
        "{}.filter(|x| true)",
        "[].run(1)",
        "[1].run()",
        "[1].spawn()",
        "[[1]].parallel()",
        "1.sleep()",
        "\"x\".trim(1)",
        "path(\"x\").read_text(1)",
        "\"x\".replace(\"x\", \"y\", bad=true)",
        "\"x\".trim",
        "path(\"x\").exists",
        "path(\"x\").read_text",
    ] {
        assert!(check(source).is_err(), "{source}");
    }
    for source in [
        "\"1\".parse_json().trim()",
        "\"[1]\".parse_yaml().run()",
        "{}.get(\"missing\").trim()",
        "[1].take(0 - 1)",
        "\"1\".parse_json().map(|x| x)",
        "\"x\".replace(\"x\", \"y\", 1)",
    ] {
        assert!(rill::run_source(source, vec![]).is_err(), "{source}");
    }
}

#[test]
fn registry_arities_conform_between_checker_and_evaluator() {
    use rill::methods::{Argument, Kind, Target};
    use std::collections::BTreeSet;

    let scratch = Scratch::new();
    let mut method_names = BTreeSet::new();
    for method in rill::methods::METHODS {
        assert!(method_names.insert(method.name), "duplicate method {}", method.name);
        if let Target::Builtin(implementation) = method.target {
            assert_eq!(implementation.name(), method.name);
        }
        let receiver = match method.name {
            "await" => r#"["true"].spawn()"#.to_string(),
            "is_ok" | "is_err" | "ok" | "err" => "ok(1)".to_string(),
            "is_some" | "is_none" => "some(1)".to_string(),
            "unwrap" | "unwrap_or" if method.receivers.contains(&Kind::Result) => {
                "ok(1)".to_string()
            }
            "run" | "spawn" => r#"["true"]"#.to_string(),
            "parallel" => "[]".to_string(),
            "map" | "parallel_map" => "[]".to_string(),
            _ => match method.receivers.first().copied().unwrap_or(Kind::Unknown) {
                Kind::Unknown | Kind::Null => "null".to_string(),
                Kind::Str => r#""abc""#.to_string(),
                Kind::List => "[]".to_string(),
                Kind::Map => "{}".to_string(),
                Kind::Path => format!(r#"path("{}")"#, scratch.0.join(method.name).display()),
                Kind::Regex => "/a/".to_string(),
                Kind::Duration => "0s".to_string(),
                Kind::ProcessHandle => r#"["true"].spawn()"#.to_string(),
                Kind::Option => "some(1)".to_string(),
                Kind::Result => "ok(1)".to_string(),
                other => panic!("no conformance receiver for {other:?}"),
            },
        };
        let arguments = method
            .arguments
            .iter()
            .map(|argument| match argument {
                Argument::Any => "null",
                Argument::Str => r#""a""#,
                Argument::Int => "0",
                Argument::Function => "|x| x",
                Argument::List => "[]",
                Argument::Map => "{}",
                Argument::PathOrStr | Argument::JoinPart => r#""child""#,
                Argument::MatchPattern => r#""a""#,
                Argument::NamedBool(name) => {
                    assert_eq!(*name, "all");
                    "all=true"
                }
            })
            .collect::<Vec<_>>();
        let source = |arity: usize| {
            format!(
                "({receiver}).{}({})",
                method.name,
                arguments[..arity.min(arguments.len())].join(", ")
            )
        };

        for arity in method.min_args..=method.max_args {
            let valid = source(arity);
            check(&valid).unwrap_or_else(|error| {
                panic!("checker rejected registered arity for {}: {error}", method.name)
            });
            if let Err(error) = eval_without_check(&valid) {
                let message = error.to_string();
                assert!(
                    !message.contains("arguments, got") && !message.contains("args, got"),
                    "evaluator rejected registered arity for {}: {message}",
                    method.name
                );
            }
        }

        if method.min_args > 0 {
            let too_few = source(method.min_args - 1);
            assert!(check(&too_few).is_err(), "checker accepted too few args: {too_few}");
            assert!(
                eval_without_check(&too_few).is_err(),
                "evaluator accepted too few args: {too_few}"
            );
        }

        let mut too_many_args = arguments.clone();
        too_many_args.push("null");
        let too_many = format!(
            "({receiver}).{}({})",
            method.name,
            too_many_args.join(", ")
        );
        assert!(check(&too_many).is_err(), "checker accepted too many args: {too_many}");
        assert!(
            eval_without_check(&too_many).is_err(),
            "evaluator accepted too many args: {too_many}"
        );
    }

    let mut builtin_names = BTreeSet::new();
    for builtin in rill::methods::BUILTINS {
        assert!(builtin_names.insert(builtin.name), "duplicate builtin {}", builtin.name);
        let argument = |index: usize| match builtin.argument(index) {
            Argument::Int => "0",
            _ if builtin.name == "set_env" && index == 0 => r#""RILL_REGISTRY_CONFORMANCE""#,
            _ if builtin.name == "set_env" => r#""ok""#,
            _ if builtin.name == "sh" => r#""true""#,
            _ => "null",
        };
        let source = |arity: usize| {
            let args = (0..arity).map(argument).collect::<Vec<_>>().join(", ");
            format!("{}({args})", builtin.name)
        };
        let mut valid_arities = vec![builtin.min_args];
        if builtin.max_args == usize::MAX {
            valid_arities.push(builtin.min_args + 2);
        } else if builtin.max_args != builtin.min_args {
            valid_arities.push(builtin.max_args);
        }
        for arity in valid_arities {
            let valid = source(arity);
            check(&valid).unwrap_or_else(|error| {
                panic!("checker rejected registered arity for {}: {error}", builtin.name)
            });
            if let Err(error) = eval_without_check(&valid) {
                let message = error.to_string();
                assert!(
                    !message.contains("args, got"),
                    "evaluator rejected registered arity for {}: {message}",
                    builtin.name
                );
            }
        }
        if builtin.min_args > 0 {
            let too_few = source(builtin.min_args - 1);
            assert!(check(&too_few).is_err(), "checker accepted too few args: {too_few}");
            assert!(eval_without_check(&too_few).is_err(), "evaluator accepted too few args: {too_few}");
        }
        if builtin.max_args != usize::MAX {
            let too_many = source(builtin.max_args + 1);
            assert!(check(&too_many).is_err(), "checker accepted too many args: {too_many}");
            assert!(eval_without_check(&too_many).is_err(), "evaluator accepted too many args: {too_many}");
        }
    }
    std::env::remove_var("RILL_REGISTRY_CONFORMANCE");

    let regression = "some(1).is_some(2)";
    assert!(check(regression).is_err());
    assert!(eval_without_check(regression).is_err());
}

#[test]
fn removed_forms_have_migration_diagnostics() {
    for (source, replacement) in [
        ("get({}, \"x\")", "get"),
        ("map([], |x| x)", "map"),
        ("trim(\"x\")", "trim"),
        ("lines(\"x\")", "lines"),
        ("read(\"x\")", "read_text"),
        ("spawn([\"true\"])", "spawn"),
        ("[] |> map(|x| x)", "map"),
    ] {
        let error = check(source).expect_err(source).to_string();
        assert!(
            error.contains("removed") && error.contains(replacement),
            "{source}: {error}"
        );
        let program = rill::parser::parse(source).unwrap();
        assert!(
            rill::eval::Evaluator::new(vec![])
                .eval_program(&program)
                .is_err(),
            "{source}"
        );
    }
    assert!(check(r#"run printf "x" | lines"#).is_err());
    assert!(check(r#"run printf "x" | lines()"#).is_err());
}

#[test]
fn namespace_apis_share_method_behavior() {
    for (namespace, method) in [
        (r#"json.parse("[1, 2]")"#, r#""[1, 2]".parse_json()"#),
        (r#"yaml.parse("[1, 2]")"#, r#""[1, 2]".parse_yaml()"#),
        (r#"toml.parse("x = 1")"#, r#""x = 1".parse_toml()"#),
        (r#"csv.parse("a,b")"#, r#""a,b".parse_csv()"#),
        (
            r#"url.parse("https://example.com/a")"#,
            r#""https://example.com/a".parse_url()"#,
        ),
        (r#"json.stringify({a: 1})"#, r#"({a: 1}).to_json()"#),
        (r#"json.pretty({a: 1})"#, r#"({a: 1}).to_json_pretty()"#),
        (r#"yaml.stringify({a: 1})"#, r#"({a: 1}).to_yaml()"#),
        (r#"toml.stringify({a: 1})"#, r#"({a: 1}).to_toml()"#),
        (
            r#"path.join("/tmp", "child")"#,
            r#"path("/tmp").join("child")"#,
        ),
        (r#"fs.exists("/tmp")"#, r#"path("/tmp").exists()"#),
        (r#"hashes.sha256("abc")"#, r#""abc".sha256()"#),
        (
            r#"regex.is_match("a.*", "abc")"#,
            r#""abc".is_match("a.*")"#,
        ),
        (
            r#"regex.is_match(/a.*/, "abc")"#,
            r#"/a.*/.is_match("abc")"#,
        ),
        (
            r#"process.run(["printf", "ok"])"#,
            r#"["printf", "ok"].run()"#,
        ),
        (
            r#"process.exec(["printf", "ok"])"#,
            r#"["printf", "ok"].run()"#,
        ),
        ("time.sleep(0)", "0s.sleep()"),
        ("time.sleep(0s)", "0s.sleep()"),
    ] {
        assert_eq!(run(namespace), run(method), "{namespace}");
    }
    assert_eq!(
        run(r#""[1]" |> json.parse() |> .take(1)"#),
        Value::List(vec![Value::Int(1)])
    );
    let scratch = Scratch::new();
    let file = scratch.0.join("file");
    assert_eq!(
        run(&format!(r#"fs.write("{}", "hello")"#, file.display())),
        Value::Null
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), "hello");
    for source in [
        "yaml.read()",
        "yaml.read(1)",
        "json.parse(1)",
        "json.parse(\"{}\", 1)",
        "path.join(1, \"x\")",
        "regex.is_match(1, \"x\")",
        "time.sleep(\"x\")",
    ] {
        assert!(check(source).is_err(), "{source}");
    }
    assert!(rill::run_source("yaml.read(json.parse(\"1\"))", vec![]).is_err());
}
