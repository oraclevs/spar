# Task Runner

Spar tasks are command-runner tasks. They are not Make-style timestamp/file
build targets: there is no incremental rebuilding, no input/output tracking,
and no caching. A task runs its shell commands every time it is invoked.

See [`../../examples/tasks.spar`](../../examples/tasks.spar) for a complete,
checked-in example combining ordinary Spar configuration values with tasks.

## Declaring tasks

```spar
task [Build] {
    run {
        cargo build;
    };
};
```

A task's name follows the same `[PascalCase]` convention as a section. `spar
tasks`/`spar run` address it by its lowercased form (`build`), and two tasks
whose names collide once lowercased are a compile error.

`spar <name> [args...]` is shorthand for `spar run <name> [args...]` — any
first argument that isn't one of `spar`'s own subcommands (`check`, `emit`,
`fmt`, `tasks`, `run`, `show`, `dump`) is treated as a task name, so `spar
build` and `spar run build` are identical. If a task's lowercased name
collides with one of those reserved subcommands (a task named `Check`, say),
the subcommand always wins for bare dispatch — that task is only reachable
via the explicit `spar run check`, and `spar tasks`/`spar run`/`spar show`/
`spar dump` print a warning about the shadowing.

The `run { ... }` block holds raw shell text, not ordinary Spar statements —
quotes, pipes, redirects, and multiple commands separated by `;` are all
preserved verbatim and handed to the platform shell one command at a time, in
declaration order.

`${expr}` interpolates a Spar value, while bare shell variables such as
`$HOME` remain untouched. Write `#{...}` when the shell itself must receive a
literal `${...}` parameter expansion; for example, `#{HOME:-/tmp}` is executed
as `${HOME:-/tmp}`.

## Dependencies

```spar
task [Build] {
    run { cargo build; };
};

task [Test] {
    dependsOn: [Build];

    run { cargo test; };
};
```

`spar run test` resolves `Test`'s dependencies before running `Test` itself.
Dependencies are validated before anything executes:

- an unknown dependency (`dependsOn: [DoesNotExist];`) is a compile-time
  diagnostic;
- a dependency cycle (`A` depends on `B`, `B` depends on `A`) is a
  compile-time diagnostic naming the cycle path, e.g. `A -> B -> A`;
- a task reachable through more than one path runs at most once per
  invocation, in first-reached order.

If a command in the dependency chain fails, execution stops there — later
tasks (including the one originally requested) do not run, and `spar`
exits non-zero.

## Arguments

```spar
task [Deploy](environment: str) {
    run {
        ./deploy.sh ${environment};
    };
};
```

```bash
spar run deploy production -f deploy.spar
```

Task parameters are scalar (`str`, `int`, `float`, or `bool`) — lists and
sections are rejected at compile time. A bare `${paramName}` reference in a
`run` block is substituted with the CLI-supplied argument, converted to the
declared type; wrong argument count or a value that doesn't parse as the
declared type is a runtime error before any command executes. A `${...}`
interpolation cannot combine a task parameter with anything else — reference
the parameter alone, or use a literal/global value instead.

### Default values and variadic parameters

```spar
task [Deploy](environment: str = "staging", *extra: str) {
    run {
        ./deploy.sh ${environment} ${extra};
    };
};
```

- `name: type = <expr>` gives a parameter a default the caller may omit —
  the default is an ordinary Spar expression, type-checked against the
  parameter's declared type.
- A single trailing parameter may be marked variadic with a leading `*`
  (`*extra: str`). It captures every remaining CLI argument as a list;
  `${extra}` expands to those values space-joined, unquoted. A variadic
  parameter can't have a default, must be last, and only one is allowed.
- Required parameters (no default) must still come before any parameter
  with a default, and before the variadic parameter if present.

### Named arguments

```bash
spar run cpd out=result -f cpd.spar   # skip `file`, keep its default
spar run cpd out=result file=lib.dart -f cpd.spar   # any order
```

