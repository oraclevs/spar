# OS-Conditional `run` Blocks Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the task-level `os: [...]` metadata field with multiple `run <os-label>? { ... }` blocks per task, so a single task can define different shell commands per operating system, auto-selected by matching `std::env::consts::OS` at task-lowering time.

**Architecture:** A task's single `run { ... }` block becomes N blocks: at most one bare `run { }` (the default/fallback, same syntax every task already uses today) plus any number of labeled `run windows { }` / `run linux { }` / `run macos { }` overrides. The lexer learns to recognize an optional bare identifier between `run` and `{`. The parser collects `Vec<RunBlock>` instead of one `Vec<ShellCommand>`, rejecting duplicate labels (including two bare defaults) at parse time. OS selection happens once, at compile time, inside `task_lowering.rs` — it picks the block matching `std::env::consts::OS`, falling back to the bare default, erroring clearly if neither exists. The runner layer needs zero changes: it already only ever sees one flat, pre-resolved command list per task. The existing `os: [...]` field and its runner-layer gating (`runner::Task.os`, `graph.rs`'s `plan()` check, `RunnerError::UnsupportedOperatingSystem`) are deleted outright — no back-compat shim.

**Tech Stack:** Rust (spar compiler crate: lexer, parser, ast, resolver, typechecker, task_lowering, formatter, runner), spar-ls (tower-lsp), TextMate grammar (vscode-spar).

**Spec:** This document — design was settled via a clarifying-questions round with the project owner (2026-08-30), decisions below are final, not open for re-litigation during execution:
- Bare `run { }` is the default/fallback block (not `run default { }`).
- OS labels are exactly `windows`, `linux`, `macos` — matching `std::env::consts::OS` verbatim, no translation layer (NOT `darwin`).
- If the current OS has no matching block and there's no bare default: a clear runtime (task-lowering-time) error, not a compile-time (`spar check`) error — confirmed this falls out naturally, since `spar check` doesn't invoke task lowering at all (see Task 1, Step 9).
- The old `os: [...]` field is removed entirely, no deprecated alias.

## Global Constraints

- No backwards-compatibility shim for `@DotenvLoad`-style aliasing of the old `os:` field — clean removal only.
- Only 2 real-world `.spar` files anywhere use `os: [...]` today (both outside this repo, at `/home/occ/Projects/Spar/multiConfigTest/main.spar` and a scratch `sparmake.txt` in the same directory) — migrating those is explicitly OUT of scope for this plan; done by hand afterward, not as a plan task.
- Keep diffs idiomatic to each file's existing style: no comments explaining WHAT code does, only WHY when genuinely non-obvious (matching this codebase's established comment density — see the file-by-file code shown in this plan for the tone to match).
- Every task must leave `cargo test` (in `spar/`, then `spar-ls/` for its own task) fully green before being considered done — no "fix tests later."
- Reinstall both binaries (`cd spar && cargo install --path . --force`, then `cd spar-ls && cargo install --path . --force`) after Task 1, since `spar-ls` links `spar` as a library and goes stale silently otherwise.

---

## File Structure

- Modify `spar/src/ast.rs` — remove `TaskDecl.os`, replace `TaskDecl.run: Vec<ShellCommand>` with `TaskDecl.run_blocks: Vec<RunBlock>`; add `pub struct RunBlock { pub os: Option<String>, pub os_span: Option<Span>, pub commands: Vec<ShellCommand>, pub span: Span }`.
- Modify `spar/src/lexer.rs` — `maybe_enter_run_body` learns to optionally consume a label identifier before the `{`.
- Modify `spar/src/parser.rs` — `parse_task_decl`'s `"run"`/`"os"` arms, duplicate-label detection, final "at least one run block" check.
- Modify `spar/src/resolver.rs` — `resolve_task`: delete `.os` handling, walk `.run_blocks` instead of `.run`.
- Modify `spar/src/typechecker.rs` — `check_task`: delete `.os` handling, walk `.run_blocks` instead of `.run`.
- Modify `spar/src/task_lowering.rs` — `lower_one_task`: delete `.os` handling, add OS-block-selection before the existing command-flattening loop, add new lowering error for "no matching block and no default".
- Modify `spar/src/formatter.rs` — `format_task_decl`: delete `os:` printing, loop over `run_blocks` printing each with its optional label, in source order.
- Modify `spar/src/runner/task.rs` — delete `Task.os` field.
- Modify `spar/src/runner/graph.rs` — delete the OS-gating check in `plan()`, delete/replace its two tests.
- Modify `spar/src/runner/error.rs` — delete `RunnerError::UnsupportedOperatingSystem` and its `Display` arm.
- Modify `spar-ls/src/completion.rs` — remove `"os"` from task-metadata completion items; reconsider the `run`-already-present filter now that multiple `run` blocks are legal.
- Modify `spar-ls/src/hover.rs` — remove `os: None,` from the test-fixture `Task` literal.
- Modify `spar-ls/src/semantic_tokens.rs` — remove the `("os", task.os.as_ref())` entry; make the run-body interpolation token walk iterate `task.run_blocks` (nested loop) instead of `task.run`.
- Modify `spar-ls/src/references.rs` — remove `&td.os` from the references-walk array.
- Modify `editors/vscode-spar/syntaxes/spar.tmLanguage.json` — drop `os` from `task_metadata_fields`'s alternation; extend `task_run_block`'s `begin` regex to capture an optional label.

