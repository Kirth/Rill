//! `sql.*` against a real Postgres.  Set `RILL_TEST_DATABASE_URL` (or run
//! `just test-sql`, which starts a throwaway server); without it these
//! tests pass without doing anything.
#![cfg(feature = "postgres")]

use rill::types::{Map, Value};
use std::collections::BTreeMap;

fn database() -> Option<String> {
    let url = std::env::var("RILL_TEST_DATABASE_URL").ok().filter(|u| !u.is_empty());
    if url.is_none() {
        eprintln!("RILL_TEST_DATABASE_URL is not set; skipping");
    }
    url
}

fn environment(url: &str) -> BTreeMap<String, String> {
    let mut env = rill::eval::process_environment();
    env.insert("RILL_TEST_DATABASE_URL".into(), url.into());
    env
}

/// Run `body` with `db` connected; each test gets its own connection, so
/// temporary tables never collide.
fn with_db(body: &str) -> Option<Result<Value, String>> {
    let url = database()?;
    let source = format!("let db = sql.connect(\"$RILL_TEST_DATABASE_URL\")\n{body}");
    Some(
        rill::run_source_with_environment(&source, Vec::new(), None, environment(&url))
            .map_err(|e| format!("{e:?}")),
    )
}

fn ok(body: &str) -> Option<Value> {
    with_db(body).map(|result| result.unwrap_or_else(|e| panic!("{body}\n{e}")))
}

fn err(body: &str) -> Option<String> {
    with_db(body).map(|result| result.expect_err(body))
}

fn record(fields: &[(&str, Value)]) -> Value {
    Value::map(fields.iter().map(|(k, v)| (k.to_string(), v.clone())).collect::<Map>())
}

#[test]
fn interpolated_values_are_bound_not_spliced() {
    let Some(value) = ok(r#"
        db.exec("create temp table users (id int, name text)")
        let evil = "x'); drop table users; --"
        db.exec("insert into users values ({1}, $evil)")
        let id = 1
        db.query("select id, name from users where id = $id")
    "#) else {
        return;
    };
    assert_eq!(
        value,
        Value::list(vec![record(&[
            ("id", Value::Int(1)),
            ("name", Value::Str("x'); drop table users; --".into()))
        ])])
    );
}

#[test]
fn one_scalar_exec_and_batch() {
    let Some(value) = ok(r#"
        db.batch("create temp table t (n int); insert into t values (1), (2), (3)")
        let changed = db.exec("update t set n = n * 10 where n >= {2}")
        [changed, db.scalar("select sum(n) from t"), db.one("select n from t where n = {99}"),
         db.one("select n from t order by n").unwrap().n, db.scalar("select 1 where false")]
    "#) else {
        return;
    };
    assert_eq!(
        value,
        Value::list(vec![
            Value::Int(2),
            Value::Int(51),
            Value::Option(None),
            Value::Int(1),
            Value::Null
        ])
    );
}

#[test]
fn explicit_params_for_non_literal_queries() {
    let Some(value) = ok(r#"
        let q = r"select $1::int + $2::int as total"
        db.one(q, [40, 2]).unwrap().total
    "#) else {
        return;
    };
    assert_eq!(value, Value::Int(42));
}