Positional arguments fill parameters in declaration order, so overriding a
later defaulted parameter normally means also supplying every parameter
before it. Named arguments (`name=value`) are the alternative: give any
subset of a task's parameters by name, in any order, and every parameter
you omit falls back to its default (or is an error, if it's required).

- A call is in named-argument mode only if its *first* argument has the
  form `name=value` where `name` is one of the task's declared parameters.
  Every remaining argument must then also be `name=value`, or it's an
  error — named and positional arguments never mix in one call.
- A name that isn't a declared parameter, or given twice for a
  non-variadic parameter, is a compile-time-checked runtime error before
  any command executes. A required parameter never given by name is also
  an error.
- A variadic parameter collects every argument with its name, in the
  order given: `tags=a tags=b` binds `tags` to `["a", "b"]`.
- Because mode is decided by the first argument only, a *positional*
  value that happens to contain `=` (e.g. `spar run setenv KEY=value`) is
  never mistaken for a named argument unless it exactly matches
  `<a real parameter name>=...` — so a single mistyped named argument
  (e.g. `out=result` misspelled as `otu=result`, where `otu` isn't a
  parameter) is instead read as one positional argument, not an error.
  Double-check the parameter name with `spar show`/`spar tasks` if a
  named-argument call doesn't behave as expected.

## Environment

```spar
task [Server] {
    env: {
        RUST_LOG: "debug";
        PORT: "8080";
    };

    run {
        cargo run;
    };
};
```

Declared environment variables overlay the process's inherited environment
for that task's commands — nothing else is filtered or reset. Values are
ordinary Spar string expressions, so `"${port}"` referencing a global
`export var port` works the same way it does inside `run` blocks.

### Loading a `.env` file

```spar
@LoadEnv

export var appName: str = "demo";
...
```

`@LoadEnv` must be the first line of the file. When present, `spar`
loads `KEY=VALUE` pairs (`#` comments, blank lines, and quoted values are
supported) from a `.env` file next to the `.spar` source, before every
task's own `env: {}` is applied. Precedence, lowest to highest:

Pass a string path to load a different file from the same directory, for
example `@LoadEnv(".env.production")`.

1. `.env` file values — never override a variable already set in the real
   process environment;
2. the inherited process environment;
3. the task's own `env: {}` block, which always wins.

A missing `.env` with `@LoadEnv` present is not an error.

## Working directories

```spar
task [Web] {
    cwd: "./web";

    run {
        npm run dev;
    };
};
```

A relative `cwd` resolves against the directory containing the `.spar` file
that was used — not the process's current working directory.

## Attributes

```spar
task [Deploy] {
    private: true;
    group: "release";
    confirm: "Really deploy to production?";

    run { ./deploy.sh; };
};
```

- `private: bool` — hidden from `spar tasks` unless `--all` is passed;
  still runnable by explicit name (`spar run deploy`).
- `group: str` — `spar tasks` clusters tasks under this heading; tasks with
  no group list first, under `Ungrouped`.
- `confirm: str` — before running (never during `--dry-run` or `spar
  show`), prints the message with a `[y/N]` prompt on stdin. Every
  `confirm`-marked task in the plan is prompted upfront, in dependency
  order, before any command runs; declining any one of them aborts the
  whole plan with a non-zero exit — nothing runs, not even an earlier
  task without its own `confirm`.

## OS-specific run blocks

A task can declare more than one `run` block, each labeled with a platform:

```spar
task [Build] {
    run windows {
        cmd /c build.bat;
    };
    run linux {
        ./build.sh;
    };
    run macos {
        ./build.sh;
    };
};
```

At lowering time, Spar picks the block whose label matches the running
platform (`windows`, `linux`, or `macos`, from `std::env::consts::OS`) —
checked whether the task was requested directly or reached as a
dependency. A label-less `run { ... }` block, if present alongside labeled
ones, is the fallback when no label matches the current platform; if
neither a matching label nor a label-less block exists, that's a
compile-time error, not a silent skip. A bare `run { ... }` with no other
`run` blocks (the common case) always runs, on every platform.

## Shell customization

```spar
task [Web] {
    shell: ["bash", "-euo", "pipefail", "-c"];

    run { npm run dev; };
};
```

Overrides the default shell (`sh -cu` on Unix, `cmd /S /C` on Windows) used
to run that task's `run { ... }` commands. A shebang script (below) ignores
this — its own `#!` line picks the interpreter.

## Script (shebang) recipes

```spar
task [Script] {
    run {
        #!/usr/bin/env bash
        set -euo pipefail
        echo "one interpreter, one script"
    };
};
```

If a `run { ... }` block's first line starts with `#!`, the whole block is
treated as one script instead of being split into separate `;`-terminated
commands. `spar` resolves interpolation, writes the script to a temporary
file, and on Unix marks it executable and runs it directly (its own `#!`
line picks the interpreter); on Windows the shebang line's interpreter is
invoked explicitly against the temp file, since Windows doesn't honor `#!`.

## Default task

```spar
task [Test] {
    description: "Run the complete test suite";
    default: true;

    run {
        cargo test;
    };
};
```

Exactly one task may set `default: true;`. `spar run` with no task name
runs the default task; with no default configured, it prints a diagnostic
and the task list instead of guessing. Two or more tasks marked default is
a compile-time error naming all of them.

## Finding the task file

```bash
spar tasks -f server.spar     # explicit file
spar run   --file server.spar deploy production
spar tasks                    # no file: search for it
```

`-f`/`--file` points `tasks`/`run`/`show`/`dump` at an explicit `.spar`
file. Without it, `spar` searches the current directory and then each
parent directory in turn for a file literally named `SparMake.spar`; not
found is a clear diagnostic naming that filename, not a raw filesystem
error. `check`/`emit`/`fmt` are unaffected by discovery — they always take
an explicit file positional.

## Listing tasks

```bash
spar tasks -f server.spar
```

```
Ungrouped:
  build       Build the project
  test        Run the test suite

release:
  deploy      Deploy to an environment
```

Tasks are grouped by `group:` (ungrouped tasks list first, under
`Ungrouped`), sorted by name within each group. `private: true;` tasks are
hidden unless `--all` is passed.

## Running tasks

```bash
spar run -f server.spar               # default task
spar run build -f server.spar         # explicit task
spar run deploy prod -f server.spar   # explicit task with an argument
spar run --choose -f server.spar      # pick a task interactively

# bare shorthand — identical to the `run` forms above
spar -f server.spar
spar build -f server.spar
spar deploy prod -f server.spar
```

`--choose` (only valid with no task name given) prints a numbered,
grouped list of runnable tasks to stderr and reads a number or task name
from stdin, then proceeds exactly as if that name had been given directly.
It never offers a `private` task, `--all` or not.

## Dry-run

```bash
spar run test --dry-run -f server.spar
```

`--dry-run` is accepted before or after a task's own arguments. It resolves
the full dependency graph, validates arguments and interpolation, and prints
every command it would run in execution order (a shebang task prints its
full resolved script) — without spawning any process, and without
triggering a `confirm:` prompt.

## Inspecting tasks

```bash
spar show deploy production -f server.spar   # one task's resolved commands
spar dump -f server.spar                     # the whole task catalog as JSON
```

`spar show <task> [args...]` binds arguments and prints that one task's
commands (or full script, for a shebang task) without resolving
dependencies or executing anything. `spar dump` prints every declared
task's full metadata and command templates as JSON — parameter slots that
depend on a CLI argument (`${name}`) are left as unbound placeholders,
since `dump` has no arguments to bind.

## Failure behavior

A command that exits non-zero stops that task (and anything depending on
it), and `spar run` exits non-zero itself. `spar` never suppresses a child
program's own stdout/stderr; `quiet` only stops the command line itself
from being echoed before it runs (and, on failure, keeps the full
script out of the error message).

Tasks are `quiet: true;` by default — commands never echo unless the
task sets `quiet: false;` explicitly. This is a security default: tasks
loaded via `@LoadEnv` often interpolate secret values into the rendered
command line, and quiet-by-default keeps those values out of the shell
and logs. `--dry-run` always shows the full command plan regardless of
`quiet`, since previewing commands is the point of `--dry-run`.
