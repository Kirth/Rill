> Original design note. It describes the intent behind Rill; some details differ from what was
> implemented. See [../reference.md](../reference.md) for the language as it is.

# Rill

Yeah. The niche is basically:

Perl's "text is data and the OS is my runtime" worldview, but with sane syntax, real types, predictable semantics, and first-class processes/pipes.

Call it Rill for now.

The core design: it should feel like a scripting language written by someone who actually likes Unix, rather than a general-purpose language awkwardly wearing `/bin/sh` as a hat.

```rill
#!/usr/bin/env rill

fn main() -> Result[Null, Error] {
let hosts = path("hosts.txt").read_text()?.lines()
    |> .map(.trim())
    |> .filter(|x| x != "")

for host in hosts {
    let result = run ssh $host "uptime"

    if result.ok {
        print "$host: {result.out.trim()}"
    } else {
        warn "$host failed: {result.err}"
    }
}
null
}
main()
```

The important bit: commands are language constructs, not magic string soup.

```rill
run rsync -av ./src/ $server:/srv/app/
```

Variables interpolate as arguments, not shell-expanded text.

```rill
let file = "some dumb filename.txt"

run rm $file
```

That is exactly one argument. No quoting ceremony. No accidental globbing. No eval. The ancient demons stay sealed.

If you explicitly want shellish expansion:

```rill
let files = "*.tmp".glob()
run rm $files
```

## Pipelines should pass values until they need bytes

```rill
ps()
    |> .filter(.cpu > 50)
    |> sort_by(.cpu)
    |> .reverse()
    |> .take(10)
    |> table
```

But external Unix tools remain completely natural:

```rill
run journalctl -u nginx
    | run grep "timeout"
    | .lines()
    |> .take(20)
```

I'd actually distinguish:

```text
|    byte/process pipe
|>   value pipe
```

Tiny piece of syntax, massive semantic win.

So:

```rill
run cat /var/log/foo
    | run grep ERROR
    | .lines()
    |> .map(parse_log)
    |> .filter(.service == "nova")
```

This is where things like Nushell get close, but afaict they lean too far toward "everything is structured data". Sometimes I just want to chuck bytes through sed, man.

## Perl's regex superpowers survive

Regex should be deeply integrated, bc Perl got this spectacularly right:

```rill
if line =~ /user=(?<user>\w+) status=(?<status>\d+)/ {
    print "{user}: {status}"
}
```

Substitution:

```rill
let x = text =~ s/foo/bar/g
```

Maybe better:

```rill
let x = text.replace(/foo/, "bar", all=true)
```

I'd support both, honestly. The terse form earns its keep in scripts.

Parsing:

```rill
for line in log_text.lines() {
    match line {
        /(?<ip>\S+) .* "(?<method>\w+) (?<path>\S+)/ =>
            print "{ip} {method} {path}"

        _ => continue
    }
}
```

This is Perl DNA worth preserving.

## Types, but don't make me file paperwork

Inference everywhere:

```rill
let n = 42
let name = "bob"
let hosts = ["a", "b", "c"]
```

Optional annotations:

```rill
fn retry(times: Int, work: Fn() -> Result) -> Result {
    ...
}
```

Basic set:

```text
Int Float Bool Str Bytes
List[T]
Tuple[T,U,...]
Map[K,V]
Option[T]
Result[T,E]
Path
Duration
Regex
Process
```

Tuples are fixed-size heterogeneous products. They contain at least two values;
parentheses without a comma remain ordinary grouping:

```rill
let endpoint: Tuple[Str, Int] = ("localhost", 8080)
let (host, port) = endpoint
```

Tuple indexing supports negative indices. The checker preserves each slot's
type for literal indices, and tuple destructuring requires the exact arity.
Tuples serialize as arrays, but parsed arrays remain lists.

Crucially:

```rill
let timeout = 5s
let size = 20MiB
```

No reason ops languages should pretend durations and byte quantities are integers.

```rill
if file.size > 2GiB {
    ...
}
```

## Errors: absolutely no `$?`

Commands return something like:

```rill
ProcessResult {
    status: Int
    out: Str
    err: Str
}
```

Conveniences:

```rill
let r = run curl https://example.com

if !r.ok {
    die "curl exploded: {r.err}"
}
```

But the normal scripting path should be:

```rill
let output = run! curl https://example.com
```

`run!` means nonzero exit -> error propagation.

So:

```rill
fn deploy(host) {
    run! ssh $host "systemctl stop app"
    run! rsync -a ./build/ $host:/srv/app/
    run! ssh $host "systemctl start app"
}
```

For functions, steal Rust's `?` because it is just good:

```rill
fn config() -> Result[Config] {
    let text = path("config.toml").read_text()?
    text.parse_toml()
}
```

No exceptions. No Bash `set -e` eldritch jurisprudence.

## `$variables` only where shelliness is useful

I think this distinction could actually slap:

```rill
let host = "server1"

print host
run ssh $host uptime
```

Inside ordinary Rill expressions, variables are normal identifiers.

Inside command syntax, `$host` makes it visually obvious:

```text
this expression becomes argv.
```

```rill
run cp $src $dst
```

Very Perl-ish without Perl's sigil infestation.

## Process substitution should cease being arcana

Bash:

```sh
diff <(sort a) <(sort b)
```

