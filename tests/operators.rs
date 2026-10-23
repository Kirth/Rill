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
