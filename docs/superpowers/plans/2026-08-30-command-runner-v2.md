# Command Runner V2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend Spar's V1 command runner with recipe metadata, flexible parameters, script recipes, dotenv and shell configuration, task-file discovery, and richer task CLI commands.

**Architecture:** Keep the existing parser -> `task_lowering` -> neutral `runner` IR -> graph/executor pipeline. Deepen the runner interface with reusable binding and rendering so `run`, `show`, and `dump` do not each implement template semantics; keep source-language evaluation and dotenv adaptation in `task_lowering`, and keep filesystem discovery in the CLI.

**Tech Stack:** Rust 2021, existing hand-written Spar lexer/parser/compiler, `serde_json`, `std::process`, and `tempfile` as a runtime dependency for shebang scripts.

**Spec:** `docs/superpowers/specs/2026-08-30-command-runner-v2-design.md`

## Global Constraints

- Build on V1 without changing the parser -> `task_lowering` -> runner IR -> executor architecture.
- Donor `/home/occ/Projects/just` remains read-only at `b20386abdbae867a49cdff6c3c0f2b547faa9b23`.
- Update V1 `DISCARD` decisions to `ADAPT`/`REIMPLEMENT` with reasons; never silently replace provenance rows.
- Do not add aliases, export-all-vars, Just modules/imports, caching, incremental builds, Make semantics, parallel execution, or an external chooser.
- Task-runner file discovery searches only for literal `SparMake.spar`, from current directory through its parents.
- Preserve explicit file positionals for `check`, `emit`, and `fmt`.
- Every production behavior starts with a failing test whose failure is observed before implementation.
- Baseline on 2026-08-30: 625 library tests and 39 integration/doc tests pass; 0 failures.
- Preserve the user's existing change to `examples/tasks.spar` unless the documentation task deliberately extends it.

---

### Task 1: Deepen the neutral runner model and template interface

**Files:**
- Modify: `src/runner/task.rs`
- Modify: `src/runner/graph.rs`
- Modify: `src/runner/error.rs`
- Modify: `src/runner/mod.rs`

**Interfaces:**
- Produces `TaskCommand::{Shell(CommandTemplate), Script(CommandTemplate)}`.
- Produces `TaskParameter { name, kind, default: Option<String>, variadic: bool }`.
- Produces `BoundValue::{Scalar(String), Variadic(Vec<String>)}` and `BoundTask { parameter_values: BTreeMap<String, BoundValue> }`.
- Adds task fields `private`, `group`, `confirm`, `os`, and `shell` with owned runner-only types.
- Produces `CommandTemplate::render(&BTreeMap<String, BoundValue>) -> String` and `render_unbound() -> String`.

- [ ] **Step 1: Write runner-model RED tests**

  Add tests constructing both command variants and a task with every V2 field. Add a renderer test with literal expected output:

  ```rust
  let template = CommandTemplate { parts: vec![
      TemplatePart::Literal("deploy ".into()),
      TemplatePart::Parameter("environment".into()),
      TemplatePart::Literal(" ".into()),
      TemplatePart::Parameter("extra".into()),
  ]};
  let values = BTreeMap::from([
      ("environment".into(), BoundValue::Scalar("staging".into())),
      ("extra".into(), BoundValue::Variadic(vec!["--force".into(), "blue".into()])),
  ]);
  assert_eq!(template.render(&values), "deploy staging --force blue");
  assert_eq!(template.render_unbound(), "deploy ${environment} ${extra}");
  ```

- [ ] **Step 2: Observe RED**

  Run `cargo test runner::task::tests::renders_scalar_and_variadic_values` and confirm compilation fails because `BoundValue`, command variants, and render methods do not exist.

- [ ] **Step 3: Implement the minimal runner model**

  Replace the V1 command wrapper with:

  ```rust
  pub enum TaskCommand {
      Shell(CommandTemplate),
      Script(CommandTemplate),
  }

  pub enum BoundValue {
      Scalar(String),
      Variadic(Vec<String>),
  }
  ```

  Add these `Task` fields with `false`/`None`/empty defaults in every test fixture: `private: bool`, `group: Option<String>`, `confirm: Option<String>`, `os: Vec<String>`, `shell: Option<Vec<String>>`. Centralize all placeholder expansion in `CommandTemplate`; a missing bound parameter renders as an empty string for execution and as `${name}` for `render_unbound`.

