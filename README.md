# Spar

**A statically typed configuration language. Emits JSON. Deserializes directly into Rust structs.**

Write your configuration in `.spar` files — with types, computed values, cross-file imports, and schema validation — then either run `spar emit` to produce clean JSON, or load the config straight into your Rust application with one call:

```spar
// server.spar
var host: str = env("HOST") ?? "localhost";
var port: int = 8080;

[Server] {
    host:  str  = host;
    port:  int  = port;
    debug: bool = false;
};

private [Defaults] {
    timeout: int = 5000;
    retries: int = 3;
};

[Database] {
    url:     str = env("DATABASE_URL") ?? "postgres://localhost:5432/myapp";
    timeout: int = Defaults::timeout;
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
  - [Sections](#sections)
  - [Private sections](#private-sections)
  - [Cross-section references](#cross-section-references)
  - [Spread operator](#spread-operator)
  - [Environment variables with fallback](#environment-variables-with-fallback)
  - [String interpolation](#string-interpolation)
  - [Lists](#lists)
  - [Inline section fields](#inline-section-fields)
  - [Functions](#functions)
  - [Cross-file imports](#cross-file-imports)
  - [Schema validation](#schema-validation)
- [Rust Integration](#rust-integration)
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

### Variables and types

```spar
var name:    str   = "myapp";
var workers: int   = 4;
var ratio:   float = 0.75;
var enabled: bool  = true;
var tags:    [str] = ["web", "api", "v2"];
```

Scalar types: `str`, `int`, `float`, `bool`.  
List types: `[str]`, `[int]`, `[float]`, `[bool]`.

Plain `var` is internal — it will not appear in `spar emit` output. To expose a scalar at the JSON root, use `export`:

```spar
export var version: str = "1.4.2";   // appears in output
var secret:         str = "hidden";  // does not appear in output
```

### Sections

Sections produce top-level objects in the JSON output:

```spar
[Http] {
    host:    str  = "0.0.0.0";
    port:    int  = 8080;
    timeout: int  = 30;
};
```

```json
{ "Http": { "host": "0.0.0.0", "port": 8080, "timeout": 30 } }
```

### Private sections

A `private` section is visible within the file for reference and spread, but is excluded from `spar emit` output. Use it for shared defaults:

```spar
private [Defaults] {
    timeout:   int  = 5000;
    retries:   int  = 3;
    keepalive: bool = true;
};

[ApiClient] {
    endpoint: str  = "https://api.example.com";
    timeout:  int  = Defaults::timeout;
    retries:  int  = Defaults::retries;
};

[CacheClient] {
    endpoint:  str  = "redis://localhost:6379";
    timeout:   int  = Defaults::timeout;
    keepalive: bool = Defaults::keepalive;
};
```

`Defaults` is not in the output. `ApiClient` and `CacheClient` are.

### Cross-section references

Reference any field in any section with `Section::field`:

```spar
[Build] {
    version: str = "2.1.0";
};

[Deploy] {
    image: str = "myapp:${Build::version}";
    tag:   str = Build::version;
};
```

### Spread operator

Pull all fields from a section with `...`:

```spar
private [CommonHttp] {
    timeout:    int  = 10000;
    keep_alive: bool = true;
    max_conns:  int  = 100;
};

[Frontend] {
    host: str = "0.0.0.0";
    port: int = 3000;
    ...CommonHttp;
};

[Backend] {
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

[Build] {
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
var hosts:   [str] = ["web-1", "web-2", "web-3"];
var ports:   [int] = [8080, 8081, 8082];
var allowed: [str] = [env("EXTRA_HOST") ?? "localhost", "127.0.0.1"];

[Cluster] {
    hosts: [str] = hosts;
    ports: [int] = ports;
};
```

### Inline section fields

A field may hold an inline nested section using the `section` type:

```spar
[Config] {
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
}

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
}

[Frontend] {
    ...service(name: "web", port: 3000);
    image: str = "nginx:alpine";
};

[Backend] {
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

[Retry] {
    max:     int = 3;
    backoff: int = 500;
};
```

```spar
// api.spar
import "shared/timeouts.spar" as t;

[Api] {
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

[Server] {
    host: str = "0.0.0.0";
    port: int = 443;
    ssl:  bool = true;
};

[Database] {
    url:  str = env("DATABASE_URL") ?? "postgres://db:5432/prod";
    pool: int = 20;
};
```

`spar check production.spar` validates the config against the schema — missing required fields, extra undeclared fields, and type mismatches are all reported before emit:

```
error[schema]: section `Server` is missing required field `port`
  --> production.spar:3:1
  |
3 | [Server] {
  | ^
```

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

Section names map to struct fields via `#[serde(rename = "SectionName")]` (or rename-all conventions). `export var` values appear as top-level fields alongside sections. Inline nested sections map to nested structs. Lists map to `Vec<T>`. Optional fields use `Option<T>`.

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
    spar <COMMAND> <FILE>

COMMANDS:
    check              Validate — lex, parse, resolve, and type-check
    emit               Evaluate and emit config as JSON to stdout
    fmt                Format a .spar file in place
    fmt --check        Exit non-zero if the file is not already formatted

OPTIONS:
    -h, --help         Show this help message
    -V, --version      Show version

ENVIRONMENT:
    NO_COLOR=1         Disable ANSI colour in error output
```

### Examples

```bash
# Validate a file
spar check server.spar

# Emit JSON
spar emit server.spar

# Pipe to a file
spar emit server.spar > /etc/myapp/config.json

# Format in place
spar fmt server.spar

# Check formatting in CI
spar fmt --check server.spar && echo "formatted"
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
