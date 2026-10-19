use std::process::Command;

fn bounded_output(mut child: std::process::Child) -> std::process::Output {
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > std::time::Duration::from_secs(5) {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("CLI waited for input or failed to finish");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn ordinary_arguments_and_help_do_not_read_stdin() {
    for args in [
        vec!["-e", "print(args)", "not-a-filename"],
        vec!["-e", "args { count: Int = 1 } input lines", "--help"],
        vec![
            "-e",
            "args { count: Int = 1 } print(args.count)",
            "--count",
            "3",
        ],
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rill"))
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let _open_stdin = child.stdin.take().unwrap();
        let output = bounded_output(child);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn prin_writes_without_a_trailing_newline() {
    let output = Command::new(env!("CARGO_BIN_EXE_rill"))
        .args(["-e", "prin \"hello\""])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"hello");
}

#[test]
fn pretty_print_formats_and_colors_structured_values() {
    let output = Command::new(env!("CARGO_BIN_EXE_rill"))
        .args(["-e", "pretty_print({name: \"alice\", ok: true})"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\x1b["));
    assert!(stdout.contains("\"name\""));
    assert!(stdout.contains("\"alice\""));
    assert!(stdout.ends_with("\x1b[0m\n"));
}

#[test]
fn explicit_input_and_child_stdin_have_separate_ownership() {
    use std::io::Write;
    for source in [
        "print(input.lines)",
        "input lines; for line { print(line) }",
        "let r = run cat\nprint(r.out)",
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rill"))
            .args(["-e", source])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"owned input\n")
            .unwrap();
        let output = bounded_output(child);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("owned input"));
    }
}

#[test]
fn declarative_subcommand_options_are_parsed_by_the_child_group() {
    let source = r#"args {
    command {
        start {
            port: Int
        }
    }
}
print(args.command.start.port)
"#;
    let output = Command::new(env!("CARGO_BIN_EXE_rill"))
        .args(["-e", source, "start", "--port", "42"])
        .output()
        .expect("run rill");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("42"));
}

#[test]
fn cli_exit_builtin_sets_process_status() {
    let exe = env!("CARGO_BIN_EXE_rill");
    let status = Command::new(exe)
        .args(["-e", "exit(7)"])
        .status()
        .expect("run rill");
    assert_eq!(status.code(), Some(7));
}

#[test]
fn cli_renders_unhandled_result_errors() {
    let exe = env!("CARGO_BIN_EXE_rill");
    let output = Command::new(exe)
        .args([
            "-e",
            "fn failed() -> Result[Int, Error] { err(\"bad\") } failed()",
        ])
        .output()
        .expect("run rill");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("runtime error"));
    assert!(stderr.contains("bad"));
}

#[test]
fn runtime_numeric_errors_show_source_operator_and_types() {
    for operator in [">", "+", "-", "*", "/"] {
        let source = format!(
            "let record = {{value: \"text\"}}\nprint(record.get(\"value\") {operator} 1)"
        );
        let output = Command::new(env!("CARGO_BIN_EXE_rill"))
            .args(["-e", &source])
            .env("NO_COLOR", "1")
            .output()
            .expect("run rill");
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&format!("operator '{operator}'")), "{stderr}");
        assert!(stderr.contains("Str and Int"), "{stderr}");
        assert!(stderr.contains("<eval>:2:"), "{stderr}");
        assert!(stderr.contains("print(record.get"), "{stderr}");
    }
}