- [ ] **Step 4: Make existing runner tests compile and pass**

  Convert V1 fixtures to `TaskCommand::Shell(...)`, add V2 task fields, and change scalar assertions to `BoundValue::Scalar`. Run `cargo test runner::` and confirm GREEN.

- [ ] **Step 5: Commit the model change**

  ```bash
  git add src/runner/task.rs src/runner/graph.rs src/runner/error.rs src/runner/mod.rs
  git commit -m "feat: extend command runner model"
  ```

### Task 2: Parse and format V2 parameters and recipe fields

**Files:**
- Modify: `src/ast.rs`
- Modify: `src/parser.rs`
- Modify: `src/formatter.rs`
- Modify: `src/resolver.rs`
- Modify: `src/typechecker.rs`

**Interfaces:**
- Produces AST-only `TaskParam { name, ty, default: Option<Expr>, variadic, span }`; ordinary function `Param` stays unchanged.
- Extends `TaskDecl` with `private`, `group`, `confirm`, `os`, and `shell` expressions.
- Enforces one last variadic parameter, no variadic default, no required parameter after a default, and correct metadata types.

- [ ] **Step 1: Write parser RED tests for parameter grammar**

  Parse `task [Deploy](environment: str = "staging", *extra: str) { run { echo ${environment} ${extra}; }; }` and assert the literal default expression and variadic flag. Add separate rejecting tests for two variadics, a non-final variadic, a variadic default, and `required: str` following `optional: str = "x"`.

- [ ] **Step 2: Observe parameter RED**

  Run `cargo test parser::tests::task_parameters_support_defaults_and_a_final_variadic` and confirm failure at `=` or `*`.

- [ ] **Step 3: Add `TaskParam` and parameter parsing**

  Parse an optional leading `Token::Star`, then `name: type`, then optional `= expr`. Validate ordering in `parse_task_decl`, leaving function parameter parsing untouched.

- [ ] **Step 4: Write metadata parser/type RED tests**

  Parse and inspect:

  ```spar
  task [Deploy] {
      private: true;
      group: "release";
      confirm: "Really deploy?";
      os: ["linux", "macos"];
      shell: ["bash", "-euo", "pipefail", "-c"];
      run { ./deploy.sh; };
  };
  ```

  Add type tests rejecting `private: "yes"`, `group: 1`, empty/wrong-element `os`, and empty/non-string `shell`.

- [ ] **Step 5: Implement fields, resolution, and type checks**

  Resolve defaults as ordinary global expressions, not task-local expressions. Require `private: bool`, `group: str`, `confirm: str`, `os: [str]`, and `shell: [str]`; require default expressions to match their declared scalar parameter type.

- [ ] **Step 6: Format every new syntax form**

  Emit `*` before variadic names, ` = <expr>` after defaults, and fields in stable order: description/default/quiet/private/group/confirm/os/dependsOn/cwd/shell/env/run. Add a formatter round-trip test using the full snippet.

- [ ] **Step 7: Verify and commit**

  Run `cargo test parser::`, `cargo test formatter::`, `cargo test typechecker::`, `cargo test resolver::`, then `cargo test`. Commit:

  ```bash
  git add src/ast.rs src/parser.rs src/formatter.rs src/resolver.rs src/typechecker.rs
  git commit -m "feat: parse command runner v2 metadata"
  ```

### Task 3: Preserve shebang run blocks as script commands

**Files:**
- Modify: `src/ast.rs`
- Modify: `src/parser.rs`
- Modify: `src/formatter.rs`
- Modify: `tests/task_lowering.rs`

**Interfaces:**
- Adds `ShellCommand::is_shebang: bool`.
- `parse_run_block` detects `#!` at the first non-whitespace raw characters before command splitting.
- A shebang block yields exactly one `ShellCommand`, preserving internal semicolons and newlines.

- [ ] **Step 1: Write the shebang RED parser test**

  Parse a block containing `#!/usr/bin/env bash`, two newline-separated statements, and `if true; then ...; fi`; assert `run.len() == 1`, `is_shebang`, and that all semicolons remain in literal parts.

- [ ] **Step 2: Observe RED**

  Run `cargo test parser::tests::shebang_run_block_is_one_verbatim_command` and confirm V1 splits the block into several commands.

- [ ] **Step 3: Implement parse-time discrimination**

  Before V1 semicolon splitting, inspect the concatenated leading literal text until the first non-whitespace bytes. If it starts with `#!`, trim only the block's outer indentation/blank space, retain one command, and set `is_shebang = true`. Ordinary blocks retain V1 splitting and set `false`.

