use rill::types::Value;

fn run(source: &str) -> Value {
    rill::run_source(source, Vec::new()).expect(source)
}

fn fails(source: &str) -> String {
    rill::run_source(source, Vec::new()).expect_err(source).to_string()
}

fn check_err(source: &str) -> String {
    let program = rill::parser::parse(source).expect(source);
    rill::typeck::check(&program).expect_err(source).to_string()
}

#[test]
fn remainder_truncates_toward_zero_like_division() {
    assert_eq!(run("-7 % 2"), Value::Int(-1));
    assert_eq!(run("7 % -2"), Value::Int(1));
    assert_eq!(run("7.5 % 2"), Value::Float(1.5));
    assert_eq!(run("var x = 7\nx %= 3\nx"), Value::Int(1));
    assert!(fails("1 % 0").contains("remainder by zero"));
}

#[test]
fn power_is_right_associative_and_checks_overflow() {
    assert_eq!(run("2 ** 3 ** 2"), Value::Int(512));
    assert_eq!(run("-2 ** 2"), Value::Int(-4));
    assert_eq!(run("2 ** 0.5"), Value::Float(2f64.sqrt()));
    assert_eq!(run("2.0 ** -1"), Value::Float(0.5));
    assert!(fails("2 ** 63").contains("overflow"));
    assert!(fails("2 ** -1").contains("negative exponent"));
}

#[test]
fn membership_operator_covers_list_map_and_str() {
    assert_eq!(run("1 in [1, 2]"), Value::Bool(true));
    assert_eq!(run("3 not in [1, 2]"), Value::Bool(true));
    assert_eq!(run("\"a\" not in {b: 1}"), Value::Bool(true));
    assert_eq!(run("\"b\" in {b: 1}"), Value::Bool(true));
    assert_eq!(run("\"ab\" in \"cabd\""), Value::Bool(true));
    assert_eq!(run("1.0 in [1]"), Value::Bool(true));
    assert!(check_err("1 in \"abc\"").contains("needs a Str on the left"));
    assert!(check_err("1 in 2").contains("needs a List, Map, or Str"));
}

#[test]
fn strings_compare_lexicographically() {
    assert_eq!(run("\"a\" < \"b\""), Value::Bool(true));
    assert_eq!(run("\"B\" < \"a\""), Value::Bool(true));
    assert_eq!(run("\"\" <= \"\""), Value::Bool(true));
    assert!(check_err("\"a\" < 1").contains("two numbers, two strings, or two durations"));
}

#[test]
fn coalesce_unwraps_option_and_replaces_null() {
    assert_eq!(run("null ?? 3"), Value::Int(3));
    assert_eq!(run("none ?? 3"), Value::Int(3));
    assert_eq!(run("some(1) ?? 3"), Value::Int(1));
    assert_eq!(run("0 ?? 3"), Value::Int(0));
    assert_eq!(run("some(1) ?? die(\"not evaluated\")"), Value::Int(1));
    assert_eq!(run("let a = none\nlet b = some(2)\na ?? b ?? 9"), Value::Int(2));
    assert_eq!(run("let x = null\nx ?? 0 + 1"), Value::Int(1));
}

#[test]
fn numeric_equality_crosses_int_and_float() {
    assert_eq!(run("1 == 1.0"), Value::Bool(true));
    assert_eq!(run("[1] == [1.0]"), Value::Bool(true));
    assert_eq!(run("{a: 1} == {a: 1.0}"), Value::Bool(true));
    assert_eq!(run("1 != 1.5"), Value::Bool(true));
    assert_eq!(run("[1.0].contains(1)"), Value::Bool(true));
    assert_eq!(run("some(1) == some(1.0)"), Value::Bool(true));
}

#[test]
fn unary_minus_is_a_real_operator() {
    use rill::ast::{Expr, Stmt, UnaryOp};
    let program = rill::parser::parse("-x").unwrap();
    let Stmt::Expr(expr) = program.stmts[0].unspanned() else { panic!() };
    assert!(matches!(expr.unspanned(), Expr::Unary { op: UnaryOp::Neg, .. }));
    assert_eq!(run("let xs = [1, 2]\n-xs[0]"), Value::Int(-1));
    assert_eq!(run("let xs = [1, 2]\n-xs.len()"), Value::Int(-2));
    assert_eq!(run("- 1 + 2"), Value::Int(1));
    assert_eq!(run("-1.5"), Value::Float(-1.5));
    assert_eq!(run("let t = (1, \"a\")\nt[-1]"), Value::Str("a".into()));
    assert!(fails("-\"x\"").contains("unary minus needs a number"));
}

#[test]
fn inclusive_range_and_precedence() {
    assert_eq!(
        run("0..=3"),
        Value::List(vec![Value::Int(0), Value::Int(1), Value::Int(2), Value::Int(3)])
    );
    assert_eq!(run("5..3"), Value::List(vec![]));
    assert_eq!(run("(0..2) == [0, 1]"), Value::Bool(true));
    assert_eq!(run("0..2 == [0, 1]"), Value::Bool(true));
    assert_eq!(run("1 + 2 * 3 % 4 == 3 && !false"), Value::Bool(true));
    assert_eq!(run("let r = run date +%s\nr.ok"), Value::Bool(true));
}

