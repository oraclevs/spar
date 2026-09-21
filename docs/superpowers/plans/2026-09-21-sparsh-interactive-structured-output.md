# Sparsh Interactive Structured Output Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make direct interactive mixed pipelines return structured values to Sparsh, render those values as readable Nushell-style tables/trees or syntax-colored encoded documents, teach Sparsh and spar-ls the bridge syntax, and add an explicit interactive newline action without changing script byte semantics.

**Architecture:** `spar` remains the only owner of mixed byte/value parsing and execution. `sparsh-core` routes direct prompt mixed pipelines into a focused Spar interactive shell-preview entry point; `sparsh-ui` owns terminal presentation and prompt highlighting; `spar-ls` exposes editor-neutral semantic tokens. Byte-producing contexts (`-c`, piped stdin, redirects, downstream Unix commands) continue through the real serializers with no ANSI/UI decoration.

**Tech Stack:** Rust, Spar compiler/runtime, `spar-process` streaming bridge, Reedline 0.49, Crossterm, `nu-ansi-term`, `unicode-width`, Python `pty`, standard LSP semantic tokens.

**Spec:** `docs/superpowers/specs/2026-09-21-sparsh-interactive-structured-output-design.md`

## Global Constraints

- `|` remains the Unix byte/process pipe; `|>` remains the Spar structured/value pipe.
- External output is never inferred as structured data; byte/value crossings require explicit `from FORMAT` / `to FORMAT`.
- Interactive no-`to` mixed pipelines return a structured `Value`, not a byte fallback.
- Terminal `to FORMAT` is pretty/colorized only when an interactive TTY is the final consumer.
- `to FORMAT | command`, `to FORMAT > file`, `to FORMAT >> file`, `sparsh -c`, and piped stdin remain plain machine-readable bytes with no ANSI or table decoration.
- `_` keeps the full successful materialized structured value; renderer truncation never mutates it; failed submissions do not replace it.
- Records remain HashMap-backed; deterministic alphabetical field order is acceptable and source-order preservation is out of scope.
- `NO_COLOR` removes Sparsh-generated ANSI while preserving readable layout.
- Use the existing `Theme` / `SemanticRole` system; do not hardcode output colors in renderers.
- Reuse `sparsh-ui`'s existing `structured.rs`, `data_view.rs`, `encoded.rs`, `styled.rs`, and `theme.rs` rather than replacing the renderer wholesale.
- Do not add structured-value dependencies to `spar-command` or `spar-process`.
- Do not change the known limits around background structured pipelines or `exec shell` capture.
- Interactive multiline editing uses a literal newline edit action; do not add interactive backslash continuation semantics.
- `.spar` script continuation behavior is unchanged.
- Never add assistant/automation attribution or generation credits to project files.
- **Do not commit any repository.** The user explicitly requires commits only after a later instruction. Each task ends with a diff/status checkpoint instead of a commit.
- After any `spar` change, final verification reinstalls `spar`, `spar-ls`, and `sparsh` before PTY acceptance.
- Report only results actually observed from commands; do not infer a pass when the toolchain or command was unavailable.

## File Structure

### `spar`

- `spar/src/session.rs` — add a declaration-safe interactive mixed-shell preview entry point that executes one raw prompt pipeline without synthesizing a top-level `shell { ... }` expression; preserve `_`/terminal-capture context.
- `spar/src/parser.rs` — when interactive declaration parsing fails and expression parsing gets farther, return the later/more specific parse diagnostic instead of the generic “start a declaration” error.
- `spar/src/session.rs` test module — regression tests for raw mixed pipeline preview, encoded presentation, byte fallbacks, and diagnostic specificity.
- `spar/tests/mixed_structured_pipeline.rs` — keep end-to-end byte/value/byte execution coverage; add only process-boundary regressions that are clearer as integration tests.

### `sparsh`

- `sparsh/crates/sparsh-core/src/session.rs` — route `is_mixed_byte_pipeline(input)` through the new Spar interactive mixed-shell preview API; preserve `ShellResult::Structured`, `_`, and failure semantics.
- `sparsh/crates/sparsh-core/src/keybinding.rs` — add configurable `insertNewline` action.
- `sparsh/crates/sparsh-ui/src/data_view.rs` — harden table/tree rendering and add unit tests for types, width, Unicode, nested cells, and bounded row previews.
- `sparsh/crates/sparsh-ui/src/encoded.rs` — test/fix pretty JSON/JSONL/YAML/TOML/CSV/TSV presentation and bounded line previews.
- `sparsh/crates/sparsh-ui/src/highlight.rs` — make mixed-pipeline highlighting contextual; recognize `|>`, `from`, `to`, codecs, closures, and structured-stage operators without treating them as unknown commands.
- `sparsh/crates/sparsh-ui/src/editor.rs` — map `insertNewline` to `ReedlineEvent::Edit(vec![EditCommand::InsertNewline])` and add the intended `Alt+Shift+Enter` default binding while retaining user overrides.
- `sparsh/crates/sparsh-ui/src/lib.rs` — preserve interactive vs noninteractive rendering boundary; add/extend regression tests where needed.
- `sparsh/tests/structured_plan3_acceptance.rs` — acceptance through `ShellSession` + renderer.
- `sparsh/tests/cli.rs` — prove `-c` and piped stdin stay compact/plain.
- `sparsh/scripts/pty_session.py` — keep the real PTY driver and extend only what is needed for deterministic input/config setup.
- `sparsh/scripts/verify_structured_output_pty.py` — new assertion-driven PTY acceptance script.

### `spar-ls`

- `spar-ls/src/shell_semantic.rs` — emit semantic tokens for decoder bridge, codecs, structured operators/stages, encoder bridge, and downstream byte commands instead of flattening a mixed pipeline to input/output commands only.
- `spar-ls/src/intelligence_tests.rs` — exact token/range tests, including UTF-16 positioning with non-ASCII text.
- `spar-ls/src/semantic_tokens.rs` — reuse existing standard/custom legend entries (`keyword`, `shellArgument`, `structuredPipe`, function/parameter/property); no editor-specific protocol extension.

### Workspace examples

- `examples/data-passing.sparsh` — create/update copy/paste examples for structured table output, pretty encoded output, byte consumers, redirects, nested documents, large previews, and multiline prompt editing. Do not use backslash continuation in prompt examples.

## Review Focus

1. **A direct prompt pipeline ending after a structured stage** — `printf ... | from csv |> where(...)` must return `ShellResult::Structured(Pipeline)` and never expose the synthetic-wrapper parse error; covered in Tasks 1–2 and PTY Task 8.
2. **A serializer whose bytes are consumed elsewhere** — `|> to json | cat`, `|> to json > file`, `-c`, and piped stdin must remain exact plain bytes with no ANSI/pretty-only mutation; covered in Tasks 1–2, 4, and 8.
3. **Unicode/wide/narrow table content** — emoji/CJK and long cells must not break borders, UTF-8, or terminal width accounting; covered in Task 3.
4. **Large values and `_`** — a 200+ row result must show a bounded preview while `_` retains the complete materialized value and a later failure does not overwrite it; covered in Tasks 2–4 and PTY Task 8.
5. **Terminal key encoding differences** — `Alt+Shift+Enter` maps to literal newline where the terminal reports that chord; `insertNewline` remains configurable so terminals/macOS setups that cannot distinguish it can bind another chord; covered in Task 7 and PTY Task 8.

---

### Task 1: Give Spar a direct interactive mixed-shell preview path

**Files:**
- Modify: `spar/src/session.rs`
- Modify: `spar/src/parser.rs`
- Test: `spar/src/session.rs` test module
- Test: `spar/tests/mixed_structured_pipeline.rs`

**Interfaces:**
- Consumes: existing `RuntimeContext`, `InteractivePreviewResult`, `InteractiveRuntimeValue`, `InteractivePresentation`, `execute_interactive_preview_with_context`, `Session::committed_source`, and mixed-pipeline runtime capture.
- Produces:

