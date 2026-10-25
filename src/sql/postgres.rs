//! Postgres through `tokio-postgres`, on the shared runtime in
//! [`crate::blocking`].
//!
//! Parameters are converted to the type the server inferred for each `$n`
//! when it prepared the statement, so `{42}` binds as `int4`, `numeric` or
//! `text` as the query needs.  Rows come back as records in column order.

use crate::blocking::{self, Error};
use crate::diagnostic::RillError;
use crate::eval::CancellationToken;
use crate::eval_data::{json_to_value, value_to_json};
use crate::types::{ErrorKind, Map, Value};
use bytes::{BufMut, BytesMut};
use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio_postgres::config::{Host, SslMode};
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::types::{FromSql, IsNull, Kind, ToSql, Type};
use tokio_postgres::{CancelToken, Client, Config, Row, Statement};
use tokio_postgres_rustls::MakeRustlsConnect;

type BoxError = Box<dyn std::error::Error + Sync + Send>;

/// How a connection uses TLS (libpq's `sslmode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    Disable,
    /// TLS when the server offers it, without checking the certificate.
    Prefer,
    /// TLS always, without checking the certificate (as libpq's `require`).
    Require,
    /// TLS always, with the certificate checked against the web PKI roots.
    Verify,
}

impl FromStr for TlsMode {
    type Err = RillError;

    fn from_str(text: &str) -> Result<Self, RillError> {
        match text {
            "disable" | "allow" => Ok(TlsMode::Disable),
            "prefer" => Ok(TlsMode::Prefer),
            "require" => Ok(TlsMode::Require),
            "verify" | "verify-ca" | "verify-full" => Ok(TlsMode::Verify),
            other => Err(RillError::new(format!("sql: unknown tls mode '{other}'"))
                .with_help("use disable, prefer, require or verify")),
        }
    }
}

/// Settings for [`Db::connect`] beyond the URL.
#[derive(Clone, Debug, Default)]
pub struct ConnectOptions {
    pub password: Option<String>,
    pub tls: Option<TlsMode>,
    /// Per-statement limit; `RILL_SQL_TIMEOUT_SECS` or 300s by default.
    pub timeout: Option<Duration>,
    /// `application_name`, shown in `pg_stat_activity`; `rill` by default.
    pub app: Option<String>,
}

/// An open connection.  Clones share it; it closes when the last clone is
/// dropped or on [`Db::close`].
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    client: RwLock<Option<Arc<Client>>>,
    cancel: CancelToken,
    tls: Tls,
    label: String,
    timeout: Duration,
    depth: AtomicUsize,
}

impl PartialEq for Db {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl fmt::Debug for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Db").field(&self.inner.label).finish()
    }
}

impl fmt::Display for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let closed = if self.is_closed() { " (closed)" } else { "" };
        write!(f, "<Db {}{closed}>", self.inner.label)
    }
}

impl Db {
    /// Connect to `url` (`postgres://user:pass@host:port/db?sslmode=…` or
    /// `host=… user=…`).  Parts the URL leaves out come from the process's
    /// `PG*` variables, and an empty URL means `DATABASE_URL`.
    pub fn connect(
        url: &str,
        options: &ConnectOptions,
        cancellation: &CancellationToken,
    ) -> Result<Db, Error> {
        let url = (!url.is_empty()).then_some(url);
        Db::connect_with_env(url, options, &|name| std::env::var(name).ok(), cancellation)
    }

    /// [`Db::connect`] with the environment read through `lookup_env` (the
    /// evaluator's, so `with env {}` applies).
    pub fn connect_with_env(
        url: Option<&str>,
        options: &ConnectOptions,
        lookup_env: &dyn Fn(&str) -> Option<String>,
        cancellation: &CancellationToken,
    ) -> Result<Db, Error> {
        let (config, mode) = config(url, options, lookup_env)?;
        let tls = Tls(MakeRustlsConnect::new(tls_config(mode)?));
        let label = label(&config);
        let timeout =
            options.timeout.unwrap_or_else(|| blocking::env_secs("RILL_SQL_TIMEOUT_SECS", 300));
        let connector = tls.clone();
        let target = label.clone();
        let client = blocking::run(
            async move {
                let (client, connection) = config
                    .connect(connector)
                    .await
                    .map_err(|e| pg_error(e).with_help(format!("connecting to {target}")))?;
                tokio::spawn(async move {
                    let _ = connection.await;
                });
                Ok(client)
            },
            cancellation,
            timeout,
            |t| format!("sql: could not connect within {}s", t.as_secs()),
        )?;
        Ok(Db {
            inner: Arc::new(Inner {
                cancel: client.cancel_token(),
                client: RwLock::new(Some(Arc::new(client))),
                tls,
                label,
                timeout,
                depth: AtomicUsize::new(0),
            }),
        })
    }

