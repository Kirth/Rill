//! Checker warnings and multi-error reporting.

use rill::diagnostic::{RillError, Severity};

fn warnings(source: &str) -> Vec<RillError> {
    let program = rill::parser::parse(source).expect(source);
    rill::typeck::check_with_warnings(&program).expect(source)
}

fn codes(source: &str) -> Vec<&'static str> {
    warnings(source).iter().map(|warning| warning.code_name()).collect()
}

#[test]
fn unused_let_warns_unless_read_or_underscored() {
    let found = warnings("let x = 1\nprint(2)\n");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].severity, Severity::Warning);
    assert_eq!(found[0].code_name(), "rill::warn::unused_let");
    assert!(found[0].message.contains("'x'"));
    assert!(found[0].span.is_some());

    assert!(codes("let x = 1\nprint(x)\n").is_empty());
    assert!(codes("let _x = 1\nlet _ = 2\nprint(3)\n").is_empty());
    assert!(codes("var n = 0\nn += 1\n").is_empty());
    assert_eq!(codes("fn f() { let dead = 1\n2 }\nprint(f())\n"), vec!["rill::warn::unused_let"]);
    assert!(codes("let (a, _) = (1, 2)\nprint(a)\n").is_empty());
}

#[test]
fn reads_through_interpolation_and_commands_count_as_uses() {
    assert!(codes("let host = \"h\"\nprint \"$host\"\n").is_empty());
    assert!(codes("let host = \"h\"\nprint \"${host:-x}\"\n").is_empty());
    assert!(codes("let host = \"h\"\nprint \"{host}\"\n").is_empty());
    assert!(codes("let msg = \"hi\"\nrun echo \"{msg}\"\n").is_empty());
    assert!(codes("let msg = \"hi\"\nrun echo $msg\n").is_empty());
    assert!(codes("let xs = [\"a\"]\nrun echo $*xs\n").is_empty());
}

#[test]
fn unused_result_warns_for_discarded_results_only() {
    let source = "fn f() -> Result[Int, Error] { ok(1) }\nf()\nprint(1)\n";
    assert_eq!(codes(source), vec!["rill::warn::unused_result"]);
    assert!(codes("fn f() -> Result[Int, Error] { ok(1) }\nlet _ = f()\nprint(1)\n").is_empty());
    assert!(codes("fn f() -> Result[Int, Error] { ok(1) }\nfn g() -> Result[Int, Error] { f() }\nprint(g())\n").is_empty());
    assert!(codes("fn f() -> Result[Int, Error] { ok(1) }\nfn g() -> Int { f()? }\nprint(g())\n")
        .is_empty());
}

#[test]
fn unreachable_statements_warn_once_per_block() {
    let found = warnings("fn f() -> Int {\n  return 1\n  print(2)\n  print(3)\n}\nprint(f())\n");
    let unreachable: Vec<_> =
        found.iter().filter(|w| w.code_name() == "rill::warn::unreachable").collect();
    assert_eq!(unreachable.len(), 1);
    assert!(unreachable[0].help_text().unwrap().contains("`return`"));
    assert_eq!(
        codes("for x in [1] {\n  continue\n  print(x)\n}\n"),
        vec!["rill::warn::unreachable"]
    );
    assert_eq!(codes("while true {\n  break\n  print(1)\n}\n"), vec!["rill::warn::unreachable"]);
    assert_eq!(codes("die(\"x\")\nprint(1)\n"), vec!["rill::warn::unreachable"]);
}

#[test]
fn code_after_return_does_not_change_the_function_type() {
    assert!(codes("fn f() -> Int {\n  return 1\n  print(2)\n}\nprint(f())\n")
        .iter()
        .all(|code| *code == "rill::warn::unreachable"));
}

#[test]
fn regex_captures_shadowing_a_binding_warn() {
    let source = "let name = \"x\"\nif \"abc\" =~ /(?<name>b)/ { print(name) }\nprint(name)\n";
    assert_eq!(codes(source), vec!["rill::warn::shadowed_capture"]);
    let source =
        "let user = \"x\"\nmatch \"u=a\" { /u=(?<user>\\w+)/ => print(user), _ => print(user) }\n";
    assert_eq!(codes(source), vec!["rill::warn::shadowed_capture"]);
    assert!(codes("if \"abc\" =~ /(?<name>b)/ { print(name) }\n").is_empty());
}

#[test]
fn every_independent_error_is_reported_together() {
    let program = rill::parser::parse("let a = 1 + \"s\"\nprint(zzz)\nlet b: Int = \"x\"\nprint(a, b)\nfn h() { let q = undefined_thing\nq }\n").unwrap();
    let error = rill::typeck::check(&program).unwrap_err();
    assert!(error.message.contains("numeric operator"), "{}", error.message);
    let related: Vec<&str> = error.related_errors().iter().map(|e| e.message.as_str()).collect();
    assert_eq!(related.len(), 3, "{related:?}");
    assert!(related[0].contains("unknown identifier 'zzz'"));
    assert!(related[1].contains("expected Int, got Str"));
    assert!(related[2].contains("undefined_thing"));
    // A failed `let` still binds its name, so later uses do not cascade.
    assert!(!related.iter().any(|message| message.contains("unknown identifier 'a'")));
}

#[test]
fn errors_are_deduplicated_and_capped() {
    let mut source = String::new();
    for _ in 0..30 {
        source.push_str("print(zzz)\n");
    }
    let program = rill::parser::parse(&source).unwrap();
    let error = rill::typeck::check(&program).unwrap_err();
    assert!(error.related_errors().len() < 20);
}
