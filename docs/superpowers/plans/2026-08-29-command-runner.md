# Spar Command Runner Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the approved V1 Spar-native task runner and explicit-file CLI.

**Architecture:** Spar parses and validates task declarations, then `task_lowering` converts them into parser-independent runner types. `runner` validates the dependency graph and executes platform shell processes; no Just AST crosses this boundary.

**Tech Stack:** Rust 2021, standard library process APIs, existing Spar compiler pipeline, `tempfile` tests.

**Spec:** `docs/superpowers/specs/2026-08-29-command-runner-design.md`

## Global Constraints

- Donor `/home/occ/Projects/just` is read-only at commit `b20386abdbae867a49cdff6c3c0f2b547faa9b23`, license CC0-1.0.
- Implement only approved V1; no Justfile parser or Make behavior.
- Every production behavior starts with a failing test and observed RED result.
- Keep existing configuration evaluation and JSON output unchanged.
- CLI keeps an explicit `.spar` file.

---

### Task 1: Architecture and provenance audit

**Files:**
- Create: `docs/command-runner/architecture-audit.md`
- Create: `docs/command-runner/just-extraction-map.md`
- Create: `docs/command-runner/UPSTREAM-JUST.md`

**Interfaces:**
- Consumes: current Spar and local Just source trees.
- Produces: source-of-truth extraction boundary for later tasks.

- [ ] Record Spar's exact lexer-to-CLI pipeline and the baseline: 599 passed, 0 failed.
- [ ] Record Just's exact CLI-to-process pipeline and baseline: 575 unit plus 1,836 integration passed, 18 ignored, 0 failed.
- [ ] For each named Just source unit, list transitive Just-specific dependencies and classify it `REUSE`, `ADAPT`, `REIMPLEMENT`, or `DISCARD`.
- [ ] Record upstream URL, commit, license, extraction date, and adapted concepts.
- [ ] Run `git diff --check`, then commit the three documents as `docs: audit command runner extraction`.

### Task 2: Neutral runner model and catalog validation

**Files:**
- Create: `src/runner/mod.rs`
- Create: `src/runner/task.rs`
- Create: `src/runner/error.rs`
- Modify: `src/lib.rs`
- Test: unit tests beside runner modules.

**Interfaces:**
- Produces: `Task`, `TaskCommand`, `CommandTemplate`, `TemplatePart`, `TaskParameter`, `ScalarKind`, `TaskInvocation`, `TaskSet`, and `RunnerError`.
- `runner` must not import `crate::ast`, `Parser`, `Resolver`, or `Evaluator`.

- [ ] Write a failing test constructing `Task` directly and rejecting duplicate task names.
- [ ] Run `cargo test runner::task::tests::duplicate_task_names_fail`; confirm RED from missing runner API.
- [ ] Implement the minimal model using owned strings, `BTreeMap`, and `PathBuf`; implement `TaskSet::new(Vec<Task>) -> Result<TaskSet, RunnerError>`.
- [ ] Run the focused test; confirm GREEN.
- [ ] Add one failing test each for zero/multiple defaults and unknown requested task, observing RED before each implementation.
- [ ] Implement `TaskSet::default_task()` and `TaskSet::get(name)` with lowercase CLI-name lookup and collision validation.
- [ ] Run `cargo test runner::`; refactor only while green.
- [ ] Commit as `feat: add neutral task runner model`.

### Task 3: Dependency graph and invocation binding

**Files:**
- Create: `src/runner/graph.rs`
- Modify: `src/runner/task.rs`
- Modify: `src/runner/error.rs`
- Test: unit tests in `src/runner/graph.rs`.

**Interfaces:**
- Consumes: `TaskSet`, `TaskInvocation`, scalar CLI strings.
- Produces: `ExecutionPlan { tasks: Vec<BoundTask> }`, where `BoundTask` has rendered parameter values but still-neutral command templates.

