# Standard-library methods

See [reference.md](reference.md) for the language itself. This file covers the
method-based standard library.

Operations on values use methods. Namespace APIs such as `yaml.read(filename)`, `json.parse(text)`, and `fs.read(filename)` are also available and delegate to the same method implementations. Bare receiver-first functions such as `get(map, key)` and `map(values, callback)` remain removed.

```ril
let address = alloc.get("maas_ips").get(name)
let names = [" alice ", " bob "].map(.trim()).reverse()

names
|> .filter(. != "")
|> .take(10)

run printf "one\ntwo\n" | .lines() |> .reverse()
```

Methods evaluate the receiver once, then arguments from left to right. Method pipelines bind the piped value as the receiver. Implicit callback expressions bind to each collection item: `.map(.trim())` trims each string, while `.filter(.score > 5)` examines each record. Map callbacks receive `(key, value)` and produce a list; list callbacks receive one item.

## API and migration

Namespace-qualified forms in this table remain supported alongside methods. Bare receiver-first functions and old computed properties use the replacements shown.

| Function or previous form | Method form |
|---|---|
| `get(map, key)` | `map.get(key)` |
| `map(values, callback)` | `values.map(callback)` (Map or List) |
| `filter(values, predicate)` | `values.filter(predicate)` (List) |
| `take(values, count)` / `reverse(values)` | `values.take(count)` / `values.reverse()` |
| `trim(text)` / `lines(text)` | `text.trim()` / `text.lines()` |
| `text.replace(pattern, replacement, all=true)` | Unchanged; `all` is optional and defaults to `false` |
| `regex.is_match(pattern, text)` | `text.is_match(pattern)` or `regex.is_match(text)` for a Regex value |
| `hashes.sha256(text)` / `glob(pattern)` | `text.sha256()` / `pattern.glob()` |
| `json.parse(text)` / `yaml.parse(text)` | `text.parse_json()` / `text.parse_yaml()` |
| `toml.parse(text)` / `csv.parse(text)` / `url.parse(text)` | `text.parse_toml()` / `text.parse_csv()` / `text.parse_url()` |
| `json.stringify(value)` / `json.pretty(value)` | `value.to_json()` / `value.to_json_pretty()` |
| `yaml.stringify(value)` / `toml.stringify(value)` | `value.to_yaml()` / `value.to_toml()` |
| `path.join(base, child)` | `path(base).join(child)` |
| `fs.exists(filename)` / `p.exists` | `path(filename).exists()` / `p.exists()` |
| `read(filename)` / `fs.read(filename)` / `p.read_text` | `path(filename).read_text()` / `p.read_text()`; returns Result |
| `yaml.read(filename)` | `path(filename).read_yaml()`; returns Result |
| `fs.write(filename, text)` | `path(filename).write(text)` |
| `archive.list(filename)` / `archive.extract(filename, destination)` | `path(filename).archive_list()` / `path(filename).archive_extract(destination)` |
| `process.run(argv)` / `process.exec(argv)` | `argv.run()` |
| `spawn(argv)` / `await(handle)` | `argv.spawn()` / `handle.await()` |
| `parallel(commands)` | `commands.parallel()` |
| `time.sleep(seconds)` | A Duration receiver, e.g. `1s.sleep()` |
| `values |> map(callback)` / `values |> take(count)` | `values |> .map(callback)` / `values |> .take(count)` |
| `command | lines` / `command | lines()` | `command | .lines()` |

Filesystem methods require a Path receiver; `join` and `archive_extract` accept either a string or Path argument. Command methods require nonempty string argv lists; `parallel` accepts a list of argv lists. `run` command syntax and its `run!` variant are unchanged.

Filesystem namespace functions accept either a string filename or a Path, including `yaml.read`, `fs.read`, `fs.write`, `fs.exists`, `path.join`, and archive functions. `time.sleep` accepts integer seconds or a Duration. `regex.is_match(pattern, text)` retains its pattern-first argument order. Namespace arguments are evaluated once, from left to right.

Map `.get(key)` accepts a string key and returns `null` when absent. `null.get(key)` also returns `null`, allowing optional nested lookups. Direct field access still fails on a missing key. Built-in methods take precedence in calls; map entries remain accessible as fields or through `.get("name")`, including entries that contain functions. Other function-valued fields can be called normally.

