# Changelog

All notable changes to Rill are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- `show value` / `show(value, title)`: records print as a text table and anything else as
  the pretty value; a host that installs `Evaluator::set_show` (eddy, eddy-web)
  gets the value itself and draws a live table or tree.
- `sql.*` and the `Db` type: `sql.connect(url)` opens a Postgres connection
  (the default `postgres` feature; `tokio-postgres` with rustls), with
  `db.query`, `one`, `scalar`, `exec`, `batch`, `transaction` (nested ones
  use savepoints) and `close`. Interpolations in a literal query become bind
  parameters, so `db.query("… where name = $name")` is injection-safe.
  `sql.ident`/`sql.raw` splice identifiers and trusted text, and
  `sql.statement` shows what a query sends. Cancelling a script cancels the
  statement on the server.
- `Evaluator::define` and `typeck::Session::define`, so a host can hand scripts
  values such as an open `Db`.
- `just test-sql` runs the database tests against a throwaway Postgres; CI runs
  them on Linux.

This is the first release with a changelog. Everything below compares against
the single-commit prototype that preceded it.

### Breaking changes

- **Scoping.** Named functions are lexically scoped and hoisted within their
  block; they no longer see the caller's variables (dynamic scoping is gone).
  `var` bindings are shared cells, so closures and functions can mutate an
  outer `var`; `let` bindings are still captured by value.
- **Parallel bodies** (`parallel for`, `race for`, `parallel_map`,
  `parallel_filter`) may not assign to outer variables. Collect results with
  `emit` instead. The checker and the runtime both reject it.
- **Modules** run in their own scope. Only top-level `let` and `fn` bindings are
  exported; `var` and private helpers do not leak into the importer.
- **Maps** are insertion-ordered (previously sorted by key), including JSON,
  YAML and TOML round trips. `for (k, v) in map` iterates pairs;
  `entries()` and `zip()` yield tuples instead of two-element lists.
- `first()`, `last()` and `List.get(i)` return `Option[T]` instead of a bare
  value or `null`. `Map.get(key)` still returns `null` when absent.
- **Result inference.** A function or closure without a declared return type
  whose body uses `?` or `run!` is typed `Result[T, Unknown]` (or `Option`),
  and its value is wrapped at runtime.
- **Strings.** `\{`, `\}`, `\$`, `\0`, `\e` and `\u{..}` are escapes; unknown
  escapes are errors. `{expr}` interpolation is parsed at parse time and
  accepts any expression. `$` not followed by a name or `{` is literal.
  Raw strings `r"..."` have no escapes or interpolation.
- **Identifiers** no longer accept `?` or `!` suffixes; `x?` is always the
  try operator, and `T?` in an annotation means `Option[T]`.
- **Bare calls** are whitespace-aware: `print [1, 2]`, `print {a: 1}`,
  `print -1` and `print args.host` are calls; `xs[0]` and `a - 1` are not.
- Unary minus is a real operator: `-2 ** 2` is `-4`.
- Function parameters, loop variables and `if let` bindings are immutable;
  copy them into a `var` to change them.
- `1 == 1.0` is `true`; numeric equality crosses Int and Float everywhere.
- `slice(start, end)` with `start > end` is an error instead of a panic.
- Paths are tilde-expanded only when written as literals; runtime values are
  taken verbatim and `$VAR` is no longer expanded inside path strings.
- Durations display Go-style (`1m30s`, `1.5s`, `250ms`).
- The CLI treats `--help`, `--version`, `-e` and `check` as its own options;
  everything after the script name goes to the script. Script argument errors
  exit with status 2.

### Added

- Operators `%`, `**`, `in`, `not in`, `??`, `..=`; string comparison;
  Duration arithmetic (`1s + 500ms`, `2 * 1s`, `3s / 1.5s`) and comparison.
- `if` as an expression, `loop { break value }`, `||` zero-argument closures,
  default parameters, named arguments to user functions, `Fn(A, B) -> R`
  annotations, `Map[Str, V]` annotations.
- Place assignment: `m.a = 1`, `m["a"]["b"] += 2`, `xs[-1].name = "x"`.
- Block expressions `{ stmts; value }` (so match arms can be blocks) and
  string keys in map literals (`{"other-key": 3}`).
- `on host { }` and `with env { } { }` are expressions: `let user = on host
  { run! whoami }` binds the block's last value, typed by the checker.
