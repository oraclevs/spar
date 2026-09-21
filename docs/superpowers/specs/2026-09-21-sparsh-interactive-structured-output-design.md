# Sparsh Interactive Structured Output and Mixed-Pipeline UX Design

**Date:** 2026-09-21
**Scope:** `spar`, `sparsh`, `spar-ls`, top-level examples/acceptance fixtures
**Status:** Approved by user on 2026-09-21
**Commit policy:** Do not commit any repository until the user explicitly asks.

## 1. Goal

Make structured data a first-class interactive experience in Sparsh without changing Unix byte semantics.

At an interactive TTY:

- `cmd | from FORMAT |> ...` may end as a structured value and renders as a readable table/tree.
- `cmd | from FORMAT |> ... |> to FORMAT` with no downstream byte consumer renders a readable, syntax-colored representation of that format.
- tables resemble Nushell's interactive presentation: bordered, indexed, typed coloring, width-aware, and bounded for large results.
- nested records/lists and single JSON-like documents render structurally instead of as one compact line.
- `_` retains the complete successful structured value even when the terminal shows only a preview.
- `from`, `to`, codec names, and structured-pipeline syntax are highlighted as valid syntax instead of unknown commands.

Outside an interactive TTY, existing byte/data behavior remains plain and machine-safe.

A secondary editor improvement adds an explicit multiline keybinding so users can insert a newline at the prompt without backslash continuation. This is isolated behind the structured-output work and must not change `.spar` script continuation semantics.

## 2. User-facing behavior

### 2.1 Structured terminal result

This must be valid at the interactive prompt:

```spar
printf 'name,age,team\nObi,24,core\nAda,31,ops\n' | from csv |> where(fn(row) => row.age > 20)
```

It returns a structured value to Sparsh rather than requiring `|> to FORMAT`.

Expected presentation shape:

```text
╭───┬─────┬──────┬──────╮
│ # │ age │ name │ team │
├───┼─────┼──────┼──────┤
│ 0 │ 24  │ Obi  │ core │
│ 1 │ 31  │ Ada  │ ops  │
╰───┴─────┴──────┴──────╯
2 rows
```

Actual color comes from `sparsh-ui::Theme`; the example above intentionally does not prescribe ANSI codes.

### 2.2 Explicit terminal encoding

This command:

```spar
printf 'name,age,team\nObi,24,core\nAda,31,ops\n' | from csv |> to json
```

still means JSON. Interactive presentation must therefore be pretty, syntax-colored JSON rather than a table:

```json
[
  {
    "age": 24,
    "name": "Obi",
    "team": "core"
  },
  {
    "age": 31,
    "name": "Ada",
    "team": "ops"
  }
]
```

The same principle applies to `jsonl`, `yaml`, `toml`, `csv`, and `tsv`:

- JSON: indented document.
- JSONL: one logical JSON value per record, with each record readable and colored; it must remain recognizably JSONL rather than silently becoming a JSON array.
- YAML: normal YAML layout with keys/scalars colored.
- TOML: sections/keys/scalars colored.
- CSV/TSV: delimiter-oriented display with header/scalar coloring when explicitly requested by `to csv` / `to tsv`.

### 2.3 Byte-preserving contexts

Decoration is forbidden when bytes are consumed by another program or file, or when Sparsh is not in interactive-TTY mode.

These remain plain bytes:

```spar
... |> to json | jq .
... |> to json > output.json
... |> to json >> output.json
```

And:

```bash
sparsh -c '... |> to json'
printf '...' | sparsh
```

must not contain table borders, preview notices, ANSI color, or pretty-print-only formatting that changes the requested serialized stream contract.

### 2.4 Nested data

Interactive structured values use shape-aware presentation:

- `Table<Record>` or a list of records -> table.
- list of scalars -> indexed list table when useful.
- a single `Record` / JSON object -> readable key/value tree.
- nested records/lists inside cells -> compact summary in the table cell, with a readable nested/tree representation when the top-level result itself is that nested document.
- deeply nested values must be depth-bounded in the preview so one field cannot flood the terminal.