Computed operations require parentheses: `text.trim()` and `p.exists()`. Data fields such as `result.ok`, `result.out`, `input.lines`, and map entries keep property syntax. Extracting a bound method, such as `let f = text.trim`, is not supported; use a closure or `.map(.trim())`.

## List searches

`items.find(predicate)` returns `Option[T]`: `some(item)` for the first match,
or `none` when no element matches. This replaces the previous bare-item/null
return convention; use `.is_some()`, `.unwrap_or(default)`, or `match` to consume
the result. Finding a null element returns `some(null)`, distinct from `none`.

`items.any(predicate)` returns a Bool indicating whether any element matches.
Both methods stop at the first match and accept closures, named functions, or
implicit callbacks, e.g. `lines.any(.starts_with("disk:"))`. For an empty list,
`find` returns `none` and `any` returns `false` without calling the predicate.

## Relational helpers

Lists of records (maps from JSON APIs) get the usual questions answered
without loops:

```ril
servers.sort_by(.name)                  # stable; keys compare as numbers, strings, durations; nulls first
servers.sort_by(.created).reverse()
servers.all(.status == "ACTIVE")        # true for an empty list
servers.count(.status == "ERROR")
flavors.map(.vcpus).sum()               # Int when every item is an Int, else Float; 0 for []
ips.unique()                            # first occurrence wins; compares by value
let by_id = servers.index_by(.id)       # Map[Str, item]; later duplicates replace earlier ones
by_id.get(volume.attachments[0].server_id)
```

`group_by(f)` is the many-valued cousin of `index_by(f)`: both key by the
callback's result as a string, in first-seen order.

## Tuples

Tuples are fixed-size heterogeneous values with at least two elements:

```ril
let endpoint: Tuple[Str, Int] = ("localhost", 8080)
let (host, port) = endpoint

var status: Tuple[Int, Str] = (200, "ok")
status[0] += 1
status[-1] = "created"
```

`(value)` is a grouped expression; `()` and `(value,)` are rejected. Literal
indices retain the corresponding element type, negative indices count from the
end, and destructuring must match the tuple's exact arity. Mutation requires a
`var`, a literal index, and a value compatible with that position.

Tuples compare structurally and are distinct from lists. They do not support
list methods, iteration, or spreading. JSON, YAML, and TOML encode tuples as
arrays; arrays read from those formats remain lists.

## File-read errors

`fs.read(filename)` and `Path.read_text()` both return `Result[Str, Error]`. `yaml.read(filename)` and `Path.read_yaml()` both return the same Result containing the parsed YAML value. YAML can produce a map, list, scalar, or null, so the checker leaves the success type unknown until the caller supplies more information. Missing/unreadable files yield an I/O error; invalid YAML yields a parse error. Propagate these inside a Result-returning function:

```ril
fn main() -> Result[Null, Error] {
    let alloc = yaml.read("network-allocation.yml")?
    print alloc.get("maas_ips").to_json_pretty()
    null
}

main()
```

The CLI reports a final error Result and exits unsuccessfully. `?` remains restricted to functions. Text parsing, writing, and archive operations retain their previous throwing behavior.

Constructors (`path`, `file`, `some`, `none`, `ok`, `err`) and standalone actions (`print`, `warn`, `die`, `exit`, `set_env`, `sh`) remain functions. Standard-library `use` declarations remain accepted but are unnecessary for methods. Ordinary user-function pipelines still work; user-defined functions may reuse removed builtin names.

## Method index

Parentheses are required even for zero-argument methods.

| Receiver | Methods |
|---|---|
| `Str` | `trim lines split(sep) upper lower len contains(part) starts_with ends_with repeat(n) index_of(part) replace(pattern, with, all=false) is_match(regex) glob sha256 parse_json parse_yaml parse_toml parse_xml parse_csv parse_url` |
| `List` | `map filter flat_map flatten find any all(f) count(f) first last get(i) take(n) skip(n) slice(a, b) step(n) reverse sort sort_by(f) unique sum contains len join(sep) zip(other) enumerate group_by(f) index_by(f) difference symmetric_difference parallel_map parallel_filter run spawn parallel` |
| `Map` | `get(key) keys values entries contains_key merge(other) len map parallel_map` |
| XML element (`Map`) | `select(path) select_one(path) to_xml`; `select` and `select_one` also take a list of elements |
| `Option` | `is_some is_none unwrap unwrap_or unwrap_or_else expect map and_then ok_or` |
| `Result` | `is_ok is_err ok err unwrap unwrap_or unwrap_or_else expect map map_err and_then` |
| `Path` | `exists is_file is_dir read_text read_lines read_yaml write append mkdir rename stat ls join parent basename stem extension canonicalize archive_list archive_extract` |
| `Duration` | `secs ms us ns as_secs_float sleep` |
| `Time` | `year month day hour minute second weekday unix date iso format(fmt) age` |
| `Int` | `human_size` |
| `ProcessHandle` | `await` |
| any value | `to_json to_json_pretty to_yaml to_toml` |

