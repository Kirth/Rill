use rill::ast::{CommandArg, CommandPart, Expr, PipeKind, RedirectKind, Stmt};

fn expression(stmt: &Stmt) -> &Expr {
    match stmt {
        Stmt::Expr(expr) | Stmt::Let { expr, .. } => expr.unspanned(),
        _ => panic!("expected expression statement or binding"),
    }
}

#[test]
fn command_mode_keeps_flags_paths_and_var_suffixes_as_argv() {
    let program = rill::parser::parse(
        r#"
let server = "host"
run rsync -av ./src/ $server:/srv/app/
"#,
    )
    .expect("parse");

    let Expr::Command(command) = expression(&program.stmts[1]) else {
        panic!("expected command expression");
    };

    assert_eq!(command.argv[0], CommandArg::Word("rsync".to_string()));
    assert_eq!(command.argv[1], CommandArg::Word("-av".to_string()));
    assert_eq!(command.argv[2], CommandArg::Word("./src/".to_string()));
    assert_eq!(
        command.argv[3],
        CommandArg::Parts(vec![
            CommandPart::Var("server".to_string()),
            CommandPart::Text(":/srv/app/".to_string())
        ])
    );
}

#[test]
fn byte_pipeline_preserves_command_boundaries() {
    let program = rill::parser::parse(r#"run journalctl -u nginx | run grep "timeout" | .lines()"#)
        .expect("parse");

    let Expr::Pipe {
        kind: PipeKind::Byte,
        left,
        right,
    } = expression(&program.stmts[0])
    else {
        panic!("expected outer byte pipe");
    };

    assert!(matches!(right.unspanned(), Expr::Call { callee, args }
        if args.is_empty() && matches!(callee.unspanned(), Expr::ImplicitField(name) if name == "lines")));
    let Expr::Pipe {
        kind: PipeKind::Byte,
        left,
        right,
    } = left.unspanned()
    else {
        panic!("expected inner byte pipe");
    };

    assert!(matches!(left.unspanned(), Expr::Command(_)));
    assert!(matches!(right.unspanned(), Expr::Command(_)));
}

#[test]
fn parse_errors_carry_spans() {
    let err = rill::parser::parse("let = 1").expect_err("parse should fail");
    assert!(err.span.is_some());
}

#[test]
fn unterminated_string_reports_source_span() {
    let err = rill::parser::parse("print \"oops").expect_err("parse should fail");
    let span = err.span.expect("span");
    assert_eq!(span.start, 6);
    assert!(span.end > span.start);
}

#[test]
fn command_redirections_are_parsed_outside_argv() {
    let program = rill::parser::parse(
        r#"
run foo > "out.txt"
run foo err> "err.txt"
run foo all> "all.txt"
run foo < "in.txt"
"#,
    )
    .expect("parse");

    for (stmt, kind, target) in [
        (&program.stmts[0], RedirectKind::Stdout, "out.txt"),
        (&program.stmts[1], RedirectKind::Stderr, "err.txt"),
        (&program.stmts[2], RedirectKind::All, "all.txt"),
        (&program.stmts[3], RedirectKind::Stdin, "in.txt"),
    ] {
        let Expr::Command(command) = expression(stmt) else {
            panic!("expected command");
        };
        assert_eq!(command.argv, vec![CommandArg::Word("foo".to_string())]);
        assert_eq!(command.redirects.len(), 1);
        assert_eq!(command.redirects[0].kind, kind);
        assert_eq!(
            command.redirects[0].target,
            CommandArg::String(target.to_string())
        );
    }
}

#[test]
fn multiline_lists_allow_whitespace_around_items() {
    let program = r#"
let controllers = [
  "192.0.2.3",
  "192.0.2.2",
  "192.0.2.17",
]
"#;

    let program = rill::parser::parse(program).expect("parse");
    assert_eq!(program.stmts.len(), 1);
    assert!(matches!(
        expression(&program.stmts[0]),
        Expr::List(items) if items.len() == 3
    ));
}

#[test]
fn multiline_pipeline_allows_break_after_assignment_and_before_pipe() {
    let program = r#"
let interfaces =
  run! printf "one\ntwo\n"
  | .lines()
"#;

    let program = rill::parser::parse(program).expect("parse");
    assert_eq!(program.stmts.len(), 1);
    assert!(matches!(
        expression(&program.stmts[0]),
        Expr::Pipe {
            kind: PipeKind::Byte,
            ..
        }
    ));
}

#[test]
fn implicit_callback_expression_parses() {
    let program = r#"[{score: 8}] |> .filter(.score > 5)"#;
    rill::parser::parse(program).expect("parse");
}

#[test]
fn multi_parameter_and_block_closures_parse() {
    rill::parser::parse(r#"let f = |left, right| { left + right }"#).expect("parse");
}

#[test]
fn result_annotations_preserve_error_type() {
    rill::parser::parse(r#"fn load() -> Result[Str, Error] { "ok" }"#).expect("parse");
}

#[test]
fn tuple_literals_patterns_and_annotations_parse() {
    let program = rill::parser::parse(
        r#"
let pair: Tuple[Int, Str] = (1, "two")
let (number, _) = pair
(number, pair, true,)
"#,
    )
    .expect("tuple program should parse");

    assert!(matches!(
        &program.stmts[0],
        Stmt::Let {
            type_name: Some(rill::ast::TypeName::Tuple(elements)),
            expr,
            ..
        } if elements.len() == 2
            && matches!(expr.unspanned(), Expr::Tuple(values) if values.len() == 2)
    ));
    assert!(matches!(
        &program.stmts[1],
        Stmt::Let {
            pattern: rill::ast::LetPattern::Tuple(names),
            ..
        } if names == &["number", "_"]
    ));
    assert!(matches!(
        expression(&program.stmts[2]),
        Expr::Tuple(values) if values.len() == 3
    ));

    for source in ["()", "(1,)", "let (): Tuple[Int, Str] = (1, 2)", "let x: Tuple[Int] = (1, 2)"] {
        assert!(rill::parser::parse(source).is_err(), "{source}");
    }
}

#[test]
fn destructuring_for_and_if_headers_parse() {
    let program = r#"
for let (index, value) in values {
    print value
}
if let { name: value } = record {
    print value
}
"#;
    let program = rill::parser::parse(program).expect("destructuring headers should parse");
    assert!(matches!(
        &program.stmts[0],
        Stmt::Expr(Expr::For { pattern: rill::ast::LetPattern::Tuple(names), .. })
            if names == &["index", "value"]
    ));
    assert!(matches!(
        &program.stmts[1],
        Stmt::If { pattern: Some(rill::ast::LetPattern::Map(pairs)), .. }
            if pairs == &[("name".to_string(), Some("value".to_string()))]
    ));
}
