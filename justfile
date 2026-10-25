# Common development tasks; run `just` to list them.

default:
    @just --list

# Build the debug binary
build:
    cargo build

# Run the whole test suite
test:
    cargo test --all-targets

# The suite with the OpenStack namespace compiled in (fixtures, no network)
test-openstack:
    cargo test --features openstack --all-targets
    cargo clippy --features openstack --all-targets -- -D warnings

# The sql.* tests against a throwaway Postgres in target/pg (needs initdb/pg_ctl)
test-sql:
    #!/usr/bin/env bash
    set -euo pipefail
    dir="$PWD/target/pg"
    if [ ! -d "$dir/data" ]; then
        mkdir -p "$dir"
        initdb -D "$dir/data" -U rill --auth=trust >/dev/null
    fi
    pg_ctl -D "$dir/data" -o "-k $dir -c listen_addresses= -p 54329" -l "$dir/log" -w start >/dev/null
    trap 'pg_ctl -D "$dir/data" -m fast -w stop >/dev/null' EXIT
    createdb -h "$dir" -p 54329 -U rill rill_test 2>/dev/null || true
    RILL_TEST_DATABASE_URL="host=$dir port=54329 user=rill dbname=rill_test" cargo test --test sql

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