    /// The connection target without its password: `postgres://user@host:5432/db`.
    pub fn url(&self) -> &str {
        &self.inner.label
    }

    pub fn is_closed(&self) -> bool {
        self.inner.client.read().map(|c| c.is_none()).unwrap_or(true)
    }

    /// Close the connection for every clone; statements already running finish.
    pub fn close(&self) {
        if let Ok(mut client) = self.inner.client.write() {
            client.take();
        }
    }

    /// Rows as records, columns in order.
    pub fn query(
        &self,
        sql: &str,
        params: &[Value],
        cancellation: &CancellationToken,
    ) -> Result<Vec<Value>, Error> {
        let (sql, params) = (sql.to_string(), params.to_vec());
        self.run(cancellation, move |client| async move {
            let statement = client.prepare(&sql).await.map_err(pg_error)?;
            let params = bind(&statement, params)?;
            let rows = client.query(&statement, &refs(&params)).await.map_err(pg_error)?;
            rows.iter().map(row_value).collect()
        })
    }

    /// The number of rows the statement affected.
    pub fn exec(
        &self,
        sql: &str,
        params: &[Value],
        cancellation: &CancellationToken,
    ) -> Result<u64, Error> {
        let (sql, params) = (sql.to_string(), params.to_vec());
        self.run(cancellation, move |client| async move {
            let statement = client.prepare(&sql).await.map_err(pg_error)?;
            let params = bind(&statement, params)?;
            client.execute(&statement, &refs(&params)).await.map_err(pg_error)
        })
    }

    /// Several `;`-separated statements without parameters (a schema file).
    pub fn batch(&self, sql: &str, cancellation: &CancellationToken) -> Result<(), Error> {
        let sql = sql.to_string();
        self.run(cancellation, move |client| async move {
            client.batch_execute(&sql).await.map_err(pg_error)
        })
    }

    /// Start a transaction, or a savepoint inside one.
    pub fn begin(&self, cancellation: &CancellationToken) -> Result<(), Error> {
        let depth = self.inner.depth.fetch_add(1, Ordering::SeqCst);
        let sql = if depth == 0 { "BEGIN".to_string() } else { format!("SAVEPOINT rill_{depth}") };
        self.batch(&sql, cancellation).inspect_err(|_| {
            self.inner.depth.fetch_sub(1, Ordering::SeqCst);
        })
    }

    /// End the innermost [`Db::begin`]: commit (release) or roll back.  Runs
    /// even after the evaluator was cancelled, so the connection is never
    /// left inside a transaction.
    pub fn finish(&self, commit: bool) -> Result<(), RillError> {
        let depth = self.inner.depth.fetch_sub(1, Ordering::SeqCst).saturating_sub(1);
        let sql = match (depth, commit) {
            (0, true) => "COMMIT".to_string(),
            (0, false) => "ROLLBACK".to_string(),
            (d, true) => format!("RELEASE SAVEPOINT rill_{d}"),
            (d, false) => format!("ROLLBACK TO SAVEPOINT rill_{d}; RELEASE SAVEPOINT rill_{d}"),
        };
        match self.batch(&sql, &CancellationToken::new()) {
            Ok(()) => Ok(()),
            Err(Error::Failed(error)) => Err(error),
            Err(Error::Cancelled) => Err(RillError::new("sql: cancelled")),
        }
    }

