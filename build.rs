//! Records the git commit for `rill --version`; silently skipped when the
//! source is not a git checkout (crates.io tarballs, Nix sandboxes).

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
    let output = std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).output();
    if let Ok(output) = output {
        if output.status.success() {
            let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !hash.is_empty() {
                println!("cargo:rustc-env=RILL_GIT_HASH={hash}");
            }
        }
    }
}