```rust
pub fn eval_interactive_shell_preview_with_context(
    &mut self,
    body: &str,
    cwd: &Path,
    environment: &[(OsString, OsString)],
    previous_value: Option<crate::runtime::Value>,
    preview_limit: usize,
) -> Result<InteractivePreviewResult, Vec<SparError>>
```

This API accepts the raw prompt body (`printf ... | from csv |> ...`), does not commit a synthetic wrapper, and returns `RuntimeValue` for terminal structured capture or `Process` for byte contexts.

- [ ] **Step 1: Add RED tests for a raw no-`to` mixed pipeline and terminal `to json`**

In the `spar/src/session.rs` test module, add tests that call the new API directly, not through `shell { ... }`:

```rust
#[test]
fn interactive_shell_preview_accepts_raw_mixed_pipeline_without_to() {
    let mut session = Engine::default().session();
    session.set_structured_terminal(true);
    session.eval(r#"import pkg { where } from "std/data";"#).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let environment = std::env::vars_os().collect::<Vec<_>>();

    let result = session
        .eval_interactive_shell_preview_with_context(
            "printf 'name,age,team\\nObi,24,core\\nAda,31,ops\\n' | from csv |> where(fn(row) => row.age > 20)",
            &cwd,
            &environment,
            None,
            20,
        )
        .expect("raw mixed pipeline should execute");

    let InteractivePreviewResult::RuntimeValue(preview) = result else {
        panic!("expected structured runtime value, got {result:?}");
    };
    assert_eq!(preview.presentation, InteractivePresentation::Pipeline);
    let Value::Table(table) = preview.value else { panic!("expected table") };
    assert_eq!(table.len(), 2);
}

#[test]
fn interactive_shell_preview_preserves_terminal_encoder_presentation() {
    let mut session = Engine::default().session();
    session.set_structured_terminal(true);
    let cwd = std::env::current_dir().unwrap();
    let environment = std::env::vars_os().collect::<Vec<_>>();

    let result = session
        .eval_interactive_shell_preview_with_context(
            "printf 'name,age\\nObi,24\\nAda,31\\n' | from csv |> to json",
            &cwd,
            &environment,
            None,
            20,
        )
        .unwrap();

    let InteractivePreviewResult::RuntimeValue(preview) = result else {
        panic!("expected encoded structured result, got {result:?}");
    };
    assert_eq!(preview.presentation, InteractivePresentation::Encoded("json"));
    assert!(matches!(preview.value, Value::Table(_)));
}
```

- [ ] **Step 2: Run the focused Spar tests and confirm RED**

Run:

```bash
cd ~/Projects/Rust/occ_lang/spar
cargo test session::tests::interactive_shell_preview_accepts_raw_mixed_pipeline_without_to -- --exact --nocapture
cargo test session::tests::interactive_shell_preview_preserves_terminal_encoder_presentation -- --exact --nocapture
```

Expected before implementation: compile failure because `eval_interactive_shell_preview_with_context` does not exist.

- [ ] **Step 3: Implement the declaration-safe preview entry point**

In `spar/src/session.rs`, construct a temporary *function declaration* rather than a top-level `shell {}` expression. Reuse the established compiled-runtime path:

```rust
pub fn eval_interactive_shell_preview_with_context(
    &mut self,
    body: &str,
    cwd: &Path,
    environment: &[(OsString, OsString)],
    previous_value: Option<crate::runtime::Value>,
    preview_limit: usize,
) -> Result<InteractivePreviewResult, Vec<SparError>> {
    let trimmed = body.trim().trim_end_matches(';').trim();
    let previous_type = previous_value.as_ref().and_then(interactive_value_type);

    let mut function_name = "sparshInteractiveShellPreview".to_string();
    let mut suffix = 0_u64;
    while self.identifiers.contains(&function_name)
        || self.committed_source.contains(&format!("function {function_name}"))
    {
        suffix += 1;
        function_name = format!("sparshInteractiveShellPreview{suffix}");
    }

    let function_source = format!(
        "function {function_name}() -> shell {{ return shell {{\n{trimmed};\n}}; }};"
    );
    let runtime_source = join_committed_source(&self.committed_source, &function_source);
    let options = CompileOptions {
        evaluate: false,
        ..self.options.clone()
    };
    let compilation = Compiler::new(options.clone())
        .with_interactive_expressions()
        .with_interactive_previous_type(previous_type)
        .compile(&runtime_source)
        .into_result()?;
    let program = crate::compiled::CompiledProgram::from_compilation(compilation, options)?;

    let mut context = runtime_context(cwd, environment);
    context.set_previous_value(previous_value);
    context.set_structured_terminal(self.structured_terminal);
    let (execution, _) = crate::runtime::execute_interactive_preview_with_context(
        &program,
        &function_name,
        context,
        preview_limit,
    )?;

    Ok(match execution {
        crate::runtime::InteractiveRuntimeExecution::Value(value) => {
            InteractivePreviewResult::RuntimeValue(value)
        }
        crate::runtime::InteractiveRuntimeExecution::Process(outcome) => {
            InteractivePreviewResult::Process(outcome)
        }
    })
}
```

Do not append `function_source` to `committed_source`; the raw prompt pipeline is an execution, not a persistent declaration.

- [ ] **Step 4: Add RED tests for byte-preserving contexts through the new API**

Add:

```rust
#[test]
fn interactive_shell_preview_keeps_downstream_and_redirected_encoders_as_processes() {
    let mut session = Engine::default().session();
    session.set_structured_terminal(true);
    let cwd = std::env::current_dir().unwrap();
    let environment = std::env::vars_os().collect::<Vec<_>>();

    for body in [
        "printf 'n\\n1\\n' | from csv |> to json | cat > /dev/null",
        "printf 'n\\n1\\n' | from csv |> to json > /dev/null",
    ] {
        let result = session
            .eval_interactive_shell_preview_with_context(
                body,
                &cwd,
                &environment,
                None,
                20,
            )
            .unwrap();
        assert!(matches!(result, InteractivePreviewResult::Process(_)), "{body}: {result:?}");
    }
}
```

- [ ] **Step 5: Improve interactive parse-error selection**

In `spar/src/parser.rs`, keep the declaration parse error only when expression parsing does not make greater source progress. Capture the expression parse error and prefer the one with the later span:

```rust
fn parse_error_start(error: &SparError) -> usize {
    match error {
        SparError::ParseError { span, .. } | SparError::LexError { span, .. } => span.start,
        _ => 0,
    }
}
```

Change `parse_top_level_item_or_expression` so an expression failure is retained:

```rust
let expression_error = match self.parse_expr() {
    Ok(expression) if self.at(&Token::Semicolon) => {
        self.advance();
        return Ok(TopLevelItem::Statement(Statement::Expression(expression, span)));
    }
    Ok(expression) if self.at(&Token::Eof) => {
        return Ok(TopLevelItem::Statement(Statement::Expression(expression, span)));
    }
    Ok(_) => self.error(format!("unexpected {} after interactive expression", self.peek().human_name())),
    Err(error) => error,
};
self.pos = start;
if parse_error_start(&expression_error) > parse_error_start(&original) {
    Err(expression_error)
} else {
    Err(original)
}
```

Add a test with malformed content inside an interactive shell/mixed stage and assert the diagnostic is not the generic byte-0 declaration error:

```rust
#[test]
fn interactive_shell_error_reports_the_inner_failure() {
    let mut session = Engine::default().session();
    session.set_structured_terminal(true);
    session.eval(r#"import pkg { where } from "std/data";"#).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let environment = std::env::vars_os().collect::<Vec<_>>();

    let errors = session
        .eval_interactive_shell_preview_with_context(
            "printf 'age\\n24\\n' | from csv |> where(fn(row) => row.age > )",
            &cwd,
            &environment,
            None,
            20,
        )
        .unwrap_err();
    let rendered = errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(!rendered.contains("start a declaration"), "{rendered}");
}
```