Records remain HashMap-backed in this milestone, so deterministic alphabetical field ordering is acceptable and is not changed here.

## 3. Large-output policy

Interactive presentation is a preview, not a lossy runtime value.

### 3.1 Materialized tables/lists/documents

The renderer shows a bounded number of rows/lines based on `RenderOptions` and terminal width. When content is hidden, the footer states that more data exists, for example:

```text
… 173 more rows (200 total), full value in `_`
```

The full materialized value remains in `_`.

### 3.2 Streams

For `Stream<T>`, preview materialization remains bounded. The runtime may consume only the preview budget plus the minimum look-ahead needed to determine truncation. The preview reports that more rows exist. It must not eagerly collect an unbounded stream merely to render it.

The existing stream resource/cancellation architecture remains authoritative.

### 3.3 Pager

A pager is optional for this milestone. The required behavior is bounded preview + explicit hint + `_` containing the complete materialized value when a complete value exists. The implementation may add an opt-in pager only if it can be done without changing the byte/noninteractive contract or delaying the core fix.

## 4. Architecture

### 4.1 Ownership boundaries

The existing ownership model remains unchanged:

- `spar`: grammar, mixed byte/value execution, structured runtime values, format registry, interactive result metadata.
- `spar-process`: byte/process execution only. No structured rendering logic.
- `spar-command`: command/process-neutral plans only. No Spar `Value` dependency.
- `sparsh-core`: prompt/session dispatch and interactive result ownership.
- `sparsh-ui`: terminal rendering, semantic theme, syntax highlighting, width/layout.
- `spar-ls`: editor-neutral language intelligence using standard LSP.

No second structured-query parser is added to Sparsh.

### 4.2 Mixed pipeline without `to`

`ShellMixedPipeline.encoder` remains optional.

The parser/runtime path accepts:

```text
Unix byte producer
    | from FORMAT
    |> zero or more structured stages
    [end]
```

When the pipeline is the sole interactive terminal result and nothing consumes serialized bytes, Spar returns an `InteractiveRuntimeValue` whose presentation is `InteractivePresentation::Pipeline`.

Sparsh must receive the structured `Value` (`Table`, list, record, scalar, or bounded stream preview), not a JSONL byte fallback.

When the same pipeline is noninteractive, embedded in a context requiring bytes, or otherwise lacks terminal structured capture, the established JSONL fallback remains available for compatibility.

### 4.3 Mixed pipeline ending in `to FORMAT`

When all of these are true:

1. interactive TTY session;
2. one mixed pipeline is the terminal result;
3. `to FORMAT` exists;
4. no `| command` follows;
5. no redirect follows;

Spar returns the structured value plus `InteractivePresentation::Encoded(format)` instead of emitting the serialized bytes directly.

`sparsh-ui` then renders that value according to the named format.

Any downstream byte consumer disables terminal capture and uses the serializer normally.

### 4.4 Interactive dispatch fix

The exact direct-prompt regression must be tested through `sparsh-core::ShellSession::submit`, not only through lower-level Spar session tests.

Current Sparsh detects direct mixed pipelines and wraps them as `shell { ...; }` before calling the generic interactive preview path. The implementation must make this route reliable for pipelines that end in a structured value.

Preferred implementation order:

1. Add the direct `ShellSession::submit` regression test first.
2. If the existing wrapper is valid once the real parse failure is exposed, keep it and fix the smallest underlying parser/session issue.
3. If the wrapper remains structurally fragile, add a focused Spar interactive mixed-shell entrypoint that compiles/executes the shell expression without committing synthetic wrapper source to the user session. Do not duplicate mixed-pipeline parsing in Sparsh.

The generic interactive parser must also stop masking a more specific fallback-expression error with the earlier generic top-level error. When parsing an interactive expression fails farther into the source than declaration parsing, the diagnostic should report the more useful failure span/message.

This diagnostic rule is important because a malformed stage should not surface merely as:

```text
unexpected 'shell {' ...
```

when the actual failure is inside the mixed pipeline.

### 4.5 `_` semantics

On successful interactive structured execution:

- `_` updates to the full successful structured value when the runtime has a complete value.
- renderer truncation never mutates the stored value.
- a failed command/pipeline does not replace `_`.
- stream previews keep the existing bounded/materialized semantics; the UI must not imply that a bounded stream preview is an unconsumed live stream.

## 5. Renderer design

### 5.1 Reuse current renderer

Build on the existing `sparsh-ui` modules (`structured.rs`, `data_view.rs`, `encoded.rs`, `styled.rs`, `theme.rs`). Do not replace them with an unrelated table library unless a concrete limitation is proven by tests.

### 5.2 Semantic roles

All Sparsh-generated color uses `Theme` / `SemanticRole`.

Required roles include the existing data/table roles:

- `TableHeader`
- `TableIndex`
- `TableBorder`
- `DataKey`
- `DataString`
- `DataNumber`
- `DataBool`
- `DataNull`
- `DataPunct`
- `Secondary`

`NO_COLOR` selects plain rendering and must remove Sparsh-generated ANSI sequences without changing content/layout semantics.

### 5.3 Table layout

Tables require:

- index column starting at 0;
- colored header row;
- typed value coloring;
- Unicode display-width accounting;
- terminal-width-aware column sizing;
- wrapping/truncation that does not split UTF-8 code points or leave malformed ANSI sequences;
- sensible handling of a single very wide cell;
- hidden-column hint when the terminal cannot display all columns;
- row-count / truncated-row footer.

A narrow terminal may hide or shrink columns, but it must remain readable and must not collapse back into compact JSON.

### 5.4 Record/tree view

A top-level record uses a readable vertical key/value representation. Nested objects/lists recurse with indentation and depth/line bounds. Short scalar lists may remain inline; long or nested lists become indexed blocks.

### 5.5 Encoded output

`encoded.rs` remains responsible for interactive `to FORMAT` presentation. The data registry remains responsible for the real serializer used in byte contexts.

Interactive JSON should be generated from the structured `Value`, not by taking compact serializer bytes and applying ad-hoc whitespace regexes. This preserves type-aware coloring.

For YAML/TOML/CSV/TSV, using the real serializer as the textual source is acceptable, followed by syntax-aware coloring, provided the interactive renderer alone performs the decoration and line limiting.

## 6. Syntax highlighting

### 6.1 Sparsh prompt highlighter

`from` and `to` are contextual bridge syntax, not globally reserved commands.

The highlighter must understand mixed-pipeline state:

```spar
printf ... | from csv |> where(...) |> to json
```

Required roles:

- `printf`: external/builtin according to command resolution.
- `|`: operator.
- `from`: valid Spar/mixed-pipeline syntax, never `UnknownCommand` in bridge position.
- codec (`json`, `jsonl`, `csv`, `tsv`, `yaml`, `toml`, `lines`, `text`): valid format token, using an existing suitable semantic role unless a new role is demonstrably useful.
- `|>`: one operator token; the scanner must prefer it over matching bare `|` first.
- structured stage callable such as `where`: function/Spar syntax according to known-symbol information.
- closure tokens `fn`, parameters, `=>`, field access/comparison: valid Spar expression coloring rather than shell-command coloring.
- terminal `to`: bridge syntax, with following codec recognized.

This is a lexical/UI highlighter. It must preserve every input byte and must not decide execution semantics.

### 6.2 `spar-ls`

The language server must expose semantic tokens for mixed shell pipelines rather than only visiting Unix command segments.

Use standard semantic token types supported by the existing legend. Do not add VS Code-only protocol behavior.

Coverage must include:

- `from` bridge word;
- decoder codec;
- `|>` operator;
- identifiers/callables/closure parameters inside structured stages;
- `to` bridge word;
- encoder codec.

Existing definition/completion/hover behavior for structured functions remains authoritative.

## 7. Interactive multiline editing

This is secondary to the structured-output fix.

### 7.1 Behavior

Interactive Sparsh needs an explicit action that inserts a literal newline into the current Reedline buffer without submitting it.

Default intended chord:

```text
Alt+Shift+Enter
```

Normal Enter continues to use the current submit/completeness behavior.