Rill:

```rill
run diff proc(run sort a) proc(run sort b)
```

Or perhaps:

```rill
run diff @(run sort a) @(run sort b)
```

I'd want shorthand because process plumbing is core-domain stuff.

## Redirection

No `2>&1` hieroglyphs unless compatibility mode:

```rill
run foo > "output.txt"
run foo err> "errors.txt"
run foo all> "everything.txt"
run foo < "input.txt"
```

And programmatically:

```rill
let p = run foo {
    stdin: file("in"),
    stdout: capture,
    stderr: inherit,
}
```

The compact syntax and structured syntax can coexist.

## Environment variables shouldn't be fake variables

```rill
env.PATH
env.HOME

env.FOO = "bar"

with env {
    AWS_PROFILE = "prod"
    DEBUG = "1"
} {
    run terraform apply
}
```

No exported/non-exported variable ontology puzzle.

## Paths are actual things

```rill
let p = ~/".config/rill/config.toml"

if p.exists() {
    let text = p.read_text()?
    print text
}
```

Maybe:

```rill
let configs = /etc/nginx/**/*.conf
```

would be tempting, but I'd resist overly magical grammar and use:

```rill
"/etc/nginx/**/*.conf".glob()
```

Predictability > golfing.

## Maps and objects should be pleasant

```rill
let server = {
    name: "compute-01",
    ip: "10.0.0.12",
    tags: ["gpu", "prod"],
}

print server.ip
```

Destructuring:

```rill
for {name, ip} in servers {
    print "$name -> $ip"
}
```

Config-file territory therefore becomes trivial.

## Built-in text wrangling

Perl was good because this:

```perl
while (<>) {
    ...
}
```

was absurdly economical.

Keep that spirit:

```rill
for line in input.lines {
    ...
}
```

Where input automatically means stdin or filenames passed in.

Then:

```sh
rill foo.rill *.log
```

does the obvious thing.

Maybe scripts can declare:

```rill
input lines
```

and then simply:

```rill
for line {
    if line =~ /ERROR/ {
        print line
    }
}
```

Dangerously ergonomic.

## CLI arguments are part of the language

This is where scripting languages routinely shit the bed.

```rill
args {
    verbose: Bool = false   short="v"
    port: Int = 8080        short="p"
    config: Path?
    host: Str
}
```

Then automatically:

```sh
foo --help
foo -v --port 9000 server1
```

And:

```rill
args.host
args.verbose
```

No argparse library for a 30-line ops script. bitte nicht.

Subcommands:

```rill
args {
    command {
        start {
            service: Str
        }

        stop {
            service: Str
            force: Bool = false
        }
    }
}
```

Now your "little script" can grow into a real admin utility without being rewritten in Go three months later.

## Concurrency needs to be banal

```rill
parallel for host in hosts {
    run ssh $host uptime
}
```

Optional cap:

```rill
parallel(8) for host in hosts {
    ...
}
```

Results collected normally.

And processes:

```rill
let a = ["long_job_a"].spawn()
let b = ["long_job_b"].spawn()

[a.await(), b.await()]
```

No fork/wait/process-job-control theology required.

## Modules should be aggressively boring

```rill
use json
use http
use ./deploy
fn load() -> Result[Map, Error] {
    path("foo.json").read_text()?.parse_json()
}
```

A standard library should include the things scripting actually needs:

- `json`
- `toml`
- `yaml`
- `csv`
- `http`
- `url`
- `path`
- `fs`
- `process`
- `time`
- `regex`
- crypto hashes
- `archive`
- ssh maybe

No "download 73 npm packages to parse TOML."

## Perl-ish implicitness: only where it pays rent

Perl's problem wasn't terseness. It was ambient context.

Stuff like:

```perl
$_
@_
$1
$?
$!
$/
$\
```

becomes language archaeology.

Rill should permit a small set of obvious contextual conveniences.

Regex captures:

```rill
if x =~ /foo (?<bar>\w+)/ {
    print bar
}
```

Fine.

Anonymous-function implicit argument:

```rill
numbers |> .filter(. > 5)
```

Excellent.

But no universe of punctuation globals.

## The key principle

There are basically three worlds:

- Rill values
- Unix byte streams
- OS processes

The language should make transitions between them explicit but cheap.

```rill
run foo              # ProcessResult
run foo | run bar    # OS pipe
run foo | .lines()      # bytes -> List[Str]
x |> .map(...)        # structured pipeline
encode_json(x) | run jq .
```

That's the conceptual kernel.

Everything else is garnish.

And critically, a valid 5-line Rill script should look like this:

```rill
for host in hosts_text.lines() {
    let disk = run! ssh $host "df -h /"
    print "\n== $host ==\n$disk"
}
```

not:

```rust
use std::process::{Command, Stdio};
use anyhow::{Context, Result};
```

and not:

```sh
IFS=$'\n'
set -euo pipefail
for host in $(cat hosts); do
```

The former is software engineering when you wanted scripting; the latter begins with a ritual to appease an angry 1970s god.

Perl's great insight was that scripting deserves its own language semantics. We somehow responded by spending 25 years either pretending Bash is fine or turning scripts into tiny enterprise applications.

Rill should occupy that missing middle: shell-native, regex-pilled, typed, composable, unsurprising, and utterly shameless about being an OS automation language.