- [ ] Write and observe failing tests for dependency-first order and shared dependency run-once.
- [ ] Implement depth-first planning with permanent/temporary visit sets.
- [ ] Write and observe failing tests for missing dependency, direct cycle, and indirect cycle with literal cycle path `A -> B -> A`.
- [ ] Implement preflight dependency and cycle validation before returning any plan.
- [ ] Write and observe failing tests for scalar `str`, `int`, `float`, and `bool` argument conversion plus wrong count/type.
- [ ] Implement `TaskSet::plan(requested, args)`; only the requested task consumes CLI arguments in V1.
- [ ] Run `cargo test runner::graph`; then `cargo test runner::`.
- [ ] Commit as `feat: plan task dependency execution`.

### Task 4: Process executor

**Files:**
- Create: `src/runner/executor.rs`
- Create: `src/runner/shell.rs`
- Create: `src/runner/environment.rs`
- Modify: `src/runner/mod.rs`
- Modify: `src/runner/error.rs`
- Test: unit tests in executor and shell modules.

**Interfaces:**
- Consumes: `ExecutionPlan`, `ExecutionOptions { dry_run, base_dir }`.
- Produces: `ExecutionReport { commands: Vec<String> }` or `RunnerError` containing task, command, and exit status.

- [ ] Write and observe RED for a direct-IR task whose shell command creates a temporary marker file.
- [ ] Implement platform shell selection: Unix `sh -cu`; Windows `cmd /S /C`; inherit stdio.
- [ ] Add RED/GREEN cycles for command declaration order, command failure, and dependent suppression.
- [ ] Add RED/GREEN cycles for environment inheritance/override and cwd relative to `base_dir`.
- [ ] Add RED/GREEN cycles proving dry-run creates no marker and reports dependency order.
- [ ] Add RED/GREEN cycles proving quiet hides command echo only, not child output.
- [ ] Run `cargo test runner::`; run `cargo clippy --all-targets --all-features -- -D warnings`.
- [ ] Commit as `feat: execute task shell commands`.

### Task 5: Task lexer, AST, parser, and formatter

**Files:**
- Modify: `src/token.rs`
- Modify: `src/lexer.rs`
- Modify: `src/ast.rs`
- Modify: `src/parser.rs`
- Modify: `src/formatter.rs`
- Test: lexer/parser/formatter unit tests.

**Interfaces:**
- Produces: `TopLevelItem::Task(TaskDecl)` with metadata expressions, typed `Param`s, dependencies, and `ShellTemplatePart::{Literal, Expr}`.
- Raw shell text must retain quotes, redirects, pipes, line breaks, semicolons, and braces.

- [ ] Write a lexer test for a raw `run` block containing quotes, `|`, `>`, loop braces, `${port}`, `$HOME`, and `#{HOME:-x}`; observe RED.
- [ ] Implement task-scoped raw-run lexer state and interpolation tokenization without changing ordinary `run` identifiers.
- [ ] Write parser RED tests for minimal task, dependencies, parameters, env, cwd, description/default/quiet, and malformed declarations.
- [ ] Implement `parse_task_decl` and task-specific field parsing; do not parse shell fragments as statements.
- [ ] Write formatter round-trip RED tests, implement canonical task formatting, and keep shell body content stable.
- [ ] Run `cargo test lexer:: parser:: formatter::` and full `cargo test`.
- [ ] Commit as `feat: parse Spar task declarations`.

### Task 6: Resolution, type checking, imports, and lowering

**Files:**
- Modify: `src/resolver.rs`
- Modify: `src/typechecker.rs`
- Modify: `src/evaluator.rs`
- Modify: `src/loader.rs`
- Create: `src/task_lowering.rs`
- Modify: `src/compiler.rs`
- Modify: `src/lib.rs`
- Test: resolver/typechecker/evaluator/compiler tests.

