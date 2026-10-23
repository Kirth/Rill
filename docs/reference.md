# Rill language reference

This describes the language as implemented by the interpreter in this
repository. The design notes in [docs/design](design/) describe intent and
ideas that may not be implemented; when they disagree, this file and the
test suite win.

## Running scripts

```
rill script.ril [ARGS...]      rill -e 'print "hello"'      echo 'print 1' | rill -
rill check [-W] [--json] [--quiet] script.ril...
```

Scripts execute top-level statements in order; there is no implicit `main`.
A `#!/usr/bin/env rill` shebang is accepted. Statements are separated by
newlines or `;`. Blocks use braces. Comments start with `#` or `//`.

Exit status: 0 on success, 1 for any script error (parse, type, runtime), 2
for a usage error (bad `rill` options, or bad script arguments against an
`args { }` block), 70 for an internal error, or the value passed to `exit()`.

## Bindings and values

```rill
let name = "alice"          # immutable
var count: Int = 0          # mutable, annotation optional
count += 1                  # = += -= *= /= %=
let maybe: Int? = none      # T? is shorthand for Option[T]
```

`let` cannot be reassigned. Closures and functions capture `let` bindings by
value and share `var` bindings by reference, so a function can update an
outer `var`:

```rill
var total = 0
[1, 2, 3].map(|x| { total += x })
print(total)                # 6
```

### Types

`Null`, `Bool`, `Int`, `Float`, `Str`, `Bytes`, `List[T]`, `Tuple[A, B, ...]`,
`Map[Str, V]` (or just `Map`), `Path`, `Duration`, `Regex`, `Error`,
`ProcessResult`, `ProcessHandle`, `Option[T]` (or `T?`), `Result[T, E]` (or
`Result[T]`), `Fn`, `Fn(A, B) -> R` (also written `Fn[A, B] -> R`).

Map keys are always strings. There are no user-defined structs.

Literals: `null`, `true`, `42`, `1.5`, `"text"`, `r"raw"`, `[1, 2]`,
`{name: "alice", "other-key": 3}`, `("localhost", 8080)`, `/regex/`,
durations `100ns 10us 250ms 1.5s 2m 1h`, sizes `20B 20KB 20MB 20GB 20KiB
20MiB 20GiB` (plain `Int` bytes), ranges `0..5` (exclusive) and `0..=5`.

Conversions: `Str(x)`, `Int(x)`, `Float(x)`, `Bool(x)`; `type(x)` returns the
type name as a string.

### Strings

Double-quoted strings support the escapes `\n \t \r \0 \e \\ \" \{ \} \$ \u{1F600}`.
Unknown escapes are errors. Raw strings `r"..."` have no escapes and no
interpolation.

Interpolation is resolved at parse time and type-checked:

| Form | Meaning |
|---|---|
| `"$name"`, `"${name}"` | a binding, falling back to the environment variable |
| `"${NAME:-default}"` | the binding or environment variable, or `default` when unset or empty |
| `"{expr}"` | any expression: `"{xs[0] + 1}"`, `"{m.get("k")}"`, `"{xs.map(|x| x * 2)}"` |
| `"\{"`, `"\$"` | literal brace or dollar; `"{}"` is also literal (JSON stays JSON) |

A `$` not followed by a name or `{` is literal (`"cost: $5"`).

### Operators

From loosest to tightest binding:

| Operators | Notes |
|---|---|
| `\|` `\|>` | byte pipe, value pipe |
| `or` `\|\|` | short-circuit |
| `and` `&&` | short-circuit |
| `== != < <= > >= =~ in not in` | comparison; `<` works on numbers, strings and durations |
| `??` | unwrap an `Option`, or replace `null`; right side is only evaluated when needed |
| `..` `..=` | ranges |
| `+ -` | `+` also joins strings and lists |
| `* / %` | `/` on two Ints truncates; `%` has the sign of the dividend; division by zero is an error |
| `- ! not` (prefix) | `-2 ** 2` is `-4` |
| `**` | right-associative; Int results are overflow-checked, negative Int exponents are errors |
| `?` `.field` `f()` `[i]` `[a..b]` | postfix |

`1 == 1.0` is `true`. `in` works on lists, map keys, strings, and ranges.
Duration arithmetic: `1s + 500ms`, `2 * 1s`, `1s / 2`, `3s / 1.5s == 2.0`,
`1500ms > 1s`; `Dur - Dur` that would go negative is an error.

### Indexing, slicing and places