#[test]
fn durations_support_arithmetic_comparison_and_accessors() {
    use std::time::Duration;
    assert_eq!(run("1s + 500ms"), Value::Duration(Duration::from_millis(1500)));
    assert_eq!(run("2 * 1s"), Value::Duration(Duration::from_secs(2)));
    assert_eq!(run("1s * 1.5"), Value::Duration(Duration::from_millis(1500)));
    assert_eq!(run("1s / 2"), Value::Duration(Duration::from_millis(500)));
    assert_eq!(run("3s / 1.5s"), Value::Float(2.0));
    assert_eq!(run("5s % 2s"), Value::Duration(Duration::from_secs(1)));
    assert_eq!(run("1500ms > 1s"), Value::Bool(true));
    assert_eq!(run("1s == 1000ms"), Value::Bool(true));
    assert_eq!(
        run("var t = 1s\nt += 250ms\nt *= 2\nt"),
        Value::Duration(Duration::from_millis(2500))
    );
    assert_eq!(
        run("[1500ms.secs(), 1500ms.ms(), 1500ms.us(), 1500ms.ns()]"),
        Value::List(vec![
            Value::Int(1),
            Value::Int(1500),
            Value::Int(1_500_000),
            Value::Int(1_500_000_000)
        ])
    );
    assert_eq!(run("1500ms.as_secs_float()"), Value::Float(1.5));
    assert!(fails("1s - 2s").contains("underflow"));
    assert!(fails("1s * -1").contains("negative"));
    assert!(check_err("1s * 1s").contains("not defined for Duration and Duration"));
    assert!(check_err("1s + 1").contains("not defined for Duration and Int"));
}

#[test]
fn durations_display_like_go() {
    for (source, expected) in [
        ("0s", "0s"),
        ("100ns", "100ns"),
        ("1500ns", "1.5us"),
        ("250ms", "250ms"),
        ("1.5s", "1.5s"),
        ("90s", "1m30s"),
        ("1h", "1h"),
        ("5400s", "1h30m"),
        ("2m + 5.5s", "2m5.5s"),
        ("3h + 2m + 5s", "3h2m5s"),
    ] {
        assert_eq!(run(&format!("Str({source})")), Value::Str(expected.into()), "{source}");
    }
    assert_eq!(run("(20 * 1024 * 1024).human_size()"), Value::Str("20.0 MiB".into()));
    assert_eq!(run("20MiB.human_size()"), Value::Str("20.0 MiB".into()));
    assert_eq!(run("512.human_size()"), Value::Str("512 B".into()));
}

#[test]
fn if_is_an_expression() {
    assert_eq!(run("let x = if true { 1 } else { 2 }\nx"), Value::Int(1));
    assert_eq!(run("if false { 1 } else if true { 2 } else { 3 }"), Value::Int(2));
    assert_eq!(
        run("[1, 2, 3].map(|n| if n > 1 { \"big\" } else { \"small\" })"),
        Value::List(vec![
            Value::Str("small".into()),
            Value::Str("big".into()),
            Value::Str("big".into())
        ])
    );
    assert_eq!(run("fn f(v) { if v { return 7 }\n9 }\nf(true)"), Value::Int(7));
    assert_eq!(
        run("for n in [1, 2] { if n == 2 { emit n * 10 } }"),
        Value::List(vec![Value::Int(20)])
    );
    assert_eq!(run("let m = {a: 1}\nif let {a} = m { a } else { 0 }"), Value::Int(1));
    assert!(check_err("let z = if true { 1 }").contains("needs an else branch"));
}

#[test]
fn loop_breaks_with_value() {
    assert_eq!(run("var i = 0\nloop { i += 1\nif i == 3 { break i * 10 } }"), Value::Int(30));
    assert_eq!(run("loop { break }"), Value::Null);
    assert_eq!(run("var i = 0\nloop { i += 1\nif i < 3 { continue }\nbreak i }"), Value::Int(3));
    assert_eq!(run("let loop = 1\nloop + 1"), Value::Int(2));
    assert!(check_err("while true { break 1 }").contains("only valid inside loop"));
    assert!(check_err("break 1").contains("outside loop"));
}

#[test]
fn zero_arg_closures_and_fn_types() {
    assert_eq!(run("let f = || 42\nf()"), Value::Int(42));
    assert_eq!(
        run("let g: Fn() -> Int = || 2\nlet h: Fn(Int) -> Int = |x| x + 1\ng() + h(1)"),
        Value::Int(4)
    );
    assert_eq!(run("let g: Fn[] -> Int = || 2\ng()"), Value::Int(2));
    assert!(check_err("[1].map(|| 1)").contains("needs 1 arg, got 0"));
    assert!(check_err("let f = || 1\nf(1)").contains("needs 0 args, got 1"));
}

#[test]
fn default_parameters_and_named_arguments() {
    let prelude = "fn add(a, b = 2) { a + b }\n";
    assert_eq!(
        run(&format!("{prelude}[add(1), add(1, 5), add(b = 10, a = 1), add(a = 1)]")),
        Value::List(vec![Value::Int(3), Value::Int(6), Value::Int(11), Value::Int(3)])
    );
    assert_eq!(run("fn scale(x, factor = x * 2) { factor }\nscale(3)"), Value::Int(6));
    assert!(check_err(&format!("{prelude}add()")).contains("needs 1 to 2 args, got 0"));
    assert!(check_err(&format!("{prelude}add(1, 2, 3)")).contains("needs 1 to 2 args, got 3"));
    assert!(check_err(&format!("{prelude}add(1, c = 2)")).contains("no parameter 'c'"));
    assert!(check_err(&format!("{prelude}add(1, a = 2)")).contains("given twice"));
    assert!(check_err(&format!("{prelude}add(b = 1)"))
        .contains("missing an argument for parameter 'a'"));
    assert!(rill::parser::parse("fn g(a = 1, b) { 1 }").is_err());
    assert!(check_err("fn k(n: Int = \"x\") { n }\nk()").contains("expected Int"));
    // Runtime agrees when the checker is bypassed by dynamic types.
    assert!(fails(&format!("{prelude}let f = add\nf(1, c = 2)")).contains("no parameter 'c'"));
}
