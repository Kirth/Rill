# Remote execution: `on host { }`

> Design proposal, kept for context. The implemented behaviour is summarised in
> [../reference.md](../reference.md) under "Concurrency and remote contexts".

```text
# Proposal: `on host { }` blocks

## What it does

Executes a Rill block in the context of a remote host over SSH.
Rill evaluation (let, if, for, function calls, data manipulation) runs locally.
I/O (commands, file ops) is forwarded to the remote host.

```rill
on "nova1.prod" {
    let disks = run virsh domblklist --all
    let config = path("~/nova.conf").read_text()?
    if disks.out.contains(vol_id) {
        run virsh detach-disk $domain $dev
    }
}
```

Function calls carry the context automatically — if `check_volume()` calls
`path.exists()` inside an `on` block, that exists check runs remotely.
Nesting works: `on "host2" { }` inside `on "host1" { }` switches context,
restores on exit.


## Motivating use case

Incident detection: stream logs from N hosts, pattern-match, then
spawn a follow-up inspection on the affected machine without leaving Rill.

```rill
for line, host in stream compute_nodes { tail -f /var/log/nova/nova-compute.log } {
    if line =~ /Failed to detach volume (?P<vol_id>[a-f0-9-]{36})/ {
        on host {
            let domains = run virsh list --name
            for domain in domains.out.lines() {
                let disks = run virsh domblklist $domain --details
                if disks.out.contains(vol_id) {
                    warn "still attached to {domain}"
                    let dev = disks.out.lines()
                        |> .find(. contains vol_id)
                        |> .split(" ") |> .last()
                    warn (run lsblk -no NAME,STATE $dev).out.trim()
                }
            }
        }
    }
}
```


## Implementation

### AST

```rust
Expr::On { host: Box<Expr>, body: Vec<Stmt> }
```

Parses like an if-block without a condition: `"on" expr "{" stmts "}"`, and
like `if` it is an expression: `let user = on host { run! whoami }` binds the
block's last value. Typechecks transparently — same types inside as outside.

### Evaluator

One new field:

```rust
struct Evaluator {
    remote_host: Option<String>,
    // ...
}
```

Eval for `Expr::On`:

```rust
Expr::On { host, body } => {
    let host = self.eval_expr(host)?.as_string_lossy();
    let prev = std::mem::replace(&mut self.remote_host, Some(host));
    let result = self.eval_stmts(body);
    self.remote_host = prev;  // restore; handles nesting
    result
}
```

### Command forwarding (phase 1 — already covers 80% of use cases)

Wrap argv in SSH before dispatch:

```rust
fn resolve_argv(&self, argv: Vec<String>) -> Vec<String> {
    match &self.remote_host {
        None => argv,
        Some(host) => {
            let mut ssh = vec![
                "ssh".into(),
                "-o".into(), "ControlMaster=auto".into(),
                "-o".into(), format!("ControlPath=/tmp/rill-ssh-{host}"),
                "-o".into(), "ControlPersist=30s".into(),
                host.clone(), "--".into(),
            ];
            ssh.extend(argv);
            ssh
        }
    }
}
```

ControlMaster means the first call opens the socket, all subsequent calls
in the block reuse it — no per-command handshake cost.

### File I/O forwarding (phase 2)

Every fs/path arm in eval_stdlib gets a remote branch. Each operation maps
to a shell one-liner run over SSH:

    read_text      →  ssh host -- cat file
    read_lines     →  same, split on newlines
    write          →  ssh host -- tee file  (content via stdin; ProcessSpec.input)
    append         →  ssh host -- tee -a file
    exists         →  ssh host -- test -e path; echo $?
    is_file        →  ssh host -- test -f path; echo $?
    is_dir         →  ssh host -- test -d path; echo $?
    mkdir          →  ssh host -- mkdir -p path
    rename         →  ssh host -- mv from to
    ls             →  ssh host -- ls -1 path
    stat           →  ssh host -- stat --format '%s %Y %F' path  (GNU only)
    canonicalize   →  ssh host -- realpath path
    glob           →  ssh host -- bash -c 'printf "%s\n" pattern'

Pattern per arm:

```rust
("fs", "read_text") => {
    let path = ...;
    match &self.remote_host {
        Some(host) => run_ssh(host, &["cat", &path]).map(|r| Value::Str(r.out)),
        None => fs::read_to_string(&path).map(Value::Str)...
    }
}
```

~150-200 lines total across eval_stdlib. Mechanical, not architectural.


## Known fragility / caveats

- `stat --format` is GNU. macOS uses `stat -f`. Mixed fleets need detection
  or a portable fallback (find -printf).
- Binary files through SSH: read_text assumes UTF-8 clean channel. Fine for
  configs and logs, breaks on actual binaries.
- Tilde expansion: `~/file` currently expands locally via eval_resolve_path.
  Inside `on host`, the literal `~/file` should be passed to SSH and expanded
  remotely. Needs a flag in the path resolver to suppress local expansion.
- Large writes: content is held in memory before piping. Acceptable for config
  files, not for bulk data.
- SSH errors come back as stderr strings, not structured io::Error. Need clean
  re-surfacing.


## Phased delivery

Phase 1: command forwarding only (`run`, `spawn`, command expressions).
         `remote_host` field + `resolve_argv`. ~30 lines. Covers most real use.
         File I/O documented as local.

Phase 2: file read ops (read_text, read_lines, exists, is_file, is_dir, ls).
         Low-risk, read-only.

Phase 3: file write ops (write, append, mkdir, rename). Needs stdin piping.

Phase 4: stat, canonicalize, glob. Edge cases, can wait.


## What this enables (the headline use case)

stream → pattern match → on host { inspect } in one script, no external
tooling, no SSH boilerplate, remote context carries into function calls.

```