```rill
xs[0]  xs[-1]  m["key"]  m.key  pair[1]
xs[1..3]  xs[..2]  xs[2..]  xs[0..=1]  "hello"[1..3]      # negative bounds count from the end
var m = {a: {b: 1}, xs: [{n: 1}]}
m.a.b = 2;  m["new"] = 3;  m.xs[-1].n += 10               # any place rooted at a var
```

`m.key` on a missing key is an error; `m.get("key")` returns `null`, and
`null.get("key")` is `null`, which allows optional chains.

### Destructuring

```rill
let [a, b] = [1, 2]
let (host, port) = ("localhost", 8080)
let {name, address: ip} = {name: "node", address: "127.0.0.1"}
for (k, v) in {a: 1, b: 2} { print "{k}={v}" }
```

Use `_` to discard a slot. Tuples have at least two elements, must be
destructured with the exact arity, and cannot be iterated or spread.

## Functions and closures

```rill
fn double(n: Int) -> Int { n * 2 }
fn greet(name, greeting = "hello", punct = "!") -> Str { "{greeting} {name}{punct}" }
greet("bob", punct = "?")                 # named arguments
let twice = |x| x * 2
let answer = || 42                         # zero-argument closure
fn apply(cb: Fn(Int) -> Int, x: Int) -> Int { cb(x) }
```

A function returns the value of its last statement or an explicit `return`.
Named functions are hoisted within their block, so mutually recursive
functions work, and they resolve names lexically (not from the caller).
Parameters with defaults must come last; defaults may refer to earlier
parameters. A function whose declared return type is not `Null` must return a
value on every path.

Closure parameter types are inferred from the receiver when the closure is
passed to a method or to a `Fn(...)` parameter, so `[1].map(|x| x.upper())` is
a type error.

A function with no declared return type whose body uses `?` or `run!` is
typed `Result[T, Unknown]` (or `Option[T]` if it only unwraps options) and its
value is wrapped at runtime.

## Control flow

```rill
let label = if n > 0 { "positive" } else if n < 0 { "negative" } else { "zero" }
while count < 3 { count += 1 }
for item in [1, 2, 3] { print(item) }
for (i, item) in ["a", "b"].enumerate() { print "$i: $item" }
let squares = for n in 0..4 { emit n * n }      # for is an expression; only emit contributes
let found = loop { i += 1; if i * i > 50 { break i } }
if let {name} = record { print(name) }
```

`break` and `continue` work in all loops; `break value` only in `loop`. An
`if` used as a value needs an `else`.

### Match

```rill
match value {
    0 => "zero",
    1 | 2 => "small",
    n if n < 0 => "negative",
    (a, b) => "pair {a},{b}",
    [] => "empty",
    [first, ..] => "starts with {first}",
    "yes" => "affirmative",
    some(x) => "some {x}",
    none => "none",
    ok(v) => v,
    err(e) => die(e),
    /user=(?<user>\w+)/ => user,
    _ => "other",
}
```

Patterns are literals (numeric equality crosses Int and Float), bindings,
`_`, tuples, lists with an optional `..` rest, `some`/`none`/`ok`/`err`,
regex literals (named captures become bindings), `|` alternatives (which
must bind the same names), and `if` guards. Matches on `Bool`, `Option`,
`Result` and `Null` must be exhaustive; other matches may fall through to
`null`. Arms may be blocks.

## Option and Result

Construct with `some(x)`, `none`, `ok(x)`, `err(e)`.

```rill
fn load(filename: Str) -> Result[Str, Error] {
    let text = path(filename).read_text()?
    ok(text.trim())
}
match load("config.txt") { ok(text) => print(text), err(e) => warn(e) }
```

`?` unwraps `ok`/`some` and returns `err`/`none` from the enclosing function.
It is not allowed at top level. A script whose final value is an `err` exits
with status 1 and prints the error.

Methods: `is_ok is_err ok err unwrap unwrap_or unwrap_or_else expect map
map_err and_then` on Result; `is_some is_none unwrap unwrap_or unwrap_or_else
expect map and_then ok_or` on Option. `??` is shorthand for `unwrap_or`.

Not every fallible operation returns a Result. Parsing, writing, and most
other runtime failures raise an error directly (see the diagnostics section).

## Regular expressions

```rill
if line =~ /user=(?<user>\w+) status=(?<status>\d+)/ { print "{user}: {status}" }
let changed = text =~ s/foo/bar/g
let changed2 = text.replace(/foo/, "bar", all=true)
```

Regex literals are `/pattern/` (escape `/` as `\/`) using Rust regex syntax:
no lookaround or backreferences. Named captures become `Str` bindings in the
successful branch or arm. A capture that shadows an existing binding produces
a warning. `replace` defaults to the first occurrence.

