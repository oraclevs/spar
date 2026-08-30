# Just Extraction Map

This is a provenance map, not a vendoring plan. “Transitive Just-specific
dependencies” names the donor modules that make a unit unsuitable for direct
reuse. The runner remains Spar-owned and uses the Rust standard library for
its final process calls.

| Just source unit | Relevant responsibility | Transitive Just-specific dependencies | Decision | Spar outcome |
| --- | --- | --- | --- | --- |
| `src/main.rs` | binary entry and completion bootstrap | `clap`, `clap_complete`, `Arguments`, `run` | DISCARD | Keep Spar's hand-written file-first CLI. |
| `src/run.rs` | parse Just arguments, create `Config`, dispatch a `Subcommand`, map errors to exit code | `Arguments`, `Config`, `Loader`, `SignalHandler`, `Error`, `Verbosity`, `Color` | DISCARD | `src/main.rs` dispatches `tasks`/`run` through Spar compilation. |
| `src/subcommand.rs` | locate/compile Justfile and dispatch `Run` | `Config`, `Search`, `Compilation`, `Justfile`, loader, all Just subcommands | DISCARD | No Justfile discovery, fallback, or subcommand model. |
| `src/justfile.rs::run` / `run_recipe` | parse invocations, collect reachable dependencies, evaluate scopes, ensure once-only execution | `InvocationParser`, `Recipe`, `Dependency`, `Evaluator`, `Scope`, `Ran`, `Cache`, `Semaphore`, `ExecutionContext` | REIMPLEMENT | `runner::graph` validates and plans Spar IR before `runner::executor` starts a process. |
| `src/recipe.rs::run` / `run_shell` | command echo, shell launch, inherited child I/O, cwd, environment, exit-status propagation | `Line`, `Sigil`, `Settings`, `Config`, `Evaluator`, `Environment`, `ExecutionContext`, `CommandExt`, `SignalHandler`, Just errors | ADAPT | Adapt only the concepts: echo-before-run, child status failure, cwd and per-task environment. Execute one Spar raw block, not Just's line/sigil model. |
| `src/recipe.rs::run_script` | temp-file scripts, shebangs, cache, output tracking | `Executor`, `Shebang`, `Cache`, `CacheKey`, `Platform`, `Environment`, Just attributes/settings | DISCARD | V1 executes raw blocks directly; no script files, shebangs, cache, timestamps, or output targets. |
| `src/executor.rs` | build interpreters and temporary scripts; classify shell/script files | `Interpreter`, `Shebang`, `Platform`, `Recipe`, Just errors, tempfile | DISCARD | Do not carry Just's script abstraction. Use fixed V1 platform shell selection. |
| `src/settings.rs::shell` / `shell_command` | select default platform shell and add its arguments | `Settings`, `Config`, `Interpreter`, `CommandExt`, `Platform` | ADAPT | Reimplement only fixed selection: Unix `sh -cu`; Windows `cmd /S /C`. No configurable Just shell settings. |
| `src/shell_kind.rs` | recognize `cmd`/PowerShell for command details | `Command`, Just `ShellKind` consumers | ADAPT | Preserve the need for a Windows-specific command branch, but implement only the V1 `cmd /S /C` branch. |
| `src/command_ext.rs` | construct command, Windows raw shell argument, wait for exit status | `SignalHandler`, `Signal`, `ShellKind`, `OutputError`, `Platform` assumptions | ADAPT | Reimplement minimal `std::process::Command` construction, inherited stdio, and status mapping; do not copy signal-handler or PATH-resolution behavior. |
| `src/environment.rs` | export dotenv and evaluated Just bindings into a child | `Scope`, `Settings`, `Binding`, `Value`, `unexports`, dotenv loader | ADAPT | Inherit the process environment and overlay already-lowered task `BTreeMap<String, String>` values. No dotenv/exports/unexports semantics. |
| `src/execution_context.rs` | aggregate Just config/module/scope/search and choose tempdir/cwd | `Config`, `Justfile`, `Scope`, `Search`, `TempDir`, `Settings` | REIMPLEMENT | Use a small `ExecutionOptions { dry_run, base_dir }`; resolve task-relative cwd from the Spar source directory. |
| `src/dependency.rs` and `src/dependency_argument.rs` | recipes with expression-valued dependency arguments | `Recipe`, `Namepath`, `Expression`, Just serialization | DISCARD | V1 has named task dependencies; only the requested task consumes CLI arguments. |
| `src/recipe.rs` data model | Just recipe attributes, source lifetime, parameters, body and module metadata | `AttributeSet`, `Line`, `Name`, `Number`, `Modulepath`, `Parameter`, `Dependency` | REIMPLEMENT | Define owned neutral `Task`, `TaskParameter`, command-template, environment, cwd, quiet, and dependency fields. |
| `src/invocation_parser.rs` | Just multi-invocation and recipe-argument grammar | `Justfile`, `Recipe`, `Invocation`, Just errors | DISCARD | Parse exactly one optional task plus its positional arguments in Spar CLI code. |
| `src/ran.rs` | keyed run-once locks across recipe arguments | `Recipe`, `Value`, mutexes and Just recipe identity | REIMPLEMENT | Graph plan deduplicates task names before execution; no Just cache keys or parallel locking in V1. |
| `src/lexer.rs`, `src/parser.rs`, `src/ast.rs`, `src/compiler.rs`, `src/analyzer.rs`, `src/evaluator.rs`, `src/loader.rs` | Just language frontend and evaluation | Just token/item/expression/assignment/module/setting systems and compile errors | DISCARD | Spar's lexer, parser, loader, resolver, type checker, and evaluator remain the sole frontend. |
| `src/config.rs`, `src/arguments.rs`, `src/search.rs`, `src/load_dotenv.rs`, `src/cache*.rs`, `src/formatter.rs`, completion modules | Just configuration, discovery, dotenv, caching, formatting, completion | Just CLI/settings/filesystem/cache/formatter subsystems | DISCARD | Explicit Spar file only; no donor configuration or ancillary subsystems. |

