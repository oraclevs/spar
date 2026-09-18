# Spar

**A statically typed scripting language, with native configuration, task automation, and package management built in.**

Spar started as a typed configuration language and still is one — the same `.spar` files, `spar emit`, and Rust deserialization below are unchanged. It's grown a scripting core alongside that: mutable variables, control flow, `void` and async functions, a `main` entry point, and `spar exec`/`spar repl` to actually run a `.spar` file as a program rather than only evaluate it as config.

Write your configuration in `.spar` files — with types, computed values, cross-file imports, and schema validation — then either run `spar emit` to produce clean JSON, or load the config straight into your Rust application with one call:

```spar
// server.spar
var host: str = env("HOST") ?? "localhost";
var port: int = 8080;

struct Server {
    host:  str  = host;
    port:  int  = port;
    debug: bool = false;
};

private struct Defaults {
    timeout: int = 5000;
    retries: int = 3;
};

struct Database {
    url:     str = env("DATABASE_URL") ?? "postgres://localhost:5432/myapp";
    timeout: int = Defaults.timeout;
};
```

```rust
#[derive(serde::Deserialize)]
struct Server { host: String, port: i64, debug: bool }

#[derive(serde::Deserialize)]
struct Config { #[serde(rename = "Server")] server: Server }

let cfg: Config = spar::from_str(include_str!("server.spar"))?;
println!("{}:{}", cfg.server.host, cfg.server.port);
```

Or emit JSON for any language to consume:

```bash
$ spar emit server.spar
{
  "Database": {
    "timeout": 5000,
    "url": "postgres://localhost:5432/myapp"
  },
  "Server": {
    "debug": false,
    "host": "localhost",
    "port": 8080
  }
}
```

---

## Table of Contents