## Commands and byte pipelines

```rill
let r = run printf "%s\n" "hello"       # ProcessResult: status, ok, out, err, signal
let text = run! git rev-parse HEAD       # stdout as Str; a failure is an error (propagates with ?)
run cat $filename                        # one argument, never word-split
run cat $files                           # a List expands to several arguments
run scp $filename $host:/tmp/            # joins inside one argument
run $*words                              # explicit whitespace splitting of a Str
["echo", filename].run()                 # argv list
sh "ls | wc -l"                          # a real shell, when you mean it
```

Command words are argv, not shell source: no globbing, substitution or
operators. `"{expr}"` interpolation works inside quoted command words.

```rill
run printf "one\ntwo\n" | run grep two | .lines() |> .map(.upper())
"hello" | run cat                         # a Str feeds stdin
run cmd > "out.txt"    err> "err.txt"    all> "both.txt"    < "in.txt"
run pwd { cwd: "/tmp", env: {DEBUG: "1"}, stdout: capture, stderr: inherit }
```

`|` connects process byte streams; `| .lines()` turns the final output into
values; `|>` passes Rill values. `stdin`/`stdout`/`stderr` settings accept
`file("path")`, `capture`, `inherit` or `null`.

A process killed by a signal has `status == 128 + signal` and `signal` set.
Cancelled processes receive SIGTERM, then SIGKILL after one second.

## Concurrency and remote contexts

```rill
let outputs = parallel for host in hosts { emit run! ssh $host uptime }
let limited = parallel(6) for host in hosts { emit run! ssh $host uptime }
let winner  = race for host in hosts { emit run! ssh $host hostname }
let results = [1, 2, 3].parallel_map(|x| x * 2)
let job = ["sleep", "1"].spawn();  let done = job.await()
let all = [["echo", "a"], ["echo", "b"]].parallel()
```

`parallel for` returns emitted values in input order; `race for` returns the
first emitted value (`null` if none) and cancels the rest. Bodies run on a
shared bounded pool and may not assign to outer variables; collect results
with `emit`.

```rill
on "user@host" { run hostname }
on host { on docker("container") { run hostname } }
```

`on` prefixes every command in its block with `ssh host --` (argv is
shell-quoted, so arguments survive the remote shell) or, for a list, that
argv. `docker(name)` builds a `docker exec` prefix. Contexts nest; Rill
evaluation and filesystem methods still run locally.

## Files, data, environment

```rill
let p = path("~/file.txt")               # literal paths expand ~; runtime values are taken verbatim
p.exists()  p.read_text()?  p.read_lines()  p.write(text)  p.join("child")  p.ls()
text.parse_json()  text.parse_yaml()  text.parse_toml()  text.parse_csv()  text.parse_url()
value.to_json()  value.to_json_pretty()  value.to_yaml()  value.to_toml()
print env.HOME;  env.DEBUG = "1";  with env { DEBUG = "1" } { run env }
250ms.sleep()
```

`read_text()` and `read_yaml()` return a Result; other operations raise.
Maps keep insertion order through parse and serialise. Namespace forms such
as `json.parse(text)` and `yaml.read(filename)` also exist; `use json` is
optional for them.

Modules: `use ./helper.ril` runs the file once in its own scope and imports
its top-level `let` and `fn` bindings by name. Private helpers and `var`s are
not exported.

## Arguments and input

```rill
args {
    verbose: Bool = false short="v" description="be chatty"
    port: Int = 8080 short="p"
    config: Option[Path]
    host: Str description="target host"
    command {
        start { workers: Int = 4 }
        stop  { force: Bool = false }
    }
}
print(args.host, args.port, args.command.start.workers)
```

Bool fields are flags; fields with a default or an `Option` type are options;
remaining fields are positionals in declaration order. `--help` prints a
generated usage text. Without an `args` block, `args` is the list of
arguments.

`input lines` reads stdin (or the files named on the command line when there
is no `args` block); `for line in input.lines { }` or `for line { }` iterates it.

## Diagnostics

Errors carry a code (`rill::parse`, `rill::type`, `rill::runtime`,
`rill::process`, `rill::io`, `rill::usage`, `rill::internal`), a source span,
and often a suggestion. `rill check` reports every independent error in one
run and prints warnings: `rill::warn::unused_let`, `unused_result`,
`unreachable`, `shadowed_capture`. `-W` turns warnings into failures;
`--json` prints one JSON object per diagnostic. Running a script never prints
warnings.

Limits: call depth 10 000 (`RILL_MAX_CALL_DEPTH`), `repeat` results up to
1 GiB, ranges up to 32 million elements.