- [ ] **Step 6: Run focused and integration Spar tests**

Run:

```bash
cd ~/Projects/Rust/occ_lang/spar
cargo test session::tests::interactive_shell_preview -- --nocapture
cargo test --test mixed_structured_pipeline -- --nocapture
cargo test --test structured_plan3_acceptance -- --nocapture
```

Expected: all selected tests pass; byte-consuming cases still return `Process`/plain bytes.

- [ ] **Step 7: Checkpoint without committing**

Run:

```bash
cd ~/Projects/Rust/occ_lang/spar
git diff --check
git status --short
git diff -- src/session.rs src/parser.rs tests/mixed_structured_pipeline.rs
```

Record the observed output. Do not commit.

---

### Task 2: Route direct Sparsh mixed pipelines to the structured preview and preserve `_`

**Files:**
- Modify: `sparsh/crates/sparsh-core/src/session.rs`
- Test: `sparsh/crates/sparsh-core/src/session.rs` test module
- Test: `sparsh/tests/structured_plan3_acceptance.rs`
- Test: `sparsh/tests/cli.rs`

**Interfaces:**
- Consumes: `spar::Session::eval_interactive_shell_preview_with_context(...)` from Task 1; existing `ShellResult::{Structured, Process}`, `finish_submission`, `remember_interactive_value`.
- Produces: direct prompt mixed pipelines no longer use `format!("shell {{ {}; }}", ...)`; successful terminal results flow through `ShellResult::Structured` and failures leave `last_interactive_value` unchanged.

- [ ] **Step 1: Add the exact failing direct-prompt regression**

In `sparsh/crates/sparsh-core/src/session.rs` tests:

```rust
#[test]
fn direct_mixed_pipeline_without_to_returns_a_structured_table() {
    let mut session = ShellSession::try_new_interactive().unwrap();
    session
        .submit_spar(r#"import pkg { where } from "std/data";"#)
        .unwrap();

    let result = session
        .submit("printf 'name,age,team\\nObi,24,core\\nAda,31,ops\\n' | from csv |> where(fn(row) => row.age > 20)")
        .expect("direct prompt mixed pipeline should execute");

    let ShellResult::Structured(preview) = result else {
        panic!("expected structured result, got {result:?}");
    };
    assert_eq!(preview.presentation, spar::InteractivePresentation::Pipeline);
    let spar::Value::Table(table) = preview.value else { panic!("expected table") };
    assert_eq!(table.len(), 2);
}
```

Add the terminal encoder case:

```rust
#[test]
fn direct_terminal_to_json_returns_encoded_structured_result() {
    let mut session = ShellSession::try_new_interactive().unwrap();
    let result = session
        .submit("printf 'name,age\\nObi,24\\nAda,31\\n' | from csv |> to json")
        .unwrap();
    let ShellResult::Structured(preview) = result else { panic!("expected structured") };
    assert_eq!(preview.presentation, spar::InteractivePresentation::Encoded("json"));
}
```

- [ ] **Step 2: Run focused Sparsh-core tests and confirm the current failure**

Run:

```bash
cd ~/Projects/Rust/occ_lang/sparsh
cargo test -p sparsh-core direct_mixed_pipeline_without_to_returns_a_structured_table -- --exact --nocapture
cargo test -p sparsh-core direct_terminal_to_json_returns_encoded_structured_result -- --exact --nocapture
```

Expected before routing change: the direct no-`to` test reproduces the interactive wrapper failure or otherwise demonstrates the old wrapper path rather than the new API.

- [ ] **Step 3: Replace the synthetic top-level `shell {}` wrapper**

In `ShellSession::submit`, replace:

```rust
let wrapped = format!("shell {{ {}; }}", input.trim().trim_end_matches(';'));
let result = self.submit_spar(&wrapped);
return self.finish_submission(result);
```

with direct preview evaluation:

```rust
if crate::dispatch::is_mixed_byte_pipeline(input) {
    let cwd = self.services.directories.current().to_path_buf();
    let environment = self.services.environment.snapshot();
    let preview = self
        .spar
        .eval_interactive_shell_preview_with_context(
            input,
            &cwd,
            &environment,
            self.last_interactive_value.clone(),
            20,
        )
        .map_err(ShellError::Spar);
    let result = preview.map(|preview| match preview {
        spar::InteractivePreviewResult::RuntimeValue(value) => ShellResult::Structured(value),
        spar::InteractivePreviewResult::Process(outcome) => ShellResult::Process(outcome),
        spar::InteractivePreviewResult::Empty => ShellResult::Empty,
        spar::InteractivePreviewResult::Value(value) => ShellResult::Value(value),
    });
    return self.finish_submission(result);
}
```

Do not append this transient command to `interactive_source`; only actual persistent Spar declarations belong there.

- [ ] **Step 4: Pin `_` full-value and failure semantics**

Add:

```rust
#[test]
fn direct_mixed_preview_keeps_full_value_in_underscore_and_failure_does_not_replace_it() {
    let mut session = ShellSession::try_new_interactive().unwrap();
    let rows = (0..75)
        .map(|n| format!("row{n},{n}"))
        .collect::<Vec<_>>()
        .join("\\n");
    let command = format!("printf 'name,value\\n{rows}\\n' | from csv");

    let first = session.submit(&command).unwrap();
    let ShellResult::Structured(first) = first else { panic!("expected structured") };
    let spar::Value::Table(table) = &first.value else { panic!("expected table") };
    assert_eq!(table.len(), 75);

    assert!(session.submit("printf 'x\\n1\\n' | from csv |> definitelyMissing()").is_err());

    let recalled = session.submit("_").unwrap();
    let ShellResult::Structured(recalled) = recalled else { panic!("expected structured underscore") };
    let spar::Value::Table(table) = recalled.value else { panic!("expected table") };
    assert_eq!(table.len(), 75);
}
```

This checks the runtime value, not just the visible 50-row renderer preview.

- [ ] **Step 5: Add noninteractive byte regressions**

Extend `sparsh/tests/cli.rs` with exact byte assertions:

```rust
#[test]
fn command_mode_terminal_encoder_is_plain_compact_bytes() {
    let output = sparsh()
        .args([
            "-c",
            "printf 'name,age\\nObi,24\\nAda,31\\n' | from csv |> to json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        output.stdout,
        b"[{\"age\":24,\"name\":\"Obi\"},{\"age\":31,\"name\":\"Ada\"}]\n"
    );
    assert!(!output.stdout.contains(&0x1b));
}
```

Keep the existing piped-stdin test that expects JSONL exactly. Do not duplicate redirect coverage in this task; Task 4 adds the exact redirected-byte assertion after the encoded renderer tests are in place.

- [ ] **Step 6: Run Sparsh-core and CLI focused tests**

Run:

```bash
cd ~/Projects/Rust/occ_lang/sparsh
cargo test -p sparsh-core direct_mixed_pipeline -- --nocapture
cargo test -p sparsh-core direct_terminal_to_json -- --nocapture
cargo test -p sparsh-core direct_mixed_preview_keeps_full_value -- --nocapture
cargo test --test structured_plan3_acceptance -- --nocapture
cargo test --test cli piped_structured_results_are_plain_data_not_decorated_tables -- --exact --nocapture
cargo test --test cli command_mode_terminal_encoder_is_plain_compact_bytes -- --exact --nocapture
```

Expected: interactive session tests return structured values; CLI/noninteractive tests remain exact plain bytes.

- [ ] **Step 7: Checkpoint without committing**

```bash
cd ~/Projects/Rust/occ_lang/sparsh
git diff --check
git status --short
git diff -- crates/sparsh-core/src/session.rs tests/structured_plan3_acceptance.rs tests/cli.rs
```

