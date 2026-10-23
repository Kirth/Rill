//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

/// The `rill` binary under test, with colour disabled.
pub fn rill() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rill"));
    command.env("NO_COLOR", "1");
    command
}

/// Wait for a child, killing it if it runs longer than `limit`.
pub fn bounded_output_within(mut child: Child, limit: Duration) -> Output {
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > limit {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("CLI waited for input or failed to finish within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

/// Wait at most five seconds for a child to finish.
pub fn bounded_output(child: Child) -> Output {
    bounded_output_within(child, Duration::from_secs(5))
}