#[test]
fn types_round_trip() {
    let Some(value) = ok(r#"
        let t = "2026-10-15T14:29:21Z".parse_time()
        db.one("select {true}::bool as b, {7}::int2 as i2, {7}::int4 as i4, {7}::int8 as i8,
                       {1.5}::float4 as f4, {1.5}::float8 as f8, {"12.25"}::numeric as num,
                       {-3}::numeric as neg, {"héllo"} as txt, {"abc"}::bytea as raw,
                       {t}::timestamptz as tz, {t}::timestamp as ts, {t}::date as d,
                       {90m}::interval as span,
                       {"a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"}::uuid as id,
                       {"10.0.0.1"}::inet as ip, {"10.0.0.0/8"}::cidr as net,
                       {{a: [1, 2], b: null}}::jsonb as doc,
                       {[1, 2, 3]}::int[] as ints, array['x', null] as texts,
                       {null}::text as nothing").unwrap()
    "#) else {
        return;
    };
    let Value::Map(row) = value else { panic!("{value:?}") };
    let time = chrono::DateTime::parse_from_rfc3339("2026-10-15T14:29:21Z").unwrap().to_utc();
    let midnight = chrono::DateTime::parse_from_rfc3339("2026-10-15T00:00:00Z").unwrap().to_utc();
    let expected = [
        ("b", Value::Bool(true)),
        ("i2", Value::Int(7)),
        ("i4", Value::Int(7)),
        ("i8", Value::Int(7)),
        ("f4", Value::Float(1.5)),
        ("f8", Value::Float(1.5)),
        ("num", Value::Float(12.25)),
        ("neg", Value::Float(-3.0)),
        ("txt", Value::Str("héllo".into())),
        ("raw", Value::Bytes(b"abc".to_vec())),
        ("tz", Value::Time(time)),
        ("ts", Value::Time(time)),
        ("d", Value::Time(midnight)),
        ("span", Value::Duration(std::time::Duration::from_secs(90 * 60))),
        ("id", Value::Str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into())),
        ("ip", Value::Str("10.0.0.1".into())),
        ("net", Value::Str("10.0.0.0/8".into())),
        (
            "doc",
            record(&[("a", Value::list(vec![Value::Int(1), Value::Int(2)])), ("b", Value::Null)]),
        ),
        ("ints", Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)])),
        ("texts", Value::list(vec![Value::Str("x".into()), Value::Null])),
        ("nothing", Value::Null),
    ];
    for (name, value) in expected {
        assert_eq!(row.get(name), Some(&value), "{name}");
    }
    let names: Vec<&str> = row.keys().map(String::as_str).collect();
    assert_eq!(names[..3], ["b", "i2", "i4"], "columns keep their order");
}

#[test]
fn lists_bind_as_arrays_and_duplicate_columns_are_kept() {
    let Some(value) = ok(r#"
        let ids = [2, 3]
        let rows = db.query("select g as id, g * 10 as id from generate_series(1, 4) g where g = any({ids})")
        rows.map(|r| [r.id, r.id_2])
    "#) else {
        return;
    };
    assert_eq!(
        value,
        Value::list(vec![
            Value::list(vec![Value::Int(2), Value::Int(20)]),
            Value::list(vec![Value::Int(3), Value::Int(30)]),
        ])
    );
}

#[test]
fn transactions_commit_roll_back_and_nest() {
    let Some(value) = ok(r#"
        db.exec("create temp table ledger (n int)")
        db.transaction(|tx| tx.exec("insert into ledger values (1)"))
        let failed = db.transaction(|tx| {
            tx.exec("insert into ledger values (2)")
            err("changed my mind")
        })
        db.transaction(|tx| {
            tx.exec("insert into ledger values (3)")
            tx.transaction(|inner| {
                inner.exec("insert into ledger values (4)")
                err("inner only")
            })
            null
        })
        [failed.is_err(), db.query("select n from ledger order by n").map(|r| r.n)]
    "#) else {
        return;
    };
    assert_eq!(
        value,
        Value::list(vec![Value::Bool(true), Value::list(vec![Value::Int(1), Value::Int(3)])])
    );
}

#[test]
fn a_raised_error_rolls_back_and_still_raises() {
    let Some(error) = err(r#"
        db.exec("create temp table ledger (n int primary key)")
        db.transaction(|tx| {
            tx.exec("insert into ledger values (1)")
            tx.exec("insert into ledger values (1)")
        })
    "#) else {
        return;
    };
    assert!(error.contains("duplicate key"), "{error}");
    assert!(error.contains("23505"), "the SQLSTATE is in the message: {error}");

    // The same connection afterwards: back outside the transaction, row gone.
    let url = database().unwrap();
    let token = rill::eval::CancellationToken::new();
    let Ok(db) = rill::sql::Db::connect(&url, &Default::default(), &token) else {
        panic!("connect")
    };
    let mut evaluator = rill::eval::Evaluator::new(Vec::new());
    evaluator.define("db", Value::Db(db.clone()));
    let program = rill::parser::parse(
        r#"
        db.exec("create temp table ledger (n int)")
        db.transaction(|tx| { tx.exec("insert into ledger values (1)"); tx.exec("select 1/0") })
        "#,
    )
    .unwrap();
    let error = evaluator.eval_program(&program).expect_err("division by zero");
    assert!(error.to_string().contains("division by zero"), "{error}");
    let Ok(rows) = db.query("select count(*) as n from ledger", &[], &token) else { panic!() };
    assert_eq!(rows, vec![record(&[("n", Value::Int(0))])]);
}