- Relational list methods: `sort_by(f)`, `unique()`, `all(f)`, `count(f)`,
  `sum()`, `index_by(f)`.
- The `ostack.*` namespace (cargo feature `openstack`): OpenStack records
  through the `ostack` crate, with `inventory`, `find`, `references`, a
  result cache and fixture files for offline use. Namespace functions without
  a receiver are a new registry (`NAMESPACE_FUNCTIONS`) next to the methods.
- `ostack.search(text)` finds records by id, IP, MAC, name or fragment and
  names the owning server and project; `ostack.find` accepts addresses and
  names. Floating IPs, security groups and routers are kinds, and records
  follow relations by kind (`port.server()`, `subnet.routers()`,
  `hypervisor.servers()`, `server.security_groups()`, …).
- A `Time` type: `text.parse_time(format?)`, `time.now()`, `time.parse`,
  `time.from_unix`; comparisons, `Time ± Duration`, `Time - Time`, and
  `year month day hour minute second weekday unix date iso format age`.
  Duration literals gain `d` and `w`.
- XML: `text.parse_xml()` gives element records `{tag, attrs, text, children}`;
  `el.select(path)` / `el.select_one(path)` query them with an XPath subset
  (`a/b`, `//a`, `a[@k='v']`, `a[child]`, `a[2]`, `…/@attr`, `…/text()`);
  `el.to_xml()` writes them back.
- Field-access errors show the offending value: a missing field prints the
  record (bounded, uncoloured), a "did you mean" for close keys and a
  `.get("name")` hint; `?` on a plain value shows that value too.
- ostack relations and exact IP/MAC searches use server-side filters and
  single GETs instead of whole listings. New kinds: keypairs, server groups,
  aggregates, availability zones, compute services, network agents,
  snapshots, backups, pools, role assignments. `ostack.quota`,
  `ostack.actions` (instance actions and events), and structured server
  fields (`flavor_id`, `image_id`, `networks`) with `server.flavor()` /
  `server.image()`.
- Slices `xs[1..3]`, `xs[..n]`, `xs[n..]`, `s[1..3]`, negative bounds;
  `(0..10).step(2)`.
- General `match` patterns: literals, tuples, lists with `..`, `some`/`none`/
  `ok`/`err`, `|` alternatives, `if` guards, and exhaustiveness checking for
  Bool, Option and Result.
- Option/Result combinators `map`, `and_then`, `map_err`, `unwrap_or_else`,
  `ok_or`, `expect`; Duration accessors `secs`, `ms`, `us`, `ns`,
  `as_secs_float`; `Int.human_size()`.
- Closure parameter types are inferred from the receiver (`xs.map(|x| ...)`).
- Diagnostics: miette reports with codes (`rill::parse`, `rill::type`, ...),
  spans on every statement, "did you mean" suggestions, every independent
  checker error reported in one run, and warnings (`unused_let`,
  `unused_result`, `unreachable`, `shadowed_capture`) from `rill check`.
- `rill repl` (and bare `rill` on a terminal): an interactive session with
  persistent bindings, multi-line input, `:type`, `:env`, `:help`, `:quit`.
- CLI: `rill -` (stdin script), `rill run`, `rill check` over several files
  with `--quiet`, `--json`, `-W`; `--color`, `--no-color`, `--version`;
  exit codes 0/1/2/70; a panic hook that reports internal errors.
- Remote execution: argv is shell-quoted for `ssh`, nested `on` contexts
  compose, `docker()` passes `-i` when stdin is piped, hosts are validated.
- Processes: SIGTERM then SIGKILL on cancellation, `ProcessResult.signal`,
  bounded `parallel()`, `race` cancels awaited handles.
- Robustness: evaluator runs on a large stack, call depth is limited
  (`RILL_MAX_CALL_DEPTH`), `repeat` and ranges are size-capped, non-UTF-8
  environments no longer panic, `xs += [x]` is amortised O(1).
- Performance: lists, tuples and maps are shared and copied on write, so
  reading a variable never deep-copies a collection; `Value` shrank to 32
  bytes; cancellation is polled every 64 steps instead of locking on every
  node; sequential `for` loops reuse one scope.
- `for` expressions infer their element type from the `emit` values, and
  calling a value of unknown type is allowed.
- Repository: README, reference docs, CI (Linux and macOS, MSRV 1.80),
  flake, snapshot and conformance test suites, dual MIT/Apache-2.0 license.