---

### Task 1: Core language change (`spar/` crate)

**Files:**
- Modify: `spar/src/ast.rs:1-8` (Program — no change needed here, only TaskDecl), `spar/src/ast.rs:31-49` (TaskDecl)
- Modify: `spar/src/lexer.rs:541-568` (`maybe_enter_run_body`)
- Modify: `spar/src/parser.rs:1525-1739` (`parse_task_decl`), `spar/src/parser.rs:1749-1863` (`parse_run_block` — unchanged in body, only its call sites change)
- Modify: `spar/src/resolver.rs` (`resolve_task`, ~line 1062-1130)
- Modify: `spar/src/typechecker.rs` (`check_task`, ~line 2074-2160)
- Modify: `spar/src/task_lowering.rs:93-239` (`lower_one_task`)
- Modify: `spar/src/formatter.rs:337-469` (`format_task_decl`)
- Modify: `spar/src/runner/task.rs:7-23` (`Task` struct)
- Modify: `spar/src/runner/graph.rs:16-59` (`plan`), `spar/src/runner/graph.rs:586-637` (tests)
- Modify: `spar/src/runner/error.rs:30-34` (enum variant), `~101-109` (Display arm)
- Test: `spar/src/parser.rs` (existing task-decl test module), `spar/src/task_lowering.rs` unit tests + `spar/tests/task_lowering.rs`, `spar/src/formatter.rs` test module, `spar/src/runner/graph.rs` test module

**Interfaces:**
- Produces: `ast::RunBlock { os: Option<String>, os_span: Option<Span>, commands: Vec<ShellCommand>, span: Span }`, `ast::TaskDecl.run_blocks: Vec<RunBlock>` (replaces `run`), `ast::TaskDecl` no longer has `os`.
- Produces: a new `SparError::EvalError` raised from `task_lowering::lower_one_task` when no run block matches `std::env::consts::OS` and there's no bare default — message format: `"task '{name}' has no run block for `{os}` (defined: {labels}) and no default 'run {{}}' block"` where `{labels}` is the comma-joined list of the task's labeled (non-default) block labels.
- Consumes (Task 2, 3): `spar-ls` and the TextMate grammar consume `ast::TaskDecl.run_blocks` and the absence of `ast::TaskDecl.os` — Task 2/3 cannot start until this task's types exist.

- [ ] **Step 1: Add `RunBlock` to the AST, remove `os`**

Edit `spar/src/ast.rs`. Add this struct right after `ShellCommand`'s definition (currently ends around line 77, before the `ShellTemplatePart` enum):

```rust
/// One `run <label>? { ... }` clause inside a task. `os: None` is the bare
/// default/fallback form (`run { ... }`) every task could already write;
/// `os: Some("windows"|"linux"|"macos")` is an override selected by
/// matching `std::env::consts::OS` at task-lowering time. A task may have
/// at most one default block and at most one block per label — enforced
/// by the parser, not here.
#[derive(Debug, Clone)]
pub struct RunBlock {
    pub os: Option<String>,
    pub os_span: Option<Span>,
    pub commands: Vec<ShellCommand>,
    pub span: Span,
}
```

In `TaskDecl` (line ~31-49), delete `pub os: Option<Expr>,` and change `pub run: Vec<ShellCommand>,` to `pub run_blocks: Vec<RunBlock>,`.

- [ ] **Step 2: Teach the lexer to recognize an optional label before `run`'s `{`**

Replace `maybe_enter_run_body` in `spar/src/lexer.rs` (lines 541-568) with:

```rust
    /// Called right after an `Ident("run")` token has been pushed. Scans
    /// ahead (without committing) for an optional bare-identifier OS label
    /// followed by `{` — `run { ... }` (bare/default) or
    /// `run windows { ... }` (labeled). If neither shape is found at this
    /// position, the position is left untouched and `run`/the tentative
    /// label lex as ordinary tokens on the next loop iterations — this
    /// keeps the check honest rather than assuming `run` always opens a
    /// block.
    fn maybe_enter_run_body(&mut self, tokens: &mut Vec<SpannedToken>) -> Result<(), SparError> {
        let mut offset = 0usize;
        while matches!(
            self.peek_at(offset),
            Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            offset += 1;
        }
        let label_start = offset;
        while matches!(self.peek_at(offset), Some(b) if b.is_ascii_alphanumeric() || b == b'_') {
            offset += 1;
        }
        let label_end = offset;
        while matches!(
            self.peek_at(offset),
            Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            offset += 1;
        }
        if self.peek_at(offset) != Some(b'{') {
            return Ok(());
        }

        for _ in 0..label_start {
            self.advance();
        }
        if label_end > label_start {
            let label_pos = self.pos;
            let (label_line, label_col) = (self.line, self.col);
            let label_text = self.source[self.pos..self.pos + (label_end - label_start)].to_string();
            for _ in label_start..label_end {
                self.advance();
            }
            tokens.push(SpannedToken::new(
                Token::Ident(label_text),
                self.span_at(label_pos, label_line, label_col),
            ));
        }
        for _ in label_end..offset {
            self.advance();
        }

        let brace_pos = self.pos;
        let (brace_line, brace_col) = (self.line, self.col);
        self.advance(); // consume '{'
        tokens.push(SpannedToken::new(
            Token::RunStart,
            self.span_at(brace_pos, brace_line, brace_col),
        ));
        self.lex_run_body(tokens)
    }
```

- [ ] **Step 3: Write failing parser tests for multi-block `run`**

Add to `spar/src/parser.rs`'s test module (near the existing task-decl tests — search for a test asserting on `dotenv_load_pragma_must_be_first` at line ~2562 for the module's location/style):

```rust
    #[test]
    fn task_accepts_bare_and_labeled_run_blocks() {
        let src = "task [T] {\n\
            run {\n\
                echo default;\n\
            };\n\
            run windows {\n\
                echo win;\n\
            };\n\
            run linux {\n\
                echo linux;\n\
            };\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let TopLevelItem::Task(task) = &program.items[0] else {
            panic!("expected a task");
        };
        assert_eq!(task.run_blocks.len(), 3);
        assert_eq!(task.run_blocks[0].os, None);
        assert_eq!(task.run_blocks[1].os.as_deref(), Some("windows"));
        assert_eq!(task.run_blocks[2].os.as_deref(), Some("linux"));
    }

    #[test]
    fn task_rejects_duplicate_run_block_for_same_os() {
        let src = "task [T] {\n\
            run windows {\n\
                echo a;\n\
            };\n\
            run windows {\n\
                echo b;\n\
            };\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let err = crate::parser::Parser::new(tokens).parse().expect_err("must reject");
        let message = format!("{err}");
        assert!(message.contains("windows"), "{message}");
    }

    #[test]
    fn task_rejects_two_default_run_blocks() {
        let src = "task [T] {\n\
            run {\n\
                echo a;\n\
            };\n\
            run {\n\
                echo b;\n\
            };\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let err = crate::parser::Parser::new(tokens).parse().expect_err("must reject");
        let message = format!("{err}");
        assert!(message.contains("default") || message.contains("once"), "{message}");
    }

    #[test]
    fn task_requires_at_least_one_run_block() {
        let src = "task [T] {\n\
            description: \"no run at all\";\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        assert!(crate::parser::Parser::new(tokens).parse().is_err());
    }

    #[test]
    fn task_with_only_labeled_run_blocks_and_no_default_parses() {
        let src = "task [T] {\n\
            run windows {\n\
                echo win;\n\
            };\n\
            run macos {\n\
                echo mac;\n\
            };\n\
        };";
        let tokens = crate::lexer::Lexer::new(src).tokenize().expect("lex");
        let program = crate::parser::Parser::new(tokens).parse().expect("parse");
        let TopLevelItem::Task(task) = &program.items[0] else {
            panic!("expected a task");
        };
        assert_eq!(task.run_blocks.len(), 2);
    }
```

- [ ] **Step 4: Run the new tests, confirm they fail to compile (expected — `run_blocks` doesn't exist on the parser's output yet)**

Run: `cd spar && cargo test task_accepts_bare_and_labeled_run_blocks 2>&1 | head -30`
Expected: compile error, `no field \`run_blocks\` on type \`TaskDecl\`` (or similar) — confirms the test is exercising code that doesn't exist yet.

- [ ] **Step 5: Update `parse_task_decl` to build `Vec<RunBlock>` with duplicate detection**

In `spar/src/parser.rs`, inside `parse_task_decl` (starts line 1525):

Replace the local-variable block (lines ~1587-1594, currently `let mut os = None;` ... `let mut run = Vec::new(); let mut saw_run = false;`) — delete the `os`/`run`/`saw_run` locals, add:

```rust
        let mut run_blocks: Vec<RunBlock> = Vec::new();
        let mut seen_run_labels: std::collections::HashSet<Option<String>> = std::collections::HashSet::new();
```

Replace the `"run"` match arm (lines 1609-1618) with:

```rust
                "run" => {
                    let os_label = if let Token::Ident(label) = self.peek().clone() {
                        self.advance();
                        Some(label)
                    } else {
                        None
                    };
                    let os_span = os_label.as_ref().map(|_| field_span.clone());
                    let run_start = field_span.clone();
                    let commands = self.parse_run_block()?;
                    if !seen_run_labels.insert(os_label.clone()) {
                        let message = match &os_label {
                            Some(label) => format!("task 'run {label}' block may only appear once"),
                            None => "task can only have one default 'run {}' block".to_string(),
                        };
                        return Err(SparError::ParseError { message, span: run_start });
                    }
                    run_blocks.push(RunBlock {
                        os: os_label,
                        os_span,
                        commands,
                        span: run_start,
                    });
                }
```

Delete the `"os"` match arm entirely (lines ~1649-1653).

Update the unknown-field error (lines ~1701-1708) — remove `'os', ` from the message string.

Replace the post-loop check (lines ~1714-1719, `if !saw_run { ... }`) with:

```rust
        if run_blocks.is_empty() {
            return Err(SparError::ParseError {
                message: format!("task '{name}' must declare at least one 'run' block"),
                span,
            });
        }
```

In the final `Ok(TaskDecl { ... })` struct literal (lines ~1723-1739), delete the `os,` field and change `run,` to `run_blocks,`.

- [ ] **Step 6: Run the parser tests, confirm they pass**

Run: `cd spar && cargo test task_accepts_bare_and_labeled_run_blocks task_rejects_duplicate_run_block_for_same_os task_rejects_two_default_run_blocks task_requires_at_least_one_run_block task_with_only_labeled_run_blocks_and_no_default_parses 2>&1 | tail -20`
Expected: still fails to compile — `resolver.rs`, `typechecker.rs`, `task_lowering.rs`, `formatter.rs`, `runner/*.rs` all still reference the old `.os`/`.run` fields. Proceed to the next steps before re-running.

- [ ] **Step 7: Update the resolver**

In `spar/src/resolver.rs`, inside `resolve_task` (~line 1062): delete the `.os` block (~lines 1096-1098, `if let Some(expr) = &decl.os { self.resolve_expr(expr); }`).

Replace the `.run` walk (~lines 1117-1125):

```rust
        let locals: HashSet<String> = decl.params.iter().map(|p| p.name.clone()).collect();
        for command in &decl.run {
            for part in &command.parts {
                if let ShellTemplatePart::Expr(expr) = part {
                    if let Err(e) = self.resolve_expr_with_locals(expr, &locals) {
                        self.errors.push(e);
                    }
                }
            }
        }
```

with:

```rust
        let locals: HashSet<String> = decl.params.iter().map(|p| p.name.clone()).collect();
        for block in &decl.run_blocks {
            for command in &block.commands {
                for part in &command.parts {
                    if let ShellTemplatePart::Expr(expr) = part {
                        if let Err(e) = self.resolve_expr_with_locals(expr, &locals) {
                            self.errors.push(e);
                        }
                    }
                }
            }
        }
```

- [ ] **Step 8: Update the typechecker**

In `spar/src/typechecker.rs`, inside `check_task` (~line 2074): delete the `.os` block (~lines 2115-2117, the `self.check_task_string_list_field(expr, "os", &decl.span)` call — leave the `shell:` one right below it, `check_task_string_list_field(expr, "shell", ...)`, untouched).

Replace the `.run` walk (~lines 2133-2154) the same way as the resolver — wrap the existing per-command body in an outer `for block in &decl.run_blocks { for command in &block.commands { ... /* unchanged inner body */ ... } }`.

- [ ] **Step 9: Update `task_lowering.rs` — delete `os`, add OS-block selection**

In `spar/src/task_lowering.rs`, inside `lower_one_task` (line 93):

Delete the `os` binding (lines ~129-133):
```rust
    let os = decl
        .os
        .as_ref()
        .and_then(|e| eval_string_list(program, symbols, eval_result, e, &mut errors))
        .unwrap_or_default();
```

Immediately before the existing command-flattening loop (`let mut commands: Vec<TaskCommand> = Vec::new(); for command in &decl.run { ... }`, currently starting ~line 173), insert OS-block selection and change the loop to iterate the selected block's commands:

```rust
    let current_os = std::env::consts::OS;
    let selected_block = decl
        .run_blocks
        .iter()
        .find(|block| block.os.as_deref() == Some(current_os))
        .or_else(|| decl.run_blocks.iter().find(|block| block.os.is_none()));

    let mut commands: Vec<TaskCommand> = Vec::new();
    match selected_block {
        Some(block) => {
            for command in &block.commands {
                let mut parts: Vec<TemplatePart> = Vec::new();
                for part in &command.parts {
                    match part {
                        ShellTemplatePart::Literal(s) => {
                            parts.push(TemplatePart::Literal(s.replace("#{", "${")))
                        }
                        ShellTemplatePart::Expr(expr) => match bare_param_ref(expr, &param_names) {
                            Some(name) => parts.push(TemplatePart::Parameter(name)),
                            None => {
                                if expr_mentions_any(expr, &param_names) {
                                    errors.push(SparError::EvalError {
                                        message: format!(
                                            "task '{}': a '${{...}}' interpolation cannot combine a task \
                                             parameter with other values — reference the parameter alone \
                                             (e.g. '${{{}}}'), or use a literal / global value instead",
                                            decl.name,
                                            param_names.iter().next().cloned().unwrap_or_default()
                                        ),
                                        span: command.span.clone(),
                                    });
                                    continue;
                                }
                                match eval_any(program, symbols, eval_result, expr) {
                                    Ok(v) => parts.push(TemplatePart::Literal(v.coerce_to_str())),
                                    Err(e) => errors.push(e),
                                }
                            }
                        },
                    }
                }
                commands.push(if command.is_shebang {
                    TaskCommand::Script(CommandTemplate { parts })
                } else {
                    TaskCommand::Shell(CommandTemplate { parts })
                });
            }
        }
        None => {
            let labels: Vec<&str> = decl
                .run_blocks
                .iter()
                .filter_map(|block| block.os.as_deref())
                .collect();
            errors.push(SparError::EvalError {
                message: format!(
                    "task '{}' has no run block for `{current_os}` (defined: {}) and no default 'run {{}}' block",
                    decl.name,
                    labels.join(", ")
                ),
                span: decl.span.clone(),
            });
        }
    }
```

(Note: `param_names` is already computed above this point at line ~164 — unchanged, still needed here.)

In the final `Ok(Task { ... })` struct literal (lines ~222-238), delete the `os,` field.

- [ ] **Step 10: Add task_lowering tests for OS selection**