    fn client(&self) -> Result<Arc<Client>, RillError> {
        let client = self.inner.client.read().ok().and_then(|c| c.clone());
        let closed = client.as_ref().map_or(true, |c| c.is_closed());
        match client {
            Some(client) if !closed => Ok(client),
            _ => Err(RillError::new(format!(
                "sql: the connection to {} is closed",
                self.inner.label
            ))
            .with_kind(ErrorKind::Io)),
        }
    }

    /// Run a statement, and ask the server to stop it when the evaluator
    /// gives up on it (cancelled or timed out).
    fn run<T, F, Fut>(&self, cancellation: &CancellationToken, work: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Client>) -> Fut,
        Fut: Future<Output = Result<T, RillError>> + Send + 'static,
    {
        let future = work(self.client()?);
        let done = Arc::new(AtomicBool::new(false));
        let finished = Arc::clone(&done);
        let result = blocking::run(
            async move {
                let result = future.await;
                finished.store(true, Ordering::SeqCst);
                result
            },
            cancellation,
            self.inner.timeout,
            |t| {
                format!(
                    "sql: no answer within {}s (timeout= on sql.connect, or RILL_SQL_TIMEOUT_SECS)",
                    t.as_secs()
                )
            },
        );
        if result.is_err() && !done.load(Ordering::SeqCst) {
            // Wait (briefly) for the cancel request to go out, so the server
            // has stopped the statement by the time the script moves on.
            let (token, tls) = (self.inner.cancel.clone(), self.inner.tls.clone());
            let _ = blocking::run(
                async move { token.cancel_query(tls).await.map_err(pg_error) },
                &CancellationToken::new(),
                Duration::from_secs(2),
                |_| String::new(),
            );
        }
        result
    }
}

// ---------------------------------------------------------------- connecting

fn config(
    url: Option<&str>,
    options: &ConnectOptions,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(Config, TlsMode), RillError> {
    let env = |name: &str| env(name).filter(|v| !v.is_empty());
    let url = url.map(str::to_string).or_else(|| env("DATABASE_URL"));
    let mut mode = None;
    let mut config = match url {
        Some(url) => {
            // tokio-postgres knows disable/prefer/require; checking the
            // certificate is ours.
            let mut url = url;
            for verify in ["verify-full", "verify-ca"] {
                if url.contains(&format!("sslmode={verify}")) {
                    url = url.replace(&format!("sslmode={verify}"), "sslmode=require");
                    mode = Some(TlsMode::Verify);
                }
            }
            Config::from_str(&url).map_err(|e| {
                RillError::new(format!("sql: bad connection string: {e}"))
                    .with_help("postgres://user:password@host:5432/dbname?sslmode=prefer")
            })?
        }
        None => {
            let mut config = Config::new();
            if let Some(host) = env("PGHOST") {
                config.host(&host);
            }
            if let Some(port) = env("PGPORT").and_then(|p| p.parse().ok()) {
                config.port(port);
            }
            if let Some(db) = env("PGDATABASE") {
                config.dbname(&db);
            }
            if let Some(user) = env("PGUSER") {
                config.user(&user);
            }
            if let Some(ssl) = env("PGSSLMODE") {
                mode = Some(ssl.parse()?);
            }
            config
        }
    };
    if config.get_user().is_none() {
        if let Some(user) = env("PGUSER").or_else(|| env("USER")) {
            config.user(&user);
        }
    }
    if let Some(password) = options.password.clone().or_else(|| env("PGPASSWORD")) {
        if config.get_password().is_none() || options.password.is_some() {
            config.password(password);
        }
    }
    if config.get_hosts().is_empty() {
        let port = config.get_ports().first().copied().unwrap_or(5432);
        let socket = ["/run/postgresql", "/var/run/postgresql", "/tmp"]
            .into_iter()
            .find(|dir| std::path::Path::new(&format!("{dir}/.s.PGSQL.{port}")).exists());
        match socket {
            #[cfg(unix)]
            Some(dir) => {
                config.host_path(dir);
            }
            _ => {
                config.host("localhost");
            }
        }
    }
    config.application_name(options.app.as_deref().unwrap_or("rill"));
    config.connect_timeout(Duration::from_secs(10));
    let mode = options.tls.or(mode).unwrap_or(match config.get_ssl_mode() {
        SslMode::Disable => TlsMode::Disable,
        SslMode::Require => TlsMode::Require,
        _ => TlsMode::Prefer,
    });
    let unix = config.get_hosts().iter().all(|h| !matches!(h, Host::Tcp(_)));
    config.ssl_mode(match mode {
        _ if unix => SslMode::Disable,
        TlsMode::Disable => SslMode::Disable,
        TlsMode::Prefer => SslMode::Prefer,
        TlsMode::Require | TlsMode::Verify => SslMode::Require,
    });
    Ok((config, mode))
}

/// `postgres://user@host:port/db`, for messages and display.
fn label(config: &Config) -> String {
    let user = config.get_user().map(|u| format!("{u}@")).unwrap_or_default();
    let host = match config.get_hosts().first() {
        Some(Host::Tcp(host)) => host.clone(),
        #[cfg(unix)]
        Some(Host::Unix(path)) => path.display().to_string(),
        None => String::new(),
    };
    let port = config.get_ports().first().map(|p| format!(":{p}")).unwrap_or_default();
    let db = config.get_dbname().unwrap_or_default();
    format!("postgres://{user}{host}{port}/{db}")
}

fn tls_config(mode: TlsMode) -> Result<rustls::ClientConfig, RillError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| RillError::new(format!("sql: tls: {e}")))?;
    let config = if mode == TlsMode::Verify {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        builder.with_root_certificates(roots).with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
            .with_no_client_auth()
    };
    Ok(config)
}

