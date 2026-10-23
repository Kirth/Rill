//! Conformance fixtures: every `tests/fixtures/scripts/<name>.ril` is run with
//! `rill` and its stdout, stderr and exit status are compared with
//! `<name>.out`, `<name>.err` and `<name>.code` (both optional: an absent
//! `.err` means empty stderr, an absent `.code` means exit status 0).  `<name>.in` is piped to stdin and `<name>.args`
//! holds one argument per line.
//!
//! Run with `UPDATE_EXPECT=1` to rewrite the expectation files.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scripts");
    let mut scripts: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ril"))
        .collect();
    scripts.sort();
    scripts
}

fn sidecar(script: &Path, extension: &str) -> Option<String> {
    std::fs::read_to_string(script.with_extension(extension)).ok()
}

struct Outcome {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_fixture(script: &Path) -> Outcome {
    let args: Vec<String> = sidecar(script, "args")
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let input = sidecar(script, "in");
    // Run by file name from the fixture directory so diagnostics print a
    // relative path and the expectations stay portable.
    let mut child = common::rill()
        .arg(script.file_name().unwrap())
        .args(&args)
        .current_dir(script.parent().unwrap())
        .env("RILL_FIXTURE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        if let Some(input) = input {
            stdin.write_all(input.as_bytes()).unwrap();
        }
    }
    let output = common::bounded_output_within(child, Duration::from_secs(10));
    Outcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code().unwrap_or(-1),
    }
}

#[test]
fn conformance_scripts_produce_expected_output() {
    let update = std::env::var_os("UPDATE_EXPECT").is_some();
    let mut failures = Vec::new();
    let scripts = fixtures();
    assert!(!scripts.is_empty(), "no conformance scripts found");
    for script in &scripts {
        let outcome = run_fixture(script);
        let name = script.file_name().unwrap().to_string_lossy().into_owned();
        if update {
            // Always write all three sidecars (never delete), so a rerun
            // overwrites stale expectations in place.
            std::fs::write(script.with_extension("out"), &outcome.stdout).unwrap();
            std::fs::write(script.with_extension("err"), &outcome.stderr).unwrap();
            std::fs::write(script.with_extension("code"), format!("{}\n", outcome.code)).unwrap();
            continue;
        }
        let expected_out = sidecar(script, "out").unwrap_or_else(|| {
            panic!("{name}: missing .out file (run with UPDATE_EXPECT=1 to create it)")
        });
        let expected_err = sidecar(script, "err").unwrap_or_default();
        let expected_code =
            sidecar(script, "code").map(|text| text.trim().parse::<i32>().unwrap()).unwrap_or(0);
        let mut problems = Vec::new();
        if outcome.stdout != expected_out {
            problems.push(format!(
                "stdout differs\n--- expected\n{expected_out}--- actual\n{}",
                outcome.stdout
            ));
        }
        if outcome.stderr != expected_err {
            problems.push(format!(
                "stderr differs\n--- expected\n{expected_err}--- actual\n{}",
                outcome.stderr
            ));
        }
        if outcome.code != expected_code {
            problems.push(format!("exit code {} (expected {expected_code})", outcome.code));
        }
        if !problems.is_empty() {
            failures.push(format!("== {name}\n{}", problems.join("\n")));
        }
    }
    assert!(failures.is_empty(), "{} fixture(s) failed:\n{}", failures.len(), failures.join("\n"));
}