The user can therefore write:

```spar
printf 'name,age,team\nObi,24,core\nAda,31,ops\n'
    | from csv
    |> where(fn(row) => row.age > 20)
    |> select(["name", "team"])
```

without backslash continuation.

### 7.2 Portability rule

Do not encode multiline semantics as a Bash backslash convention. The buffer contains ordinary newline characters and the parser receives the same text on Linux and macOS.

The keybinding layer already models `alt`, `shift`, and `enter`; add a dedicated action such as `insertNewline` mapped to the appropriate Reedline newline-editing event. Verify the actual terminal/Reedline event behavior with a PTY before treating the exact default chord as portable.

If a terminal cannot distinguish `Alt+Shift+Enter`, the action remains configurable so another chord can be selected without changing parser semantics.

`.spar` script continuation rules are unchanged by this feature.

## 8. Tests

All behavior is test-first.

### 8.1 `spar`

Add focused tests for:

- parsing a mixed pipeline without `to`;
- type/lowering/runtime of a no-encoder mixed pipeline;
- terminal structured capture returns `InteractivePresentation::Pipeline`;
- terminal `to json` returns `InteractivePresentation::Encoded("json")` and the full structured value;
- `to FORMAT | command` stays bytes;
- `to FORMAT > file` stays bytes;
- noninteractive execution stays bytes/plain data;
- diagnostic specificity for malformed content inside an interactive `shell { ... }` / mixed-pipeline wrapper.

Do not change the known limit that a mixed `|>` pipeline is a returned shell value / one prompt submission.

### 8.2 `sparsh-core`

Add the exact regression through `ShellSession::submit`:

```spar
printf 'name,age,team\nObi,24,core\nAda,31,ops\n' | from csv |> where(fn(row) => row.age > 20)
```

with the required `std/data` symbol available in the session.

Assert:

- no parse error;
- result is `ShellResult::Structured`;
- presentation is `Pipeline`;
- value has two rows;
- `_` returns/contains the successful full value;
- failure afterward does not overwrite `_`.

Also cover terminal `|> to json` as `Encoded("json")`.

### 8.3 `sparsh-ui`

Unit tests use both `Theme::plain()` and `Theme::colored()`.

Table tests:

- index/header presence;
- stable alphabetical record columns under current HashMap model;
- scalar type roles produce ANSI in colored theme;
- `Theme::plain()` produces no ANSI;
- narrow-width truncation/hiding;
- Unicode/wide-cell display width;
- 200+ rows show bounded preview and `… N more` footer;
- nested record/list cells do not become giant one-line dumps.

Document tests:

- top-level record is tree-like/readable;
- nested object/list indentation;
- line/depth bounds.

Encoded tests:

- pretty JSON indentation;
- JSON key/string/number/bool/null roles;
- JSONL remains record-delimited;
- YAML/TOML syntax roles;
- CSV/TSV header/scalar roles;
- preview footer for long documents;
- plain theme has no ANSI.

Highlighter tests:

- exact `| from csv |> where(...) |> to json` token roles;
- `|>` is not split as `|` then `>`;
- `from` outside bridge position may still be an ordinary command/identifier;
- all supported codec names are recognized contextually;
- highlighted output preserves every input byte.

### 8.4 `spar-ls`

Add semantic-token tests for the complete mixed pipeline, including UTF-16 ranges where non-ASCII input is present. Assert standard token categories and exact ranges for bridge words/codecs/operators/stage expressions.

### 8.5 PTY integration

Keep/add a Python PTY harness under `sparsh/scripts` or `sparsh/tests` that:

- spawns the built Sparsh binary under a real PTY;
- answers `ESC[6n` cursor-position queries with `ESC[10;1R`;
- sets terminal rows/columns;
- captures raw ANSI output and a stripped form.

PTY acceptance cases:

1. direct no-`to` CSV pipeline -> bordered table, no parse error;
2. direct `to json` -> multiline JSON + ANSI colors;
3. `NO_COLOR=1` -> same readable structure, no ANSI;
4. narrow terminal -> bounded/width-aware output + hint;
5. 200+ rows -> preview footer;
6. `_` after preview -> full structured value remains usable;
7. multiline newline insertion action -> multiple physical lines submit as one logical command.