/// The rustls connector, which tokio-postgres also asks for a Unix socket
/// (with an empty host name it would reject) even though TLS is off there.
#[derive(Clone)]
struct Tls(MakeRustlsConnect);

impl<S> MakeTlsConnect<S> for Tls
where
    MakeRustlsConnect: MakeTlsConnect<S>,
{
    type Stream = <MakeRustlsConnect as MakeTlsConnect<S>>::Stream;
    type TlsConnect = <MakeRustlsConnect as MakeTlsConnect<S>>::TlsConnect;
    type Error = <MakeRustlsConnect as MakeTlsConnect<S>>::Error;

    fn make_tls_connect(&mut self, hostname: &str) -> Result<Self::TlsConnect, Self::Error> {
        self.0.make_tls_connect(if hostname.is_empty() { "localhost" } else { hostname })
    }
}

/// Encrypt without checking who is on the other end, as libpq's
/// `sslmode=require` does.
#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn pg_error(error: tokio_postgres::Error) -> RillError {
    if let Some(db) = error.as_db_error() {
        let mut help: Vec<String> = Vec::new();
        if let Some(detail) = db.detail() {
            help.push(detail.to_string());
        }
        if let Some(hint) = db.hint() {
            help.push(hint.to_string());
        }
        let message = format!("sql: {} (SQLSTATE {})", db.message(), db.code().code());
        let error = RillError::new(message).with_kind(ErrorKind::Io);
        return if help.is_empty() { error } else { error.with_help(help.join("\n")) };
    }
    let source = std::error::Error::source(&error).map(|s| format!(": {s}")).unwrap_or_default();
    RillError::new(format!("sql: {error}{source}")).with_kind(ErrorKind::Io)
}

// ---------------------------------------------------------------- parameters

/// A Rill value bound to a parameter of whatever type the server asks for.
#[derive(Debug)]
struct Param(Value);

fn bind(statement: &Statement, params: Vec<Value>) -> Result<Vec<Param>, RillError> {
    let types = statement.params();
    if types.len() != params.len() {
        return Err(RillError::new(format!(
            "sql: the query has {} parameter{} but got {} value{}",
            types.len(),
            if types.len() == 1 { "" } else { "s" },
            params.len(),
            if params.len() == 1 { "" } else { "s" },
        )));
    }
    // Encode once up front so a mismatch names its `$n`.
    let mut scratch = BytesMut::new();
    for (index, (value, ty)) in params.iter().zip(types).enumerate() {
        scratch.clear();
        Param(value.clone()).to_sql(ty, &mut scratch).map_err(|e| {
            RillError::new(format!("sql: ${}: {e}", index + 1)).with_help(format!(
                "cast in SQL to pick another type, such as ${}::text",
                index + 1
            ))
        })?;
    }
    Ok(params.into_iter().map(Param).collect())
}

