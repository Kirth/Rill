//! Snapshots of the CLI help texts.

mod common;

fn stdout_of(args: &[&str]) -> String {
    let output = common::rill().args(args).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn rill_help() {
    insta::assert_snapshot!(stdout_of(&["--help"]));
}

#[test]
fn rill_check_help() {
    insta::assert_snapshot!(stdout_of(&["check", "--help"]));
}

#[test]
fn generated_args_help() {
    let source = r#"args {
    verbose: Bool = false short="v" description="print every step"
    port: Int = 8080 short="p" description="port to listen on"
    config: Option[Path] description="override the config file"
    timeout: Duration = 30s
    host: Str description="host to deploy to"
    command {
        start {
            workers: Int = 4
        }
        stop {
            force: Bool = false
        }
    }
}
print(args.host)
"#;
    insta::assert_snapshot!(stdout_of(&["-e", source, "--help"]));
}