**Interfaces:**
- Produces: lowered `TaskSet` from the expanded `Program`, `SymbolTable`, evaluated configuration values, and task invocation arguments.
- `task_lowering` is the sole module allowed to translate `ast::TaskDecl` into `runner` types.

- [ ] Write and observe RED diagnostics for duplicate tasks, duplicate defaults, unknown dependencies, bad metadata types, non-scalar parameters, and task-local unknown names.
- [ ] Register and resolve tasks and task-local parameters while reusing normal expression resolution.
- [ ] Type-check metadata and interpolation through existing `SparType`; reject lists/sections as task parameters.
- [ ] Write import RED tests for selective and `asPartOf` public task behavior, then extend loader match arms without importing Just module semantics.
- [ ] Write the critical RED integration test where a normal Spar value and task argument both reach a command template.
- [ ] Implement lowering: pre-evaluate ordinary Spar expressions, preserve neutral parameter slots, escape `#{` to literal `${`, and reject unsupported parameter-dependent compound expressions with a precise diagnostic if the existing evaluator cannot represent them safely.
- [ ] Extend `Compilation` with lowered tasks only when normal compilation succeeds; do not add tasks to emitted JSON.
- [ ] Run focused compiler tests and full `cargo test`.
- [ ] Commit as `feat: lower Spar tasks into runner IR`.

### Task 7: CLI integration

**Files:**
- Modify: `src/main.rs`
- Modify: `tests/cli_golden.rs` or create `tests/task_cli.rs`
- Add: portable shell fixtures under `tests/fixtures/tasks/`.

**Interfaces:**
- Produces: `spar tasks <file>`, `spar run <file> [task] [args...] [--dry-run]`.

- [ ] Write CLI RED tests for listing tasks with descriptions and stable lowercase names.
- [ ] Implement `Cmd::Tasks` and listing from the compiler's lowered task set.
- [ ] Write CLI RED tests for explicit task, default task, typed arguments, unknown task, no default, multiple defaults, failure exit, and dry-run.
- [ ] Implement unambiguous parsing where `--dry-run` is accepted before or after task arguments and removed before binding.
- [ ] Render compilation errors with existing `ErrorRenderer`; render runner failures without hiding child output.
- [ ] Run `cargo test --test task_cli` and full `cargo test`.
- [ ] Commit as `feat: add task runner CLI commands`.

### Task 8: Example and user documentation

**Files:**
- Create: `examples/tasks.spar`
- Create: `docs/command-runner/tasks.md`
- Modify: `README.md`
- Modify: `docs/command-runner/just-extraction-map.md` if adaptation changed.

**Interfaces:**
- Documents the exact checked implementation and file-first CLI.

- [ ] Add one example combining exported configuration values, a default task, dependency, env, argument, and interpolation.
- [ ] Document declarations, dependencies, arguments, env, cwd, quiet, defaults, listing, running, dry-run, failures, interpolation escape, and platform shells.
- [ ] State: “Spar tasks are command-runner tasks. They are not Make-style timestamp/file build targets.”
- [ ] Smoke-test the checked-in example with task listing, default dry-run, and explicit dry-run.
- [ ] Run `git diff --check`; commit as `docs: document Spar task runner`.

### Task 9: Verification and review

**Files:**
- Modify only files required by verified defects.

**Interfaces:**
- Produces final evidence and no known V1 defects.

- [ ] Use `superpowers:verification-before-completion` and run `cargo fmt --check`.
- [ ] Run `cargo check`.
- [ ] Run `cargo clippy --all-targets --all-features -- -D warnings`.
- [ ] Run `cargo test` and capture every suite summary.
- [ ] Run manual `spar tasks`, default run, explicit run, argument run, failure, and dry-run smoke tests against temporary files.
- [ ] Use the requested code-review skill against the pre-feature commit `137a020`; fix findings with RED/GREEN proof.
- [ ] Re-run all verification after fixes and record exact summaries in `docs/command-runner/verification.md`.
- [ ] Commit as `docs: record command runner verification`.
