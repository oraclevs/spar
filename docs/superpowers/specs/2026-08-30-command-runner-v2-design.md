# Command Runner V2 — Design Spec

**Status:** approved scope (user-selected buckets), design decisions below are
mine and should be treated as defaults to correct, not settled law.

**Builds on:** V1 (`docs/superpowers/specs/2026-08-29-command-runner-design.md`,
`docs/superpowers/plans/2026-08-29-command-runner.md`), already merged into
`feat/command-runner`. V2 does not change V1's core architecture (Spar
parser → `task_lowering` → `runner` IR → executor); it extends the IR and
grammar.

**Donor:** same read-only `~/Projects/just` at `b20386abdbae867a49cdff6c3c0f2b547faa9b23`.
V2 revisits some V1 DISCARD decisions in `just-extraction-map.md` — those
rows must be updated (from DISCARD to ADAPT/REIMPLEMENT) with the reasoning
for the change, not silently overwritten.

## Scope (user-selected, 2026-08-30)

- Recipe-level: shebang/script recipes; attributes (`private`, `group`,
  `os`, `confirm`); variadic and default parameters.
- Env/config: dotenv loading; shell customization.
- CLI: richer `spar tasks` listing; `--choose` interactive picker; a
  `show`/`dump` pair covering "show one task's commands" and "evaluate/dump
  the whole task catalog"; file discovery for a default `SparMake.spar`.
- Explicitly still out: aliases, export-all-vars setting, `mod`/justfile-style
  imports (Spar owns its own import system), anything from V1's out-of-scope
  list (caching, incremental builds, Make semantics, parallel execution).

## Breaking CLI change

V1 shipped `spar run <file.spar> [task] [args...] [--dry-run]` and
`spar tasks <file.spar>` with the file as a required positional. File
discovery makes the file *optional*, which collides with treating the next
bare word as a task name. Resolution: the file becomes a `-f`/`--file <path>`
flag (default: search); `tasks`/`run`/`show`/`dump` no longer take the file
as a positional. `check`/`emit`/`fmt` are unaffected — they keep their
explicit-file positional, this discovery behavior is task-runner-only.

```
spar tasks [-f FILE] [--all]
spar run   [task] [args...] [-f FILE] [--dry-run] [--choose]
spar show  <task> [args...] [-f FILE]
spar dump  [-f FILE]
```

Discovery: if `-f`/`--file` is absent, search the current directory, then
each parent directory in turn, for a file literally named `SparMake.spar`
(one canonical name, no case variants, no global-config-dir fallback — Just's
richer search is DISCARD here). Not found → clear diagnostic naming the
searched name, not a raw "file not found" from `std::fs`.

## Recipe attributes (body fields, not bracket syntax)

Kept as ordinary task-body fields, consistent with the existing
`description`/`default`/`quiet` fields — this is Spar's own declarative
style, not Just's `[attr]`-before-recipe bracket syntax (which really is
"the justfile language" and stays out).

```spar
task [Deploy] {
    private: true;
    group: "release";
    confirm: "Really deploy to production?";
    os: ["linux", "macos"];

    run { ./deploy.sh; };
}
```

- `private: bool` (default `false`) — hidden from `spar tasks` unless
  `--all`; still runnable by explicit name via `spar run deploy`.
- `group: str` (optional) — `spar tasks` clusters tasks under this heading;
  ungrouped tasks list under a plain heading first.
- `confirm: str` (optional; presence enables it) — before running (never
  during `--dry-run` or `--show`), print the message and a `[y/N]` prompt on
  stdin; declining exits 1 with "aborted" and runs nothing, including
  dependents.