## Directly adapted concepts

The only concepts eligible for implementation are command construction,
platform shell branching, inherited standard streams, exit-status handling,
parent-environment extension, task cwd application, command echo, and
dry-run suppression of process spawn. Their source provenance is the `ADAPT`
rows above; their implementation belongs to neutral Spar runner modules, not
to copied Just source.

`REUSE` is intentionally absent: no Just Rust source unit can be reused
directly without importing its Just-specific types or semantics.

## V2 additions (2026-08-30)

V2 revisits several V1 boundary decisions above after a user-approved scope
expansion. These rows supersede the original ones for the units named —
the V1 rows above are left as-is for historical record of what V1 actually
decided and why, rather than silently overwritten.

| Just source unit | Relevant responsibility | Transitive Just-specific dependencies | V1 decision | V2 decision | Spar outcome |
| --- | --- | --- | --- | --- | --- |
| `src/executor.rs`, `src/recipe.rs::run_script` | temp-file scripts, shebang interpreter dispatch | `Interpreter`, `Shebang`, `Platform`, tempfile | DISCARD | ADAPT | A `run { ... }` block whose first line is `#!` is executed as one script: written to a temp file, `chmod +x` + direct exec on Unix, explicit `<interpreter> <path>` on Windows since it doesn't honor `#!`. `tempfile` promoted from dev- to a normal dependency. No script cache, output tracking, or attribute-driven interpreter selection — just the shebang line itself. |
| `src/load_dotenv.rs` (referenced from the V1 `config.rs`/`arguments.rs`/... DISCARD row) | load `.env` into the recipe environment | Just's dotenv crate integration, `Config`, `Settings` | DISCARD | ADAPT | A hand-rolled ~60-line parser (`src/dotenv.rs`) reads `KEY=VALUE` lines (comments, quoting) from a `.env` next to the source file, gated by a new `@DotenvLoad` file pragma (reusing the existing `@SchemaFile` pragma mechanism, not a Just concept). Loaded values never override an already-set real environment variable; a task's own `env: {}` always wins over both. |
| `src/settings.rs::shell` / `shell_command` (extends the V1 ADAPT row) | shell selection | `Settings`, `Interpreter` | ADAPT (fixed only) | ADAPT (fixed default + override) | Added a per-task `shell: [str];` field overriding the fixed `sh -cu`/`cmd /S /C` default for that task's non-script commands. Still no configurable global `set shell`/`set windows-shell` — override is per-task, declared the same way other task metadata is. |
| `src/search.rs` | locate the justfile by walking up from the invocation directory | `Config`, `dirs`, global-config-dir fallback, multiple candidate filenames | DISCARD | ADAPT | `spar tasks`/`run`/`show`/`dump` search the current directory and each parent for one canonical filename, `SparMake.spar`, when `-f`/`--file` is absent. No case variants, no global-config-dir fallback, no multi-name candidate list — `check`/`emit`/`fmt` are unaffected. |
| `src/attribute.rs`, `src/arg_attribute.rs` | recipe attributes (`[private]`, `[group(...)]`, `[confirm(...)]`, `[linux]`/`[macos]`/`[windows]`, and others out of scope) | Just's `[attr]`-before-recipe bracket syntax, full attribute enum | (not addressed) | REIMPLEMENT | `private`/`group`/`confirm`/`os` are ordinary task-body fields (`private: true;`), consistent with Spar's existing `description`/`default`/`quiet` style — not Just's bracket-attribute syntax, which is part of the justfile language and stays out. `os` matches against `std::env::consts::OS` at runtime (`"linux"`/`"macos"`/`"windows"`), erroring (not silently skipping) on mismatch whether the task is requested directly or as a dependency. |
| — (`--choose`, an external-chooser CLI feature, not a single source unit) | interactive recipe picker via an external binary (`fzf` by default) | `Chooser`, subprocess invocation of a user-configured binary | (not addressed) | REIMPLEMENT | `spar run --choose` is a minimal built-in numbered/grouped picker reading one line from stdin — no external chooser binary dependency, unlike Just's default `fzf` integration. |
| Recipe parameter model (`src/recipe.rs`, `src/parameter.rs`) | default parameter values, variadic (`*`/`+`) parameters | Just's parameter/variadic grammar and evaluation | REIMPLEMENT (fixed arity only) | REIMPLEMENT (extended) | Task parameters now support `name: type = <expr>` defaults (pre-evaluated the same way other task metadata is) and a single trailing `*name: type` variadic parameter capturing all remaining CLI arguments, space-joined unquoted on interpolation. |