fn refs(params: &[Param]) -> Vec<&(dyn ToSql + Sync)> {
    params.iter().map(|p| p as &(dyn ToSql + Sync)).collect()
}

fn mismatch(value: &Value, ty: &Type) -> BoxError {
    format!("cannot bind {} to a parameter of type {}", value.type_name(), ty.name()).into()
}

fn is_text(ty: &Type) -> bool {
    matches!(*ty, Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN)
        || matches!(ty.kind(), Kind::Enum(_))
        || ty.name() == "citext"
}

impl ToSql for Param {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, BoxError> {
        let value = &self.0;
        match value {
            Value::Null | Value::Option(None) => return Ok(IsNull::Yes),
            Value::Option(Some(inner)) => return Param((**inner).clone()).to_sql(ty, out),
            _ => {}
        }
        if let Kind::Domain(base) = ty.kind() {
            return self.to_sql(base, out);
        }
        if let Kind::Array(_) = ty.kind() {
            let Value::List(items) = value else { return Err(mismatch(value, ty)) };
            let items: Vec<Param> = items.iter().cloned().map(Param).collect();
            return items.to_sql(ty, out);
        }
        match (*ty).clone() {
            Type::BOOL => match value {
                Value::Bool(b) => b.to_sql(ty, out),
                _ => Err(mismatch(value, ty)),
            },
            Type::INT2 => int(value, ty).and_then(|i| i16::try_from(i)?.to_sql(ty, out)),
            Type::INT4 => int(value, ty).and_then(|i| i32::try_from(i)?.to_sql(ty, out)),
            Type::INT8 => int(value, ty).and_then(|i| i.to_sql(ty, out)),
            Type::OID => int(value, ty).and_then(|i| u32::try_from(i)?.to_sql(ty, out)),
            Type::FLOAT4 => float(value, ty).and_then(|f| (f as f32).to_sql(ty, out)),
            Type::FLOAT8 => float(value, ty).and_then(|f| f.to_sql(ty, out)),
            Type::NUMERIC => {
                let text = match value {
                    Value::Int(i) => i.to_string(),
                    Value::Float(f) if f.is_finite() => f.to_string(),
                    Value::Str(s) => s.trim().to_string(),
                    _ => return Err(mismatch(value, ty)),
                };
                numeric_to_sql(&text, out)?;
                Ok(IsNull::No)
            }
            Type::BYTEA => match value {
                Value::Bytes(b) => b.as_slice().to_sql(ty, out),
                Value::Str(s) => s.as_bytes().to_sql(ty, out),
                _ => Err(mismatch(value, ty)),
            },
            Type::TIMESTAMPTZ => time(value, ty).and_then(|t| t.to_sql(ty, out)),
            Type::TIMESTAMP => time(value, ty).and_then(|t| t.naive_utc().to_sql(ty, out)),
            Type::DATE => match value {
                Value::Str(s) => {
                    chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")?.to_sql(ty, out)
                }
                _ => time(value, ty).and_then(|t| t.date_naive().to_sql(ty, out)),
            },
            Type::INTERVAL => match value {
                Value::Duration(d) => {
                    out.put_i64(i64::try_from(d.as_micros())?);
                    out.put_i32(0);
                    out.put_i32(0);
                    Ok(IsNull::No)
                }
                _ => Err(mismatch(value, ty)),
            },
            Type::UUID => match value {
                Value::Str(s) => {
                    out.put_slice(&uuid_from_str(s)?);
                    Ok(IsNull::No)
                }
                _ => Err(mismatch(value, ty)),
            },
            Type::INET | Type::CIDR => match value {
                Value::Str(s) => {
                    inet_to_sql(s, *ty == Type::CIDR, out)?;
                    Ok(IsNull::No)
                }
                _ => Err(mismatch(value, ty)),
            },
            Type::JSON | Type::JSONB => value_to_json(value).to_sql(ty, out),
            _ if is_text(ty) => match value {
                Value::Str(s) => {
                    out.put_slice(s.as_bytes());
                    Ok(IsNull::No)
                }
                Value::Map(_)
                | Value::List(_)
                | Value::Tuple(_)
                | Value::Function(_)
                | Value::Db(_) => Err(mismatch(value, ty)),
                other => {
                    out.put_slice(other.as_string_lossy().as_bytes());
                    Ok(IsNull::No)
                }
            },
            _ => Err(mismatch(value, ty)),
        }
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    tokio_postgres::types::to_sql_checked!();
}

fn int(value: &Value, ty: &Type) -> Result<i64, BoxError> {
    match value {
        Value::Int(i) => Ok(*i),
        Value::Bool(b) => Ok(*b as i64),
        Value::Str(s) => Ok(s.trim().parse()?),
        _ => Err(mismatch(value, ty)),
    }
}

fn float(value: &Value, ty: &Type) -> Result<f64, BoxError> {
    match value {
        Value::Float(f) => Ok(*f),
        Value::Int(i) => Ok(*i as f64),
        Value::Str(s) => Ok(s.trim().parse()?),
        _ => Err(mismatch(value, ty)),
    }
}

fn time(value: &Value, ty: &Type) -> Result<chrono::DateTime<chrono::Utc>, BoxError> {
    match value {
        Value::Time(t) => Ok(*t),
        Value::Str(s) => Ok(chrono::DateTime::parse_from_rfc3339(s.trim())?.to_utc()),
        _ => Err(mismatch(value, ty)),
    }
}

/// A decimal string (`-12.50`) in Postgres's binary `numeric` form: base
/// 10000 digit groups around the decimal point.
fn numeric_to_sql(text: &str, out: &mut BytesMut) -> Result<(), BoxError> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole.chars().chain(fraction.chars()).all(|c| c.is_ascii_digit())
    {
        return Err(format!("'{text}' is not a decimal number").into());
    }
    let scale = u16::try_from(fraction.len())?;
    let pad_left = (4 - whole.len() % 4) % 4;
    let pad_right = (4 - fraction.len() % 4) % 4;
    let whole = format!("{}{whole}", "0".repeat(pad_left));
    let fraction = format!("{fraction}{}", "0".repeat(pad_right));
    let group = |s: &str| {
        s.as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap().parse::<i16>().unwrap())
            .collect::<Vec<_>>()
    };
    let mut groups = group(&whole);
    let mut weight = groups.len() as i16 - 1;
    groups.extend(group(&fraction));
    while groups.first() == Some(&0) {
        groups.remove(0);
        weight -= 1;
    }
    while groups.last() == Some(&0) {
        groups.pop();
    }
    if groups.is_empty() {
        weight = 0;
    }
    out.put_i16(i16::try_from(groups.len())?);
    out.put_i16(weight);
    out.put_u16(if negative && !groups.is_empty() { 0x4000 } else { 0 });
    out.put_u16(scale);
    for g in groups {
        out.put_i16(g);
    }
    Ok(())
}