- `os: [str]` (optional, non-empty, each value one of `"linux"`, `"macos"`,
  `"windows"`) — validated at lowering time against that fixed set. Checked
  at plan time against `std::env::consts::OS` (no `cfg!` needed — this is a
  runtime string comparison, so cross-compilation isn't a concern). A
  mismatch is a `RunnerError`, whether the task was requested directly or
  reached as a dependency — never a silent skip.

## Variadic and default parameters

```spar
task [Deploy](environment: str = "staging", *extra: str) {
    run { ./deploy.sh ${environment} ${extra}; };
}
```

- Default value: `name: type = <expr>`. The expr is pre-evaluated the same
  way task metadata is (`Evaluator::eval_standalone`), must type-check
  against the parameter's declared scalar type, and is used whenever the
  caller doesn't supply that positional argument.
- Variadic: at most one parameter, must be last, marked with a leading `*`
  and no default. Captures every remaining CLI argument as a `Vec<String>`.
  `${extra}` in a `run`/script body expands to those values space-joined,
  unquoted — same "no implicit shell quoting" contract V1 already has for
  ordinary parameters.
- Argument-count validation changes: minimum required = parameters without a
  default and without the variadic flag; maximum = unbounded if variadic,
  else parameter count. `graph::bind_arguments` needs rewriting, not just
  extending — the exact-count check it has today is a special case of this.

## Shebang / script recipes

Detected at **parse time**, not lowering time: if a `run { ... }` block's
raw content starts with `#!` (first non-whitespace characters), the parser
must *not* split it into multiple `;`-terminated `ShellCommand`s — the whole
block, semicolons and all, is one verbatim unit. `ast::ShellCommand` gets an
`is_shebang: bool` (or equivalent discriminant) the parser sets in that
case. `task_lowering` turns a shebang block into `runner::TaskCommand::Script`
instead of `TaskCommand::Shell` — this means `TaskCommand` becomes an enum:

```rust
pub enum TaskCommand {
    Shell(CommandTemplate),
    Script(CommandTemplate),
}
```

Executor behavior for `Script` (adapted from Just's `executor.rs` shebang
handling — this reverses V1's DISCARD on that file for this one behavior,
document the reversal in the extraction map):

- Resolve interpolation into the full script text.
- Write it to a temp file (`tempfile` — promote from dev-dependency to a
  normal dependency).
- Unix: `chmod +x`, execute the file directly; its own `#!` line selects the
  interpreter, exactly like Just.
- Windows: parse the shebang line's interpreter token and invoke
  `<interpreter> <tempfile>` explicitly, since Windows doesn't honor `#!`.
- Dry-run: print the resolved script text (not just a one-line command) and
  spawn nothing, same contract as `Shell`.

## dotenv loading

File-level, opt-in, reusing the existing `@SchemaFile`-style directive
convention already in the parser (see the `is_schema_file` handling):

```spar
@DotenvLoad
export var appName: str = "demo";
...
```

When present (must be the first line, before any other top-level item),
`task_lowering` looks for `.env` next to the source file (same directory as
`base_dir`) and parses simple `KEY=VALUE` lines (adapt just enough of Just's
`load_dotenv.rs` parsing rules to handle quoting/comments — do not pull in
its full dependency chain if a hand-rolled parser covers it in <50 lines).
Precedence, lowest to highest: dotenv file → inherited process environment →
task's own `env: {}` overlay. (I.e. dotenv never clobbers an already-set
real env var; a task's explicit `env:` always wins over both.) Missing
`.env` with `@DotenvLoad` present is not an error — it's a no-op, matching
Just's non-`dotenv-required` default.

## Shell customization

Per-task field, list of strings — program plus fixed arguments, with the
resolved script text appended as the final argument (same convention as
Just's `set shell := [...]`):

```spar
task [Web] {
    shell: ["bash", "-euo", "pipefail", "-c"];

    run { npm run dev; };
}
```

Overrides the fixed default (`sh -cu` unix / `cmd /S /C` windows) for that
task's `Shell` commands only — `Script` (shebang) commands ignore it, their
interpreter comes from the shebang line itself.

## CLI additions

- `spar tasks [--all]`: groups by `group:`, private tasks hidden unless
  `--all`.
- `spar run --choose`: with no task named, print a numbered list of
  runnable tasks (respecting `private`/`group` the same way `spar tasks`
  does) to stderr, read a line from stdin (accepts a number or a task name),
  then proceed exactly as if that name had been given on the command line.
  Built-in, no `fzf`/external chooser dependency (Just supports an external
  chooser binary; that's DISCARD here — one more moving part than this
  scope needs).
- `spar show <task> [args...]`: resolve (bind arguments, no dependency
  execution) and print that one task's commands — `Shell` as the joined
  script per `;`-command, `Script` as the full resolved script text — same
  rendering `--dry-run` already uses, without needing the rest of the
  dependency plan.
- `spar dump`: JSON-dump the whole lowered task catalog — name, description,
  group, private, confirm, os, dependencies, parameters (with defaults/
  variadic flag), environment, cwd, shell override, and commands as
  templates (literal parts resolved, parameter slots left as
  `"${name}"` placeholders). This is the closest match to Just's
  `--evaluate`/`--dump` pair without inventing three overlapping flags for
  one underlying need — flag if you wanted them kept genuinely separate.

## Open items to confirm before/while implementing

- Exact `.env` parsing edge cases (quoting, comments, multiline) — keep
  minimal, extend only if a real fixture needs it.
- Whether `confirm`'s prompt reads from a real TTY or plain stdin in tests —
  CLI integration tests will need a way to feed "y\n"/"n\n" without a PTY;
  use stdin piping, not a PTY-emulation dependency.
