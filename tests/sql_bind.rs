//! How `sql.*` turns interpolated queries into SQL text and bind parameters.
//! No database needed: `sql.statement` shows what a query would send.

use rill::types::Value;

fn run(source: &str) -> Value {
    rill::run_source(source, Vec::new()).expect(source)
}

fn run_err(source: &str) -> String {
    rill::run_source(source, Vec::new()).expect_err(source).to_string()
}

fn statement(source: &str) -> (String, Vec<Value>) {
    let Value::Map(map) = run(source) else { panic!("{source}: not a map") };
    let Some(Value::Str(text)) = map.get("text") else { panic!("no text") };
    let Some(Value::List(params)) = map.get("params") else { panic!("no params") };
    (text.clone(), params.to_vec())
}

#[test]
fn interpolations_become_parameters() {
    let (text, params) = statement(
        r#"
        let project = "ops'; drop table servers; --"
        let min = 3
        sql.statement("select * from servers where project = $project and vcpus >= {min + 1}")
        "#,
    );
    assert_eq!(text, "select * from servers where project = $1 and vcpus >= $2");
    assert_eq!(params, vec![Value::Str("ops'; drop table servers; --".into()), Value::Int(4)]);
}

#[test]
fn env_fallback_and_defaults_bind_too() {
    let (text, params) = statement(
        r#"
        with env { RILL_SQL_PROBE = "from-env" } {
            sql.statement("select ${RILL_SQL_PROBE}, ${RILL_SQL_UNSET:-fallback}")
        }
        "#,
    );
    assert_eq!(text, "select $1, $2");
    assert_eq!(params, vec![Value::Str("from-env".into()), Value::Str("fallback".into())]);
}

#[test]
fn identifiers_and_raw_fragments_are_spliced() {
    let (text, params) = statement(
        r#"
        let table = "audit log"
        let id = 7
        sql.statement("select count(*) from {sql.ident(table)} where {sql.raw("true")} and id = {id}")
        "#,
    );
    assert_eq!(text, r#"select count(*) from "audit log" where true and id = $1"#);
    assert_eq!(params, vec![Value::Int(7)]);
    assert_eq!(
        statement(r#"sql.statement("select * from {sql.ident("public.my\"t")}")"#).0,
        r#"select * from "public"."my""t""#
    );
}

#[test]
fn queries_without_interpolation_pass_through() {
    let (text, params) = statement(r#"sql.statement("select $1::int, '\{\"a\": 1}'::jsonb")"#);
    assert_eq!(text, r#"select $1::int, '{"a": 1}'::jsonb"#);
    assert!(params.is_empty());
    let (text, _) = statement(r#"let q = "select {1}"; sql.statement(q)"#);
    assert_eq!(text, "select 1", "a variable holds already-interpolated text");
}

#[test]
fn interpolating_inside_sql_strings_is_refused() {
    let error = run_err(r#"let x = "a"; sql.statement("select * from t where name like '%{x}%'")"#);
    assert!(error.contains("inside a quoted SQL string"), "{error}");
    let error = run_err(r#"let x = "a"; sql.statement("select 1 -- {x}")"#);
    assert!(error.contains("inside a SQL comment"), "{error}");
}

#[test]
fn query_types_check() {
    let program = rill::parser::parse(
        r#"
        fn report(db: Db) -> List[Map] {
            let n = 3
            db.query("select * from t where n = {n}")
        }
        fn first(db: Db) -> Option[Map] { db.one("select 1 as one") }
        fn changed(db: Db) -> Int { db.exec(r"delete from t where id = $1", [1]) }
        "#,
    )
    .unwrap();
    rill::typeck::check(&program).expect("Db methods are typed");
    let program = rill::parser::parse("fn f(db: Db) { db.query(1) }").unwrap();
    assert!(rill::typeck::check(&program).is_err(), "the query must be a Str");
}

#[cfg(not(feature = "postgres"))]
#[test]
fn connect_without_the_driver_explains_itself() {
    let error = run_err("sql.connect()");
    assert!(error.contains("built without"), "{error}");
}