PTY tests must not depend on the user's personal `~/.sparsh` config; use an isolated temporary `HOME`.

## 9. Examples

Update or create top-level `examples/data-passing.sparsh` with copy/paste examples for:

- CSV -> structured table;
- CSV -> `where` -> table;
- CSV -> pretty JSON;
- JSONL -> structured transform -> table;
- explicit `to jsonl | external-command` byte path;
- redirect to a file;
- nested JSON/record rendering;
- a generated large dataset demonstrating preview behavior.

Do not use interactive backslash continuation in these prompt examples. If a multiline prompt example is included, document the newline keybinding instead.

The uploaded snapshot does not currently contain the top-level `examples/` directory referenced by the user's local workspace, so the implementation artifact may need to add that directory without moving repository contents.

## 10. Quality and verification gates

For every touched repository:

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Expected touched repositories are `spar`, `sparsh`, and `spar-ls`. `spar-command` and `spar-process` should remain untouched unless a failing regression proves a change is required.

After any `spar` change, reinstall all three user-facing binaries required by the project rules:

```bash
cd ~/Projects/Rust/occ_lang/spar
cargo install --path . --force

cd ~/Projects/Rust/occ_lang/spar-ls
cargo install --path . --force

cd ~/Projects/Rust/occ_lang/sparsh
cargo install --path . --force
```

Then run the PTY harness against the installed `sparsh`, not only `target/debug/sparsh`.

Finally run the existing ecosystem acceptance command:

```bash
cd ~/Projects/Rust/occ_lang/examples
sparsh -c ./stress.spar
```

and require the observed output to contain:

```text
failures: 0
```

Only report commands as passing when their real command output was observed. If the execution environment cannot run Rust or the user's local paths, report that verification as not run rather than inferred.

## 11. Compatibility/non-goals

This work does not:

- change `|` from a Unix byte pipe;
- infer structured data from arbitrary command output without explicit `from FORMAT`;
- serialize structured values into Unix commands without explicit `to FORMAT`;
- replace HashMap-backed record storage or preserve source field order;
- add background structured pipelines;
- change `exec shell` capture semantics;
- require an interactive backslash continuation syntax;
- add editor-specific LSP extensions;
- commit changes without explicit user approval.

## 12. Acceptance checklist

The milestone is complete only when all of the following are demonstrated by tests/commands:

- [ ] Direct interactive `| from csv |> where(...)` succeeds without `to`.
- [ ] The result is rendered as a colored indexed table at a normal TTY.
- [ ] `NO_COLOR` preserves layout and removes ANSI.
- [ ] Direct interactive `|> to json` is indented and syntax-colored.
- [ ] YAML/TOML/JSONL/CSV/TSV terminal presentations are readable and colored where applicable.
- [ ] Pipe/redirect/`-c`/piped-stdin contexts remain plain bytes/data.
- [ ] A single nested record renders as a readable tree/record view.
- [ ] Wide cells/narrow terminals degrade sensibly.
- [ ] 200+ rows/doc lines are bounded with a clear remainder hint.
- [ ] `_` preserves the full successful structured value according to the runtime ownership rules.
- [ ] Sparsh prompt highlighting recognizes `from`, `to`, codec names, and `|>`.
- [ ] `spar-ls` semantic tokens cover mixed-pipeline bridge syntax and stage expressions.
- [ ] PTY harness answers `ESC[6n` and passes the interactive cases.
- [ ] Multiline newline insertion is available through a configurable action, with `Alt+Shift+Enter` used when verified by the terminal/Reedline path.
- [ ] `cargo fmt --check`, `cargo check`, `cargo test`, and strict clippy pass in every touched repo.
- [ ] `spar`, `spar-ls`, and `sparsh` are reinstalled after Spar changes.
- [ ] `sparsh -c ./stress.spar` reports `failures: 0` on the user's ecosystem checkout.
- [ ] No repository is committed until the user explicitly requests commits.