- [ ] **Step 4: Preserve scripts through formatting**

  Format shebang bodies as one block without injecting `;` after internal script lines. Add a format-parse-format equality test.

- [ ] **Step 5: Verify and commit**

  Run `cargo test parser::tests::shebang`, `cargo test formatter::tests::task`, and `cargo test`. Commit:

  ```bash
  git add src/ast.rs src/parser.rs src/formatter.rs tests/task_lowering.rs
  git commit -m "feat: parse shebang task scripts"
  ```

### Task 4: Lower metadata, defaults, shell settings, and dotenv

**Files:**
- Create: `src/dotenv.rs`
- Modify: `src/lib.rs`
- Modify: `src/ast.rs`
- Modify: `src/parser.rs`
- Modify: `src/formatter.rs`
- Modify: `src/compiler.rs`
- Modify: `src/task_lowering.rs`
- Modify: `tests/task_lowering.rs`

**Interfaces:**
- Adds `Program::load_env: Option<String>` for first-line `@LoadEnv`; `is_schema_file` remains independent and existing schema behavior stays stable.
- Produces `dotenv::load(path: &Path) -> Result<BTreeMap<String, String>, SparError>`; missing file returns an empty map.
- Changes `lower_tasks(..., base_dir: &Path)` so `.env` is read next to the source.
- Lowers all V2 fields and turns `is_shebang` into `TaskCommand::Script`.

- [ ] **Step 1: Write pragma RED tests**

  Accept `@LoadEnv` only as the first item and reject it after a variable/task. Assert formatter output starts with `@LoadEnv\n`. Keep `@SchemaFile` tests unchanged.

- [ ] **Step 2: Observe pragma RED and implement it**

  Run `cargo test parser::tests::load_env_pragma_defaults_to_dotenv_and_must_be_first`; confirm the parser reports an unknown pragma. Add the program path and formatter support.

- [ ] **Step 3: Write dotenv parser RED tests**

  Use literal expectations for blank lines, full-line comments, `KEY=value`, `KEY="quoted value"`, `KEY='literal # value'`, and `KEY=value # comment`. Reject malformed non-empty lines without `=` and empty keys. Confirm missing `.env` returns an empty map.

- [ ] **Step 4: Implement the small dotenv module**

  Strip matching single/double outer quotes, treat `#` as a comment only outside quotes and after whitespace, and do not implement multiline or escape expansion. Keep parsing logic under 50 lines excluding tests.

- [ ] **Step 5: Write lowering RED tests**

  Compile from a temporary source directory and assert: metadata values lower exactly; `os` rejects values outside `linux|macos|windows`; shell lists are non-empty; defaults become canonical strings; a shebang becomes `TaskCommand::Script`; dotenv keys fill absent environment entries; a real process variable wins over dotenv; task `env` wins over both.

- [ ] **Step 6: Implement lowering and precedence**

  Pass `CompileOptions.base_dir` into `lower_tasks`. Start each task environment from dotenv entries whose keys are absent from `std::env`, then overlay task `env`. Evaluate defaults through `Evaluator::eval_standalone`, type-check them before lowering, and lower lists to owned strings.

- [ ] **Step 7: Verify and commit**

  Run `cargo test dotenv::`, `cargo test --test task_lowering`, and full `cargo test`. Commit:

  ```bash
  git add src/dotenv.rs src/lib.rs src/ast.rs src/parser.rs src/formatter.rs src/compiler.rs src/task_lowering.rs tests/task_lowering.rs
  git commit -m "feat: lower command runner v2 configuration"
  ```

### Task 5: Bind default and variadic arguments and validate operating systems

**Files:**
- Modify: `src/runner/graph.rs`
- Modify: `src/runner/error.rs`
- Modify: `src/runner/task.rs`

**Interfaces:**
- Produces `TaskSet::bind(&TaskInvocation) -> Result<BoundTask, RunnerError>` for `show` without dependencies.
- `TaskSet::plan` reuses `bind` and validates `Task.os` for every reachable task before returning a plan.
- Replaces exact argument count errors with a range-aware error containing minimum, optional maximum, and actual count.

- [ ] **Step 1: Write binding RED tests**

  Cover no supplied args using a default, an explicit arg replacing a default, zero/many variadic values, required-before-default minimum count, too many non-variadic args, and scalar conversion before storage in `BoundValue`.

