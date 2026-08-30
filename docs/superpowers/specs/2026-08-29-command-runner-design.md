# Spar Command Runner Design

## Goal

Add V1 command-runner tasks to Spar while keeping Spar's lexer, parser, AST,
resolver, type checker, evaluator, imports, and CLI as the frontend. Just at
`/home/occ/Projects/just` is a read-only donor for runtime behavior only.

## Scope

V1 includes task declarations, shell blocks, dependencies, cycle and missing
dependency diagnostics, scalar task arguments, environment, working directory,
quiet command echo, failure propagation, dependency run-once behavior, a default
task, task listing, dry-run, and inherited stdout/stderr. Multiple requested
tasks are deferred because positional task arguments make them ambiguous.

Make-style targets, timestamps, patterns, outputs, caches, Justfile syntax,
Just expressions, Just modules, aliases, formatting, LSP, and completion are out
of scope.

## Syntax

```spar
export var port: int = 8080;

task [Build] {
    description: "Build the project";
    default: true;
    quiet: false;
    env: {
        PORT: "${port}";
    };
    cwd: ".";
    run {
        cargo build --port ${port};
    };
};

task [Deploy](environment: str) {
    dependsOn: [Build];
    run {
        ./deploy.sh ${environment};
    };
};
```

`run` bodies are raw shell, not Spar statements. `${expr}` evaluates through
Spar. `$NAME` remains shell expansion. `#{...}` emits literal `${...}` for
advanced shell parameter expansion. A run block executes as one shell script,
so quoting, pipes, redirects, loops, and shell state survive between lines.

Task source names are PascalCase. CLI lookup accepts their lowercase form.

## Architecture

```text
Spar source -> lexer/parser -> task AST -> resolver/typechecker/evaluator
            -> task lowering -> Spar-owned runner IR -> graph -> executor -> OS
```

`src/runner/` owns neutral task, graph, executor, shell, and error types. It has
no dependency on Spar AST or parser types. `src/task_lowering.rs` is the only
language-to-runner adapter. A runner test must construct tasks directly.

The existing compiler facade retains the expanded program and evaluated values.
It will additionally expose lowered tasks only after normal compilation succeeds.
Existing JSON output excludes task declarations.

## Language integration

The lexer enters a task-run-body mode only after `run {` inside a task. It emits
literal shell fragments and ordinary Spar expression tokens for interpolation.
The parser builds task templates containing literal and expression parts.

The resolver registers tasks, detects duplicate names/defaults, resolves task
dependencies, and resolves interpolation and metadata expressions using existing
symbol rules. The type checker requires scalar parameter types, string `cwd` and
`description`, boolean `default` and `quiet`, and scalar environment values. The
lowerer converts evaluated Spar values and validated CLI arguments to runner IR.

Tasks follow existing import expansion. Public tasks may be selected or merged
with `asPartOf`; aliased imports do not invent a second task namespace in V1.

## Runner behavior

The graph validates every requested task and dependency before execution, uses
depth-first ordering, reports concrete cycles such as `A -> B -> A`, and tracks
completed tasks once per invocation. Failure stops dependents.

Unix runs raw blocks with `sh -cu`. Windows runs them with `cmd /S /C`. Child
stdout, stderr, and stdin are inherited. Environment extends the parent process.
Relative `cwd` is resolved from the `.spar` file's directory. Quiet suppresses
only command echo. Dry-run performs validation/interpolation and prints ordered
commands without spawning children.

## CLI

Spar currently requires explicit files, so V1 preserves that contract:

```text
spar tasks <file.spar>
spar run <file.spar> [task] [args...] [--dry-run]
```

With no task, exactly one default is required. With no default, the CLI reports
the problem and lists tasks. Unknown tasks and bad argument counts fail before
any child starts.

## Just extraction boundary

Adapt the minimal ideas behind command construction, inherited I/O, exit-status
handling, environment application, cwd, shell selection, and platform branches.
Reimplement graph planning against Spar task IR. Discard Just parser/compiler,
recipe AST, evaluator, module/import system, settings, formatter, and CLI.

Every directly adapted unit is recorded with the upstream commit and source file
in `docs/command-runner/just-extraction-map.md` and
`docs/command-runner/UPSTREAM-JUST.md`.

## Testing

Use strict red-green-refactor. Runner unit tests cover direct execution, ordering,
shared dependencies, graph failures, process failure, env, cwd, arguments,
dry-run, quiet, defaults, unknown tasks, and output usability. Parser/compiler
tests cover every syntax form and Spar-value interpolation. CLI tests cover task
listing, default and explicit runs, arguments, failure, and dry-run.

Final verification is formatting, check, Clippy with warnings denied, full tests,
and manual CLI smoke tests.
