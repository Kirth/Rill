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
        vec!["-e", "args { count: Int = 1 } print(args.count)", "--count", "3"],
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
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
}

#[test]
fn prin_writes_without_a_trailing_newline() {
    let output =
        Command::new(env!("CARGO_BIN_EXE_rill")).args(["-e", "prin \"hello\""]).output().unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"hello");
}

#[test]
fn pretty_print_formats_and_colors_structured_values() {
    let output = Command::new(env!("CARGO_BIN_EXE_rill"))
        .args(["--color=always", "-e", "pretty_print({name: \"alice\", ok: true})"])
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
        child.stdin.take().unwrap().write_all(b"owned input\n").unwrap();
        let output = bounded_output(child);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
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
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("42"));
}

#[test]
fn cli_exit_builtin_sets_process_status() {
    let exe = env!("CARGO_BIN_EXE_rill");
    let status = Command::new(exe).args(["-e", "exit(7)"]).status().expect("run rill");
    assert_eq!(status.code(), Some(7));
}

#[test]
fn cli_renders_unhandled_result_errors() {
    let exe = env!("CARGO_BIN_EXE_rill");
    let output = Command::new(exe)
        .args(["-e", "fn failed() -> Result[Int, Error] { err(\"bad\") } failed()"])
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
        let source =
            format!("let record = {{value: \"text\"}}\nprint(record.get(\"value\") {operator} 1)");
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

fn rill(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rill")).args(args).env("NO_COLOR", "1").output().unwrap()
}

#[test]
fn version_and_help_flags_work() {
    let output = rill(&["--version"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("rill 0.1.0"));
    let output = rill(&["-V"]);
    assert!(output.status.success());
    for flag in ["--help", "-h"] {
        let output = rill(&[flag]);
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: rill"));
    }
}

#[test]
fn usage_errors_exit_with_two_and_print_usage() {
    let output = rill(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage: rill"));
    let output = rill(&["--bogus", "x.ril"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option '--bogus'"));
    let output = rill(&["check"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn missing_script_is_an_io_error() {
    let output = rill(&["/nonexistent/script.ril"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rill::io"), "{stderr}");
    assert!(stderr.contains("failed to read"), "{stderr}");
}

#[test]
fn scripts_can_be_read_from_stdin() {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rill"))
        .args(["-", "a", "b"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"print(args.join(\"-\"))\n").unwrap();
    let output = bounded_output(child);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "a-b\n");
}

#[test]
fn check_handles_several_files_and_json_output() {
    let dir = std::env::temp_dir().join(format!("rill-cli-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("good.ril");
    let bad = dir.join("bad.ril");
    std::fs::write(&good, "let x = 1\n").unwrap();
    std::fs::write(&bad, "let x = 1\nprint(y)\n").unwrap();
    let output = rill(&["check", good.to_str().unwrap(), bad.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("good.ril: ok"), "{stdout}");
    assert!(stderr.contains("unknown identifier 'y'"), "{stderr}");
    assert!(stderr.contains("rill::type"), "{stderr}");

    let output = rill(&["check", "--json", good.to_str().unwrap(), bad.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[0]["ok"], true);
    assert_eq!(lines[1]["code"], "rill::type");
    assert_eq!(lines[1]["line"], 2);
    assert_eq!(lines[1]["col"], 7);
    assert_eq!(lines[1]["severity"], "error");
    assert!(lines[1]["message"].as_str().unwrap().contains("unknown identifier"));

    let output = rill(&["check", "--quiet", good.to_str().unwrap()]);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn colour_is_off_when_piped_and_can_be_forced_or_disabled() {
    let output = Command::new(env!("CARGO_BIN_EXE_rill"))
        .args(["-e", "pretty_print({a: 1})\nprint(zzz)"])
        .env_remove("NO_COLOR")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\x1b["));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("\x1b["));
    let output = Command::new(env!("CARGO_BIN_EXE_rill"))
        .args(["--color", "always", "-e", "print(zzz)"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("\x1b["));
    let output = Command::new(env!("CARGO_BIN_EXE_rill"))
        .args(["--no-color", "-e", "pretty_print({a: 1})"])
        .env("CLICOLOR_FORCE", "1")
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\x1b["));
}

#[test]
fn generated_args_help_lists_positionals_defaults_and_commands() {
    let source = r#"args {
    verbose: Bool = false short="v" description="be chatty"
    port: Int = 8080 short="p"
    config: Option[Path]
    host: Str description="target host"
}
print(args.host)
"#;
    let output = rill(&["-e", source, "--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: rill <script> [options] <host>"), "{stdout}");
    assert!(stdout.contains("-v, --verbose          be chatty [default: false]"), "{stdout}");
    assert!(stdout.contains("--port <Int>"), "{stdout}");
    assert!(stdout.contains("[default: 8080]"), "{stdout}");
    assert!(stdout.contains("--config <Path>"), "{stdout}");
    assert!(stdout.contains("[optional]"), "{stdout}");
    assert!(stdout.contains("<host>"), "{stdout}");
    assert!(stdout.contains("target host"), "{stdout}");
}

#[test]
fn script_argument_errors_are_usage_errors() {
    let output = rill(&["-e", "args { host: Str }\nprint(args.host)"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rill::usage"), "{stderr}");
    assert!(stderr.contains("missing required argument 'host'"), "{stderr}");
    assert!(stderr.contains("--help"), "{stderr}");
    let output = rill(&["-e", "args { host: Str }\nprint(args.host)", "a", "--bogus"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown option '--bogus'"));
}
