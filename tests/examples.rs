//! Every script in `examples/` must pass `rill check` and stay warning-free.

mod common;

use std::path::Path;

#[test]
fn examples_type_check_without_warnings() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut scripts: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ril"))
        .collect();
    scripts.sort();
    assert!(!scripts.is_empty(), "no example scripts found");
    for script in scripts {
        let output = common::rill().args(["check", "-W"]).arg(&script).output().unwrap();
        assert!(
            output.status.success(),
            "{} failed rill check -W:\n{}",
            script.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