- [Why Spar](#why-spar)
- [Language Tour](#language-tour)
  - [Variables and types](#variables-and-types)
  - [Structs](#structs)
  - [Private structs](#private-structs)
  - [Cross-section references](#cross-section-references)
  - [Spread operator](#spread-operator)
  - [Environment variables with fallback](#environment-variables-with-fallback)
  - [String interpolation](#string-interpolation)
  - [Lists](#lists)
  - [Inline section fields](#inline-section-fields)
  - [Functions](#functions)
  - [Cross-file imports](#cross-file-imports)
  - [Schema validation](#schema-validation)
- [Scripting](#scripting)
  - [Mutable variables and assignment](#mutable-variables-and-assignment)
  - [Global control flow](#global-control-flow)
  - [Indexed iteration, break, and continue](#indexed-iteration-break-and-continue)
  - [void functions and main](#void-functions-and-main)
  - [Async functions and promises](#async-functions-and-promises)
  - [Running a script](#running-a-script)
  - [REPL](#repl)
- [Rust Integration](#rust-integration)
- [Task Runner](#task-runner)
- [Packages](#packages)
- [Installation](#installation)
- [CLI Reference](#cli-reference)
- [Editor Support](#editor-support)
- [Tooling Ecosystem](#tooling-ecosystem)
- [Contributing](#contributing)
- [License](#license)

---

## Why Spar

Most config formats — YAML, TOML, JSON — are untyped containers for static values. They offer no way to express that `port` must be an integer, no way to share values across files, no way to compute one field from another, and no way to validate that a config matches a declared shape. You discover problems at runtime, not at the desk.

Spar is designed around a different idea: config files should behave more like code.

- **Typed** — every variable and field declares its type; `spar check` catches mismatches before your config is ever used
- **Composable** — import other `.spar` files and reference their sections and exported variables
- **Computable** — arithmetic, string interpolation, functions with control flow, environment variable lookups
- **Schema-validated** — declare the expected shape of a config in a schema file; `spar check` and `spar emit` both validate against it
- **Visibility-controlled** — `private` sections are reusable internally but never appear in output; `export var` surfaces scalar values at the JSON root; plain `var` stays internal
- **Rust-native** — the `spar` crate exposes `from_str::<T>()` and `from_eval::<T>()`: parse, evaluate, and deserialize a config file directly into any `serde::Deserialize` type, the same way `toml::from_str` works
- **Deterministic output** — `spar emit` always produces keys in sorted order, so diffs are clean
- **Formattable** — `spar fmt` canonicalizes your source; `spar fmt --check` works in CI

---

## Language Tour

Canonical v1 syntax and migration behavior are specified in [Struct, Type, List, and Error Model](docs/struct-type-list-error-model.md).

### Variables and types

```spar
var name:    str   = "myapp";
var workers: int   = 4;
var ratio:   float = 0.75;
var enabled: bool  = true;
var tags:    List<str> = ["web", "api", "v2"];
```

Scalar types: `str`, `int`, `float`, `bool`.  
List types: `List<str>`, `List<int>`, `List<float>`, `List<bool>`.

Plain `var` is internal — it will not appear in `spar emit` output. To expose a scalar at the JSON root, use `export`:

```spar
export var version: str = "1.4.2";   // appears in output
var secret:         str = "hidden";  // does not appear in output
```

### Structs

Structs are named concrete configuration values and produce top-level objects in JSON output:

```spar
struct Http {
    host:    str  = "0.0.0.0";
    port:    int  = 8080;
    timeout: int  = 30;
};
```

```json
{ "Http": { "host": "0.0.0.0", "port": 8080, "timeout": 30 } }
```

### Private structs

A `private` struct is visible within the file for reference and spread, but is excluded from `spar emit` output. Use it for shared defaults:

```spar
private struct Defaults {
    timeout:   int  = 5000;
    retries:   int  = 3;
    keepalive: bool = true;
};

struct ApiClient {
    endpoint: str  = "https://api.example.com";
    timeout:  int  = Defaults.timeout;
    retries:  int  = Defaults.retries;
};

struct CacheClient {
    endpoint:  str  = "redis://localhost:6379";
    timeout:   int  = Defaults.timeout;
    keepalive: bool = Defaults.keepalive;
};
```

`Defaults` is not in the output. `ApiClient` and `CacheClient` are.

### Cross-section references

Reference any struct field with `Struct.field`:

```spar
struct Build {
    version: str = "2.1.0";
};

struct Deploy {
    image: str = "myapp:${Build.version}";
    tag:   str = Build.version;
};
```

### Spread operator

Pull all fields from a section with `...`:

```spar
private struct CommonHttp {
    timeout:    int  = 10000;
    keep_alive: bool = true;
    max_conns:  int  = 100;
};

struct Frontend {
    host: str = "0.0.0.0";
    port: int = 3000;
    ...CommonHttp;
};

struct Backend {
    host: str = "0.0.0.0";
    port: int = 8080;
    ...CommonHttp;
};
```

`Frontend` and `Backend` each get `timeout`, `keep_alive`, and `max_conns` from `CommonHttp`.

### Environment variables with fallback

`env("KEY")` reads an environment variable as a string. The `??` operator provides a fallback when the left side is absent:

```spar
var host: str = env("HOST") ?? "localhost";
var port: str = env("PORT") ?? "8080";

// Chain fallbacks
var log_level: str = env("LOG_LEVEL") ?? env("APP_LOG") ?? "info";
```

`??` is right-associative and works on any expression, not just environment variables.

### String interpolation

Embed any expression inside a string with `${}`:

```spar
var major: int = 2;
var minor: int = 1;

struct Build {
    version: str = "${major}.${minor}.0";
    tag:     str = "v${major}.${minor}";
    image:   str = "myapp:${major}.${minor}.0";
};
```

```json
{ "Build": { "image": "myapp:2.1.0", "tag": "v2.1", "version": "2.1.0" } }
```

### Lists

Lists are homogeneous. Any scalar type can form a list:

```spar
var hosts:   List<str> = ["web-1", "web-2", "web-3"];
var ports:   List<int> = [8080, 8081, 8082];
var allowed: List<str> = [env("EXTRA_HOST") ?? "localhost", "127.0.0.1"];

struct Cluster {
    hosts: List<str> = hosts;
    ports: List<int> = ports;
};
```

### Inline section fields

A field may hold an inline nested section using the `section` type:

```spar
struct Config {
    name: str = "myapp";
    db: section = {
        host: str = "localhost";
        port: int = 5432;
        ssl:  bool = true;
    };
};
```

```json
{
  "Config": {
    "db": { "host": "localhost", "port": 5432, "ssl": true },
    "name": "myapp"
  }
}
```

### Functions

Functions compute values and can return any type — including `section`, which lets them act as config templates:

```spar
function clamp(value: int, lo: int, hi: int) -> int {
    if value < lo { return lo; }
    if value > hi { return hi; }
    return value;
};

var workers: int = clamp(value: 32, lo: 1, hi: 16);

export var w: int = workers;
```

```json
{ "w": 16 }
```

A function that returns `section` can be spread directly into a section body:

```spar
function service(name: str, port: int) -> section {
    return {
        name:    str = name;
        port:    int = port;
        restart: str = "unless-stopped";
    };
};

struct Frontend {
    ...service(name: "web", port: 3000);
    image: str = "nginx:alpine";
};

struct Backend {
    ...service(name: "api", port: 8080);
    image: str = "myapp:latest";
};
```

```json
{
  "Backend":  { "image": "myapp:latest",  "name": "api", "port": 8080, "restart": "unless-stopped" },
  "Frontend": { "image": "nginx:alpine",  "name": "web", "port": 3000, "restart": "unless-stopped" }
}
```

Functions support `if`, `for`, and `return`. Mark a function `private` to keep it out of the symbol table exposed to importers.

### Cross-file imports

Split config across files and import by alias:

```spar
// shared/timeouts.spar
export var connect: int = 3000;
export var read:    int = 15000;

export struct Retry {
    max:     int = 3;
    backoff: int = 500;
};
```

```spar
// api.spar
import "shared/timeouts.spar" as t;

struct Api {
    endpoint:       str = "https://api.example.com/v2";
    connect_timeout: int = t::connect;
    read_timeout:    int = t::read;
    max_retries:     int = t::Retry::max;
};
```

Access rules:
- `alias::exported_var` — imports `export var` from the other file
- `alias::Section::field` — reads a field from a section in the other file
- Plain `var` in another file is not accessible from importers

If no alias is provided, the file's stem is used: `import "shared/base.spar";` → access as `base::`.

### Schema validation

Declare the required shape of a config in a schema file, then validate any config against it.

**Schema file** — no trailing semicolon on section declarations; fields use `;` separator:

```spar
// schema/server.spar
@SchemaFile

[Server]<Schema> {
    host: str;
    port: int;
    ssl?: bool;
}

[Database]<Schema> {
    url:  str;
    pool: int;
}
```

`field?: type` marks a field as optional; required fields must be present.

**Config file**:

```spar
// production.spar
import schema "schema/server.spar";

struct Server {
    host: str = "0.0.0.0";
    port: int = 443;
    ssl:  bool = true;
};

struct Database {
    url:  str = env("DATABASE_URL") ?? "postgres://db:5432/prod";
    pool: int = 20;
};
```

`spar check production.spar` validates the config against the schema — missing required fields, extra undeclared fields, and type mismatches are all reported before emit:

```
error[schema]: section `Server` is missing required field `port`
  --> production.spar:3:1
  |
3 | struct Server {
  | ^
```

---

## Scripting

Everything above evaluates a `.spar` file as configuration — computed once, emitted as data. Spar can also run a `.spar` file as a program.

### Mutable variables and assignment

`var` is immutable by default, same as everywhere else in this doc. Add `mut` to allow reassignment:

```spar
var mut count: int = 0;
count = count + 1;
```

Reassigning a plain `var`, or a name that was never declared, is a resolve-time error — the diagnostic for the first case suggests adding `mut`.

### Global control flow

`if` and `for` work at module scope, not just inside functions:

```spar
var mut total: int = 0;
var values: List<int> = [1, 2, 3];

for value in values {
    if value > 1 {
        total = total + value;
    }
}
```

Each `{ ... }` block is its own lexical scope — a `var` declared inside an `if`/`for` body doesn't leak out, and it can shadow an outer name of the same name without affecting the outer one.

### Indexed iteration, break, and continue

```spar
for (index, value) in values {
    if value == 2 {
        continue;
    }
    if index == 2 {
        break;
    }
}
```

`index` is always `int`; `value`'s type is the list's element type. `break`/`continue` are only valid inside a loop and always target the innermost one.

### void functions and main

```spar
function log(message: str) -> void {
    return;   // bare return — only legal in a void function
}

function main() -> int {
    log(message: "starting");
    return 0;
}
```

A `void` function may also fall off the end of its body with no explicit `return` at all. `main` is an ordinary function name, not a keyword — it just has special meaning to `spar exec`: zero parameters, returning `int` (used as the process exit status) or `void` (exits `0` unless a runtime error occurs).

### Async functions and promises

```spar
async function double(value: int) -> int {
    return value * 2;
};

async function main() -> int {
    var pending: Promise<int> = double(value: 21);
    return await pending - 42;
};
```

An async call returns `Promise<T>` and starts its task. `await` is valid only inside an async function; it returns the task's value or raises its error for `try`/`catch`. Async `main` is driven automatically by `spar exec`. See [Async functions and promises](docs/async-await.md) for lifecycle and boundary rules.

### Running a script

```bash
spar exec app.spar
spar exec app.spar -- arg1 arg2   # -- separates spar's own args from the program's
spar ./app.spar                    # shorthand for exec, when the name looks like a path
```

`spar exec` loads the module graph, initializes it (module-scope statements run once, in source order), locates `main`, calls it, and exits with its status. A leading `#!/usr/bin/env spar` line is recognized and preserved by the formatter, so a script can be made directly executable with `chmod +x`.

`spar check`/`spar emit` never call `main` — they stay pure static-analysis/config-evaluation surfaces, safe for LSP and CI use even on a file that declares one.

### REPL

```bash
spar repl
```

A minimal, persistent scripting session: each fragment you enter is evaluated against everything entered before it. A fragment that fails to typecheck or evaluate never commits — the session's state is exactly what it was before that fragment, so one bad line doesn't corrupt the session. Input is buffered until brace/bracket/paren nesting balances and the fragment ends in `;` (every top-level Spar statement already requires one), so a multi-line `function`/`task` body doesn't get evaluated one line at a time.

---

## Rust Integration

The `spar` crate is both a CLI tool and a Rust library. Add it to your project:

```toml
# Cargo.toml
[dependencies]
spar  = "0.1"
serde = { version = "1", features = ["derive"] }
```

### Deserializing a config file into a struct

`spar::from_str` works like `toml::from_str` or `serde_json::from_str` — parse, evaluate, and deserialize in one call:

```rust
use serde::Deserialize;

#[derive(Deserialize)]
struct Database {
    url:  String,
    pool: i64,
}

#[derive(Deserialize)]
struct Server {
    host:  String,
    port:  i64,
    debug: bool,
}

#[derive(Deserialize)]
struct Config {
    #[serde(rename = "Server")]
    server:   Server,
    #[serde(rename = "Database")]
    database: Database,
}

fn main() -> Result<(), spar::SparDeserError> {
    let src = std::fs::read_to_string("config/production.spar")?;
    let cfg: Config = spar::from_str(&src)?;
    println!("Connecting to {} with pool {}", cfg.database.url, cfg.database.pool);
    Ok(())
}
```

Spar struct names map to Rust struct fields via `#[serde(rename = "StructName")]` (or rename-all conventions). `export var` values appear as top-level fields alongside structs. Anonymous nested objects map to nested Rust structs. Lists map to `Vec<T>`. Optional fields use `Option<T>`.

### Error handling

`SparDeserError` wraps both compiler errors (type mismatches, unknown identifiers, missing imports) and serde mapping errors:

```rust
match spar::from_str::<Config>(&src) {
    Ok(cfg) => { /* use cfg */ }
    Err(e)  => {
        for msg in e.messages() { eprintln!("{msg}"); }
    }
}
```

### Deserializing from an already-evaluated result

If you run the Spar pipeline yourself (e.g., for multi-file configs that require loading imports), use `spar::from_eval`:

```rust
use spar::{Lexer, Parser};
use spar::resolver::Resolver;
use spar::typechecker::TypeChecker;
use spar::evaluator::Evaluator;

let tokens  = Lexer::new(&src).tokenize()?;
let program = Parser::new(tokens).parse()?;
let symbols = Resolver::new().resolve(&program, &[])?;
TypeChecker::check(&program, &symbols)?;
let result  = Evaluator::evaluate(&program, &symbols)?;

let cfg: Config = spar::from_eval(&result)?;
```

---

## Task Runner

A `.spar` file can also declare command-runner tasks alongside its
configuration values — dependencies, arguments, environment overrides,
working directories, a default task, and a `--dry-run` preview:

```spar
export var appName: str = "demo";

task [Test] {
    description: "Run tests";
    default: true;

    run {
        echo "testing ${appName}";
    };
};

task [Deploy](environment: str) {
    dependsOn: [Test];

    run {
        ./deploy.sh ${environment};
    };
};
```

```bash
spar tasks  -f server.spar
spar        -f server.spar               # bare = run the default task
spar deploy production -f server.spar    # bare <task> = spar run <task>
spar test --dry-run -f server.spar
```

Any name that isn't a `spar` subcommand is treated as a task name — `spar
<task> [args...]` is shorthand for `spar run <task> [args...]`. The explicit
`spar run <task>` form still works and is identical. If a task's name
happens to collide with a reserved subcommand (`check`, `run`, `tasks`, ...),
the subcommand always wins for bare dispatch — reach that task with `spar
run <name>` instead; `spar tasks` warns about the collision.

With no `-f`/`--file`, `spar tasks`/`spar run`/`spar show`/`spar dump`
(and bare task dispatch) search the current directory and its parents for
`SparMake.spar`.

Tasks also support default/variadic/named parameters, `private`/`group`/
`confirm` attributes, OS-labeled `run` blocks, shebang script recipes,
`.env` loading via `@LoadEnv`, per-task shell overrides, and an interactive
`--choose`
picker. Tasks are not Make-style timestamp/file build targets — no
incremental rebuilds, no input/output tracking, no caching. See
[`docs/command-runner/tasks.md`](docs/command-runner/tasks.md) for the full
reference and [`examples/tasks.spar`](examples/tasks.spar) for a runnable
example.

---

## Packages

Reusable Spar code, versioned and shared via GitHub or a local path — declared in a manifest written in Spar itself, `spar.package.spar`:

```spar
struct Package: SparPackage {
    name = "my-app";
    version = "1.0.0";
    kind = "application";
    entry = "src/main.spar";
};

struct Dependencies {
    http: str = "github:owner/spar-http@1.4.0";
};
```

Import a dependency explicitly with `import pkg`. Local modules keep the normal `import` form, and local `.spar` extensions may be omitted:

```spar
import { helper } from "./utils/helper";
import pkg "http" as http;
import pkg { get, post } from "http";
import pkg { Client } from "http/client";
```

The exact filename activates Spar's built-in `SparPackage` type: `spar check` and `spar-ls` validate required fields and offer field/kind completions without copying a type into each project. `Dependencies` and `Overrides` remain open alias maps, but every value is validated as a literal package request; overrides must use `path:`.

`spar init` scaffolds a manifest and entry file; `spar add <alias> <request>` resolves a dependency (`github:owner/repo@1.4.0`, `github:owner/repo#branch`, or `path:../local`) and locks it into `spar.package.lock.spar`. The generated lock is typed Spar source (`struct Lock: SparPackageLock`), not TOML, and records exact graph edges, remote commits, and integrity identities. Commit it to version control; don't hand-edit it.

`spar install` materializes every locked dependency from its exact recorded revision — never re-resolving a version requirement or branch, so an upstream tag moving after you've locked it can't silently change what gets installed — and `spar install --offline` fails clearly instead of touching the network if anything's still missing. `spar update [alias]` is the explicit, opposite operation: re-resolve against the manifest's current requests. `spar remove <alias>` drops a dependency and re-locks. `spar tree` prints the resolved dependency tree.

Immutable resolved packages live once per machine, deduplicated by exact revision, under `$XDG_DATA_HOME/spar/store` (`~/.local/share/spar/store` by default) — never inside a project directory, and never something a project-local `node_modules`-style folder would need. A `path:../local` dependency is different: it is a live development checkout outside the application and resolves directly rather than being copied into the immutable store. Ordinary execution (`check`, `emit`, `exec`, task runs, ordinary imports) only ever reads `spar.package.lock.spar`, live locked paths, and the store; it never touches the network. Installing a package never executes any code from it — there are no install lifecycle scripts.

---

## Installation

Spar is built with Rust. You need the Rust toolchain installed (`rustup.rs`).

```bash
git clone https://github.com/oraclevs/spar.git
cd spar
cargo build --release
```

The compiled binary is at `target/release/spar`. Copy it to a directory on your PATH:

```bash
sudo cp target/release/spar /usr/local/bin/
```

Verify:

```bash
spar --version
# spar 0.1.0
```

---

## CLI Reference

```
USAGE:
    spar <task> [args...] [OPTIONS]     Shorthand for `spar run <task> [args...]`
    spar <COMMAND> [OPTIONS]

COMMANDS:
    <task>        [args...] [-f FILE] [--dry-run] [--choose]
                                         Shorthand for `run <task>` — any name that isn't
                                         a command below is treated as a task name
    check         <file.spar>           Validate — lex, parse, resolve, and type-check
    emit          <file.spar> [-j|-y|-t]
                                         Evaluate and emit config to stdout as JSON
                                         (default), YAML (-y/--yaml), or TOML (-t/--toml)
    fmt           <file.spar>           Format a .spar file in place
    fmt --check   <file.spar>           Exit non-zero if the file is not already formatted
    tasks         [-f FILE] [--all]     List declared tasks (grouped; private hidden unless --all)
    run           [task] [args...] [-f FILE] [--dry-run] [--choose]
                                         Run a task (the default task if none is named);
                                         same as bare `spar <task>`
    show          <task> [args...] [-f FILE]
                                         Print one task's resolved commands without running it
    dump          [-f FILE]             Print the whole task catalog as JSON
    exec          <file.spar> [-- args...]
                                         Run file.spar's `main` and exit with its status
    repl                                 Start an interactive scripting session
    init          [name] [--app|--lib|--config]
                                         Create spar.package.spar in the current directory
    add           <alias> <request>     Add/update a dependency, resolve, and lock it
    remove        <alias>               Remove a dependency and re-lock
    install       [--offline]           Materialize every locked dependency into the store
    update        [alias]               Re-resolve one dependency, or all of them
    tree                                 Print the locked dependency tree

OPTIONS:
    -h, --help         Show this help message
    -V, --version      Show version

ENVIRONMENT:
    NO_COLOR=1         Disable ANSI colour in error output
```

`tasks`/`run`/`show`/`dump` (and bare task dispatch) take the file via
`-f`/`--file`; without it, `spar` searches for `SparMake.spar` in the
current directory and its parents. `check`/`emit`/`fmt` always take an
explicit file positional.

### Examples

```bash
# Validate a file
spar check server.spar

# Emit JSON (default), YAML, or TOML
spar emit server.spar
spar emit server.spar -y
spar emit server.spar -t

# Pipe to a file
spar emit server.spar > /etc/myapp/config.json

# Format in place
spar fmt server.spar

# Check formatting in CI
spar fmt --check server.spar && echo "formatted"

# List declared tasks (grouped, private hidden)
spar tasks -f server.spar

# Run the default task
spar run -f server.spar
spar -f server.spar                      # same, bare shorthand

# Run a specific task with an argument
spar run deploy production -f server.spar
spar deploy production -f server.spar    # same, bare shorthand

# Named arguments: override one defaulted parameter, skip the rest
spar run cpd out=result -f cpd.spar

# Preview commands without running them
spar run test --dry-run -f server.spar

# Pick a task interactively
spar run --choose -f server.spar

# Inspect one task, or dump the whole catalog as JSON
spar show deploy production -f server.spar
spar dump -f server.spar
```

### Error output

Spar reports errors with source spans:

```
error[type]: type mismatch — expected `int`, found `str`
  --> server.spar:4:22
  |
4 |     port: int = "8080";
  |                  ^^^^

error[resolve]: unknown identifier `Timeouts`
  --> server.spar:8:16
  |
8 |     timeout: int = Timeouts::read;
  |                    ^^^^^^^^
```

Spar collects and reports all errors it can find in a single pass rather than stopping at the first one.

---

## Editor Support

### VS Code

Install the [vscode-spar](https://github.com/oraclevs/vscode-spar) extension. It provides syntax highlighting, real-time diagnostics, hover information, completions, and formatting — all powered by `spar-ls`.

### Neovim / Helix and other editors

Wire up [spar-ls](https://github.com/oraclevs/spar-ls), the Language Server Protocol implementation for Spar. It communicates over stdio and works with any LSP-capable editor.

Syntax highlighting via Tree-sitter is provided by [tree-sitter-spar](https://github.com/oraclevs/tree-sitter-spar).

---

## Tooling Ecosystem

| Repo | Purpose |
|------|---------|
| **spar** (this repo) | Core compiler and CLI — lexer, parser, resolver, typechecker, evaluator, formatter |
| [spar-ls](https://github.com/oraclevs/spar-ls) | LSP language server — hover, completion, diagnostics, formatting |
| [tree-sitter-spar](https://github.com/oraclevs/tree-sitter-spar) | Tree-sitter grammar for Neovim, Helix, and other editors |
| [vscode-spar](https://github.com/oraclevs/vscode-spar) | VS Code extension |

---

## Contributing

The compiler is written in Rust with a small dependency footprint (`serde` and `serde_json`). The crate exposes both a CLI and a library API.

```
src/
  lexer.rs         Token stream
  token.rs         Token types
  parser.rs        AST construction
  ast.rs           AST node types
  resolver.rs      Name resolution and symbol table
  typechecker.rs   Type inference and validation
  evaluator.rs     Config value computation
  formatter.rs     Canonical source formatter
  renderer.rs      Error display with source spans
  loader.rs        Import resolution and schema validation
  engine.rs        Check/Emit/Execute mode facade
  session.rs       Persistent, transactional eval for the REPL
  host.rs          Native Rust function registry (ns::fn(...) calls)
  package/         Manifest, lockfile, global store, dependency resolution, package CLI
  de.rs            Serde deserializer (from_str / from_eval)
  lib.rs           Public crate API
  main.rs          CLI entry point
  tests/           Unit and integration tests
```

Run the test suite:

```bash
cargo test
```

---

## License

MIT — see [LICENSE](LICENSE).
