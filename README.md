# Rill

Rill is a typed scripting language for Unix automation. It keeps the parts of
shell scripting that are good (commands are a first-class construct, pipes are
cheap, the OS is the runtime) and replaces the parts that are not: word
splitting, stringly-typed everything, `set -euo pipefail`, and errors you only
find at 3 a.m.

```rill
#!/usr/bin/env rill

args {
    hosts: Path description="file with one host per line"
    timeout: Duration = 10s
}

let hosts = args.hosts.read_text()?.lines() |> .map(.trim()) |> .filter(. != "")

let reports = parallel(8) for host in hosts {
    let result = run ssh -o ConnectTimeout={args.timeout.secs()} $host uptime
    emit (host, result)
}

for (host, result) in reports {
    if result.ok {
        print "{host}: {result.out.trim()}"
    } else {
        warn "{host} failed ({result.status}): {result.err.trim()}"
    }
}
```

`$host` is one argument, never a shell expansion. `run` returns a value with
`status`, `ok`, `out` and `err`. `parallel for` runs the loop body on a bounded
pool and returns results in input order. `rill check` type-checks the script
before anything runs.

## Three worlds, one syntax

| Construct | What flows | Example |
|---|---|---|
| `run cmd args` | a process: argv, captured stdout/stderr, exit status | `let r = run git status --short` |
| `a \| b` | bytes between processes (and `\| .lines()` into values) | `run cat log \| run grep ERROR \| .lines()` |
| `x \|> f` | Rill values through methods and functions | `lines \|> .map(.trim()) \|> .filter(. != "")` |

Everything else is an ordinary typed language: `let`/`var`, `fn`, closures,
`if`/`match` as expressions, `Option`/`Result` with `?`, regex literals with
named captures that become bindings, insertion-ordered maps, durations and
sizes as real types, and a declarative `args { }` block that generates `--help`.

## Install

```sh
cargo install --path .          # from a checkout
cargo build --release           # binary at target/release/rill
cargo build --release --features openstack   # with the ostack.* namespace (needs ../../LeafCloud/FastOpenStackCLI)
nix run github:Kirth/Rill -- --help
nix develop                     # dev shell with cargo, clippy, rustfmt, cargo-insta
```

Rust 1.80 or newer.

## Usage

```
rill script.ril [ARGS...]        run a script; ARGS go to the script (and its args {} block)
rill -e 'print "hi"'             run inline source
echo 'print 1' | rill -          run a script from stdin
rill check [-W] [--json] a.ril b.ril
                                 parse and type-check; prints warnings, -W makes them fatal
rill repl                        interactive session (bare `rill` on a terminal does the same)
rill --help | --version
```

Exit status: 0 on success, 1 for any script error, 2 for a usage error, 70 for
an internal error, or whatever the script passed to `exit()`. Colour follows
`NO_COLOR` and whether stdout is a terminal; `--color always|never` overrides.

## Learn more

- [Language reference](docs/reference.md): syntax, types, operators, commands, concurrency, CLI
- [Methods](docs/methods.md): the standard library, by receiver type
- [Examples](examples/): small scripts that pass `rill check`
- [Design notes](docs/design/): the original vision, remote execution (`on host { }`), and parked ideas
- [Changelog](CHANGELOG.md)

## Status

Rill is a working tree-walking interpreter with a type checker, used for real
operations scripts. The language surface is settling but not frozen; breaking
changes are listed in the changelog. Not goals, at least for now: a general
purpose standard library (HTTP clients, date/time arithmetic), bytecode
compilation, and Windows.

## Contributing

```sh
cargo test                       # the whole suite (unit, integration, snapshots, conformance)
cargo insta review               # accept changed diagnostic / help / AST snapshots
UPDATE_EXPECT=1 cargo test --test scripts
                                 # rewrite conformance expectations in tests/fixtures/scripts
cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
```

A `justfile` wraps these as `just test`, `just lint`, `just snap`, `just fixtures`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