Do not commit.

---

### Task 3: Harden Nushell-style table/tree rendering and large previews

**Files:**
- Modify: `sparsh/crates/sparsh-ui/src/data_view.rs`
- Modify: `sparsh/crates/sparsh-ui/src/structured.rs` only if top-level tree routing needs adjustment
- Test: unit tests in `data_view.rs`
- Test: `sparsh/crates/sparsh-ui/src/lib.rs` test module

**Interfaces:**
- Consumes: `Value`, `TableValue`, `RenderOptions`, `Theme`, `SemanticRole`, `styled::Line`.
- Produces: bounded width-aware table/tree strings; no runtime value mutation.

- [ ] **Step 1: Add table/type-color RED tests using plain and colored themes**

Append a `#[cfg(test)]` module to `data_view.rs` with a helper table:

```rust
fn sample_table() -> TableValue {
    TableValue::from_records(vec![Value::Object(HashMap::from([
        ("name".into(), Value::String("Obi".into())),
        ("age".into(), Value::Int(24)),
        ("active".into(), Value::Bool(true)),
        ("note".into(), Value::Option(None)),
    ]))])
    .unwrap()
}

#[test]
fn plain_table_has_index_sorted_headers_and_no_ansi() {
    let output = render_table(&sample_table(), &Theme::plain(), &RenderOptions::new(100));
    assert!(output.contains("#"));
    assert!(output.contains("active"));
    assert!(output.contains("age"));
    assert!(output.contains("name"));
    assert!(output.contains("│ 0 "));
    assert!(!output.contains("\x1b["));
}

#[test]
fn colored_table_uses_semantic_roles_for_values() {
    let theme = Theme::colored();
    let output = render_table(&sample_table(), &theme, &RenderOptions::new(100));
    assert!(output.contains(&theme.paint(SemanticRole::TableHeader, "age")));
    assert!(output.contains(&theme.paint(SemanticRole::DataNumber, "24")));
    assert!(output.contains(&theme.paint(SemanticRole::DataString, "Obi")));
    assert!(output.contains(&theme.paint(SemanticRole::DataBool, "true")));
    assert!(output.contains(&theme.paint(SemanticRole::DataNull, "null")));
}
```

The `DataString` assertion is intentionally RED against the current scalar-string path if it is unstyled.

- [ ] **Step 2: Apply semantic string roles and preserve multiline string coloring**

Change scalar strings from neutral role to `DataString`:

```rust
Value::String(value) if value.contains('\n') => return None,
Value::String(value) => Line::of(Some(SemanticRole::DataString), sanitize(value)),
```

For multiline strings in `value_lines`, use the same role:

```rust
Value::String(text) if text.contains('\n') => text
    .lines()
    .map(|line| Line::of(Some(SemanticRole::DataString), sanitize(line)))
    .collect(),
```

- [ ] **Step 3: Add narrow-width, Unicode, nested-cell, and 200-row RED tests**

Add tests:

```rust
#[test]
fn narrow_table_hides_columns_without_exceeding_width() {
    let options = RenderOptions { width: 32, max_rows: 50, max_lines: 300 };
    let output = render_table(&sample_table(), &Theme::plain(), &options);
    assert!(output.contains("hidden"), "{output}");
    for line in output.lines().filter(|line| line.starts_with(['╭', '│', '├', '╰'])) {
        assert!(unicode_width::UnicodeWidthStr::width(line) <= 32, "{line:?}");
    }
}

#[test]
fn wide_unicode_cells_keep_valid_width_and_utf8() {
    let table = TableValue::from_records(vec![Value::Object(HashMap::from([
        ("name".into(), Value::String("猫🙂東京🙂猫".repeat(8))),
    ]))]).unwrap();
    let output = render_table(
        &table,
        &Theme::plain(),
        &RenderOptions { width: 28, max_rows: 50, max_lines: 300 },
    );
    assert!(output.is_char_boundary(output.len()));
    for line in output.lines().filter(|line| line.starts_with(['╭', '│', '├', '╰'])) {
        assert!(unicode_width::UnicodeWidthStr::width(line) <= 28, "{line:?}");
    }
}

#[test]
fn nested_values_are_readable_instead_of_one_long_dump() {
    let value = Value::Object(HashMap::from([
        ("user".into(), Value::Object(HashMap::from([
            ("name".into(), Value::String("Obi".into())),
            ("tags".into(), Value::List(vec![Value::String("rust".into()), Value::String("flutter".into())])),
        ]))),
    ]));
    let output = render_value_view(&value, &Theme::plain(), &RenderOptions::new(80));
    assert!(output.lines().count() > 3, "{output}");
    assert!(output.contains("user"));
    assert!(output.contains("tags"));
}

#[test]
fn large_table_is_bounded_and_reports_remaining_rows() {
    let rows = (0..225)
        .map(|n| Value::Object(HashMap::from([("n".into(), Value::Int(n))])))
        .collect();
    let table = TableValue::from_records(rows).unwrap();
    let output = render_table(
        &table,
        &Theme::plain(),
        &RenderOptions { width: 80, max_rows: 50, max_lines: 300 },
    );
    assert!(output.contains("… 175 more rows (225 total), full value in `_`"), "{output}");
}
```

- [ ] **Step 4: Run renderer tests and fix only proven layout defects**

Run:

```bash
cd ~/Projects/Rust/occ_lang/sparsh
cargo test -p sparsh-ui data_view::tests -- --nocapture
```

If a width assertion fails, fix `draw_grid` / `wrap_cell` / `ellipsize` using display width, not byte length. Do not remove borders or collapse to JSON as a workaround.

- [ ] **Step 5: Verify `NO_COLOR` at the render boundary**

Keep `ColorPolicy::for_environment` as the authority. Add/extend the `sparsh-ui/src/lib.rs` test:

```rust
#[test]
fn no_color_policy_keeps_structured_layout_without_ansi() {
    let theme = ColorPolicy::for_environment(true, true).theme();
    assert!(!theme.enabled());
    // Render a Structured table and assert borders remain while ESC does not.
}
```

- [ ] **Step 6: Checkpoint without committing**

```bash
cd ~/Projects/Rust/occ_lang/sparsh
git diff --check
git status --short
git diff -- crates/sparsh-ui/src/data_view.rs crates/sparsh-ui/src/structured.rs crates/sparsh-ui/src/lib.rs
```

Do not commit.

---

### Task 4: Lock down interactive encoded formats and exact noninteractive bytes

**Files:**
- Modify: `sparsh/crates/sparsh-ui/src/encoded.rs`
- Modify: `sparsh/crates/sparsh-ui/src/lib.rs` only if render-boundary fixes are required
- Test: unit tests in `encoded.rs`
- Test: `sparsh/tests/cli.rs`

**Interfaces:**
- Consumes: `InteractivePresentation::Encoded(format)`, `StructuredFormatRegistry`, `Theme`, `RenderOptions`.
- Produces: pretty/colorized terminal representation only; serializer bytes remain authoritative outside interactive UI.

- [ ] **Step 1: Add pretty JSON and semantic-color tests**

```rust
#[test]
fn json_is_pretty_and_semantically_colored() {
    let value = Value::Object(HashMap::from([
        ("name".into(), Value::String("Obi".into())),
        ("age".into(), Value::Int(24)),
        ("active".into(), Value::Bool(true)),
        ("missing".into(), Value::Option(None)),
    ]));
    let options = RenderOptions::new(100);

    let plain = render_encoded("json", &value, &Theme::plain(), &options);
    assert!(plain.contains("\n  \"active\": true"), "{plain}");
    assert!(plain.contains("\n  \"age\": 24"), "{plain}");
    assert!(!plain.contains("\x1b["));

    let theme = Theme::colored();
    let colored = render_encoded("json", &value, &theme, &options);
    assert!(colored.contains(&theme.paint(SemanticRole::DataKey, "\"name\"")));
    assert!(colored.contains(&theme.paint(SemanticRole::DataString, "\"Obi\"")));
    assert!(colored.contains(&theme.paint(SemanticRole::DataNumber, "24")));
    assert!(colored.contains(&theme.paint(SemanticRole::DataBool, "true")));
    assert!(colored.contains(&theme.paint(SemanticRole::DataNull, "null")));
}
```