- [ ] **Step 2: Observe RED**

  Run `cargo test runner::graph::tests::binding_uses_defaults_and_collects_variadic_arguments`; confirm V1's exact-count check fails.

- [ ] **Step 3: Rewrite argument binding**

  Compute `minimum` from parameters with neither default nor variadic; set `maximum = None` for variadic tasks and `Some(parameters.len())` otherwise. Bind supplied positionals left-to-right, then defaults, then all remaining values to `BoundValue::Variadic`.

- [ ] **Step 4: Write OS RED tests**

  Add one direct-task and one dependency mismatch test using an allowed list guaranteed not to contain `std::env::consts::OS`; assert the returned `RunnerError::UnsupportedOperatingSystem` names the task, actual OS, and allowed values.

- [ ] **Step 5: Implement preflight OS validation**

  Validate every task after graph traversal but before returning `ExecutionPlan`, so execution cannot partially start. Do not silently skip mismatches.

- [ ] **Step 6: Verify and commit**

  Run `cargo test runner::graph` and `cargo test runner::`. Commit:

  ```bash
  git add src/runner/graph.rs src/runner/error.rs src/runner/task.rs
  git commit -m "feat: bind flexible task arguments"
  ```

### Task 6: Execute scripts, custom shells, and confirmations

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/runner/shell.rs`
- Modify: `src/runner/executor.rs`
- Modify: `src/runner/error.rs`

**Interfaces:**
- Promotes `tempfile = "3"` from dev-dependency to dependency.
- Produces shell construction that accepts `Option<&[String]>`; custom shell uses item zero as program, middle items as fixed args, and resolved script as final arg.
- Executes `TaskCommand::Script` via a temporary executable on Unix and via the parsed shebang interpreter on Windows.
- Confirmation prompts are completed for the whole plan before any process starts; dry-run bypasses prompting.

- [ ] **Step 1: Write custom-shell RED test**

  On Unix, configure `shell: ["sh", "-c"]` and a marker command; assert marker creation. Add a construction-level test proving the script is the final argument after every configured fixed argument.

- [ ] **Step 2: Observe RED and implement custom shell**

  Run `cargo test runner::executor::tests::custom_shell_appends_script_as_final_argument`; confirm the override is ignored. Route only `TaskCommand::Shell` through the override.

- [ ] **Step 3: Write script RED tests**

  On Unix, execute `#!/bin/sh\nprintf script > '<marker>'`; assert the marker exists and the report contains full script text. Add dry-run proof that no temp-script process runs and output remains full text. Add a platform-neutral shebang-token parser test for `#!/usr/bin/env bash`.

- [ ] **Step 4: Implement script execution**

  Create a named temp file, write resolved bytes, flush, set Unix mode `0o700`, and execute it directly. On Windows, split the first shebang line into interpreter tokens and run those tokens followed by the temp path. Map malformed shebangs and process failures to contextual `RunnerError` variants.

- [ ] **Step 5: Write confirmation RED tests**

  Feed `n\n` through the executor's internal I/O seam and assert `RunnerError::Aborted`, prompt text ending in `[y/N]`, and no dependency marker. Feed `y\n` and assert execution. Assert dry-run neither reads input nor prints a prompt.

- [ ] **Step 6: Implement pre-execution confirmation**

  Before the first child is spawned, walk planned tasks and prompt for every present `confirm`. Accept case-insensitive `y` or `yes`; EOF and every other answer decline. Keep the public `execute(plan, options)` interface and use a private `execute_with_io` for deterministic unit tests.

- [ ] **Step 7: Verify and commit**

  Run `cargo test runner::executor`, `cargo test runner::shell`, then `cargo clippy --all-targets --all-features -- -D warnings`. Commit:

  ```bash
  git add Cargo.toml Cargo.lock src/runner/shell.rs src/runner/executor.rs src/runner/error.rs
  git commit -m "feat: execute configurable task scripts"
  ```

### Task 7: Add task-file discovery and V2 CLI grammar

**Files:**
- Modify: `src/main.rs`
- Modify: `tests/task_cli.rs`

**Interfaces:**
- Produces `discover_task_file(start: &Path) -> Result<PathBuf, String>`.
- Changes task commands to `tasks [-f FILE] [--all]`, `run [task] [args...] [-f FILE] [--dry-run] [--choose]`, `show <task> [args...] [-f FILE]`, and `dump [-f FILE]`.
- Explicit `-f/--file` bypasses discovery; all task command handlers receive a resolved `PathBuf`.

