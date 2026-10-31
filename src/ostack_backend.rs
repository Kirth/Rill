//! The extension point behind the `ostack.*` namespace.
//!
//! The namespace always type-checks: its functions, methods and `OS…` records
//! are part of the core (`methods.rs`, `records.rs`, `ostack.ril`).  What runs
//! a call is a [`Backend`] that a front end registers once per process before
//! evaluating anything.  The core crate ships none, so it builds without
//! Kirth's OpenStack client; `ostack-rill/` in this repository registers the
//! real one and builds a `rill` binary with it.  Without a backend every call
//! raises an error that says so.

use crate::eval::CancellationToken;
use crate::types::{Map, Value};
use std::sync::OnceLock;

pub use crate::blocking::Error;
pub use crate::eval_data::json_to_value;

/// One `ostack.name(...)` call from the evaluator.
pub struct Call<'a> {
    pub name: &'static str,
    pub positional: Vec<Value>,
    pub named: Vec<(String, Value)>,
    /// The evaluator's environment (`OS_*`, `RILL_OPENSTACK_*`).
    pub lookup_env: &'a dyn Fn(&str) -> Option<String>,
    pub cancellation: CancellationToken,
}

/// What runs `ostack.*` calls and record relations (`server.ports()`, …).
pub trait Backend: Send + Sync + 'static {
    /// `ostack.name(...)`.
    fn call(&self, call: Call<'_>) -> Result<Value, Error>;

    /// `method` on a record of `kind` (see `methods::ostack_relations`).
    fn relation_call(
        &self,
        kind: &str,
        method: &str,
        record: Map,
        lookup_env: &dyn Fn(&str) -> Option<String>,
        cancellation: CancellationToken,
    ) -> Result<Value, Error>;
}

static BACKEND: OnceLock<Box<dyn Backend>> = OnceLock::new();

/// Install the process-wide backend.  The first registration wins; a later
/// one returns `false` and is dropped.
pub fn register(backend: Box<dyn Backend>) -> bool {
    BACKEND.set(backend).is_ok()
}

/// The registered backend, if any.
pub fn backend() -> Option<&'static dyn Backend> {
    BACKEND.get().map(|backend| backend.as_ref())
}
