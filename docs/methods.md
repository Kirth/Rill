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
| `Str` | `trim lines split(sep) upper lower len contains(part) starts_with ends_with repeat(n) index_of(part) replace(pattern, with, all=false) is_match(regex) glob sha256 parse_json parse_yaml parse_toml parse_csv parse_url` |
| `List` | `map filter flat_map flatten find any first last get(i) take(n) skip(n) slice(a, b) step(n) reverse sort contains len join(sep) zip(other) enumerate group_by(f) difference symmetric_difference parallel_map parallel_filter run spawn parallel` |
| `Map` | `get(key) keys values entries contains_key merge(other) len map parallel_map` |
| `Option` | `is_some is_none unwrap unwrap_or unwrap_or_else expect map and_then ok_or` |
| `Result` | `is_ok is_err ok err unwrap unwrap_or unwrap_or_else expect map map_err and_then` |
| `Path` | `exists is_file is_dir read_text read_lines read_yaml write append mkdir rename stat ls join parent basename stem extension canonicalize archive_list archive_extract` |
| `Duration` | `secs ms us ns as_secs_float sleep` |
| `Int` | `human_size` |
| `ProcessHandle` | `await` |
| any value | `to_json to_json_pretty to_yaml to_toml` |

`first()`, `last()`, `get(i)` and `find(f)` return `Option[T]`; `Map.get(key)`
returns `null` when the key is absent. `entries()`, `zip()` and `enumerate()`
yield tuples. `sort()`, `reverse()` and friends return new collections. Methods
that take a callback accept closures, named functions and implicit callbacks
such as `.map(.trim())`; closure parameter types are inferred from the receiver.
