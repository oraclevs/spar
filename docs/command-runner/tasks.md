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
}
```

A task's name follows the same `[PascalCase]` convention as a section. `spar
tasks`/`spar run` address it by its lowercased form (`build`), and two tasks
whose names collide once lowercased are a compile error.

The `run { ... }` block holds raw shell text, not ordinary Spar statements —
quotes, pipes, redirects, and multiple commands separated by `;` are all
preserved verbatim and handed to the platform shell one command at a time, in
declaration order.

## Dependencies

```spar
task [Build] {
    run { cargo build; };
}

task [Test] {
    dependsOn: [Build];

    run { cargo test; };
}
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
}
```

```bash
spar run deploy.spar deploy production
```

Task parameters are scalar (`str`, `int`, `float`, or `bool`) — lists and
sections are rejected at compile time. A bare `${paramName}` reference in a
`run` block is substituted with the CLI-supplied argument, converted to the
declared type; wrong argument count or a value that doesn't parse as the
declared type is a runtime error before any command executes. A `${...}`
interpolation cannot combine a task parameter with anything else — reference
the parameter alone, or use a literal/global value instead.

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
}
```

Declared environment variables overlay the process's inherited environment
for that task's commands — nothing else is filtered or reset. Values are
ordinary Spar string expressions, so `"${port}"` referencing a global
`export var port` works the same way it does inside `run` blocks.

## Working directories

```spar
task [Web] {
    cwd: "./web";

    run {
        npm run dev;
    };
}
```

A relative `cwd` resolves against the directory containing the `.spar` file
that was passed to `spar run`/`spar tasks` — not the process's current
working directory.

## Default task

```spar
task [Test] {
    description: "Run the complete test suite";
    default: true;

    run {
        cargo test;
    };
}
```

Exactly one task may set `default: true;`. `spar run <file.spar>` with no
task name runs the default task; with no default configured, it prints a
diagnostic and the task list instead of guessing. Two or more tasks marked
default is a compile-time error naming all of them.

## Listing tasks

```bash
spar tasks server.spar
```

```
Available tasks:

  build       Build the project
  test        Run the test suite
  deploy      Deploy to an environment
```

## Running tasks

```bash
spar run server.spar               # default task
spar run server.spar build         # explicit task
spar run server.spar deploy prod   # explicit task with an argument
```

## Dry-run

```bash
spar run server.spar test --dry-run
```

`--dry-run` is accepted before or after a task's own arguments. It resolves
the full dependency graph, validates arguments and interpolation, and prints
every command it would run in execution order — without spawning any
process.

## Failure behavior

A command that exits non-zero stops that task (and anything depending on
it), and `spar run` exits non-zero itself. `spar` never suppresses a child
program's own stdout/stderr; a task marked `quiet: true;` only stops the
command line itself from being echoed before it runs.