#[test]
fn errors_name_the_problem() {
    let Some(error) = err(r#"db.query("selec 1")"#) else { return };
    assert!(error.contains("syntax error"), "{error}");
    let Some(error) = err(r#"db.query("select {"seven"}::int")"#) else { return };
    assert!(error.contains("$1"), "{error}");
    let Some(error) = err(r#"db.close(); db.query("select 1")"#) else { return };
    assert!(error.contains("closed"), "{error}");
    let Some(error) = err(r#"db.query(r"select $1::int")"#) else { return };
    assert!(error.contains("1 parameter but got 0 values"), "{error}");
}

#[test]
fn handles_print_without_secrets() {
    let Some(value) = ok(r#"[type(db), Str(db)]"#) else { return };
    let Value::List(items) = value else { panic!() };
    assert_eq!(items[0], Value::Str("Db".into()));
    let text = items[1].as_string_lossy();
    assert!(text.starts_with("<Db postgres://"), "{text}");
}

#[test]
fn connect_reads_the_evaluators_environment() {
    let Some(url) = database() else { return };
    let source = r#"with env { DATABASE_URL = "$RILL_TEST_DATABASE_URL" } { sql.connect().scalar("select 41 + 1") }"#;
    let value =
        rill::run_source_with_environment(source, Vec::new(), None, environment(&url)).unwrap();
    assert_eq!(value, Value::Int(42));
}

#[test]
fn cancellation_stops_a_running_query() {
    let Some(url) = database() else { return };
    let program = rill::parser::parse(
        r#"let db = sql.connect("$RILL_TEST_DATABASE_URL")
           db.query("select pg_sleep(30)")"#,
    )
    .unwrap();
    let token = rill::eval::CancellationToken::new();
    let cancel = token.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        cancel.cancel();
    });
    let started = std::time::Instant::now();
    let mut evaluator =
        rill::eval::Evaluator::with_environment(Vec::new(), None, environment(&url));
    evaluator.set_cancellation(token);
    let result = evaluator.eval_program(&program);
    assert!(result.is_err(), "cancelled");
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());

    // The server stopped it too, rather than sleeping on.
    let source = r#"sql.connect("$RILL_TEST_DATABASE_URL").scalar("select count(*) from pg_stat_activity
        where state = 'active' and query like '%pg_sleep(30)%' and pid <> pg_backend_pid()")"#;
    let value =
        rill::run_source_with_environment(source, Vec::new(), None, environment(&url)).unwrap();
    assert_eq!(value, Value::Int(0));
}

#[test]
fn a_host_can_hand_a_script_its_own_handle() {
    let Some(url) = database() else { return };
    let token = rill::eval::CancellationToken::new();
    let db =
        rill::sql::Db::connect(&url, &Default::default(), &token).unwrap_or_else(|e| match e {
            rill::blocking::Error::Failed(e) => panic!("{e:?}"),
            rill::blocking::Error::Cancelled => panic!("cancelled"),
        });
    let rows = db
        .query("select $1::int * 2 as n", &[Value::Int(21)], &token)
        .unwrap_or_else(|_| panic!("query"));
    assert_eq!(rows, vec![record(&[("n", Value::Int(42))])]);

    let mut evaluator = rill::eval::Evaluator::new(Vec::new());
    evaluator.define("db", Value::Db(db));
    let program = rill::parser::parse(r#"db.scalar("select 'from the host'")"#).unwrap();
    let mut session = rill::typeck::Session::new();
    session.define("db", &evaluator.binding("db").unwrap());
    session.check(&program).expect("the checker knows db");
    let value = evaluator.eval_program(&program).unwrap();
    assert_eq!(value, Value::Str("from the host".into()));
}