fn numeric_from_sql(raw: &[u8]) -> Result<Value, BoxError> {
    let word = |i: usize| -> Result<u16, BoxError> {
        raw.get(i * 2..i * 2 + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .ok_or_else(|| "short numeric".into())
    };
    let count = word(0)? as usize;
    let weight = word(1)? as i16 as i32;
    let sign = word(2)?;
    match sign {
        0xC000 => return Ok(Value::Float(f64::NAN)),
        0xD000 => return Ok(Value::Float(f64::INFINITY)),
        0xF000 => return Ok(Value::Float(f64::NEG_INFINITY)),
        _ => {}
    }
    let mut text = String::from(if sign == 0x4000 { "-" } else { "" });
    let mut whole = String::new();
    let mut fraction = String::new();
    for i in 0..count {
        let digit = word(4 + i)?;
        let position = weight - i as i32;
        if position >= 0 {
            whole.push_str(&format!("{digit:04}"));
        } else {
            if fraction.is_empty() {
                fraction.push_str(&"0000".repeat((-position - 1) as usize));
            }
            fraction.push_str(&format!("{digit:04}"));
        }
    }
    if weight >= count as i32 {
        whole.push_str(&"0000".repeat((weight + 1 - count as i32) as usize));
    }
    text.push_str(if whole.is_empty() { "0" } else { &whole });
    text.push('.');
    text.push_str(if fraction.is_empty() { "0" } else { &fraction });
    Ok(Value::Float(text.parse()?))
}

fn uuid_from_str(text: &str) -> Result<[u8; 16], BoxError> {
    let hex: String =
        text.trim().trim_matches(|c| c == '{' || c == '}').chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return Err(format!("'{text}' is not a UUID").into());
    }
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("'{text}' is not a UUID"))?;
    }
    Ok(bytes)
}