- [ ] **Step 1: Write discovery RED tests**

  In nested temporary directories, assert current-directory preference, parent fallback, explicit-file bypass, and a not-found message containing `SparMake.spar`.

- [ ] **Step 2: Observe RED and implement discovery**

  Run `cargo test --bin spar discover_task_file`; confirm the helper is absent. Iterate `start.ancestors()` and return the first regular `SparMake.spar`.

- [ ] **Step 3: Write argument-parser RED tests**

  Assert flags before/after task arguments, both file flag spellings, new `show`/`dump` variants, and rejection of a bare file argument for `tasks`. Add an integration test proving old `spar run file.spar task` no longer treats the first word as a file when `-f` is absent.

- [ ] **Step 4: Implement the V2 command enum and parser**

  Give each task command an `Option<PathBuf>` file field; remove `-f` and its value plus recognized boolean flags before assigning remaining words to task/args. Reject duplicate file flags, missing file values, and `--choose` with an explicit task. Before a task name, reject unknown options beginning with `-`; after the task name, preserve unknown dash-prefixed words such as `--force` as positional task arguments for variadic parameters.

- [ ] **Step 5: Update help text and compile path handling**

  Show the exact four V2 usage lines from the spec. Resolve/discover the path before reading source so diagnostics and `CompileOptions::for_path` use the actual file.

- [ ] **Step 6: Verify and commit**

  Run `cargo test --bin spar` and focused discovery CLI tests. Commit:

  ```bash
  git add src/main.rs tests/task_cli.rs
  git commit -m "feat: discover Spar task files"
  ```

### Task 8: Group task listing and add the built-in chooser

**Files:**
- Modify: `src/main.rs`
- Modify: `tests/task_cli.rs`

**Interfaces:**
- `print_task_list(tasks, include_private, writer)` is shared by `tasks`, chooser rendering, and missing-default diagnostics.
- Ungrouped public tasks appear first; named groups and task names use deterministic lexical order.
- Chooser reads plain stdin and accepts either displayed number or case-insensitive task name.

- [ ] **Step 1: Write grouped-list RED CLI tests**

  Create a catalog with ungrouped, grouped, and private tasks. Assert default listing excludes private tasks, `--all` includes them, ungrouped comes first, and group headings cluster members.

- [ ] **Step 2: Observe RED and implement shared listing**

  Run `cargo test --test task_cli tasks_groups_public_tasks_and_hides_private_tasks`; confirm V1 emits a flat list. Build ordered rows from `TaskSet::iter()` without exposing runner internals.

- [ ] **Step 3: Write chooser RED tests**

  Extend the test helper to use `Command::stdin(Stdio::piped())`, write `1\n` or `build\n`, and assert the selected command runs. Add invalid number/name and EOF cases; assert private tasks are absent.

- [ ] **Step 4: Implement chooser selection**

  Require no explicit task when `--choose` is set. Print numbered public tasks to stderr using the same grouping/order rules, read one line, map number or normalized name, and feed the result through the ordinary `TaskInvocation` path.

- [ ] **Step 5: Verify and commit**

  Run `cargo test --test task_cli tasks_`, `cargo test --test task_cli run_choose_`, and full `cargo test --test task_cli`. Commit:

  ```bash
  git add src/main.rs tests/task_cli.rs
  git commit -m "feat: add grouped task chooser"
  ```

### Task 9: Add `show` and JSON `dump`

**Files:**
- Modify: `src/main.rs`
- Modify: `src/runner/task.rs`
- Modify: `tests/task_cli.rs`

**Interfaces:**
- `show` calls `TaskSet::bind`, not `plan`, so dependencies are neither rendered nor executed.
- Both shell and script output use `TaskCommand::render`; no command rendering is duplicated in CLI code.
- `dump` emits valid pretty JSON with stable task-name ordering and unbound `${name}` placeholders.

- [ ] **Step 1: Write `show` RED tests**

  Use a requested task with a dependency and argument. Assert stdout contains only the requested resolved command, excludes the dependency command, no marker is created, no confirmation prompt appears, and a shebang script is printed in full.

- [ ] **Step 2: Observe RED and implement `show`**

  Run `cargo test --test task_cli show_binds_only_the_requested_task`; confirm `show` is unknown. Bind through the runner interface and print one rendered command/script per catalog command.