- [ ] **Step 2: Add JSONL/YAML/TOML/CSV/TSV tests**

Use format-appropriate values and assert these invariants:

```rust
#[test]
fn jsonl_stays_one_json_value_per_physical_line() {
    let value = Value::List(vec![record("Obi", 24), record("Ada", 31)]);
    let output = render_encoded("jsonl", &value, &Theme::plain(), &RenderOptions::new(100));
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|line| line.starts_with('{') && line.ends_with('}')));
}

#[test]
fn yaml_and_toml_color_keys_and_typed_scalars() {
    use std::collections::HashMap;

    let value = Value::Object(HashMap::from([
        ("active".into(), Value::Bool(true)),
        ("age".into(), Value::Int(24)),
        ("name".into(), Value::String("Obi".into())),
    ]));
    let options = RenderOptions::new(100);

    for format in ["yaml", "toml"] {
        let plain = render_encoded(format, &value, &Theme::plain(), &options);
        assert!(plain.contains("name"), "{format}: {plain}");
        assert!(plain.contains("Obi"), "{format}: {plain}");
        assert!(plain.contains("24"), "{format}: {plain}");
        assert!(plain.contains("true"), "{format}: {plain}");
        assert!(!plain.contains("\x1b["), "{format}: {plain}");

        let theme = Theme::colored();
        let colored = render_encoded(format, &value, &theme, &options);
        assert!(
            colored.contains(&theme.paint(SemanticRole::DataKey, "name")),
            "{format}: {colored}"
        );
        assert!(colored.contains("\x1b["), "{format}: {colored}");
    }
}

#[test]
fn csv_and_tsv_color_headers_and_keep_delimiters_visible() {
    use std::collections::HashMap;

    let value = Value::List(vec![
        Value::Object(HashMap::from([
            ("age".into(), Value::Int(24)),
            ("name".into(), Value::String("Obi".into())),
            ("team".into(), Value::String("core".into())),
        ])),
        Value::Object(HashMap::from([
            ("age".into(), Value::Int(31)),
            ("name".into(), Value::String("Ada".into())),
            ("team".into(), Value::String("ops".into())),
        ])),
    ]);
    let options = RenderOptions::new(100);

    for (format, delimiter) in [("csv", ','), ("tsv", '\t')] {
        let plain = render_encoded(format, &value, &Theme::plain(), &options);
        let header = plain.lines().next().expect("header row");
        assert!(header.contains("age") && header.contains("name") && header.contains("team"));
        assert!(header.contains(delimiter), "{format}: {header}");
        assert!(plain.contains("Obi") && plain.contains("Ada"), "{format}: {plain}");

        let theme = Theme::colored();
        let colored = render_encoded(format, &value, &theme, &options);
        for header in ["age", "name", "team"] {
            assert!(
                colored.contains(&theme.paint(SemanticRole::TableHeader, header)),
                "{format}: missing colored header {header}: {colored}"
            );
        }
        assert!(colored.contains(delimiter), "{format}: {colored}");
    }
}
```

- [ ] **Step 3: Add long-document preview test**

```rust
#[test]
fn encoded_document_has_a_bounded_line_preview() {
    let value = Value::List(
        (0..200)
            .map(|n| Value::Object(HashMap::from([("n".into(), Value::Int(n))])))
            .collect(),
    );
    let output = render_encoded(
        "json",
        &value,
        &Theme::plain(),
        &RenderOptions { width: 100, max_rows: 50, max_lines: 20 },
    );
    assert!(output.contains("more lines"), "{output}");
    assert!(output.contains("full value in `_`"), "{output}");
}
```

- [ ] **Step 4: Run encoded renderer tests and make minimal fixes**

```bash
cd ~/Projects/Rust/occ_lang/sparsh
cargo test -p sparsh-ui encoded::tests -- --nocapture
```

JSON must be produced from the structured `Value` emitter. YAML/TOML/CSV/TSV may use the real registry serializer as textual input, but only `sparsh-ui` may add ANSI/preview decoration.

- [ ] **Step 5: Prove serializer byte paths did not change**

Run:

```bash
cd ~/Projects/Rust/occ_lang/sparsh
cargo test --test cli mixed_byte_and_value_pipeline_can_be_typed_directly -- --exact --nocapture
cargo test --test cli command_mode_terminal_encoder_is_plain_compact_bytes -- --exact --nocapture
cargo test --test cli piped_structured_results_are_plain_data_not_decorated_tables -- --exact --nocapture
```

Add this exact redirect regression in `sparsh/tests/cli.rs` using `tempfile::tempdir()` and the existing CLI test helper:

```rust
#[test]
fn redirected_terminal_encoder_writes_compact_plain_json_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let output = sparsh()
        .current_dir(temp.path())
        .args([
            "-c",
            "printf 'name,age\nObi,24\n' | from csv |> to json > people.json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.stdout.is_empty());
    assert_eq!(
        std::fs::read(temp.path().join("people.json")).unwrap(),
        br#"[{"age":24,"name":"Obi"}]
"#
    );
}
```
The expected bytes above are derived from the current Spar document serializer contract: compact JSON plus one trailing newline.

- [ ] **Step 6: Checkpoint without committing**

```bash
cd ~/Projects/Rust/occ_lang/sparsh
git diff --check
git status --short
git diff -- crates/sparsh-ui/src/encoded.rs crates/sparsh-ui/src/lib.rs tests/cli.rs
```

Do not commit.

---

### Task 5: Make the Sparsh prompt highlighter understand mixed structured pipelines

**Files:**
- Modify: `sparsh/crates/sparsh-ui/src/highlight.rs`
- Test: `sparsh/crates/sparsh-ui/src/highlight.rs` test module

**Interfaces:**
- Consumes: `ShellUiSnapshot` command/function knowledge and existing `SemanticRole` values.
- Produces: byte-preserving `Vec<HighlightSpan>` where bridge syntax is valid/contextual and `|>` is a single operator span.

- [ ] **Step 1: Add the exact mixed-pipeline highlighter RED test**

```rust
#[test]
fn mixed_pipeline_bridge_and_structured_stage_are_not_unknown_commands() {
    let mut session = ShellSession::new();
    session
        .submit_spar(r#"import pkg { where } from "std/data";"#)
        .unwrap();
    let snapshot = session.ui_snapshot();
    let source = "printf 'name,age\\nObi,24\\n' | from csv |> where(fn(row) => row.age > 20) |> to json";
    let spans = scan(source, &snapshot);

    let slice = |span: &HighlightSpan| &source[span.range.clone()];
    assert!(spans.iter().any(|s| slice(s) == "from" && s.role == SemanticRole::SparSyntax));
    assert!(spans.iter().any(|s| slice(s) == "csv" && s.role != SemanticRole::UnknownCommand));
    assert!(spans.iter().any(|s| slice(s) == "|>" && s.role == SemanticRole::Operator));
    assert!(spans.iter().any(|s| slice(s) == "where" && s.role == SemanticRole::Function));
    assert!(spans.iter().any(|s| slice(s) == "fn" && s.role == SemanticRole::SparSyntax));
    assert!(spans.iter().any(|s| slice(s) == "to" && s.role == SemanticRole::SparSyntax));
    assert!(spans.iter().any(|s| slice(s) == "json" && s.role != SemanticRole::UnknownCommand));
    assert!(
        spans.iter().all(|s| s.role != SemanticRole::UnknownCommand),
        "known mixed-pipeline syntax must not render as an unknown command: {spans:?}"
    );
}
```