Add to `spar/tests/task_lowering.rs` (check the file's existing test style first — it likely already has helpers for building a `Program`/`Compiler` and asserting on the lowered `TaskSet`; mirror that pattern). Since `std::env::consts::OS` is fixed to whatever OS the test suite actually runs on, write the tests in terms of the CURRENT OS via `std::env::consts::OS` rather than hardcoding `"linux"`, so they pass in CI regardless of platform:

```rust
#[test]
fn run_block_matching_current_os_is_selected_over_default() {
    let os = std::env::consts::OS;
    let src = format!(
        "task [T] {{\n\
            run {{\n\
                echo default;\n\
            }};\n\
            run {os} {{\n\
                echo current-os;\n\
            }};\n\
        }};"
    );
    // use this file's existing lower-and-inspect helper here, asserting
    // the lowered task's single resolved command is "echo current-os",
    // not "echo default" — match whatever helper/assertion style the
    // rest of this file already uses (e.g. lowering via `Compiler`,
    // then inspecting `compilation.tasks.unwrap().get("T")`).
}

#[test]
fn default_run_block_is_selected_when_no_os_specific_block_matches() {
    // task has only `run windows {}` and `run { ... default ... }`
    // (assuming the test machine isn't windows — use an OS that is
    // provably NOT std::env::consts::OS, e.g. pick from
    // ["windows", "linux", "macos"] minus the current one) — assert the
    // default block's command is what gets selected.
}

#[test]
fn missing_run_block_for_current_os_with_no_default_is_a_clear_lowering_error() {
    // task has only run blocks for OSes that are provably NOT the
    // current std::env::consts::OS, and no bare default — assert
    // lowering fails with an error message containing the current OS
    // name and the word "default".
}
```

Fill in each test body against this file's actual existing helper functions (read the top of `spar/tests/task_lowering.rs` for the exact compile-and-lower helper name/signature before writing these — do not invent a helper name).

- [ ] **Step 11: Update the formatter**

In `spar/src/formatter.rs`, inside `format_task_decl` (line 337): delete the `os:` printing block (lines ~400-405).

Replace the single `run { ... }` printing block (lines ~444-466) with a loop over `td.run_blocks` in source order:

```rust
    for (i, block) in td.run_blocks.iter().enumerate() {
        if i > 0 {
            // no extra blank line between consecutive run blocks — keep them visually grouped
        }
        out.push_str(&body_indent);
        out.push_str("run");
        if let Some(os) = &block.os {
            out.push(' ');
            out.push_str(os);
        }
        out.push_str(" {\n");
        let run_indent = indent(2, config);
        for cmd in &block.commands {
            out.push_str(&run_indent);
            for part in &cmd.parts {
                match part {
                    ShellTemplatePart::Literal(s) => out.push_str(s),
                    ShellTemplatePart::Expr(e) => {
                        out.push_str("${");
                        format_expr(e, 0, 2, config, out);
                        out.push('}');
                    }
                }
            }
            if cmd.is_shebang {
                out.push('\n');
            } else {
                out.push_str(";\n");
            }
        }
        out.push_str(&body_indent);
        out.push_str("};\n");
    }
```

- [ ] **Step 12: Add a formatter round-trip test for multiple run blocks**

Add to `spar/src/formatter.rs`'s test module (mirror the style of the existing `dotenv_load_pragma_formats_first`-style tests near line ~1901, now renamed for `@LoadEnv` by the earlier session work — check the current name before citing it):

```rust
    #[test]
    fn multiple_run_blocks_format_in_source_order_with_labels() {
        let src = "task [T] {\n    run {\n        echo default;\n    };\n\n    run windows {\n        echo win;\n    };\n};\n";
        let formatted = format_source(src).expect("must format");
        assert_eq!(formatted, src);
    }
```

(Use whichever top-level formatting entry point this test module already calls — e.g. `format_source`/`Formatter::format` — check an existing nearby test for the exact function name before citing it verbatim.)

- [ ] **Step 13: Delete the runner-layer OS gating — `runner/task.rs`, `runner/graph.rs`, `runner/error.rs`**

In `spar/src/runner/task.rs`: delete `pub os: Vec<String>,` from the `Task` struct (line 16). Find and delete the corresponding `os: Vec::new(),` (or similar) initializers wherever `Task { ... }` literals are built for tests in this file and in `graph.rs`/`executor.rs` (grep `os:` across `spar/src/runner/*.rs` to find every remaining struct-literal site after this edit and delete each).

In `spar/src/runner/error.rs`: delete the `UnsupportedOperatingSystem { task: String, actual: String, allowed: Vec<String> },` variant (lines ~30-34) and its `Display` arm (lines ~101-109).

In `spar/src/runner/graph.rs`: delete the OS-gating loop inside `plan()` (lines 35-43):
```rust
        for task in &ordered {
            if !task.os.is_empty() && !task.os.iter().any(|os| os == std::env::consts::OS) {
                return Err(RunnerError::UnsupportedOperatingSystem {
                    task: task.name.clone(),
                    actual: std::env::consts::OS.to_owned(),
                    allowed: task.os.clone(),
                });
            }
        }
```
Delete the two tests that exercise it: `plan_rejects_a_requested_task_on_an_unsupported_operating_system` (~line 586-614) and `plan_rejects_an_unsupported_dependency_before_execution` (~line 616-640) — these scenarios are now covered by Task 1 Step 10's task_lowering-level tests instead (OS mismatch is now a lowering-time error, never reaches the runner/graph layer at all).

- [ ] **Step 14: Full workspace build and test**

Run: `cd spar && cargo build --release 2>&1 | tail -40`
Expected: clean build. Fix any remaining compile errors from missed `.os`/`.run` references (grep `\.os\b` and `\bdecl\.run\b`/`\btd\.run\b`/`\btask\.run\b` across `spar/src/` once more to be sure nothing was missed — the investigation for this plan found every reference, but re-grep as a safety net).

Run: `cd spar && cargo test 2>&1 | grep -E "FAILED|test result"`
Expected: every suite `0 failed`.

- [ ] **Step 15: Reinstall both binaries and smoke-test manually**

Run:
```bash
cd spar && cargo install --path . --force
cd ../spar-ls && cargo install --path . --force
```

Write a throwaway scratch file (anywhere under the scratchpad, not committed) reproducing the example from this plan's spec — a task with a bare default and one `run <current-os> { }` override — and run `spar run <task> -f <file>` twice: once as-is (confirm the OS-specific block's command runs, not the default's), and once after temporarily renaming the OS-specific block to an OS that provably isn't the current machine's with NO default present (confirm the clean lowering error fires with the expected message shape). Delete the scratch file afterward.

- [ ] **Step 16: Commit**

```bash
cd /home/occ/Projects/Rust/occ_lang
git -C spar add -A
git -C spar commit -m "feat(spar): replace task os: field with OS-conditional run blocks"
```

---

### Task 2: spar-ls updates

**Files:**
- Modify: `spar-ls/src/completion.rs` (task-metadata completion items, ~line 111-129; the `run`-presence filter, ~line 105-108)
- Modify: `spar-ls/src/hover.rs` (test-fixture `Task` literal, ~line 312)
- Modify: `spar-ls/src/semantic_tokens.rs` (metadata token array, ~line 486-496; run-body interpolation walk)
- Modify: `spar-ls/src/references.rs` (references-walk array, ~line 201-222)
- Test: `spar-ls/src/main.rs` (existing `#[cfg(test)] mod tests` block)

**Interfaces:**
- Consumes: `ast::TaskDecl.run_blocks: Vec<RunBlock>` from Task 1 — this task cannot start until Task 1 is merged and `spar-ls`'s `Cargo.lock` picks up the new `spar` version via a fresh `cargo build`.

- [ ] **Step 1: Update `Cargo.lock` and confirm the break**

Run: `cd spar-ls && cargo build 2>&1 | tail -40`
Expected: compile errors at every `.os`/`.run` reference found in Task 1's investigation — confirms scope, nothing missed.

- [ ] **Step 2: Remove `os` from completion, reconsider the `run`-presence filter**

In `spar-ls/src/completion.rs`, delete this line from `task_metadata_completion_items`'s field array (~line 123):
```rust
("os", "Operating systems allowed to run the task", "os: [$1];"),
```

At ~line 105-108, the presence check `after.trim_start().starts_with(if name == "run" { '{' } else { ':' })` currently treats `run` as a single always-or-never-offered field. Since a task can now have multiple `run` blocks, change the semantics to: offer `run` in metadata completion whenever the task has NO bare/default `run { }` block yet (regardless of how many labeled blocks already exist) — this requires checking the task's already-parsed `run_blocks` for a block with `os: None`, not a flat text-presence check. Locate the calling context (`task_metadata_completion_items`'s caller, likely in `main.rs`'s completion handler) to see whether the task's `TaskDecl` (not just raw source text) is available at that call site — if it is, switch this specific check to `!task.run_blocks.iter().any(|b| b.os.is_none())` instead of the text-presence heuristic; if only raw source text is available at that point, leave the existing text-based heuristic in place for `run` (still correct for the common case of offering `run` when no run block of any kind exists yet) and note in the commit message that multi-block-aware `run` completion refinement is a follow-up, not attempted here — this is an acceptable, explicitly-flagged scope reduction, not a silent gap.

- [ ] **Step 3: Fix the hover test fixture**

In `spar-ls/src/hover.rs`, delete `os: None,` from the `Task { ... }` test-fixture literal (~line 312).

- [ ] **Step 4: Fix semantic_tokens.rs**

In `spar-ls/src/semantic_tokens.rs`: delete `("os", task.os.as_ref()),` from the metadata-fields array (~line 493, inside the function spanning ~486-496).

Find the run-body interpolation-token loop (from this session's earlier LSP work — pattern is `for command in &task.run { for part in &command.parts { ... TT_VARIABLE/TT_PARAMETER ... } }`). Change it to iterate `for block in &task.run_blocks { for command in &block.commands { for part in &command.parts { ... unchanged inner body ... } } }`.

- [ ] **Step 5: Fix references.rs**

In `spar-ls/src/references.rs`, delete `&td.os,` from the array of `&Option<Expr>` walked for find-all-references (~line 201-222, one entry among several).

- [ ] **Step 6: Build and run the full spar-ls suite**

Run: `cd spar-ls && cargo build --release 2>&1 | tail -40`
Expected: clean build.

Run: `cd spar-ls && cargo test 2>&1 | grep -E "FAILED|test result"`
Expected: `0 failed`. Fix any test that referenced the deleted `os` field or the old flat `task.run` shape (search test names containing `run` or `os` in `spar-ls/src/main.rs` for anything not yet updated).

- [ ] **Step 7: Reinstall**

Run: `cd spar-ls && cargo install --path . --force`

- [ ] **Step 8: Commit**

```bash
cd /home/occ/Projects/Rust/occ_lang
git -C spar-ls add -A
git -C spar-ls commit -m "feat(spar-ls): support OS-conditional run blocks, drop os field"
```

---

### Task 3: TextMate grammar update

**Files:**
- Modify: `editors/vscode-spar/syntaxes/spar.tmLanguage.json` (`task_metadata_fields`, `task_run_block`)
- Modify: `editors/vscode-spar/package.json` (version bump)
- Modify: `editors/vscode-spar/CHANGELOG.md` (new entry)

**Interfaces:**
- Consumes: nothing from Task 1/2 directly (grammar is purely syntactic) — can technically run in parallel with Task 1/2, but sequenced last here since it's the smallest, least-risky task and benefits from the language change being settled first in case wording/labels shift during Task 1's execution.

- [ ] **Step 1: Remove `os` from the metadata-fields grammar rule**

In `editors/vscode-spar/syntaxes/spar.tmLanguage.json`, find `task_metadata_fields` (currently):
```json
"task_metadata_fields": {
  "name": "support.type.property-name.task.spar",
  "match": "\\b(description|default|quiet|private|group|confirm|os|dependsOn|cwd|shell|env)(?=\\s*:)"
},
```
Remove `os|` from the alternation.

- [ ] **Step 2: Let `task_run_block` capture an optional label**

Find `task_run_block` (currently):
```json
"task_run_block": {
  "comment": "Raw shell recipe with Spar ${...} expression islands; the recursive brace rule mirrors the lexer's literal shell-brace depth.",
  "name": "meta.embedded.block.shell.spar",
  "contentName": "source.shell",
  "begin": "\\b(run)\\b\\s*(\\{)",
  "beginCaptures": {
    "1": { "name": "keyword.control.run.spar" },
    "2": { "name": "punctuation.section.embedded.begin.spar" }
  },
  "end": "\\}(?=\\s*;)",
  "endCaptures": { "0": { "name": "punctuation.section.embedded.end.spar" } },
  "patterns": [
    { "include": "#run_hash_escape" },
    { "include": "#run_interpolation" },
    { "include": "#run_shell_braces" },
    { "include": "source.shell" }
  ]
},
```
Change `begin` to `"\\b(run)\\b\\s*([a-zA-Z_][a-zA-Z0-9_]*)?\\s*(\\{)"` and `beginCaptures` to:
```json
"beginCaptures": {
  "1": { "name": "keyword.control.run.spar" },
  "2": { "name": "entity.name.tag.os.spar" },
  "3": { "name": "punctuation.section.embedded.begin.spar" }
},
```
(Regex capture-group numbering stays correct whether or not group 2 actually matches — an unmatched optional group simply has no corresponding capture at runtime, `beginCaptures."2"` just doesn't apply to that instance.)

- [ ] **Step 3: Validate the grammar JSON and re-run the tokenizer harness**

Run: `python3 -c "import json; json.load(open('editors/vscode-spar/syntaxes/spar.tmLanguage.json'))" && echo valid`
Expected: `valid`.

If the vscode-textmate/vscode-oniguruma scratch tokenizer harness from earlier this session still exists under the scratchpad, reuse it to tokenize a small fixture with `run windows { echo hi; }` and confirm `windows` gets scope `entity.name.tag.os.spar` and the rest tokenizes exactly as a bare `run { }` block already did. If the harness no longer exists, skip this verification step rather than rebuilding it from scratch — Step 4's real VS Code install is the actual gate.

- [ ] **Step 4: Bump version, changelog, rebuild, install**

Bump `editors/vscode-spar/package.json`'s `version` by one patch. Add a `CHANGELOG.md` entry (match the tone/format of existing entries — see the file's own history for examples) describing: `os:` field removed from task-metadata highlighting; `run` blocks now support an optional OS label (`run windows { }`), highlighted distinctly.

Run:
```bash
cd editors/vscode-spar
npm run build
npx -y @vscode/vsce package
code --install-extension vscode-spar-<new-version>.vsix
```

- [ ] **Step 5: Commit**

```bash
cd /home/occ/Projects/Rust/occ_lang
git -C editors/vscode-spar add -A
git -C editors/vscode-spar commit -m "feat: highlight OS-conditional run block labels, drop os field"
```

---

## Self-Review

**Spec coverage:**
- Bare `run { }` as default → Task 1 Step 2 (lexer), Step 5 (parser `os_label = None` case). ✓
- `run windows|linux|macos { }` labels, matching `std::env::consts::OS` verbatim → Task 1 Step 9 (`current_os = std::env::consts::OS`, direct string match, no translation table). ✓
- Runtime error, clear message, when no match and no default → Task 1 Step 9 (error message format matches spec wording), Step 10 (test), Step 15 (manual smoke test). ✓
- `os: [...]` field removed entirely, no shim → Task 1 Steps 1, 5, 7, 8, 9, 11, 13 (every consumer). ✓
- `spar check` unaffected (doesn't invoke lowering) → confirmed as a side-effect of the architecture, not a separate task (task_lowering only runs when `CompileOptions.evaluate == true`, which `cmd_check` sets to `false` — no code change needed, just noted in the plan header). ✓
- External example-file migration explicitly out of scope → stated in Global Constraints. ✓

**Placeholder scan:** No "TBD"/"handle appropriately"/unfilled steps remain, except two explicitly-flagged, justified exceptions: Task 1 Step 10's test bodies ask the executor to match the existing test-helper style in `spar/tests/task_lowering.rs` rather than inventing one sight-unseen (that file's exact helper name wasn't part of this session's investigation — reading it is a 30-second first step, not a meaningful gap), and Task 1 Step 12 similarly asks to confirm the exact formatter-entry-point function name from a neighboring test before citing it. Both are narrow, bounded lookups against real, already-known files — not open-ended "figure it out" gaps.

**Type consistency:** `RunBlock { os: Option<String>, os_span: Option<Span>, commands: Vec<ShellCommand>, span: Span }` (Task 1 Step 1) is the type used consistently in every later step (parser Step 5, resolver Step 7, typechecker Step 8, task_lowering Step 9, formatter Step 11, spar-ls Step 2/4). `TaskDecl.run_blocks: Vec<RunBlock>` name matches everywhere it's referenced (no leftover `.run`/`.os` in any later step's code).
