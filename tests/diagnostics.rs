//! Snapshot every diagnostic fixture: parse errors, type errors (and warnings),
//! and runtime errors, rendered the way `rill` prints them without colour.
//!
//! Review changes with `cargo insta review` (or `INSTA_UPDATE=always cargo test`).

use rill::diagnostic::RillError;
use std::path::Path;

fn fixture_unit(path: &Path) -> rill::SourceUnit {
    let kind = path.parent().and_then(|p| p.file_name()).unwrap().to_string_lossy();
    let name = path.file_name().unwrap().to_string_lossy();
    rill::SourceUnit {
        name: format!("{kind}/{name}"),
        source: std::fs::read_to_string(path).unwrap(),
        base_dir: path.parent().unwrap().to_path_buf(),
    }
}

fn render(error: &RillError) -> String {
    rill::diagnostic::render(error, false, 80)
}

/// Compile (and for runtime fixtures run) a script, returning what the CLI
/// would print on stderr.
fn diagnose(path: &Path) -> String {
    let unit = fixture_unit(path);
    let (name, source) = (unit.name.clone(), unit.source.clone());
    let program = match rill::CompilationSession::new().compile(unit) {
        Ok(program) => program,
        Err(error) => return render(&error),
    };
    let mut output = String::new();
    for warning in &program.warnings {
        output.push_str(&render(warning));
    }
    let is_runtime = path.to_string_lossy().contains("/runtime/");
    if is_runtime {
        match rill::run_compiled(&program, Vec::new(), None) {
            Ok(value) => {
                if let Some(error) = rill::unhandled_error(&value) {
                    output.push_str(&render(&error.with_source(name, source)));
                } else {
                    output.push_str("(no error)\n");
                }
            }
            Err(error) => output.push_str(&render(&error)),
        }
    } else if output.is_empty() {
        output.push_str("(no diagnostics)\n");
    }
    output
}

#[test]
fn diagnostics_match_snapshots() {
    insta::glob!("fixtures/diagnostics/**/*.ril", |path| {
        insta::assert_snapshot!(diagnose(path));
    });
}