`ShellSession::ui_snapshot()` already exposes Sparsh builtins, including `printf`; keep this fixture fully known so any `UnknownCommand` span is a real regression.

- [ ] **Step 2: Make `|>` win over bare `|`**

Replace the boolean `starts_command` operator result with an explicit kind:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperatorKind {
    StructuredPipe,
    ShellPipe,
    CommandSeparator,
    Expression,
}

fn operator_at(line: &str, index: usize) -> Option<(usize, OperatorKind)> {
    for (operator, kind) in [
        ("|>", OperatorKind::StructuredPipe),
        ("2>>", OperatorKind::Expression),
        ("&&", OperatorKind::CommandSeparator),
        ("||", OperatorKind::CommandSeparator),
        (">>", OperatorKind::Expression),
        ("2>", OperatorKind::Expression),
        ("=>", OperatorKind::Expression),
        (">=", OperatorKind::Expression),
        ("<=", OperatorKind::Expression),
        ("==", OperatorKind::Expression),
        ("!=", OperatorKind::Expression),
        ("|", OperatorKind::ShellPipe),
        (";", OperatorKind::CommandSeparator),
        ("<", OperatorKind::Expression),
        (">", OperatorKind::Expression),
        ("=", OperatorKind::Expression),
    ] {
        if line[index..].starts_with(operator) {
            return Some((operator.len(), kind));
        }
    }
    None
}
```

- [ ] **Step 3: Add a small mixed-pipeline lexical state machine**

Track enough state to classify bridge words contextually without making `from`/`to` global keywords:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MixedHighlightState {
    Shell,
    AfterBytePipe,
    DecoderFormat,
    Structured,
    AfterStructuredPipe,
    EncoderFormat,
    ByteOutput,
}
```

Rules:

- `ShellPipe` -> `AfterBytePipe`.
- In `AfterBytePipe`, literal `from` -> `SparSyntax`, then `DecoderFormat`; otherwise resume normal command classification.
- Supported codec in `DecoderFormat` -> `Argument` (valid, non-red), then `Structured`.
- `StructuredPipe` -> `AfterStructuredPipe`.
- In `AfterStructuredPipe`, literal `to` -> `SparSyntax`, then `EncoderFormat`; otherwise classify the stage as Spar syntax/expression and transition back to `Structured`.
- Supported codec in `EncoderFormat` -> `Argument`, then `ByteOutput`.
- A subsequent Unix `|` from `ByteOutput` resumes normal command classification.

Use one shared codec predicate:

```rust
fn is_structured_codec(word: &str) -> bool {
    matches!(word, "json" | "jsonl" | "csv" | "tsv" | "yaml" | "toml" | "lines" | "text")
}
```

Inside structured mode, recognize `fn`, `true`, `false`, `null`/`None` where applicable as Spar syntax/data rather than commands; known function calls remain `Function`; closure parameter tokens and field/comparison pieces must never be `UnknownCommand`.

- [ ] **Step 4: Add context and byte-preservation tests**

```rust
#[test]
fn from_is_not_a_global_bridge_keyword() {
    let snapshot = ShellSession::new().ui_snapshot();
    let spans = scan("from something", &snapshot);
    assert_ne!(spans[0].role, SemanticRole::SparSyntax);
}

#[test]
fn structured_pipe_is_one_span_not_pipe_plus_greater_than() {
    let snapshot = ShellSession::new().ui_snapshot();
    let source = "value |> take(2)";
    let spans = scan(source, &snapshot);
    let pipe = spans.iter().find(|span| &source[span.range.clone()] == "|>").unwrap();
    assert_eq!(pipe.role, SemanticRole::Operator);
}

#[test]
fn mixed_highlighting_preserves_every_input_byte() {
    let snapshot = Arc::new(RwLock::new(ShellSession::new().ui_snapshot()));
    let highlighter = SparshHighlighter::new(snapshot, Theme::colored());
    let source = "printf 'x\\n' | from lines |> to json";
    let styled = highlighter.highlight(source, source.len());
    let reconstructed = styled.buffer.iter().map(|(_, text)| text.as_str()).collect::<String>();
    assert_eq!(reconstructed, source);
}
```

- [ ] **Step 5: Run the highlighter test module**

```bash
cd ~/Projects/Rust/occ_lang/sparsh
cargo test -p sparsh-ui highlight::tests -- --nocapture
```

Expected: bridge words/codecs are valid, `|>` is a single operator, and the original input bytes reconstruct exactly.

- [ ] **Step 6: Checkpoint without committing**

```bash
cd ~/Projects/Rust/occ_lang/sparsh
git diff --check
git status --short
git diff -- crates/sparsh-ui/src/highlight.rs
```

Do not commit.

---

### Task 6: Extend spar-ls semantic tokens across mixed shell pipelines

**Files:**
- Modify: `spar-ls/src/shell_semantic.rs`
- Modify: `spar-ls/src/semantic_tokens.rs` only if a small shared helper is needed; do not reorder existing legend entries
- Test: `spar-ls/src/intelligence_tests.rs`

**Interfaces:**
- Consumes: `ShellStep::MixedPipeline`, decoder/encoder spans, stage `Expr` spans, `collect_expr_tokens`, `TT_KEYWORD`, `TT_SHELL_ARGUMENT`, `TT_SHELL_OPERATOR`, `TT_STRUCTURED_PIPE`.
- Produces: non-overlapping standard semantic tokens for all parts of a mixed pipeline; existing token indices remain stable.

- [ ] **Step 1: Add a full mixed-pipeline semantic-token RED test**

```rust
#[test]
fn mixed_pipeline_semantic_tokens_cover_bridges_codecs_and_structured_stage() {
    let source = r#"
import pkg { where } from "std/data";
function demo() -> shell {
    return shell {
        printf 'name,age\nObi,24\n'
            | from csv
            |> where(fn(row) => row.age > 20)
            |> to json;
    };
};
"#;
    let tokens = rendered_tokens(source);

    assert!(tokens.iter().any(|(text, ty, _)| text == "from" && *ty == TT_KEYWORD), "{tokens:?}");
    assert!(tokens.iter().any(|(text, ty, _)| text == "csv" && *ty == TT_SHELL_ARGUMENT), "{tokens:?}");
    assert!(tokens.iter().filter(|(text, ty, _)| text == "|>" && *ty == TT_STRUCTURED_PIPE).count() >= 2, "{tokens:?}");
    assert!(tokens.iter().any(|(text, ty, _)| text == "where" && *ty == TT_FUNCTION), "{tokens:?}");
    assert!(tokens.iter().any(|(text, ty, _)| text == "row" && *ty == TT_PARAMETER), "{tokens:?}");
    assert!(tokens.iter().any(|(text, ty, _)| text == "age" && *ty == TT_PROPERTY), "{tokens:?}");
    assert!(tokens.iter().any(|(text, ty, _)| text == "to" && *ty == TT_KEYWORD), "{tokens:?}");
    assert!(tokens.iter().any(|(text, ty, _)| text == "json" && *ty == TT_SHELL_ARGUMENT), "{tokens:?}");
}
```

- [ ] **Step 2: Add a UTF-16 range regression**

Put non-ASCII content before the mixed pipeline and assert the rendered token text is still exact:

```rust
#[test]
fn mixed_pipeline_semantic_ranges_survive_utf16_prefixes() {
    let source = r#"function demo() -> shell { return shell { printf '猫🙂\n' | from lines |> to json; }; };"#;
    let tokens = rendered_tokens(source);
    for needle in ["from", "lines", "|>", "to", "json"] {
        assert!(tokens.iter().any(|(text, _, _)| text == needle), "missing {needle}: {tokens:?}");
    }
}
```

- [ ] **Step 3: Replace mixed-pipeline command flattening with an explicit collector**

In `shell_semantic.rs`, add:

