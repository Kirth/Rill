//! One shared tokio runtime for namespaces that talk to the network
//! (`ostack.*`, `sql.*`), and the bridge that lets the synchronous evaluator
//! wait on it.

use crate::diagnostic::RillError;
use crate::eval::CancellationToken;
use crate::types::ErrorKind;
use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

/// Why a [`run`] did not produce a value.
pub enum Error {
    Cancelled,
    Failed(RillError),
}

impl From<RillError> for Error {
    fn from(error: RillError) -> Self {
        Error::Failed(error)
    }
}

pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("rill-io")
            .enable_all()
            .build()
            .expect("tokio runtime for network namespaces")
    })
}

/// Seconds from an environment variable, or `default`.
pub fn env_secs(name: &str, default: u64) -> Duration {
    Duration::from_secs(std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default))
}

/// Run `future` on the shared runtime and wait for it here, giving up when
/// the evaluator is cancelled or the call takes longer than `timeout`
/// (`on_timeout` names the knob in the error).  The future is spawned rather
/// than blocked on, so this works from inside rayon workers and from threads
/// that already run a tokio runtime.
pub fn run<T: Send + 'static>(
    future: impl Future<Output = Result<T, RillError>> + Send + 'static,
    cancellation: &CancellationToken,
    timeout: Duration,
    on_timeout: impl FnOnce(Duration) -> String + Send + 'static,
) -> Result<T, Error> {
    let cancellation = cancellation.clone();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    runtime().spawn(async move {
        let result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(Error::Cancelled),
            outcome = tokio::time::timeout(timeout, future) => match outcome {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(Error::Failed(error)),
                Err(_) => Err(Error::Failed(
                    RillError::new(on_timeout(timeout)).with_kind(ErrorKind::Io),
                )),
            },
        };
        let _ = sender.send(result);
    });
    receiver.recv().unwrap_or_else(|_| Err(Error::Failed(RillError::new("the request task died"))))
}