`first()`, `last()`, `get(i)` and `find(f)` return `Option[T]`; `Map.get(key)`
returns `null` when the key is absent. `entries()`, `zip()` and `enumerate()`
yield tuples. `sort()`, `reverse()` and friends return new collections. Methods
that take a callback accept closures, named functions and implicit callbacks
such as `.map(.trim())`; closure parameter types are inferred from the receiver.

## ostack namespace

OpenStack, through the `ostack` crate linked into rill when it is built with
`--features openstack` (the namespace always type-checks; calling it in a
build without the feature raises an error that says so). Every function takes
no receiver and returns full records as maps, with the crate's normalisation
(`tenant_id` → `project_id`, server `flavor`/`image` as names, `addresses` as
one string; ports carry the IPs).

| Function | Returns |
|---|---|
| `ostack.info()` | `Map`: user, project, region, roles, catalog summary |
| `ostack.kinds()` | `List[Str]`: `servers volumes ports networks subnets images flavors hypervisors projects users loadbalancers amphorae floating_ips security_groups routers keypairs server_groups aggregates availability_zones compute_services network_agents snapshots backups pools role_assignments` |
| `ostack.list("servers")`, `ostack.servers()`, `ostack.volumes()`, … one per kind | `List[Map]` |
| `ostack.show("server", id_or_name)` | `Map`; an ambiguous name is an error that lists the ids |
| `ostack.server(id_or_name)`, `ostack.volume(..)`, `ostack.port(..)`, `ostack.floating_ip(address)`, … one per kind | the same, one record; a unique name fragment also works |
| `ostack.get("compute", "/os-services", {binary: "nova-compute"})` | the raw response body |
| `ostack.inventory(["servers", "ports"])` | `Map[Str, List[Map]]`, fetched in parallel (default: servers volumes ports networks subnets images flavors hypervisors projects) |
| `ostack.find(x)` | `Option[Map]`: `some({kind: "volume", record: {...}})` for an id, or the best exact match on an IP, MAC or name; `none` otherwise |
| `ostack.search(text)` | `List[Map]` of `{kind, field, match, record, owner, project}`: every record whose id, IP, MAC, name or host matches exactly, then name and address fragments; `owner` is the server or router behind a port or floating IP |
| `ostack.references(id, kinds=[...])` | `List[Map]` of `{kind, field, record}` for every record whose JSON mentions the id (`attachments[0].server_id`, `device_id`, …) |
| `ostack.project("name or id")` | a handle `Map` (the project record plus an `@ostack` marker) whose `.servers()`, `.volumes()`, … `.list(kind)` and `.inventory([kinds])` are scoped to that project |
| `ostack.all_projects()` | a handle whose `.servers()` and `.volumes()` span every project (`all_tenants=1`) |
| `ostack.quota(project=, usage=true)` | `List[Map]` of `{service, scope, resource, limit, in_use, reserved}` for a project (default: the token's); also `project.quota()` |
| `ostack.actions(server, events=false, limit=)` | a server's instance actions (`action request_id user_id start_time message`), newest first; `events=true` adds each action's `events` (one request per action, `limit` 20 by default); also `server.actions()` |
| `ostack.refresh()` | drop every cached result |

```ril
ostack.project("foobar").servers().filter(.status == "ACTIVE")
ostack.all_projects().volumes().count(.status == "error")
```

Records know their relations. The kind is read from the record's fields, and
the related records come from the cached inventory of the record's project:

| Record | Methods |
|---|---|
| server | `attached_volumes ports security_groups floating_ips hypervisor flavor image server_groups actions project` |
| volume | `attached_servers snapshots backups project` |
| snapshot, backup | `volume project` |
| port | `server network subnets security_groups floating_ips project` (`server()` is the router for router ports) |
| network | `ports subnets routers project` |
| subnet | `network ports routers project` |
| router | `ports subnets project` |
| floating_ip | `port server project` |
| security_group | `ports servers project` |
| hypervisor | `servers compute_service network_agents aggregates` (admin) |
| aggregate | `hypervisors` |
| server_group | `servers project` |
| pool | `members loadbalancers project` |
| loadbalancer | `pools project` |
| user | `role_assignments created_servers project_memberships`: servers the user booted (any project), and the projects the user can reach directly or through a group, as handles; `user.project_memberships().flat_map(.servers())` lists what they can access |
| project | the listings, like a handle: `servers() volumes() … inventory()`, plus `quota()` |

Relations ask the APIs for exactly the related records: filtered listings
(`ports?device_id=`, `ports?fixed_ips=subnet_id=`, `servers?host=`,
`snapshots?volume_id=`, …) and single GETs by id, never a whole collection
when a filter exists. When an API rejects a filter, the listing is fetched in
the same scope and filtered locally. An exact IP or MAC given to `search` or
`find` likewise becomes a few filtered requests instead of an inventory scan.

Server records keep the friendly `flavor`, `image` and `addresses` strings and
add `flavor_id`, `image_id` and `networks: [{network, addr, version, type,
mac}]`, so `server.flavor()` and `server.image()` follow ids.

Singular relations (`server`, `network`, `port`, `hypervisor`, `flavor`,
`image`, `volume`, `compute_service`, `project`) return `Option[Map]`; `project()` returns a handle, so
`server.project().unwrap().servers()` lists its neighbours. Singular getters
fall back to a unique name fragment, so `ostack.hypervisor("compute-17")` finds
`compute-17.example.maas`.

```ril
let hit = ostack.search("10.0.4.17", all_projects=true)[0]
print "{hit.kind} {hit.record.id} owned by {hit.owner.name} in {hit.project.name}"
ostack.hypervisor("compute-17").servers().map(.name)
for s in ostack.project("foobar").servers() { print s.name, s.attached_volumes().map(.name) }
```

Handle methods take no named arguments; use the namespace form for
`fresh=` and `query=` (`ostack.servers(project="foobar", fresh=true)`).

List functions accept `all_projects=true` (servers and volumes), `project="name or id"`
(resolved through the projects list), `query={key: value}` for raw query
parameters, and `fresh=true` to bypass the cache.

Results are cached per identity, collection and query for 120 seconds
(`RILL_OPENSTACK_CACHE_TTL_SECS`), so repeated questions, replays and
backtracking in eddy do not refetch. Calls give up after 60 seconds
(`RILL_OPENSTACK_TIMEOUT_SECS`) and stop when the evaluator is cancelled.

Credentials are the `OS_*` variables of the script's environment (so `with env
{ OS_PROJECT_NAME = "x" } { ... }` switches project): password or application
credential, `OS_REGION_NAME`, `OS_INTERFACE`. The token cache under
`~/.cache/ostack` is shared with the CLI (`RILL_OPENSTACK_NO_AUTH_CACHE=1`
disables it). TLS verification is off unless `OS_VERIFY_TLS=1`, as in the CLI.
`RILL_OPENSTACK_FIXTURE=<dir>` serves `<dir>/<collection>.json` files instead of
a cloud; see `tests/fixtures/openstack` and `examples/openstack_attached.ril`.

## Times

`text.parse_time()` turns a timestamp into a `Time` (kept in UTC): RFC 3339 /
ISO 8601 with `Z` or an offset, the same without a zone (taken as UTC, as Nova
and Keystone write it), `YYYY-MM-DD HH:MM:SS`, or a bare `YYYY-MM-DD`. With an
argument it uses strftime syntax: `"15/10/2026 14:29".parse_time("%d/%m/%Y %H:%M")`.
A string that does not parse raises an error.

```ril
let updated = server.updated.parse_time()
updated < time.now() - 7d                 # compare with <, <=, >, >=, ==, !=
updated.age() > 30d                       # how long ago, as a Duration
let gap = time.now() - updated            # Time - Time is a Duration
servers.sort_by(.updated.parse_time())    # times sort and group like numbers
updated.format("%Y-%m-%d %H:%M")  updated.date()  updated.weekday()  updated.unix()
time.now()  time.parse("2026-10-15")  time.from_unix(1792074561)
```

`Time ± Duration` is a `Time`; `Time - Time` is a `Duration` and raises when the
result would be negative. Duration literals include `d` (days) and `w` (weeks).
Times print and serialise as RFC 3339 (`2026-10-15T14:29:21Z`).

## XML

`text.parse_xml()` turns XML into its root element. Every element is a plain
record with the same four fields, so a tag that appears once or many times is
always a list entry in `children`:

| Field | Value |
|---|---|
| `tag` | the name as written, with its prefix (`domain`, `qemu:commandline`) |
| `attrs` | a Map of attribute values in document order; namespace declarations (`xmlns:qemu`) included |
| `text` | the element's own text, trimmed (`""` when none); CDATA counts as text |
| `children` | a List of child element records |

Comments, processing instructions and whitespace-only text are dropped.
`el.select(path)` returns every match as a List and `el.select_one(path)` the
first as an Option. Both also take a list of elements, so selections chain.
Paths are a small XPath subset, relative to the element:

| Path | Matches |
|---|---|
| `devices/disk` | child elements by tag, step by step |
| `*` / `.` | any child element / the element itself |
| `//graphics`, `devices//source` | elements at any depth |
| `disk[@device]`, `disk[@device='cdrom']` | elements with the attribute, or with that value |
| `disk[readonly]`, `.[name='vm1']` | elements with that child, or whose child has that text |
| `disk[2]` | the second match, counting from 1 |
| `…/@file` | the attribute's value as a Str (elements without it are skipped) |
| `…/text()` | the element's text as a Str |

```ril
let dom = $(virsh dumpxml vm1).out.parse_xml()
dom.attrs.type                                           # "kvm"
dom.select_one("name/text()")                            # some("vm1")
dom.select("devices/disk[@device='disk']/source/@file")  # the disk images
dom.select("devices/interface").map(|i| (i.select_one("mac/@address"), i.select_one("source/@bridge")))
dom.select("//graphics/@port")
```

`el.to_xml()` writes an element back as indented XML without a declaration, as
`virsh dumpxml` does; `text.parse_xml().to_xml().parse_xml()` gives the same
record. Missing `attrs`, `text` or `children` count as empty and attribute
values may be any value, so `{tag: "boot", attrs: {dev: "hd"}}.to_xml()` works.
Edit a document with nested assignment on a `var`
(`d.children[0].text = "vm2"`); `select` returns copies, so changing its results
does not change the document.

## sql namespace

Databases, through a `Db` handle. Postgres is the one driver so far; it is
compiled in by default (the `postgres` cargo feature). A build without it
still type-checks `sql.*`, and `sql.connect` raises an error that says so.

| Function | Returns |
|---|---|
| `sql.connect(url?, password=, tls=, timeout=, app=)` | a `Db`. `url` is `postgres://user:pass@host:5432/db?sslmode=…` or `host=… user=…`; without it, `$DATABASE_URL`, then `PGHOST PGPORT PGUSER PGDATABASE PGPASSWORD PGSSLMODE` (and a local Unix socket when no host is set). The variables are read from the script's environment, so `with env {}` applies. `tls` is `disable`, `prefer` (default), `require` (encrypt, no certificate check, as libpq) or `verify` (check against the web PKI roots; also `sslmode=verify-full`). `timeout` (a Duration) limits each statement; default `RILL_SQL_TIMEOUT_SECS` or 300s. `app` sets `application_name` (default `rill`). |
| `sql.ident(name)` | a fragment: `name` as a quoted identifier (`"my table"`; `schema.table` quotes each part) |
| `sql.raw(text)` | a fragment: trusted SQL spliced verbatim |
| `sql.statement(query)` | `{text, params}`: what a query string turns into, without running it |

| Method on `Db` | Returns |
|---|---|
| `db.query(sql, params?)` | `List[Map]`: the rows as records, columns in order (a repeated name becomes `id_2`, …) |
| `db.one(sql, params?)` | `Option[Map]`: the first row, or `none` |
| `db.scalar(sql, params?)` | the first column of the first row, or `null` |
| `db.exec(sql, params?)` | `Int`: rows affected |
| `db.batch(sql)` | runs several `;`-separated statements, without parameters (a schema file) |
| `db.transaction(\|tx\| …)` | what the function returns. `BEGIN`, then `COMMIT` when it returns, or `ROLLBACK` when it raises an error or returns `err(..)` (the error still raises). Nested calls use savepoints. `tx` is the same connection. |
| `db.close()` | closes the connection for every copy of the handle |

**Interpolation binds.** When the query is a string literal, every `$name`,
`${name:-default}` and `{expr}` in it becomes a `$1`, `$2`, … parameter, and
the value goes to the server separately. It is never spliced into the text,
so quoting and injection are not a concern:

```ril
let db = sql.connect("postgres://kirth@localhost/leaf")
let project = "ops"
db.query("select id, name from servers where project = $project and vcpus >= {min + 1}")
db.query("select * from servers where id = any({ids})")       # a List binds as an array
db.query("select * from users where name ilike {"%" + q + "%"}")
db.query("select count(*) from {sql.ident(table)} where {sql.raw(filter)}")
```

- An interpolation inside a quoted SQL string (`'%{q}%'`), a quoted identifier
  or a comment is an error: there it could not be a parameter. Bind the whole
  value instead, as above.
- A query held in a variable has already been interpolated as text. Pass its
  values as a list instead: `db.query(q, [a, b])` with `$1`, `$2` in `q`
  (write `r"…"` or `\$` so Rill leaves `$1` alone in strings that interpolate).
  One query can't use both interpolations and a params list.
- A literal `{` in SQL is written `\{` (`'\{1,2}'::int[]`), as in any Rill
  string. Dollar-quoted bodies need `\$tag\$` or a raw string.
- Each parameter is converted to the type the server expects for it, such as
  `{42}` against an `int4`, `numeric` or `text` column. Where nothing gives it
  a type (`select {x}`), the server treats it as `text`, so cast it there:
  `select {x}::int`.

Values convert as follows. Anything else is an error that names the `$n` or
the column, and suggests a `::text` cast:

| Rill | Postgres parameter |
|---|---|
| `Null`, `none` | `NULL` |
| `Bool` | `bool` |
| `Int` | `int2` `int4` `int8` `oid` `float4` `float8` `numeric` |
| `Float` | `float4` `float8` `numeric` |
| `Str` | text types (`text varchar char name citext`, enums). It is also parsed for `int*` `float*` `numeric` `uuid` `inet` `cidr` `date` `timestamptz` `bytea` |
| `Bytes` | `bytea` |
| `Time` | `timestamptz` `timestamp` `date` |
| `Duration` | `interval` |
| `List` | an array of its element type, or `json`/`jsonb` |
| `Map` | `json` `jsonb` |

| Postgres column | Rill |
|---|---|
| `bool` | `Bool` |
| `int2` `int4` `int8` `oid` | `Int` |
| `float4` `float8`, `numeric` | `Float` (cast `numeric` to `::text` to keep every digit) |
| text types, enums, `uuid`, `inet`, `cidr`, `time` | `Str` |
| `bytea` | `Bytes` |
| `timestamptz` `timestamp` `date` | `Time` (UTC; a `date` is midnight) |
| `interval` | `Duration` (a month counts as 30 days) |
| `json` `jsonb` | `Map`, `List` or scalars |
| arrays | `List` |
| `NULL` | `null` |

Errors from the server raise with the SQLSTATE in the message and the
server's detail and hint as help: `sql: duplicate key value violates unique
constraint "users_pkey" (SQLSTATE 23505)`. Cancelling the script (Ctrl-C, or
the host's cancellation token) also cancels the running statement on the
server.

A host can open a connection itself and hand it to scripts:

```rust
use rill::sql::{ConnectOptions, Db};
use rill::types::Value;

let cancel = rill::eval::CancellationToken::new();
let db = Db::connect("postgres://kirth@localhost/leaf", &ConnectOptions::default(), &cancel)?;
let rows: Vec<Value> = db.query("select * from servers where id = $1", &[Value::Int(7)], &cancel)?;

let mut evaluator = rill::eval::Evaluator::new(vec![]);
evaluator.define("db", Value::Db(db.clone()));
let mut session = rill::typeck::Session::new();
session.define("db", &Value::Db(db));             // so `db.query(..)` type-checks
```
