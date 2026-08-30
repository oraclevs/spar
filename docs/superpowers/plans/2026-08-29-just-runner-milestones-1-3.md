# Just Runner Milestones 1–3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Copy Just into Spar, expose a cooked-task bridge, and execute `task [Hello] { run { echo hello; }; }` through Just's runtime.

**Architecture:** Keep a near-verbatim Just library at `crates/command-runner`. New Spar task syntax produces command parts containing Spar expressions; Spar evaluates them to cooked strings, then the bridge constructs Just `Recipe`/`Justfile` runtime values and invokes existing execution code.

**Tech Stack:** Rust 2024 command-runner crate, Rust 2021 Spar crate, Cargo path dependency, existing Just dependencies, Spar lexer/parser/evaluator, Just process runtime.

**Spec:** `docs/superpowers/specs/2026-08-29-just-runner-milestones-1-3-design.md`

## Global Constraints

- Do not modify `/home/occ/Projects/just`.
- Do not expose or feed Just syntax from Spar.
- Preserve copied Just source layout and CC0 license.
- Scope ends at milestones 1–3; dependencies, parameters, env, cwd settings, Clap redesign, and LSP features remain later.
- Add production behavior only after its focused test fails for the expected reason.

---

### Task 1: Transplant the Just crate

**Files:**
- Create: `crates/command-runner/Cargo.toml`
- Copy unchanged: `/home/occ/Projects/just/src/**` to `crates/command-runner/src/**`
- Copy unchanged: `/home/occ/Projects/just/completions/**`, `CHANGELOG.md`, and `LICENSE`
- Modify: `Cargo.toml`
- Modify: `Cargo.lock` through Cargo

**Interfaces:**
- Produces: local crate `spar-command-runner`, initially with Just's existing `run` API.

- [ ] **Step 1: Record clean baselines**

Run `cargo test` in `spar`, then `cargo check` and `cargo test --lib` in `/home/occ/Projects/just`. Expected: PASS before copying.

- [ ] **Step 2: Copy Just mechanically**

Copy the listed tree and assets. Derive the new manifest from Just's manifest with these exact package changes:

```toml
[package]
name = "spar-command-runner"
version = "0.1.0"
edition = "2024"
rust-version = "1.89.0"
publish = false
autobins = false
autotests = false
license = "CC0-1.0"
```

Keep Just's normal and target dependencies. Remove its `[workspace]`, `[[bin]]`, and external integration-test declaration. Keep `[lib] doctest = false`.

- [ ] **Step 3: Attach the path dependency**

Add to Spar's root manifest:

```toml
spar-command-runner = { path = "crates/command-runner" }
```

- [ ] **Step 4: Prove milestone 1**

Run `cargo check`, `cargo tree --manifest-path crates/command-runner/Cargo.toml --depth 1`, and `cargo test --manifest-path crates/command-runner/Cargo.toml --lib` from `spar`. Expected: copied code compiles; dependency tree uses one compatible version per direct dependency; retained library tests pass.

- [ ] **Step 5: Commit milestone 1**

```bash
git add Cargo.toml Cargo.lock crates/command-runner
git commit -m "feat: transplant Just command runner"
```

---

### Task 2: Add the cooked-task bridge

**Files:**
- Create: `crates/command-runner/src/bridge.rs`
- Modify: `crates/command-runner/src/lib.rs`

**Interfaces:**
- Produces: `CookedTask { name: String, commands: Vec<String> }`
- Produces: `RunOptions { working_directory: PathBuf, dry_run: bool, quiet: bool }`
- Produces: `run_task(&CookedTask, &RunOptions) -> Result<(), RunError>`

- [ ] **Step 1: Read the test rules**

Read `superpowers:test-driven-development/writing-good-tests.md`. Name the production change that makes each bridge test fail: the missing `run_task` bridge.

- [ ] **Step 2: Write the failing bridge test**

In `bridge.rs`, add a unit test that uses a temp directory and runs:

```rust
let task = CookedTask::new("Hello", ["echo hello > hello.txt"]);
run_task(&task, &RunOptions::for_directory(temp.path())).unwrap();
assert_eq!(std::fs::read_to_string(temp.path().join("hello.txt")).unwrap(), "hello\n");
```

