# Common development tasks; run `just` to list them.

default:
    @just --list

# Build the debug binary
build:
    cargo build

# Run the whole test suite
test:
    cargo test --all-targets

# Format and lint the way CI does
lint:
    cargo fmt --all -- --check
    cargo clippy --all-targets -- -D warnings

# Format the sources in place
fmt:
    cargo fmt --all

# Review changed insta snapshots (diagnostics, help, parser AST)
snap:
    cargo insta test --review

# Rewrite the conformance fixture expectations in tests/fixtures/scripts
fixtures:
    UPDATE_EXPECT=1 cargo test --test scripts

# Type-check every example script
examples:
    for f in examples/*.ril; do cargo run -q -- check "$f"; done

# Build the optimised binary
release:
    cargo build --release