fn uuid_text(raw: &[u8]) -> String {
    let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

fn inet_to_sql(text: &str, cidr: bool, out: &mut BytesMut) -> Result<(), BoxError> {
    let (address, bits) =
        text.trim().split_once('/').map_or((text.trim(), None), |(a, b)| (a, Some(b)));
    let address: IpAddr = address.parse()?;
    let (family, octets): (u8, Vec<u8>) = match address {
        IpAddr::V4(v4) => (2, v4.octets().to_vec()),
        IpAddr::V6(v6) => (3, v6.octets().to_vec()),
    };
    let max = (octets.len() * 8) as u8;
    let bits = match bits {
        Some(bits) => bits.parse::<u8>().ok().filter(|b| *b <= max).ok_or("bad prefix length")?,
        None => max,
    };
    out.put_u8(family);
    out.put_u8(bits);
    out.put_u8(cidr as u8);
    out.put_u8(octets.len() as u8);
    out.put_slice(&octets);
    Ok(())
}

fn inet_text(raw: &[u8], cidr: bool) -> Result<String, BoxError> {
    let [family, bits, _, len, rest @ ..] = raw else { return Err("short inet".into()) };
    let address = match (family, *len as usize, rest) {
        (2, 4, [a, b, c, d]) => IpAddr::from([*a, *b, *c, *d]),
        (3, 16, bytes) if bytes.len() == 16 => {
            IpAddr::from(<[u8; 16]>::try_from(bytes).expect("16 bytes"))
        }
        _ => return Err("bad inet".into()),
    };
    let full = if address.is_ipv4() { 32 } else { 128 };
    Ok(if *bits == full && !cidr { address.to_string() } else { format!("{address}/{bits}") })
}

// ---------------------------------------------------------------- rows

/// One column of a row as a Rill value.
struct Cell(Value);

impl<'a> FromSql<'a> for Cell {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, BoxError> {
        if let Kind::Domain(base) = ty.kind() {
            return Cell::from_sql(base, raw);
        }
        if let Kind::Array(_) = ty.kind() {
            let items = Vec::<Cell>::from_sql(ty, raw)?;
            return Ok(Cell(Value::list(items.into_iter().map(|c| c.0).collect())));
        }
        let value = match (*ty).clone() {
            Type::BOOL => Value::Bool(bool::from_sql(ty, raw)?),
            Type::INT2 => Value::Int(i16::from_sql(ty, raw)?.into()),
            Type::INT4 => Value::Int(i32::from_sql(ty, raw)?.into()),
            Type::INT8 => Value::Int(i64::from_sql(ty, raw)?),
            Type::OID => Value::Int(u32::from_sql(ty, raw)?.into()),
            Type::FLOAT4 => Value::Float(f32::from_sql(ty, raw)?.into()),
            Type::FLOAT8 => Value::Float(f64::from_sql(ty, raw)?),
            Type::NUMERIC => numeric_from_sql(raw)?,
            Type::BYTEA => Value::Bytes(raw.to_vec()),
            Type::TIMESTAMPTZ => Value::Time(chrono::DateTime::<chrono::Utc>::from_sql(ty, raw)?),
            Type::TIMESTAMP => Value::Time(chrono::NaiveDateTime::from_sql(ty, raw)?.and_utc()),
            Type::DATE => Value::Time(
                chrono::NaiveDate::from_sql(ty, raw)?
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight")
                    .and_utc(),
            ),
            Type::TIME => Value::Str(chrono::NaiveTime::from_sql(ty, raw)?.to_string()),
            Type::INTERVAL => {
                let micros = i64::from_be_bytes(raw.get(0..8).ok_or("short interval")?.try_into()?);
                let days = i32::from_be_bytes(raw.get(8..12).ok_or("short interval")?.try_into()?);
                let months =
                    i32::from_be_bytes(raw.get(12..16).ok_or("short interval")?.try_into()?);
                let total = micros + (i64::from(days) + i64::from(months) * 30) * 86_400_000_000;
                let micros = u64::try_from(total).map_err(|_| "a negative interval")?;
                Value::Duration(Duration::from_micros(micros))
            }
            Type::UUID => Value::Str(uuid_text(raw)),
            Type::INET => Value::Str(inet_text(raw, false)?),
            Type::CIDR => Value::Str(inet_text(raw, true)?),
            Type::JSON | Type::JSONB => json_to_value(serde_json::Value::from_sql(ty, raw)?),
            Type::VOID => Value::Null,
            Type::CHAR => Value::Str((i8::from_sql(ty, raw)? as u8 as char).to_string()),
            _ if is_text(ty) => Value::Str(String::from_utf8_lossy(raw).into_owned()),
            _ => return Err(format!("type {} is not supported", ty.name()).into()),
        };
        Ok(Cell(value))
    }

    fn from_sql_null(_: &Type) -> Result<Self, BoxError> {
        Ok(Cell(Value::Null))
    }

    fn accepts(_: &Type) -> bool {
        true
    }
}

fn row_value(row: &Row) -> Result<Value, RillError> {
    let mut map = Map::new();
    for (index, column) in row.columns().iter().enumerate() {
        let Cell(value) = row.try_get(index).map_err(|e| {
            let reason = std::error::Error::source(&e)
                .map(|s| s.to_string())
                .unwrap_or_else(|| e.to_string());
            RillError::new(format!(
                "sql: column {} ({}): {reason}",
                column.name(),
                column.type_().name()
            ))
            .with_help(format!("cast it in the query: {}::text", column.name()))
        })?;
        // `select a.id, b.id`: the second becomes `id_2`.
        let mut key = column.name().to_string();
        let mut n = 2;
        while map.contains_key(&key) {
            key = format!("{}_{n}", column.name());
            n += 1;
        }
        map.insert(key, value);
    }
    Ok(Value::map(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(text: &str) -> Value {
        let mut out = BytesMut::new();
        numeric_to_sql(text, &mut out).unwrap();
        numeric_from_sql(&out).unwrap()
    }

    #[test]
    fn numeric_round_trips() {
        for (text, value) in [
            ("0", 0.0),
            ("12.5", 12.5),
            ("-12.50", -12.5),
            ("100000000", 1e8),
            ("0.00012", 0.00012),
            ("123456789.9876", 123456789.9876),
        ] {
            assert_eq!(roundtrip(text), Value::Float(value), "{text}");
        }
        assert!(numeric_to_sql("12a", &mut BytesMut::new()).is_err());
    }

    #[test]
    fn uuid_and_inet_round_trip() {
        let uuid = "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11";
        assert_eq!(uuid_text(&uuid_from_str(uuid).unwrap()), uuid);
        for (text, cidr) in
            [("10.0.0.1", false), ("10.0.0.0/8", true), ("::1", false), ("fe80::/10", true)]
        {
            let mut out = BytesMut::new();
            inet_to_sql(text, cidr, &mut out).unwrap();
            assert_eq!(inet_text(&out, cidr).unwrap(), text);
        }
    }

    #[test]
    fn connection_strings_fill_gaps_from_the_environment() {
        let env = |name: &str| match name {
            "PGPASSWORD" => Some("secret".to_string()),
            "USER" => Some("kirth".to_string()),
            _ => None,
        };
        let (config, mode) = config(
            Some("postgres://db.example:6543/leaf?sslmode=verify-full"),
            &ConnectOptions::default(),
            &env,
        )
        .unwrap();
        assert_eq!(config.get_user(), Some("kirth"));
        assert_eq!(config.get_password(), Some(&b"secret"[..]));
        assert_eq!(mode, TlsMode::Verify);
        assert_eq!(label(&config), "postgres://kirth@db.example:6543/leaf");
    }
}
