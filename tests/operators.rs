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
        Value::list(vec![Value::Int(0), Value::Int(1), Value::Int(2), Value::Int(3)])
    );
    assert_eq!(run("5..3"), Value::list(vec![]));
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
        Value::list(vec![
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
        Value::list(vec![
            Value::Str("small".into()),
            Value::Str("big".into()),
            Value::Str("big".into())
        ])
    );
    assert_eq!(run("fn f(v) { if v { return 7 }\n9 }\nf(true)"), Value::Int(7));
    assert_eq!(
        run("for n in [1, 2] { if n == 2 { emit n * 10 } }"),
        Value::list(vec![Value::Int(20)])
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
        Value::list(vec![Value::Int(3), Value::Int(6), Value::Int(11), Value::Int(3)])
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

#[test]
fn nested_place_assignment() {
    assert_eq!(run("var m = {a: 1}\nm.a = 2\nm.a"), Value::Int(2));
    assert_eq!(run("var m = {a: {b: 1}}\nm[\"a\"][\"b\"] = 2\nm.a.b"), Value::Int(2));
    assert_eq!(run("var m = {a: {b: 1}}\nm.a.b += 5\nm.a.b"), Value::Int(6));
    assert_eq!(
        run("var xs = [{name: \"a\"}]\nxs[0].name = \"x\"\nxs[0].name"),
        Value::Str("x".into())
    );
    assert_eq!(
        run("var xs = [{name: \"a\"}]\nxs[-1].name += \"y\"\nxs[-1].name"),
        Value::Str("ay".into())
    );
    assert_eq!(run("var m = {}\nm.k = 1\nm.k"), Value::Int(1));
    assert_eq!(run("var m = {}\nm.k += 1\nm.k"), Value::Int(1));
    assert_eq!(run("var t = (1, \"a\")\nt[1] = \"b\"\nt[1]"), Value::Str("b".into()));
    assert_eq!(run("var xs = [1]\nxs += [2]\nxs"), Value::list(vec![Value::Int(1), Value::Int(2)]));
    assert_eq!(
        run("var xs = [[1]]\nxs[0] += [2]\nxs[0]"),
        Value::list(vec![Value::Int(1), Value::Int(2)])
    );
    assert!(fails("var m: Map = {a: 1}\nm.missing.inner = 1").contains("key 'missing' not in map"));
    assert!(check_err("var m = {a: 1}\nm.missing.inner = 1")
        .contains("cannot assign to field 'inner' of Int"));
    assert!(check_err("let m = {a: 1}\nm.a = 2").contains("immutable variable 'm'"));
    assert!(check_err("var s = \"x\"\ns[0] = \"y\"").contains("cannot assign into Str"));
    assert!(check_err("var xs = [1]\nxs[0] = \"s\"").contains("expected Int, got Str"));
    assert!(check_err("var n = 1\nn.a = 2").contains("cannot assign to field 'a' of Int"));
    assert!(check_err("var xs = [1]\nxs[\"a\"] = 2").contains("expected Int"));
}

#[test]
fn map_iteration_yields_key_value_tuples() {
    assert_eq!(
        run("for (k, v) in {b: 2, a: 1} { emit \"$k=$v\" }"),
        Value::list(vec![Value::Str("b=2".into()), Value::Str("a=1".into())])
    );
    assert_eq!(
        run("{a: 1}.entries()"),
        Value::list(vec![Value::tuple(vec![Value::Str("a".into()), Value::Int(1)])])
    );
    assert_eq!(
        run("for (k, v) in {a: 1}.entries() { emit k }"),
        Value::list(vec![Value::Str("a".into())])
    );
    assert_eq!(
        run("parallel for (k, v) in {a: 1, b: 2} { emit v * 10 }"),
        Value::list(vec![Value::Int(10), Value::Int(20)])
    );
    assert!(check_err("for x in 5 { }").contains("for loop needs List, Map, or Stream"));
}

#[test]
fn ranges_steps_and_slices() {
    let ints = |values: &[i64]| Value::List(values.iter().map(|v| Value::Int(*v)).collect());
    assert_eq!(run("(0..10).step(3)"), ints(&[0, 3, 6, 9]));
    assert!(fails("[1].step(0)").contains("positive step"));
    assert_eq!(run("[1, 2, 3, 4][1..3]"), ints(&[2, 3]));
    assert_eq!(run("let xs = [1, 2, 3, 4]\nxs[..2]"), ints(&[1, 2]));
    assert_eq!(run("let xs = [1, 2, 3, 4]\nxs[2..]"), ints(&[3, 4]));
    assert_eq!(run("let xs = [1, 2, 3, 4]\nxs[-2..]"), ints(&[3, 4]));
    assert_eq!(run("let xs = [1, 2, 3, 4]\nxs[1..=2]"), ints(&[2, 3]));
    assert_eq!(run("let xs = [1, 2, 3, 4]\nxs[..]"), ints(&[1, 2, 3, 4]));
    assert_eq!(run("let xs = [1, 2, 3, 4]\nxs[3..1]"), ints(&[]));
    assert_eq!(run("let xs = [1, 2, 3, 4]\nxs[1..100]"), ints(&[2, 3, 4]));
    assert_eq!(run("\"hello\"[1..3]"), Value::Str("el".into()));
    assert_eq!(run("\"hello\"[-2..]"), Value::Str("lo".into()));
    assert_eq!(run("[1, 2, 3].slice(-2, 3)"), ints(&[2, 3]));
    assert_eq!(run("let xs = [1, 2, 3]\nlet i = 1\nxs[i + 1..]"), ints(&[3]));
    assert_eq!(run("let xs = [1, 2, 3]\nxs[xs.len() - 1]"), Value::Int(3));
    assert!(check_err("let t = (1, 2)\nt[0..1]").contains("cannot slice a Tuple"));
    assert!(check_err("[1][\"a\"..]").contains("slice bounds must be Int"));
}

#[test]
fn option_and_result_combinators() {
    let some = |v: Value| Value::Option(Some(Box::new(v)));
    let ok = |v: Value| Value::Result(Ok(Box::new(v)));
    let err = |v: Value| Value::Result(Err(Box::new(v)));
    assert_eq!(run("some(1).map(|x| x + 1)"), some(Value::Int(2)));
    assert_eq!(run("none.map(|x| x + 1)"), Value::Option(None));
    assert_eq!(run("some({n: 1}).map(.n)"), some(Value::Int(1)));
    assert_eq!(run("ok(1).map(|x| x + 1)"), ok(Value::Int(2)));
    assert_eq!(run("err(\"e\").map(|x| x + 1)"), err(Value::Str("e".into())));
    assert_eq!(run("ok(1).map_err(|e| \"x\")"), ok(Value::Int(1)));
    assert_eq!(run("err(\"e\").map_err(.upper())"), err(Value::Str("E".into())));
    assert_eq!(
        run("some(2).and_then(|x| if x > 1 { some(x) } else { none })"),
        some(Value::Int(2))
    );
    assert_eq!(
        run("some(0).and_then(|x| if x > 1 { some(x) } else { none })"),
        Value::Option(None)
    );
    assert_eq!(run("none.unwrap_or_else(|| 5)"), Value::Int(5));
    assert_eq!(run("err(\"e\").unwrap_or_else(|e| e + \"!\")"), Value::Str("e!".into()));
    assert_eq!(run("some(1).ok_or(\"missing\")"), ok(Value::Int(1)));
    assert_eq!(run("none.ok_or(\"missing\")"), err(Value::Str("missing".into())));
    assert_eq!(run("some(1).expect(\"boom\")"), Value::Int(1));
    assert!(fails("none.expect(\"boom\")").contains("boom"));
    assert!(fails("err(\"bad\").expect(\"boom\")").contains("boom: bad"));
    assert!(fails("some(1).and_then(|x| x)").contains("must return an Option or Result"));
}

#[test]
fn first_last_and_get_return_options() {
    let some = |v: Value| Value::Option(Some(Box::new(v)));
    assert_eq!(run("[].first()"), Value::Option(None));
    assert_eq!(run("[1, 2].first()"), some(Value::Int(1)));
    assert_eq!(run("[1, 2].last()"), some(Value::Int(2)));
    assert_eq!(run("[1, 2].get(-1)"), some(Value::Int(2)));
    assert_eq!(run("[1].get(5)"), Value::Option(None));
    assert_eq!(run("{a: 1}.get(\"b\")"), Value::Null);
    assert_eq!(run("let xs = [1]\nlet n: Int = xs.first() ?? 0\nn"), Value::Int(1));
    assert_eq!(run("[\"\", \"a\"].find(. != \"\").unwrap_or(\"none\")"), Value::Str("a".into()));
    assert!(check_err("[1].get(\"a\")").contains("invalid .get() argument 1: Str"));
}

#[test]
fn closure_parameters_are_inferred_from_the_receiver() {
    assert!(check_err("[1, 2].map(|x| x.upper())").contains("not supported on Int"));
    assert!(check_err("[1, 2].map(.upper())").contains("not supported on Int"));
    let program = rill::parser::parse("[\"a\"].map(|s| s.upper())").unwrap();
    rill::typeck::check(&program).unwrap();
    let program = rill::parser::parse("{a: 1}.map(|k, v| k.upper())").unwrap();
    rill::typeck::check(&program).unwrap();
    assert!(
        check_err("fn f(g: Fn[Str] -> Str) { g(\"a\") }\nf(|s| s + 1)").contains("got Str and Int")
    );
    assert!(check_err("some(1).unwrap_or_else(|x| x)").contains("needs 0 args, got 1"));
    assert!(check_err("err(\"e\").map_err(|a, b| a)").contains("needs 1 arg, got 2"));
    assert!(fails("let o = some(1)\nlet f = |x| x\no.unwrap_or_else(f)")
        .contains("needs 0 args, got 1"));
}

#[test]
fn match_literals_tuples_lists_and_guards() {
    assert_eq!(run("match 2 { 1 | 2 => \"low\", _ => \"high\" }"), Value::Str("low".into()));
    assert_eq!(run("match -1 { -1 => \"neg\", _ => \"other\" }"), Value::Str("neg".into()));
    assert_eq!(run("match 1.0 { 1 => \"one\", _ => \"other\" }"), Value::Str("one".into()));
    assert_eq!(run("match \"b\" { \"a\" => 1, \"b\" => 2, _ => 3 }"), Value::Int(2));
    assert_eq!(run("match true { true => 1, false => 0 }"), Value::Int(1));
    assert_eq!(run("match null { null => \"n\", _ => \"o\" }"), Value::Str("n".into()));
    assert_eq!(run("match (1, \"a\") { (1, s) => s, _ => \"\" }"), Value::Str("a".into()));
    assert_eq!(run("match [1, 2, 3] { [first, ..] => first, _ => 0 }"), Value::Int(1));
    assert_eq!(run("match [1, 2] { [a, b] => a + b, _ => 0 }"), Value::Int(3));
    assert_eq!(run("match [1, 2, 3] { [a, b] => a + b, _ => 0 }"), Value::Int(0));
    assert_eq!(run("match [] { [] => \"empty\", _ => \"full\" }"), Value::Str("empty".into()));
    assert_eq!(
        run(
            "match some(5) { some(n) if n > 3 => \"big\", some(_) => \"small\", none => \"none\" }"
        ),
        Value::Str("big".into())
    );
    assert_eq!(run("match ok(some(1)) { ok(some(x)) => x, _ => 0 }"), Value::Int(1));
    assert_eq!(run("match err(\"e\") { ok(v) => v, err(e) => e.upper() }"), Value::Str("E".into()));
    assert_eq!(
        run("match 7 { n if n % 2 == 0 => \"even\", n => \"odd\" }"),
        Value::Str("odd".into())
    );
    assert_eq!(
        run("match \"a-1\" { /^(?<k>\\w)-(?<v>\\d)$/ | /^(?<k>\\w)=(?<v>\\d)$/ => \"$k$v\", _ => \"\" }"),
        Value::Str("a1".into())
    );
    assert_eq!(run("match {a: 1} { m => m.a }"), Value::Int(1));
    assert_eq!(run("match \"zzz\" { /a/ => 1 }"), Value::Null);
}

#[test]
fn match_exhaustiveness_and_pattern_types() {
    assert!(check_err("match some(1) { some(x) => x }").contains("non-exhaustive match on Option"));
    assert!(check_err("match true { true => 1 }").contains("non-exhaustive match on Bool"));
    assert!(check_err("match ok(1) { ok(x) => x }").contains("non-exhaustive match on Result"));
    assert!(
        check_err("match some(1) { some(x) if x > 0 => x, none => 0 }").contains("non-exhaustive")
    );
    let ok_sources = [
        "match some(1) { some(x) => x, none => 0 }",
        "match ok(1) { ok(x) => x, err(_) => 0 }",
        "match true { true => 1, false => 0 }",
        "match 1 { 1 => \"one\" }",
        "match \"x\" { /a/ => 1 }",
        "match some(1) { some(_) | none => 0 }",
    ];
    for source in ok_sources {
        let program = rill::parser::parse(source).expect(source);
        rill::typeck::check(&program).expect(source);
    }
    assert!(
        check_err("match 1 { some(x) => x, _ => 0 }").contains("some pattern cannot match a Int")
    );
    assert!(check_err("match 1 { \"a\" => 1, _ => 0 }")
        .contains("pattern of type Str cannot match a Int"));
    assert!(check_err("match (1, 2) { (a, b, c) => a, _ => 0 }")
        .contains("has 3 elements but the value has 2"));
    assert!(check_err("match 1 { a | 2 => a, _ => 0 }").contains("bind the same names"));
    assert!(check_err("match 1 { n if n => 1, _ => 0 }").contains("match guard must be Bool"));
    let error = rill::parser::parse("match \"a\" { \"{x}\" => 1, _ => 0 }").unwrap_err();
    assert!(error.to_string().contains("interpolation is not allowed"), "{error}");
}
