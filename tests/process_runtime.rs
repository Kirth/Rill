use rill::types::Value;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("rill-{name}-{nanos}"))
}

#[test]
fn process_stress_worker() {
    if std::env::var_os("RILL_PROCESS_STRESS_WORKER").is_none() {
        return;
    }
    use rill::runtime::{
        run_command, run_pipeline, CommandIo, ProcessRuntime, ProcessSpec, TokioProcessRuntime,
    };
    let bytes = vec![b'x'; 2 * 1024 * 1024];
    let argv = vec![
        "sh".into(),
        "-c".into(),
        "head -c 1048576 /dev/zero >&2; cat".into(),
    ];
    let result = run_command(&argv, Some(bytes.clone()), &CommandIo::default()).unwrap();
    assert_eq!(result.out, bytes);
    assert_eq!(result.err.len(), 1048576);
    let handle = TokioProcessRuntime
        .spawn(ProcessSpec {
            argv,
            input: Some(bytes.clone()),
            io: CommandIo::default(),
        })
        .unwrap();
    let result = TokioProcessRuntime.await_process(handle).unwrap();
    assert_eq!(result.out, bytes);
    assert_eq!(result.err.len(), 1048576);
    let result = run_pipeline(
        &[
            vec!["yes".into()],
            vec!["head".into(), "-n".into(), "1".into()],
        ],
        &[CommandIo::default(), CommandIo::default()],
    )
    .unwrap();
    assert_eq!(result.out, b"y\n");
    assert!(run_pipeline(
        &[
            vec!["sleep".into(), "30".into()],
            vec!["/rill/nonexistent-command".into()]
        ],
        &[CommandIo::default(), CommandIo::default()]
    )
    .is_err());
}

#[test]
fn process_io_stress_finishes_within_deadline() {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "process_stress_worker", "--nocapture"])
        .env("RILL_PROCESS_STRESS_WORKER", "1")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(10) {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("process I/O deadlocked");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn merged_redirection_preserves_bytes_and_order() {
    use rill::runtime::{run_command, CommandIo};
    let output = temp_path("binary-merged");
    let result = run_command(
        &[
            "sh".into(),
            "-c".into(),
            "printf 'a'; printf 'b' >&2; printf '\\377'".into(),
        ],
        None,
        &CommandIo {
            all_file: Some(output.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(fs::read(&output).unwrap(), b"ab\xff");
    assert!(result.out.is_empty() && result.err.is_empty());
    fs::remove_file(output).unwrap();
}

fn run(source: &str) -> Value {
    rill::run_source(source, Vec::new()).expect(source)
}

#[test]
fn spawn_and_await_use_a_process_handle() {
    let value = run(r#"
let handle = ["printf", "hello"].spawn()
handle.await()
"#);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.status, 0);
    assert_eq!(result.out, b"hello");
}

#[test]
fn process_output_preserves_invalid_utf8() {
    use rill::runtime::{run_command, CommandIo};
    let result = run_command(
        &["sh".into(), "-c".into(), "printf '\\377' >&1; printf '\\376' >&2".into()],
        None,
        &CommandIo::default(),
    )
    .unwrap();
    assert_eq!(result.out, vec![0xff]);
    assert_eq!(result.err, vec![0xfe]);
    assert!(result.stdout_text().is_err());
    assert!(result.stderr_text().is_err());
    assert_eq!(result.stdout_text_lossy(), "�");
    assert_eq!(result.stderr_text_lossy(), "�");
}

#[test]
fn dropping_last_process_handle_cancels_and_reaps_child() {
    use rill::runtime::{CommandIo, ProcessRuntime, ProcessSpec, TokioProcessRuntime};

    let marker = temp_path("dropped-handle");
    let script = format!("sleep 1; touch '{}'", marker.display());
    let handle = TokioProcessRuntime
        .spawn(ProcessSpec {
            argv: vec!["sh".into(), "-c".into(), script],
            input: None,
            io: CommandIo::default(),
        })
        .unwrap();
    let start = std::time::Instant::now();
    drop(handle);
    assert!(start.elapsed() < std::time::Duration::from_millis(500));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(!marker.exists(), "dropped process continued running");
}

#[test]
fn parallel_starts_all_processes_before_waiting() {
    let value = run(r#"
[
    ["printf", "one"],
    ["printf", "two"],
].parallel()
"#);
    let Value::List(results) = value else {
        panic!("expected result list");
    };
    assert_eq!(results.len(), 2);
    assert!(results
        .iter()
        .all(|value| matches!(value, Value::ProcessResult(result) if result.ok())));
}

#[test]
fn stdout_redirection_writes_file_and_clears_captured_stdout() {
    let out = temp_path("stdout");
    let source = format!(r#"run printf "hello" > "{}""#, out.display());
    let value = run(&source);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.status, 0);
    assert_eq!(result.out, b"");
    assert_eq!(fs::read_to_string(&out).expect("read out"), "hello");
    let _ = fs::remove_file(out);
}

#[test]
fn stderr_redirection_writes_file_and_clears_captured_stderr() {
    let err = temp_path("stderr");
    let source = format!(
        r#"run sh "-c" "printf problem >&2" err> "{}""#,
        err.display()
    );
    let value = run(&source);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.status, 0);
    assert_eq!(result.err, b"");
    assert_eq!(fs::read_to_string(&err).expect("read err"), "problem");
    let _ = fs::remove_file(err);
}

#[test]
fn all_redirection_writes_stdout_and_stderr_and_clears_both() {
    let all = temp_path("all");
    let source = format!(
        r#"run sh "-c" "printf out; printf err >&2" all> "{}""#,
        all.display()
    );
    let value = run(&source);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.status, 0);
    assert_eq!(result.out, b"");
    assert_eq!(result.err, b"");
    let contents = fs::read_to_string(&all).expect("read all");
    assert!(contents.contains("out"));
    assert!(contents.contains("err"));
    let _ = fs::remove_file(all);
}

#[test]
fn stdin_redirection_feeds_command_stdin() {
    let input = temp_path("stdin");
    fs::write(&input, "fed").expect("write input");
    let source = format!(r#"run cat < "{}""#, input.display());
    let value = run(&source);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.status, 0);
    assert_eq!(result.out, b"fed");
    let _ = fs::remove_file(input);
}

#[test]
fn programmatic_command_config_sets_cwd_and_environment() {
    let value = run(r#"
run sh "-c" "pwd; env" {
    cwd: "/tmp"
    env: { RILL_CONFIG: "configured" }
}
"#);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.status, 0);
    assert!(result.out.starts_with(b"/tmp\n"));
    assert!(result
        .stdout_text_lossy()
        .lines()
        .any(|line| line == "RILL_CONFIG=configured"));
}

#[test]
fn programmatic_command_config_can_write_streams() {
    let out = temp_path("config-out");
    let source = format!(
        r#"run printf "configured" {{ stdout: file("{}") }}"#,
        out.display()
    );
    let value = run(&source);
    let Value::ProcessResult(result) = value else {
        panic!("expected ProcessResult");
    };
    assert_eq!(result.out, b"");
    assert_eq!(
        fs::read_to_string(&out).expect("read config output"),
        "configured"
    );
    let _ = fs::remove_file(out);
}