```rust
fn collect_mixed_pipeline_tokens(
    mixed: &spar::ast::ShellMixedPipeline,
    source: &str,
    kinds: &SemanticKinds,
    resolver: &CommandResolver,
    out: &mut Vec<RawToken>,
) {
    // 1. input Unix commands + byte `|` operators
    // 2. `from` as TT_KEYWORD and decoder format as TT_SHELL_ARGUMENT
    // 3. each `|>` as TT_STRUCTURED_PIPE and each stage via collect_expr_tokens
    // 4. terminal `to` as TT_KEYWORD + encoder format as TT_SHELL_ARGUMENT
    // 5. redirect operator/target or downstream Unix commands as existing shell tokens
}
```

Use source-span scanning helpers that take bounded `[start, end)` ranges and `byte_to_lsp_pos`; do not search the whole file for `from`/`to`, because repeated words elsewhere would produce wrong ranges.

For each structured stage, call the existing `collect_expr_tokens(stage, source, kinds, out)` so closure parameters and field access inherit the compiler-derived semantic categories.

For each structured separator, find the `|>` immediately before the next stage/encoder span and emit `TT_STRUCTURED_PIPE`. Keep `TT_SHELL_OPERATOR` for Unix `|` only.

- [ ] **Step 4: Preserve semantic-token legend indices**

Do not insert new token types before index 22. The existing assertion:

```rust
assert_eq!(TOKEN_TYPES[TT_STRUCTURED_PIPE as usize], SemanticTokenType::new("structuredPipe"));
```

must remain true. `from`/`to` use standard `keyword`; codecs use the existing `shellArgument` token type.

- [ ] **Step 5: Run LSP semantic tests**

```bash
cd ~/Projects/Rust/occ_lang/spar-ls
cargo test mixed_pipeline_semantic_tokens_cover_bridges_codecs_and_structured_stage -- --exact --nocapture
cargo test mixed_pipeline_semantic_ranges_survive_utf16_prefixes -- --exact --nocapture
cargo test semantic_token_legend_keeps_existing_indices_and_appends_shell_types -- --exact --nocapture
cargo test semantic_tokens_are_in_bounds_sorted_and_non_overlapping -- --exact --nocapture
```

Expected: bridge/stage tokens are present, ranges are correct, and no legend/index regression occurs.

- [ ] **Step 6: Checkpoint without committing**

```bash
cd ~/Projects/Rust/occ_lang/spar-ls
git diff --check
git status --short
git diff -- src/shell_semantic.rs src/semantic_tokens.rs src/intelligence_tests.rs
```

Do not commit.

---

### Task 7: Add configurable literal-newline editing with `Alt+Shift+Enter`

**Files:**
- Modify: `sparsh/crates/sparsh-core/src/keybinding.rs`
- Modify: `sparsh/crates/sparsh-ui/src/editor.rs`
- Test: both modules' unit tests
- Optionally document in: `sparsh/examples/config.spar` if the existing example already demonstrates keybindings

**Interfaces:**
- Consumes: `KeybindingAction`, `KeyChord`, Reedline `Keybindings`, `ReedlineEvent`, `EditCommand`.
- Produces: `KeybindingAction::InsertNewline`; default `Alt+Shift+Enter`; user-configurable alternate chord.

- [ ] **Step 1: Add RED core action tests**

Add `"insertNewline"` to the expected action list test and:

```rust
#[test]
fn parses_insert_newline_action_and_alt_shift_enter() {
    assert_eq!(
        KeybindingAction::parse("insertNewline").unwrap(),
        KeybindingAction::InsertNewline
    );
    assert_eq!(
        KeyChord::parse("alt+shift+enter").unwrap(),
        KeyChord {
            control: false,
            alt: true,
            shift: true,
            key: KeybindingKey::Enter,
        }
    );
}
```

- [ ] **Step 2: Add the action enum/parser entry**

Update the public action names and enum:

```rust
pub const KEYBINDING_ACTION_NAMES: &[&str] = &[
    // existing entries...
    "insertNewline",
];

pub enum KeybindingAction {
    // existing variants...
    InsertNewline,
}
```

and:

```rust
"insertNewline" => Ok(Self::InsertNewline),
```

- [ ] **Step 3: Map the action to Reedline's literal newline edit**

Import `EditCommand` in `sparsh-ui/src/editor.rs` and map:

```rust
KeybindingAction::InsertNewline => {
    ReedlineEvent::Edit(vec![EditCommand::InsertNewline])
}
```

Add the intended default before user overrides:

```rust
keybindings.add_binding(
    KeyModifiers::ALT | KeyModifiers::SHIFT,
    KeyCode::Enter,
    ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
);
```

Because user overrides are applied afterward, a config entry for the same chord still wins.

- [ ] **Step 4: Add UI mapping/override tests**

```rust
#[test]
fn alt_shift_enter_inserts_a_literal_newline() {
    let keybindings = sparsh_emacs_keybindings(&[]);
    assert_eq!(
        keybindings.find_binding(KeyModifiers::ALT | KeyModifiers::SHIFT, KeyCode::Enter),
        Some(ReedlineEvent::Edit(vec![EditCommand::InsertNewline]))
    );
}

#[test]
fn insert_newline_action_can_be_bound_to_a_portable_alternate_chord() {
    let overrides = vec![KeybindingConfig {
        chord: KeyChord::parse("alt+n").unwrap(),
        action: KeybindingAction::InsertNewline,
    }];
    let keybindings = sparsh_emacs_keybindings(&overrides);
    assert_eq!(
        keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('n')),
        Some(ReedlineEvent::Edit(vec![EditCommand::InsertNewline]))
    );
}
```

This intentionally separates the *newline action* from any terminal's ability to distinguish a particular physical chord.

- [ ] **Step 5: Run keybinding/editor tests**

```bash
cd ~/Projects/Rust/occ_lang/sparsh
cargo test -p sparsh-core keybinding::tests -- --nocapture
cargo test -p sparsh-ui editor::tests -- --nocapture
```

Expected: normal Enter remains `SubmitOrNewline`; `Alt+Shift+Enter` is literal newline; user overrides remain additive/overriding.

- [ ] **Step 6: Checkpoint without committing**

```bash
cd ~/Projects/Rust/occ_lang/sparsh
git diff --check
git status --short
git diff -- crates/sparsh-core/src/keybinding.rs crates/sparsh-ui/src/editor.rs examples/config.spar
```

Do not commit.

---

### Task 8: Add real PTY acceptance, examples, and run the ecosystem verification gate

**Files:**
- Modify: `sparsh/scripts/pty_session.py`
- Create: `sparsh/scripts/verify_structured_output_pty.py`
- Create/Modify: `examples/data-passing.sparsh`
- Test: installed `sparsh` under a real PTY

**Interfaces:**
- Consumes: installed `sparsh`, existing PTY reply behavior for `ESC[6n` -> `ESC[10;1R`, temporary isolated `HOME`.
- Produces: one executable PTY verifier with nonzero exit on regression; user-facing examples with no prompt backslash continuation.

- [ ] **Step 1: Extend the PTY helper for deterministic setup without weakening isolation**

Keep the existing temporary HOME and cursor-position response. Add optional setup files and raw key sends rather than depending on the developer's real `~/.sparsh`:

```python
def run(
    commands,
    columns=100,
    rows=40,
    env=None,
    binary=None,
    quiet=0.6,
    timeout=20,
    home_files=None,
):
    # existing temp HOME setup
    for relative, content in (home_files or {}).items():
        path = os.path.join(home, relative)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(content)
```

Represent raw editor keystrokes explicitly so the PTY verifier can insert a newline without submitting. Add this helper and let `run` accept either normal command strings or `RawInput` steps:

```python
from dataclasses import dataclass

@dataclass(frozen=True)
class RawInput:
    data: bytes

# Inside run(...):
for step in commands:
    if isinstance(step, RawInput):
        os.write(fd, step.data)
    else:
        os.write(fd, step.encode() + b"\r")
    output += _drain(fd, quiet=quiet, limit=timeout)
```