- [ ] **Step 3: Write `dump` RED test with literal JSON assertions**

  Parse stdout with `serde_json` and assert every required field: name, description, group, private, confirm, os, dependencies, parameter default/variadic, environment, cwd, shell, command kind, and unbound template text `deploy ${environment} ${extra}`.

- [ ] **Step 4: Implement stable catalog serialization**

  Build `serde_json::Value` objects from `TaskSet::iter()`. Serialize absent optional values as `null`, collections as arrays/objects, and commands as `{ "kind": "shell"|"script", "template": "..." }`. Pretty-print with `serde_json::to_string_pretty`.

- [ ] **Step 5: Verify and commit**

  Run `cargo test --test task_cli show_`, `cargo test --test task_cli dump_`, and full `cargo test --test task_cli`. Commit:

  ```bash
  git add src/main.rs src/runner/task.rs tests/task_cli.rs
  git commit -m "feat: show and dump task catalogs"
  ```

### Task 10: Update provenance, examples, and command-runner documentation

**Files:**
- Modify: `docs/command-runner/just-extraction-map.md`
- Modify: `docs/command-runner/UPSTREAM-JUST.md`
- Modify: `docs/command-runner/tasks.md`
- Modify: `README.md`
- Modify: `examples/tasks.spar`

**Interfaces:**
- Documents the checked V2 syntax, breaking CLI grammar, discovery rules, execution semantics, and rejected Just scope.

- [ ] **Step 1: Update provenance decisions explicitly**

  Change the `recipe.rs::run_script`, `executor.rs`, search, dotenv, parameter, attribute, and CLI-related rows from their V1 decisions to V2 `ADAPT` or `REIMPLEMENT`; retain each old decision in the reasoning text as “V1 discarded; V2 adapts/reimplements only ...”. Record the same donor commit.

- [ ] **Step 2: Extend the example without replacing user work**

  Preserve existing tasks and add focused examples of defaults/variadic values, one group/private task, shell override, and comments showing optional `@LoadEnv`. Keep dangerous confirmation/script examples dry-run-safe.

- [ ] **Step 3: Rewrite user command examples**

  Replace positional task files with `-f`; document discovery from `SparMake.spar`, `tasks --all`, `run --choose`, `show`, and `dump`. State exact dotenv precedence and minimal parser rules, OS mismatch behavior, and unquoted variadic joining.

- [ ] **Step 4: Smoke-test every documented command**

  Run `cargo run -- tasks -f examples/tasks.spar`, `cargo run -- run -f examples/tasks.spar --dry-run`, `cargo run -- show build -f examples/tasks.spar`, and `cargo run -- dump -f examples/tasks.spar`.

- [ ] **Step 5: Verify and commit**

  Run `git diff --check`. Commit:

  ```bash
  git add docs/command-runner/just-extraction-map.md docs/command-runner/UPSTREAM-JUST.md docs/command-runner/tasks.md README.md examples/tasks.spar
  git commit -m "docs: document command runner v2"
  ```

### Task 11: Full verification and review

**Files:**
- Create: `docs/command-runner/v2-verification.md`
- Modify only files required by defects reproduced with failing tests.

**Interfaces:**
- Produces exact final verification evidence and no known V2 spec gaps.

- [ ] **Step 1: Use the verification skill and run static checks**

  Run `cargo fmt --check`, `cargo check`, and `cargo clippy --all-targets --all-features -- -D warnings`; fix any failure with a focused RED/GREEN cycle.

- [ ] **Step 2: Run the complete suite**

  Run `cargo test` and record each suite's exact passed/failed/ignored counts.

- [ ] **Step 3: Run manual cross-feature smoke tests**

  In a temporary nested directory containing `SparMake.spar` and `.env`, exercise discovery, grouped listing, chooser with piped input, default/variadic arguments, dry-run, show, dump, declined confirmation, custom shell, and a shebang script.

- [ ] **Step 4: Review against the V2 spec**

  Use `superpowers:requesting-code-review` against pre-V2 commit `2c0a09e`. Check every scope bullet and the breaking CLI section; reproduce each valid finding with a failing test before fixing it.

- [ ] **Step 5: Re-run verification after review fixes**

  Repeat formatting, check, Clippy, full tests, and smoke tests. Write commands and exact results to `docs/command-runner/v2-verification.md`.

- [ ] **Step 6: Commit verification evidence**

  ```bash
  git add docs/command-runner/v2-verification.md
  git commit -m "docs: record command runner v2 verification"
  ```