Also add a failure test using `exit 7` and assert `error.code() == Some(7)`.

- [ ] **Step 3: Verify RED**

Run `cargo test --manifest-path crates/command-runner/Cargo.toml bridge::tests -- --nocapture`. Expected: compile failure because bridge types/functions do not exist.

- [ ] **Step 4: Build runtime values, not Just source**

Implement the public types above. Inside `bridge.rs`, construct private Just values directly:

```rust
Recipe {
  attributes: AttributeSet::new(),
  body: commands_as_text_fragment_lines,
  dependencies: Vec::new(),
  doc: None,
  file_depth: 0,
  import_offsets: Vec::new(),
  module_path: Some(root.clone()),
  name: synthetic_identifier_name,
  number: Number(0),
  parameters: Vec::new(),
  priors: 0,
  private: false,
  quiet: false,
  recipe_path: Some(root.join(task.name())),
  shebang: false,
  variable_references: HashSet::new(),
}
```

Build `Justfile` with empty assignment/alias/module/function/group/warning tables and sets, root `module_path`, `default: Some(recipe.clone())`, a recipe table containing that recipe, `Settings::default()`, and `working_directory`/`source` derived from `RunOptions`. Build `Config::new()` with `invocation_directory`, `dry_run`, `load_dotenv: false`, `no_cache: true`, and quiet/taciturn verbosity from options. Build `Search { justfile: source, tempdir: None, working_directory }`, install Just's signal handler, and call `Justfile::run` with `[task.name()]`. Convert Just errors to public `RunError { message, code }` using `error.color_display(Color::never()).to_string()` and `Error::code()`.

- [ ] **Step 5: Verify GREEN**

Run `cargo test --manifest-path crates/command-runner/Cargo.toml bridge::tests -- --nocapture`, then `cargo test --manifest-path crates/command-runner/Cargo.toml --lib`. Expected: PASS; the file contains `hello\n`; exit code 7 is retained.

- [ ] **Step 6: Commit milestone 2**

```bash
git add crates/command-runner/src/bridge.rs crates/command-runner/src/lib.rs
git commit -m "feat: run cooked tasks through Just runtime"
```

---

### Task 3: Parse Spar task and run-block syntax

**Files:**
- Modify: `src/token.rs`
- Modify: `src/lexer.rs`
- Modify: `src/ast.rs`
- Modify: `src/parser.rs`
- Modify: `src/formatter.rs`
- Modify compile-only exhaustive matches in `src/loader.rs`, `src/resolver.rs`, `src/typechecker.rs`, and `src/evaluator.rs`
- Test: `src/tests/parser_tests.rs`

**Interfaces:**
- Produces: `TopLevelItem::Task(TaskDecl)`
- Produces: `TaskDecl { name: String, name_span: Span, commands: Vec<TaskCommand>, span: Span }`
- Produces: `TaskCommand { parts: Vec<StringPart>, span: Span }`

- [ ] **Step 1: Write failing lexer/parser tests**

Add tests for:

```spar
task [Hello] {
    run {
        echo hello;
    };
};
```

Assert one task named `Hello`, one command, and one literal part `echo hello`. Add malformed cases for missing `run`, missing command semicolon, and empty task name.

- [ ] **Step 2: Verify RED**

Run `cargo test parser_tests::task -- --nocapture`. Expected: lex/parse failure at the first unsupported task/run construct.

- [ ] **Step 3: Add task-aware command tokens**

Add `KwTask`, `KwRun`, `CommandFragment(String)`, and `CommandEnd`. The lexer enters raw command mode only after `KwRun` plus `{`. In that mode it:

- preserves shell text between delimiters;
- emits `CommandEnd` for the declaration `;`;
- emits `InterpolStart`, normal Spar expression tokens, and `InterpolEnd` for `${...}`;
- emits the run block's `RBrace`;
- reports unterminated interpolation, command, or run block with the original span.

- [ ] **Step 4: Add AST and parser**

Parse this exact grammar:

```text
taskDecl := "task" "[" ident "]" "{" "run" "{" command+ "}" ";" "}"
command  := (CommandFragment | "${" expression "}")+ CommandEnd
```