The multiline PTY case will use `RawInput(b"first fragment")`, `RawInput(b"\x1bn")` for the isolated `alt+n` binding, `RawInput(b"second fragment")`, then `RawInput(b"\r")` to submit. This exercises Reedline's real edit buffer; it must not synthesize the expected rendered output.

- [ ] **Step 2: Create an assertion-driven PTY verifier**

Create `sparsh/scripts/verify_structured_output_pty.py` importing `run` and `strip_ansi`. It must execute these cases and `raise AssertionError` with captured output on failure:

```python
TABLE = "printf 'name,age,team\\nObi,24,core\\nAda,31,ops\\n' | from csv"
IMPORT_DATA = 'import pkg { where, count } from "std/data";'
FILTERED = (
    "printf 'name,age,team\\nObi,24,core\\nAda,31,ops\\n' "
    "| from csv |> where(fn(row) => row.age > 20)"
)
ENCODED = "printf 'name,age,team\\nObi,24,core\\nAda,31,ops\\n' | from csv |> to json"
LARGE = "python3 -c 'print(\"n\"); [print(i) for i in range(225)]' | from csv"
COUNT_PREVIOUS = "_ |> count()"
```

Send `IMPORT_DATA` as its own prompt submission before `FILTERED` and `COUNT_PREVIOUS`; do not concatenate declarations and a mixed prompt line into one submission.

Assertions:

1. `TABLE`: stripped output contains `│`, `#`, `age`, `name`, `team`, `Obi`, `Ada`; does not contain `error[parse]`.
2. Filtered pipeline: same table shape, no wrapper parse error.
3. `ENCODED`: raw output contains `\x1b[`; stripped output contains multiple indented JSON lines and is not the compact one-line array.
4. `NO_COLOR=1`: stripped/raw output is readable but raw output contains no `\x1b[` generated for the result.
5. Narrow width (`columns=40`): output contains borders plus either truncation/hidden-column hint; no line layout corruption.
6. `LARGE`: output includes `more rows` and `full value in _`/``full value in `_` ``.
7. After `LARGE`, submit `COUNT_PREVIOUS` in the same PTY session and assert the full count is 225; this proves preview truncation did not replace `_` with the preview.
8. Multiline action: use an isolated config binding `alt+n` -> `insertNewline` for the PTY portability check, type two physical lines separated by the configured edit action, then submit once and assert one logical command executes. The unit test from Task 7 separately pins the intended default `Alt+Shift+Enter` mapping.

The verifier must exit 0 only after all assertions pass and print a compact per-case `PASS` summary.

- [ ] **Step 3: Add/update `examples/data-passing.sparsh`**

Use copy/paste-ready prompt examples, including:

```text
# Structured table
printf 'name,age,team\nObi,24,core\nAda,31,ops\n' | from csv

# Structured filter (run the import once in the session)
import pkg { where } from "std/data";
printf 'name,age,team\nObi,24,core\nAda,31,ops\n' | from csv |> where(fn(row) => row.age > 20)

# Explicit JSON presentation at an interactive terminal
printf 'name,age,team\nObi,24,core\nAda,31,ops\n' | from csv |> to json

# Explicit byte path to another command
printf '%s\n' '{"name":"Obi"}' '{"name":"Ada"}' | from jsonl |> to jsonl | cat

# Redirected bytes
printf 'name,age\nObi,24\n' | from csv |> to json > /tmp/people.json
```

Add nested JSON and a generated large dataset example. For multiline prompt editing, show physical lines and note `Alt+Shift+Enter` inserts a newline; do not add `\` continuation to interactive examples.

- [ ] **Step 4: Run formatting/check/test/clippy in every touched repository**

Run exactly:

```bash
cd ~/Projects/Rust/occ_lang/spar
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings

cd ~/Projects/Rust/occ_lang/sparsh
cargo fmt --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings

cd ~/Projects/Rust/occ_lang/spar-ls
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

If any stage fails, stop the release gate, capture the real failure, use systematic debugging, fix it, and rerun the failed focused test before restarting that repository's full gate.

- [ ] **Step 5: Run the untouched dependency repositories as a cross-ecosystem regression check**

Although no changes are expected there, run:

```bash
cd ~/Projects/Rust/occ_lang/spar-command
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings

cd ~/Projects/Rust/occ_lang/spar-process
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Do not edit these repositories unless a failing regression proves the structured-output change requires it.

- [ ] **Step 6: Reinstall all user-facing binaries after the Spar changes**

Run:

```bash
cd ~/Projects/Rust/occ_lang/spar
cargo install --path . --force

cd ~/Projects/Rust/occ_lang/spar-ls
cargo install --path . --force

cd ~/Projects/Rust/occ_lang/sparsh
cargo install --path . --force
```

Then verify resolution:

```bash
type -a spar
type -a spar-ls
type -a sparsh
spar --version
sparsh --version
```

- [ ] **Step 7: Run the PTY verifier against the installed binary**

```bash
cd ~/Projects/Rust/occ_lang/sparsh
python3 scripts/verify_structured_output_pty.py "$(command -v sparsh)"
```

Expected: every PTY case prints `PASS`; the script exits 0. The verifier itself must continue answering each `ESC[6n` with `ESC[10;1R` via `pty_session.py`.

- [ ] **Step 8: Run the existing stress fixture**

```bash
cd ~/Projects/Rust/occ_lang/examples
sparsh -c ./stress.spar
```

Required observed line:

```text
failures: 0
```

Do not report this gate as passed unless that exact real run succeeds.

- [ ] **Step 9: Final no-commit checkpoint**

From the ecosystem root:

```bash
cd ~/Projects/Rust/occ_lang
for repo in spar spar-command spar-process sparsh spar-ls; do
  echo "===== $repo ====="
  git -C "$repo" status --short
  git -C "$repo" diff --check
done
```

Also inspect the final diffs for forbidden attribution:

```bash
rg -n -i '<forbidden assistant attribution patterns>' \
  spar sparsh spar-ls examples || true
```

Expected: no forbidden attribution. **Do not commit.** Report changed files and the exact verification output; wait for the user's explicit commit instruction.

---

## Final Verification Matrix

| Behavior | Primary test/gate |
|---|---|
| Direct `| from csv |> where(...)` succeeds | Task 2 core test + PTY |
| No-`to` result is a table/tree | Task 3 unit + PTY |
| Terminal `to json` is pretty + colored | Task 4 unit + PTY |
| `NO_COLOR` keeps layout but removes ANSI | Task 3/4 unit + PTY |
| JSONL/YAML/TOML/CSV/TSV format presentation | Task 4 unit tests |
| `to FORMAT | cmd` stays bytes | Task 1 + CLI |
| redirect stays bytes | Task 1 + CLI/file readback |
| `-c` / piped stdin stay plain | Task 2/4 CLI |
| wide/narrow Unicode table behavior | Task 3 unit + narrow PTY |
| 200+ rows bounded preview | Task 3 + PTY |
| `_` retains full successful value | Task 2 + PTY |
| failed command does not replace `_` | Task 2 core test |
| prompt highlighter recognizes bridges/codecs/`|>` | Task 5 |
| spar-ls semantic tokens cover full mixed pipeline | Task 6 |
| UTF-16 semantic ranges remain correct | Task 6 |
| `insertNewline` action exists | Task 7 core test |
| default `Alt+Shift+Enter` maps to literal newline | Task 7 UI test |
| configurable alternate newline chord works in PTY | Task 7 + Task 8 |
| all touched repos fmt/check/test/clippy clean | Task 8 full gates |
| installed binaries are refreshed | Task 8 install gate |
| existing `stress.spar` remains green | Task 8 stress gate |
| no commits made before user approval | final git status |