Reuse `StringPart::Literal` and `StringPart::Expr`. Add `TopLevelItem::Task` handling to internal exhaustive matches. Formatter output must retain tasks instead of dropping them; LSP semantic behavior is not added in this scope.

- [ ] **Step 5: Verify GREEN**

Run `cargo test parser_tests::task -- --nocapture` and `cargo test formatter::tests -- --nocapture`. Expected: task tests and existing formatter tests pass.

- [ ] **Step 6: Commit syntax**

```bash
git add src/token.rs src/lexer.rs src/ast.rs src/parser.rs src/formatter.rs src/loader.rs src/resolver.rs src/typechecker.rs src/evaluator.rs src/tests/parser_tests.rs
git commit -m "feat: parse Spar task run blocks"
```

---

### Task 4: Adapt parsed Spar tasks to cooked Just tasks

**Files:**
- Create: `src/task.rs`
- Modify: `src/lib.rs`
- Modify: `src/evaluator.rs`
- Test: `src/tests/task_tests.rs`
- Modify: `src/tests/mod.rs`

**Interfaces:**
- Consumes: `TaskDecl`, Spar `Program`/`SymbolTable`, and `spar_command_runner::run_task`.
- Produces: `cook_task(program, symbols, task_name) -> Result<CookedTask, Vec<SparError>>`
- Produces: `run_task_source(source, task_name, working_directory) -> Result<(), TaskRunError>`

- [ ] **Step 1: Write failing adapter tests**

Add an end-to-end test that parses and runs:

```spar
task [Hello] { run { echo hello > hello.txt; }; };
```

Assert `hello.txt == "hello\n"`. Add an interpolation test:

```spar
var greeting: str = "hello";
task [Hello] { run { echo ${greeting} > hello.txt; }; };
```

Assert the same result. Add unknown-task and non-scalar interpolation error tests.

- [ ] **Step 2: Verify RED**

Run `cargo test task_tests -- --nocapture`. Expected: compile failure because `task` module and adapter APIs do not exist.

- [ ] **Step 3: Evaluate with Spar, then call Just**

Add an evaluator entry point that runs Spar's normal global evaluation, then evaluates each `StringPart::Expr` with the populated Spar evaluator caches. Scalars use `ConfigValue::coerce_to_str`; lists/sections return `SparError::EvalError`.

`cook_task` finds the unique task, joins its literal and evaluated parts, trims declaration indentation, and returns `CookedTask`. `run_task_source` runs the existing compiler stages, stops on Spar diagnostics, cooks the named task, then calls the copied bridge. It does not generate Just source.

- [ ] **Step 4: Verify GREEN**

Run `cargo test task_tests -- --nocapture`, then `cargo test`. Expected: literal and interpolated tasks execute through Just; bad tasks return stable errors; old Spar tests pass.

- [ ] **Step 5: Commit milestone 3**

```bash
git add src/task.rs src/lib.rs src/evaluator.rs src/tests/task_tests.rs src/tests/mod.rs
git commit -m "feat: execute Spar tasks with Just runner"
```

---

### Task 5: Cross-crate verification and report

**Files:**
- No planned production files; any failure triggers a return to the owning task's RED/GREEN loop.

- [ ] **Step 1: Check source provenance**

Run `git -C /home/occ/Projects/just status --short` and `git status --short`. Expected: Just is unchanged; Spar contains only intended work.

- [ ] **Step 2: Run required checks**

From `spar`, run:

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

Also run `cargo check` and `cargo test` in `spar-ls` and `spar-wasm` because they consume Spar's public AST. Expected: all pass.

- [ ] **Step 3: Inspect dependencies**

Run `cargo tree -d` and `cargo tree --manifest-path crates/command-runner/Cargo.toml --depth 1`. Record unavoidable duplicates; remove unused copied dependencies only if Cargo proves they are not required.

- [ ] **Step 4: Final diff audit**

Run `git diff --check` and inspect `git diff --stat` plus commits since `93405cf`. Report copied Just files, copied files changed, rejected files, Spar files changed, bridge, syntax, tests, and all milestones 4–8 as not yet mapped.
