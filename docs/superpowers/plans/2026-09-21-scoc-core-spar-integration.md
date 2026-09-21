# SCOC Core + Spar Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the first usable SCOC milestone: a standalone Rust parser library, JC 1.26.0 compatibility harness, six representative parsers (`df`, `ps`, `ls`, `ping`, `fstab`, `env`), native Spar `from <decoder>(...)` integration, automatic streaming, and matching `spar-ls` intelligence.

**Architecture:** SCOC is a sibling Rust library with no Spar dependency. Spar owns decoder syntax, namespace/precedence resolution, option-expression evaluation, streaming policy, conversion from `serde_json::Value` into Spar values, cancellation, and the existing structured pipeline. `spar-ls` consumes the same decoder metadata exposed by Spar rather than maintaining a second hard-coded parser inventory.

**Tech Stack:** Rust 2021; `serde_json` with `preserve_order`; `regex`; `chrono` for `ls` timestamp compatibility; existing Spar/Sparsh/Spar-LS crates; Python 3 only for the non-runtime differential compatibility harness; pinned upstream `jc` 1.26.0 commit `73fa7d5572dd730076723bd6280786bb9101d32f`.

**Spec:** `docs/superpowers/specs/2026-09-21-scoc-master-design.md`

## Global Constraints

- SCOC means **SPA Command Output Converter**. Do not reintroduce the old SJSC name anywhere.
- Create SCOC as a separate sibling repository/crate at `../scoc`; SCOC must not depend on `spar`, `sparsh`, `spar-process`, or `spar-ls`.
- The compatibility oracle is exactly `jc` **1.26.0** at commit `73fa7d5572dd730076723bd6280786bb9101d32f`; do not follow upstream `master` implicitly.
- Preserve JC-compatible field names, value types, null behavior, raw behavior, and record boundaries by default. Do not camel-case output keys.
- SCOC returns in-memory `serde_json::Value`; Spar converts it directly. Never serialize SCOC output to JSON text merely to parse it back into Spar.
- Enable `serde_json` `preserve_order` in SCOC.
- Linux and macOS are first-class targets for this milestone where the selected parser is supported and fixtures exist. Do not advertise Windows compatibility.
- Existing native Spar codecs retain unqualified precedence (`from csv` remains `codec::csv`; `from scoc::csv` is the future explicit SCOC form).
- `streaming` is a Spar decoder control, not a SCOC parse option. `raw` and `ignoreErrors` are accepted only when a parser advertises those capabilities.
- Canonical SCOC names use JC-style kebab case; Rust module names use snake_case.
- Custom Spar parser declarations are **not** implemented in this milestone. The `custom::` namespace is reserved and returns a clear unsupported/not-registered diagnostic.
- No standalone SCOC CLI, native plugin ABI, SCOC package manager, parser auto-detection, or terminal rendering is added.
- Retain the upstream JC MIT notice for copied/substantially derived fixtures or source behavior documentation.
- Do **not** create git commits unless the user explicitly asks. Replace every normal “commit checkpoint” with a test/status checkpoint.
- Do not add the assistant/Claude/OpenAI as a contributor or generated-by author.
- After Spar integration changes are verified, reinstall `spar`, `spar-ls`, and `sparsh` from their local paths before interactive verification.
- Current container preflight on 2026-09-21 found `python3` and `git`, but no `cargo`/`rustc`. At execution time, verify the Rust toolchain before modifying product code; if the execution environment still lacks Rust, do not claim Rust verification.

## Review Focus

1. **Decoder grammar with punctuation and nested expressions:** `from scoc::git-log(raw: flag, mode: choose("a,b"))` must keep kebab-case names distinct from subtraction and preserve nested expression/string syntax. Task 10 adds parser/formatter tests for this class.
2. **Codec/SCOC name collisions:** existing `from csv`, `from yaml`, and aliases must remain native codecs while explicit `scoc::` bypasses precedence. Task 11 adds registry-resolution tests.
3. **Arbitrary streaming chunk boundaries and cancellation:** a `ping` line split across multiple stdout chunks must emit exactly one record, and `|> take(N)` must cancel the producer without surfacing intentional termination as failure. Tasks 9 and 13 test this.
4. **Raw mode can change output shape:** notably `env` normalized output is a table/list of `{name,value}` while `raw: true` is a single object. Tasks 2, 5, and 12 pin shape metadata and conversion behavior.
5. **Platform claims versus parser availability:** Linux/macOS fixture parity must be proven before a platform is listed as supported; unsupported/deferred platforms must diagnose instead of silently parsing. Tasks 3, 7, 8, 9, and 11 test this.

---

## File Structure Map

### New `scoc` sibling crate

- Create `../scoc/Cargo.toml` — crate metadata and dependencies only.
- Create `../scoc/src/lib.rs` — small public API/re-exports; no parser logic.
- Create `../scoc/src/parser.rs` — `ScocParser` and `ScocStreamParser` traits.
- Create `../scoc/src/descriptor.rs` — parser/platform/output/capability/upstream metadata.
- Create `../scoc/src/options.rs` — typed option specs, values, and validation.
- Create `../scoc/src/error.rs` — stable typed SCOC errors.
- Create `../scoc/src/registry.rs` — immutable built-in registry and parser lookup.
- Create `../scoc/src/compatibility.rs` — compiled compatibility baseline constants.
- Create `../scoc/src/utils/{mod.rs,numbers.rs,sizes.rs,tables.rs,text.rs,datetime.rs}` — utilities introduced only for the first six parsers.
- Create `../scoc/src/parsers/{mod.rs,env.rs,fstab.rs,ps.rs,df.rs,ls.rs,ping.rs}` — one parser per file.
- Create `../scoc/compatibility/{jc-baseline.toml,parser-matrix.toml,jc_oracle.py,diff.py}` — pinned oracle metadata and explicit differential runner.
- Create `../scoc/THIRD_PARTY_LICENSES/JC-MIT.txt` — upstream MIT notice.
- Create `../scoc/tests/common/mod.rs` — fixture helpers shared by native parser tests.
- Create `../scoc/tests/parsers/{env.rs,fstab.rs,ps.rs,df.rs,ls.rs,ping.rs}` — native compatibility tests.
- Create `../scoc/tests/fixtures/jc-1.26.0/...` — only the fixture inputs/expected outputs used by the six first parsers, with provenance metadata.

### Existing `spar` crate

- Modify `Cargo.toml` — add sibling `scoc` path dependency.
- Modify `src/ast.rs` — split decoder and encoder AST; add `DecoderRef`, namespace, and named decoder args.
- Modify `src/lexer.rs` — emit a dedicated raw decoder-stage token after `| from` so spaces, strings, nested expressions, namespaces, and kebab-case names survive lexing.
- Modify `src/token.rs` — add the decoder-stage token kind if the lexer approach requires it.
- Modify `src/shell_lang.rs` — parse `from <decoder-ref>(named: expr, ...)` into the new AST while keeping `to FORMAT` unchanged.
- Modify `src/formatter.rs` — round-trip decoder namespaces and named args.
- Modify `src/resolver.rs` — resolve decoder argument expressions against surrounding Spar locals.
- Modify `src/typechecker.rs` — validate decoder existence, option names/types/capabilities, streaming conflicts, and structured stream element type.
- Modify `src/compiled.rs` — carry compiled decoder reference, compiled named arguments, and streaming policy.
- Modify `src/lowerer.rs` — lower decoder argument expressions and descriptor-driven stream types.
- Create `src/structured_input.rs` — higher-level codec/SCOC decoder registry, normalized metadata, resolution precedence, SCOC-to-Spar adapter, and runtime decoder enum.
- Modify `src/structured_codec.rs` — keep native codec ownership; reuse/expose the existing JSON-value conversion helper to the new input layer without JSON text round-trips.
- Modify `src/runtime.rs` — evaluate decoder arguments, select buffered/streaming SCOC path, feed stdout chunks, finish/cancel correctly, and preserve existing serializers/render capture.
- Modify `src/lib.rs` — export tooling-safe decoder metadata APIs for `spar-ls`.
- Extend `tests/acceptance_plan3.rs` (or create `tests/scoc_structured_input.rs` if isolation is clearer) — end-to-end batch, options, collision, streaming, error, and cancellation tests.

### Existing `spar-ls` crate

- Create `src/decoder_intelligence.rs` — decoder cursor detection plus completion/hover/diagnostic builders backed by Spar metadata.
- Modify `src/main.rs` — include the new intelligence module.
- Modify `src/completion.rs` — route `from`/namespace/option contexts into decoder completion.
- Modify `src/hover.rs` — return decoder metadata hover.
- Modify `src/shell_semantic.rs` — classify `from`, namespace, decoder name, option names, and option values semantically.
- Modify `src/diagnostics.rs` only if additional LSP-only decoder diagnostics are needed beyond compiler diagnostics.
- Add focused tests in `src/main.rs` test module for completion, hover, diagnostics, aliases, and semantic tokens.

### Existing `sparsh` workspace

- Prefer no product-code changes. Add/extend `crates/sparsh-core/src/session.rs` tests only if necessary to prove terminal capture renders SCOC-derived `Table`/`Record` values through the existing structured path.
- Reuse `scripts/verify_structured_output_pty.py` for installed-binary smoke verification; do not add a second renderer.

---

### Task 1: Preflight and SCOC core crate contract

**Files:**
- Create: `../scoc/Cargo.toml`
- Create: `../scoc/src/lib.rs`
- Create: `../scoc/src/parser.rs`
- Create: `../scoc/src/descriptor.rs`
- Create: `../scoc/src/options.rs`
- Create: `../scoc/src/error.rs`
- Create: `../scoc/src/compatibility.rs`
- Test: `../scoc/tests/core_api.rs`

**Interfaces:**
- Consumes: none.
- Produces: `ScocParser`, `ScocStreamParser`, `ParserDescriptor`, `ParserOutput`, `OutputShape`, `ParserCapabilities`, `Platform`, `OptionSpec`, `OptionKind`, `OptionValue`, `ParseOptions`, `ScocError`, `CompatibilityBaseline`.

- [ ] **Step 1: Verify the execution toolchain before product changes**

Run:

```bash
cargo --version
rustc --version
python3 --version
git --version
```

Expected: `cargo` and `rustc` are available. If they are still missing, stop implementation in this environment and report that Rust verification cannot be performed here; do not fabricate test results.

- [ ] **Step 2: Write the core API tests first**

Create `../scoc/tests/core_api.rs` with tests that pin descriptor/output metadata and option validation:

```rust
use scoc::{
    CompatibilityBaseline, OptionKind, OptionSpec, OptionValue, OutputShape,
    ParserCapabilities, ParserOutput, Platform,
};

#[test]
fn compatibility_baseline_is_pinned_to_jc_1_26_0() {
    let baseline = scoc::compatibility_baseline();
    assert_eq!(baseline.project, "jc");
    assert_eq!(baseline.version, "1.26.0");
    assert_eq!(baseline.commit, "73fa7d5572dd730076723bd6280786bb9101d32f");
}

#[test]
fn parser_output_can_describe_raw_shape_changes() {
    let output = ParserOutput {
        normalized: OutputShape::Table,
        raw: Some(OutputShape::Record),
        stream_item: None,
    };
    assert_eq!(output.shape(false), OutputShape::Table);
    assert_eq!(output.shape(true), OutputShape::Record);
}

#[test]
fn bool_option_rejects_a_string_value() {
    let spec = OptionSpec::bool("raw", false, "Return JC raw output");
    let error = spec.validate(&OptionValue::String("true".into())).unwrap_err();
    assert!(error.to_string().contains("raw"));
    assert!(error.to_string().contains("bool"));
}
```

This deliberately refines the design's exact `ParserOutput` signature to include `raw: Option<OutputShape>` because JC `env(raw=True)` changes from normalized table output to a raw object. The master design explicitly allows exact Rust signatures to evolve while keeping the semantic contract.

- [ ] **Step 3: Run the new test and verify it fails before the crate exists**

Run:

```bash
cd ../scoc
cargo test --test core_api
```

Expected: failure because the crate/types are not implemented yet.

- [ ] **Step 4: Create the crate manifest and minimal core types**

Use this dependency policy in `../scoc/Cargo.toml`:

```toml
[package]
name = "scoc"
version = "0.1.0"
edition = "2021"
license = "MIT"
description = "SPA Command Output Converter"

[dependencies]
chrono = { version = "0.4", default-features = true }
regex = "1"
serde_json = { version = "1", features = ["preserve_order"] }
thiserror = "2"
```

Implement these exact semantic shapes:

```rust
pub trait ScocParser: Send + Sync {
    fn descriptor(&self) -> &'static ParserDescriptor;
    fn parse(&self, input: &[u8], options: &ParseOptions) -> Result<serde_json::Value, ScocError>;
    fn stream_parser(&self, options: &ParseOptions) -> Result<Box<dyn ScocStreamParser>, ScocError>;
}

pub trait ScocStreamParser: Send {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<serde_json::Value>, ScocError>;
    fn finish(self: Box<Self>) -> Result<Vec<serde_json::Value>, ScocError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParserOutput {
    pub normalized: OutputShape,
    pub raw: Option<OutputShape>,
    pub stream_item: Option<OutputShape>,
}

impl ParserOutput {
    pub fn shape(self, raw: bool) -> OutputShape {
        if raw { self.raw.unwrap_or(self.normalized) } else { self.normalized }
    }
}
```

`ParseOptions` must expose typed lookup helpers (`bool`, `integer`, `float`, `string`) and validate supplied names against the descriptor before parser execution. Do not store untyped `serde_json::Value` as the public option API.

- [ ] **Step 5: Run the core API test until green**

Run:

```bash
cargo test --test core_api
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all pass.

- [ ] **Step 6: Record a checkpoint without committing**

Run:

```bash
git status --short 2>/dev/null || true
```

Expected: only the new SCOC core files for this task are changed/untracked. Do not commit.

---

### Task 2: Immutable registry, parser metadata, and streaming/error semantics

**Files:**
- Create: `../scoc/src/registry.rs`
- Modify: `../scoc/src/lib.rs`
- Modify: `../scoc/src/parser.rs`
- Modify: `../scoc/src/options.rs`
- Modify: `../scoc/src/error.rs`
- Test: `../scoc/tests/registry.rs`

**Interfaces:**
- Consumes: Task 1 core traits/types.
- Produces: `ParserRegistry`, `registry()`, `parser(name)`, `parsers()`, `parse(name, bytes, options)`, `stream_parser(name, options)`.

- [ ] **Step 1: Write registry tests**

Create tests that pin unknown parser errors, case normalization policy, option validation, and unsupported streaming:

```rust
#[test]
fn unknown_parser_is_typed_error() {
    let err = scoc::parse("does-not-exist", b"", &Default::default()).unwrap_err();
    assert!(matches!(err, scoc::ScocError::UnknownParser { .. }));
}

#[test]
fn forcing_streaming_on_batch_parser_is_error() {
    let parser = scoc::parser("env").expect("env registered after parser task");
    if !parser.capabilities.streaming {
        let err = scoc::stream_parser("env", &Default::default()).unwrap_err();
        assert!(matches!(err, scoc::ScocError::StreamingUnsupported { .. }));
    }
}
```

During this task, register a private test parser under `#[cfg(test)]` so registry behavior can be proven before the six real parser modules land.

- [ ] **Step 2: Run the registry test and verify failure**

Run: `cargo test --test registry`

Expected: FAIL because `ParserRegistry` and top-level dispatch do not exist.

- [ ] **Step 3: Implement a single immutable built-in registry**

Use `std::sync::OnceLock` and register parser objects by canonical name and aliases. Keep canonical descriptors unique. Normalize lookup names by trim + ASCII lowercase only; do not rewrite underscores to dashes silently.

Required public calls:

```rust
pub fn registry() -> &'static ParserRegistry;
pub fn parser(name: &str) -> Option<&'static ParserDescriptor>;
pub fn parsers() -> impl Iterator<Item = &'static ParserDescriptor>;
pub fn parse(name: &str, input: &[u8], options: &ParseOptions) -> Result<Value, ScocError>;
pub fn stream_parser(name: &str, options: &ParseOptions) -> Result<Box<dyn ScocStreamParser>, ScocError>;
```

`parse` and `stream_parser` must validate options against the selected descriptor before entering parser code.

- [ ] **Step 4: Run focused and full SCOC core tests**

Run:

```bash
cargo test --test registry
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all pass.

---

### Task 3: JC baseline, licensing, fixture layout, and differential harness

**Files:**
- Create: `../scoc/THIRD_PARTY_LICENSES/JC-MIT.txt`
- Create: `../scoc/compatibility/jc-baseline.toml`
- Create: `../scoc/compatibility/parser-matrix.toml`
- Create: `../scoc/compatibility/jc_oracle.py`
- Create: `../scoc/compatibility/diff.py`
- Create: `../scoc/tests/common/mod.rs`
- Create: `../scoc/tests/compatibility_metadata.rs`
- Create fixture subtree: `../scoc/tests/fixtures/jc-1.26.0/`

**Interfaces:**
- Consumes: Task 1 baseline constants.
- Produces: deterministic local fixture loader plus explicit Python oracle runner used only by compatibility/release checks.

- [ ] **Step 1: Copy the exact JC MIT notice and pin baseline metadata**

`compatibility/jc-baseline.toml` must be exactly semantically equivalent to:

```toml
project = "jc"
version = "1.26.0"
commit = "73fa7d5572dd730076723bd6280786bb9101d32f"
```

`THIRD_PARTY_LICENSES/JC-MIT.txt` must retain `Copyright (c) 2020 Kelly Brazil` and the complete MIT permission/warranty text from upstream `LICENSE.md` at the pinned commit.

- [ ] **Step 2: Seed the parser matrix with only truthful first-milestone states**

Start the six entries as `in-progress`, not `compatible`:

```toml
[parsers.df]
status = "in-progress"
platforms = ["linux", "macos"]
raw = true
streaming = false
jc_standard = "df"

[parsers.ps]
status = "in-progress"
platforms = ["linux", "macos"]
raw = true
streaming = false
jc_standard = "ps"

[parsers.ls]
status = "in-progress"
platforms = ["linux", "macos"]
raw = true
streaming = false
jc_standard = "ls"

[parsers.ping]
status = "in-progress"
platforms = ["linux", "macos"]
raw = true
streaming = true
jc_standard = "ping"
jc_streaming = "ping-s"

[parsers.fstab]
status = "in-progress"
platforms = ["linux"]
raw = true
streaming = false
jc_standard = "fstab"

[parsers.env]
status = "in-progress"
platforms = ["linux", "macos"]
raw = true
streaming = false
jc_standard = "env"
```

Do not list macOS for `fstab` because upstream metadata does not claim Darwin compatibility.

- [ ] **Step 3: Implement a JSON-lines oracle protocol**

`jc_oracle.py` accepts one JSON request on stdin:

```json
{"parser":"df","input_path":".../df.out","raw":false,"streaming":false,"ignore_errors":false}
```

It imports the installed pinned `jc`, verifies `jc.__version__`/`jc.lib.__version__ == "1.26.0"`, runs `jc.parse(...)`, realizes streaming iterators to a list for comparison, and prints exactly one JSON value to stdout. Any traceback goes to stderr and exits non-zero.

`diff.py` takes `--parser`, `--fixture`, optional `--raw`, `--streaming`, and `--scoc-bin`/test-driver path, invokes both sides, parses their JSON outputs, and performs structural equality. It must never be called by ordinary `cargo test`.

- [ ] **Step 4: Add compatibility metadata tests**

Tests must ensure the compiled constant, TOML baseline, and matrix baseline agree and that no `compatible` parser is missing a native fixture directory.

Run: `cargo test --test compatibility_metadata`

Expected: PASS with all six still `in-progress`.

- [ ] **Step 5: Create the pinned Python environment only for the differential check**

Use an isolated environment outside runtime dependencies:

```bash
python3 -m venv .compat-venv
.compat-venv/bin/python -m pip install --upgrade pip
.compat-venv/bin/python -m pip install "git+https://github.com/kellyjonbrazil/jc.git@73fa7d5572dd730076723bd6280786bb9101d32f"
.compat-venv/bin/python -c 'import jc; print(jc.__version__)'
```

Expected final output: `1.26.0`.

Do not add `.compat-venv` to shipped/runtime dependencies; add it to `.gitignore` if the new SCOC repo has one.

---

### Task 4: Shared numeric, size, text, and table utilities

**Files:**
- Create: `../scoc/src/utils/mod.rs`
- Create: `../scoc/src/utils/numbers.rs`
- Create: `../scoc/src/utils/sizes.rs`
- Create: `../scoc/src/utils/text.rs`
- Create: `../scoc/src/utils/tables.rs`
- Test: `../scoc/tests/utils.rs`

**Interfaces:**
- Consumes: `ScocError`.
- Produces: `to_i64_or_null`, `to_f64_or_null`, `parse_human_size`, `simple_table_parse`, `sparse_table_parse`, line/UTF-8 helpers.

- [ ] **Step 1: Write utility compatibility tests from the behavior required by the first parsers**

Pin at least these cases:

```rust
#[test]
fn simple_table_keeps_spaces_in_last_column() {
    let rows = simple_table_parse(&[
        "uid pid cmd",
        "root 1 /usr/lib/systemd --system",
    ]).unwrap();
    assert_eq!(rows[0]["cmd"], json!("/usr/lib/systemd --system"));
}

#[test]
fn sparse_table_preserves_blank_cells_as_null() {
    let rows = sparse_table_parse(&[
        "filesystem  size  used  mounted_on",
        "/dev/a      10          /",
    ]).unwrap();
    assert_eq!(rows[0]["used"], Value::Null);
}

#[test]
fn human_sizes_match_jc_binary_and_posix_rules() {
    assert_eq!(parse_human_size("1K", false).unwrap(), 1024);
    assert_eq!(parse_human_size("1K", true).unwrap(), 1024);
    assert_eq!(parse_human_size("1.5G", false).unwrap(), 1_610_612_736);
}
```

Before finalizing the expected size cases, compare them against pinned `jc.utils.convert_size_to_int` using the oracle environment; the Rust test values must match the oracle, not an assumed unit convention.

- [ ] **Step 2: Implement only the utility surface required by `df` and `ps`**

`simple_table_parse` follows JC's contract: first row is header, no blank cells expected, split data at most `headers.len() - 1` times so the last column can contain spaces.

`sparse_table_parse` follows header column boundaries and yields JSON null for blank cells. Add explicit errors for empty header input rather than panicking.

- [ ] **Step 3: Run utility tests and lints**

Run:

```bash
cargo test --test utils
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

---

### Task 5: Port `env` with raw-shape compatibility

**Files:**
- Create: `../scoc/src/parsers/mod.rs`
- Create: `../scoc/src/parsers/env.rs`
- Modify: `../scoc/src/registry.rs`
- Modify: `../scoc/compatibility/parser-matrix.toml`
- Create: `../scoc/tests/parsers/env.rs`
- Create/update: `../scoc/tests/fixtures/jc-1.26.0/generic/env/*`

**Interfaces:**
- Consumes: registry/core types.
- Produces: canonical parser `env`, aliases/magic metadata for `printenv`, normalized `Table`, raw `Record`.

- [ ] **Step 1: Vendor the exact env fixtures referenced by pinned `tests/test_env.py` and record provenance**

For every copied input, add adjacent metadata with upstream repository, commit, and original path. Generate normalized and raw expected JSON from the pinned oracle rather than hand-editing expected output.

- [ ] **Step 2: Write failing native tests for the distinctive env behavior**

Include empty input, simple variables, `=` inside values, multiline continuation, normalized output, and raw object output:

```rust
#[test]
fn raw_env_is_an_object_but_normalized_env_is_a_table() {
    let input = b"A=1\nB=two=parts\n";
    let normalized = scoc::parse("env", input, &Default::default()).unwrap();
    assert_eq!(normalized, json!([
        {"name":"A","value":"1"},
        {"name":"B","value":"two=parts"}
    ]));

    let raw = scoc::parse("env", input, &ParseOptions::from_pairs([("raw", true.into())])).unwrap();
    assert_eq!(raw, json!({"A":"1","B":"two=parts"}));
}
```

- [ ] **Step 3: Implement env parsing to JC 1.26.0 behavior**

Recognize variable starts with `^[A-Za-z_][A-Za-z0-9_]*=\S*.*$`; a non-matching following line is appended with `\n` to the current variable value. Preserve insertion order.

Descriptor requirements:

```text
name: env
parser_version: 1.5
platforms advertised by SCOC milestone: linux, macos
normalized output: Table
raw output: Record
raw capability: true
streaming: false
upstream standard: env
```

- [ ] **Step 4: Run native and differential env checks**

Run:

```bash
cargo test --test env
.compat-venv/bin/python compatibility/diff.py --parser env --all-fixtures
```

Expected: both pass. Only then change `parsers.env.status` to `compatible`.

---

### Task 6: Port `fstab`

**Files:**
- Create: `../scoc/src/parsers/fstab.rs`
- Modify: `../scoc/src/parsers/mod.rs`
- Modify: `../scoc/src/registry.rs`
- Modify: `../scoc/compatibility/parser-matrix.toml`
- Create: `../scoc/tests/parsers/fstab.rs`
- Create/update: `../scoc/tests/fixtures/jc-1.26.0/linux/fstab/*`

**Interfaces:**
- Consumes: numeric conversion helper.
- Produces: `fstab` parser with normalized/raw `Table`.

- [ ] **Step 1: Vendor and oracle-freeze all pinned `test_fstab.py` fixtures**

Include provenance metadata and both normalized/raw expected outputs.

- [ ] **Step 2: Write failing tests for comments, blanks, optional fields, and malformed rows**

Pin JC's optional `fs_freq`/`fs_passno` default of `0` for 4- and 5-field entries:

```rust
#[test]
fn fstab_defaults_optional_frequency_and_pass_number() {
    let value = scoc::parse("fstab", b"UUID=x / ext4 defaults\n", &Default::default()).unwrap();
    assert_eq!(value[0]["fs_freq"], 0);
    assert_eq!(value[0]["fs_passno"], 0);
}
```

Also test that comment-only/empty input returns `[]` and a row too short to contain the first four required fields returns a typed parse error instead of panicking.

- [ ] **Step 3: Implement fstab parser**

Descriptor: parser version `1.8`, tag `file`, milestone platform Linux only, raw true, streaming false, normalized/raw Table.

- [ ] **Step 4: Run native + differential fstab checks and update matrix only after green**

Run:

```bash
cargo test --test fstab
.compat-venv/bin/python compatibility/diff.py --parser fstab --all-fixtures
```

---

### Task 7: Port `ps`

**Files:**
- Create: `../scoc/src/parsers/ps.rs`
- Modify: `../scoc/src/parsers/mod.rs`
- Modify: `../scoc/src/registry.rs`
- Modify: `../scoc/compatibility/parser-matrix.toml`
- Create: `../scoc/tests/parsers/ps.rs`
- Create/update: `../scoc/tests/fixtures/jc-1.26.0/{linux,macos}/ps/*`

**Interfaces:**
- Consumes: `simple_table_parse`, numeric conversion helpers.
- Produces: `ps` parser, raw/normalized `Table`.

- [ ] **Step 1: Inventory and vendor every Linux/macOS fixture referenced by pinned `tests/test_ps.py`**

Generate expected raw/normalized JSON with the pinned oracle.

- [ ] **Step 2: Write failing tests for both supported layouts**

Cover `ps -ef` and `ps axu`, including a command field with spaces and TTY null normalization:

```rust
#[test]
fn ps_axu_normalizes_percent_fields_and_tty() {
    let input = b"USER PID %CPU %MEM VSZ RSS TTY STAT START TIME COMMAND\nroot 1 0.5 1.2 100 50 ? Ss 10:00 0:01 /sbin/init --system\n";
    let value = scoc::parse("ps", input, &Default::default()).unwrap();
    assert_eq!(value[0]["pid"], 1);
    assert_eq!(value[0]["cpu_percent"], 0.5);
    assert_eq!(value[0]["mem_percent"], 1.2);
    assert!(value[0]["tty"].is_null());
    assert_eq!(value[0]["command"], "/sbin/init --system");
}
```

Add raw-mode assertions that numbers remain strings and `?`/`??` remain strings.

- [ ] **Step 3: Implement ps normalization exactly**

Rename `%cpu` → `cpu_percent`, `%mem` → `mem_percent`; normalize `pid`, `ppid`, `c`, `vsz`, `rss` to integers and CPU/memory percentages to floats; normalize `tty` `?`/`??` and `tt` `??` to null only outside raw mode.

Descriptor: upstream version `1.7`; SCOC milestone platforms Linux + macOS; raw true; streaming false; output Table.

- [ ] **Step 4: Run native + differential ps checks**

Run:

```bash
cargo test --test ps
.compat-venv/bin/python compatibility/diff.py --parser ps --all-fixtures
```

Update matrix to `compatible` only after both pass on applicable fixture sets.

---

### Task 8: Port `df` and size normalization

**Files:**
- Create: `../scoc/src/parsers/df.rs`
- Modify: `../scoc/src/utils/sizes.rs`
- Modify: `../scoc/src/utils/tables.rs`
- Modify: `../scoc/src/parsers/mod.rs`
- Modify: `../scoc/src/registry.rs`
- Modify: `../scoc/compatibility/parser-matrix.toml`
- Create: `../scoc/tests/parsers/df.rs`
- Create/update: `../scoc/tests/fixtures/jc-1.26.0/{linux,macos,generic}/df/*`

**Interfaces:**
- Consumes: sparse table parser and exact JC-compatible size conversion.
- Produces: `df` parser raw/normalized `Table`.

- [ ] **Step 1: Vendor the exact fixtures used by pinned `tests/test_df.py`**

At minimum the upstream test currently references CentOS 7.7, Ubuntu 18.04, macOS 10.11.6, macOS 10.14.6, human-readable `df -h`, macOS `df -H`, and generic long-filesystem cases. Preserve original file names/paths in provenance metadata.

- [ ] **Step 2: Write failing native tests for every upstream case plus malformed safety**

Include empty input → `[]`, trailing newline stability, long filesystem names, Linux/macOS headers, `%` normalization, and human-readable size conversion.

- [ ] **Step 3: Implement df structural parsing and normalization**

Required transformations from pinned JC 2.1 behavior:

```text
lowercase header
'-' -> '_'
'mounted on' -> 'mounted_on'
'avail' -> 'available'
'use%' -> 'use_percent'
'capacity' -> 'capacity_percent'
'%iused' -> 'iused_percent'
*_blocks -> integer
percent fields -> strip '%' then integer
size/used/available -> bytes with JC posix-mode behavior
```

Implement the long-filesystem workaround behaviorally (not necessarily by copying JC's exact hash algorithm structure) and verify it against the fixture.

Descriptor: upstream version `2.1`; Linux + macOS; raw true; streaming false; Table.

- [ ] **Step 4: Run native + differential df checks and matrix gate**

Run:

```bash
cargo test --test df
.compat-venv/bin/python compatibility/diff.py --parser df --all-fixtures
```

Do not mark compatible if any macOS fixture differs.

---

### Task 9: Port `ls` including ISO timestamps and filesystem edge cases

**Files:**
- Create: `../scoc/src/parsers/ls.rs`
- Create: `../scoc/src/utils/datetime.rs`
- Modify: `../scoc/src/parsers/mod.rs`
- Modify: `../scoc/src/registry.rs`
- Modify: `../scoc/compatibility/parser-matrix.toml`
- Create: `../scoc/tests/parsers/ls.rs`
- Create/update: `../scoc/tests/fixtures/jc-1.26.0/{linux,macos,generic}/ls/*`

**Interfaces:**
- Consumes: numeric conversion and timestamp helper.
- Produces: `ls` parser raw/normalized `Table`.

- [ ] **Step 1: Inventory pinned `tests/test_ls.py` and vendor all fixtures used for Linux/macOS parity**

Include ordinary one-file-per-line output, `-l`, recursive sections/parents, symlinks, device major/minor numbers, filenames with newlines, long/full ISO time styles, and empty directory variants where upstream tests cover them.

- [ ] **Step 2: Write failing tests that pin the high-risk branches**

Required assertions include:

```rust
#[test]
fn ls_device_rows_use_major_and_minor_instead_of_size() { /* b/c mode fixture */ }

#[test]
fn ls_symlink_splits_filename_and_link_target() { /* "name -> target" */ }

#[test]
fn ls_recursive_output_carries_parent() { /* -R fixture */ }

#[test]
fn ls_long_iso_adds_epoch_fields_but_default_date_does_not_guess() { /* ISO fixture */ }
```

Also add a malformed/empty-input regression so indexing `linedata[0]` can never panic.

- [ ] **Step 3: Implement `ls` using the pinned 1.13 behavior**

Use permission-line detection equivalent to JC's `_PERM_RE`; support default date width, `--time-style=long-iso`, and `--time-style=full-iso`; preserve raw strings; normalize `links`, `size`, `major_number`, and `minor_number` to integers; add `epoch`/`epoch_utc` only when the date can be compatibly converted.

Descriptor: Linux + macOS, raw true, streaming false, Table, upstream version `1.13`.

- [ ] **Step 4: Run native + differential ls checks**

Run:

```bash
cargo test --test ls
.compat-venv/bin/python compatibility/diff.py --parser ls --all-fixtures
```

Only then mark compatible.

---

### Task 10: Port `ping` batch and streaming parsers

**Files:**
- Create: `../scoc/src/parsers/ping.rs`
- Modify: `../scoc/src/parsers/mod.rs`
- Modify: `../scoc/src/registry.rs`
- Modify: `../scoc/compatibility/parser-matrix.toml`
- Create: `../scoc/tests/parsers/ping.rs`
- Create/update: `../scoc/tests/fixtures/jc-1.26.0/{linux,macos}/ping/*`

**Interfaces:**
- Consumes: core `ScocStreamParser`, numeric helpers, UTF-8/line-buffer helper.
- Produces: canonical `ping` parser with batch `Record`, stream item `Record`, raw + `ignoreErrors` capabilities, upstream mapping `ping` + `ping-s`.

- [ ] **Step 1: Vendor fixtures from both pinned `tests/test_ping.py` and `tests/test_ping_s.py`**

Include IPv4, IPv6, Linux, macOS/BSD, timeout/error lines, summary/footer, duplicates/corruption/errors where fixtures exist, raw outputs, and `_jc_meta`/ignore-exception fixtures where upstream tests cover them.

- [ ] **Step 2: Write batch tests first and make them fail**

Pin batch output as one record containing summary fields plus `responses`, raw numeric strings, empty/malformed safety, and Linux/macOS variants.

- [ ] **Step 3: Implement batch ping parsing and get batch tests green**

Descriptor batch shape: `Record`; raw shape: `Record`; parser version `1.11` for standard mapping.

- [ ] **Step 4: Write streaming tests with deliberately fragmented chunks**

The test must prove line buffering is independent of stdout chunk boundaries:

```rust
#[test]
fn ping_stream_waits_for_complete_line_across_chunks() {
    let mut parser = scoc::stream_parser("ping", &Default::default()).unwrap();
    assert!(parser.push(b"PING 1.1.1.1 (1.1.").unwrap().is_empty());
    assert!(parser.push(b"1.1) 56(84) bytes of data.\n64 bytes fr").unwrap().is_empty());
    let rows = parser.push(b"om 1.1.1.1: icmp_seq=1 ttl=57 time=10.0 ms\n").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["type"], "reply");
}
```

Add a footer/summary test and `ignoreErrors: true` test that emits compatible `_jc_meta` instead of terminating on a malformed record.

- [ ] **Step 5: Implement a per-instance streaming state machine**

Do **not** reproduce JC's class-level mutable `_state`. Each Rust stream parser instance owns its own state so concurrent pings cannot leak state. Buffer incomplete UTF-8 lines/chunks until newline or `finish()`; emit zero or more records per push; preserve JC `ping-s` normalized/raw field names and summary record semantics.

Descriptor requirements:

```text
name: ping
normalized batch: Record
raw batch: Record
stream item: Record
raw: true
streaming: true
ignore_errors: true
platforms: linux, macos
upstream standard: ping 1.11
upstream streaming: ping-s 1.6
```

- [ ] **Step 6: Run batch, streaming, concurrency, and differential checks**

Run:

```bash
cargo test --test ping
.compat-venv/bin/python compatibility/diff.py --parser ping --all-fixtures
.compat-venv/bin/python compatibility/diff.py --parser ping --streaming --all-fixtures
```

Add a Rust test constructing two stream parsers and interleaving their chunks to prove state isolation.

Only then mark `ping` compatible.

---

### Task 11: Spar decoder grammar, AST, lowering, formatter, and static validation

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/token.rs`
- Modify: `src/lexer.rs`
- Modify: `src/ast.rs`
- Modify: `src/shell_lang.rs`
- Modify: `src/formatter.rs`
- Modify: `src/resolver.rs`
- Modify: `src/typechecker.rs`
- Modify: `src/compiled.rs`
- Modify: `src/lowerer.rs`
- Test: existing module tests in the files above

**Interfaces:**
- Consumes: SCOC descriptors/options from Tasks 1–10.
- Produces: `ShellDecodeStage`, `DecoderRef`, `DecoderNamespace`, `NamedDecoderArg`, compiled equivalents, `StreamingMode` selection metadata.

- [ ] **Step 1: Add failing lexer/parser tests before changing syntax**

Cover all of these exact forms:

```spar
df | from df
cat x | from codec::csv
git log | from scoc::git-log
ping 1.1.1.1 | from ping(streaming: false)
df | from df(raw: useRaw)
cmd | from scoc::git-log(raw: flag, mode: choose("a,b"))
```

Also pin errors for duplicate args and malformed names:

```spar
cmd | from df(raw: true, raw: false)
cmd | from ::df
cmd | from scoc::
```

- [ ] **Step 2: Change lexing so `| from ...` is captured as one decoder stage**

Add a dedicated token such as:

```rust
Token::ShellDecoderStage(String)
```

When native-shell lexing sees a Unix pipe followed by the standalone word `from`, keep `|` as the boundary operator, emit the `from` word/keyword as needed by the parser, then capture the decoder text until the next top-level `|>`, Unix `|`, `;`, `&&`, or `||`, respecting nested `()`, `[]`, `{}`, quotes, and escapes. Do not change general Spar identifier lexing.

This specialized lexing is necessary because generic shell words strip quote boundaries and split on whitespace, which would corrupt named Spar expressions.

- [ ] **Step 3: Replace shared decode/encode AST with explicit decode AST**

Use these semantic fields:

```rust
pub enum DecoderNamespace { Codec, Scoc, Custom }

pub struct DecoderRef {
    pub namespace: Option<DecoderNamespace>,
    pub name: String,
    pub span: Span,
}

pub struct NamedDecoderArg {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

pub struct ShellDecodeStage {
    pub decoder: DecoderRef,
    pub args: Vec<NamedDecoderArg>,
    pub span: Span,
}
```

Keep `ShellCodecStage` or rename it to `ShellEncodeStage` only for `to FORMAT`; do not add SCOC behavior to encoders.

- [ ] **Step 4: Parse decoder names manually and option values with the normal Spar expression parser**

Decoder name grammar is exactly:

```text
DecoderName := [A-Za-z][A-Za-z0-9_-]*
DecoderRef  := DecoderName | (codec|scoc|custom) "::" DecoderName
```

For the optional parenthesized argument list, split at top-level commas/colons while honoring nested delimiters/quotes, then call the existing Spar expression parser on each value substring with rebased spans. This permits `choose("a,b")` and list/object expressions without treating the whole decoder as a normal Spar call.

- [ ] **Step 5: Resolve and type-check argument expressions**

`resolver.rs` must walk every `NamedDecoderArg.value` against surrounding locals.

`typechecker.rs` must use the normalized decoder descriptor API (Task 12 introduces the concrete registry; temporarily write the expected call against that interface) to reject:

```text
unknown decoder / namespace
unknown option
duplicate option
wrong obvious option type
raw requested on parser without raw capability
ignoreErrors on parser without ignore-errors capability
streaming:true on parser without streaming capability
streaming:false on a forced `*-s` compatibility alias
```

Map `OptionKind::{Bool,Integer,Float,String,Enum}` to Spar types for static checks; runtime still validates evaluated values.

- [ ] **Step 6: Lower named arg expressions and preserve spans**

Compiled shape:

```rust
pub(crate) struct CompiledDecoderArg {
    pub name: String,
    pub value: CompiledExpression,
    pub span: Span,
}

pub(crate) struct CompiledShellDecodeStage {
    pub namespace: Option<DecoderNamespace>,
    pub name: String,
    pub args: Vec<CompiledDecoderArg>,
    pub span: Span,
}
```

Replace `decoder_format: String` in `CompiledShellMixedPipeline` with this stage.

- [ ] **Step 7: Make the formatter round-trip all decoder syntax**

Tests must assert format idempotence for namespace, kebab-case names, booleans, variables, nested calls, and quoted strings with commas.

Run focused tests:

```bash
cargo test shell_lang
cargo test formatter
cargo test typechecker
cargo test lowerer
```

Expected: PASS before moving to runtime integration.

---

### Task 12: Spar `StructuredInputRegistry`, decoder precedence, and direct SCOC value adapter

**Files:**
- Create: `src/structured_input.rs`
- Modify: `src/structured_codec.rs`
- Modify: `src/lib.rs`
- Modify: `src/typechecker.rs`
- Modify: `src/lowerer.rs`
- Test: `src/structured_input.rs` module tests

**Interfaces:**
- Consumes: native `StructuredFormatRegistry`; SCOC descriptor/parse APIs.
- Produces: `StructuredInputRegistry`, normalized `DecoderDescriptor`, `ResolvedDecoder`, tooling-safe `structured_decoder_descriptors()`, direct JSON→Spar conversion with explicit output shape.

- [ ] **Step 1: Write registry precedence tests first**

Pin these facts:

```rust
#[test]
fn unqualified_csv_prefers_native_codec() { /* from csv -> Codec */ }

#[test]
fn explicit_scoc_namespace_bypasses_codec_precedence() { /* future scoc::csv lookup */ }

#[test]
fn unqualified_df_resolves_to_scoc() { /* Scoc(df) */ }

#[test]
fn custom_namespace_is_reserved_but_not_implemented() { /* clear diagnostic */ }

#[test]
fn ping_s_is_a_spar_compatibility_alias_that_forces_streaming() { /* canonical ping */ }
```

- [ ] **Step 2: Implement normalized decoder metadata**

Use a single tooling/runtime descriptor containing at least:

```rust
pub enum DecoderKind { Codec, Scoc, Custom }

pub struct DecoderDescriptor {
    pub kind: DecoderKind,
    pub name: String,
    pub description: String,
    pub normalized_output: OutputShape,
    pub raw_output: Option<OutputShape>,
    pub stream_item: Option<OutputShape>,
    pub options: Vec<DecoderOptionSpec>,
    pub capabilities: DecoderCapabilities,
    pub platforms: Vec<String>,
    pub compatibility_alias_for: Option<String>,
}
```

Codec descriptors are adapted from existing `StructuredFormatRegistry`; SCOC descriptors are adapted from `scoc::parsers()`; `custom::` has no implementations yet.

- [ ] **Step 3: Implement precedence and explicit namespaces**

Resolution order for unqualified names is exactly codec → SCOC → custom. `codec::` and `scoc::` bypass precedence. Generate `ping-s`-style compatibility aliases only for SCOC descriptors that declare an upstream streaming name; an alias resolves to the canonical SCOC parser plus `StreamingMode::Enabled`.

- [ ] **Step 4: Reuse the existing JSON-value converter without serialization**

Keep `structured_codec::json_value_to_runtime` as an in-memory helper and add an adapter that honors the descriptor shape:

```rust
fn scoc_batch_to_values(value: serde_json::Value, shape: OutputShape) -> Result<Vec<Value>, SparError>
```

Rules:

```text
Scalar/Record -> one Spar value
Table -> input must be JSON array; emit each row as one stream element
List -> one Spar List value (do not infer Table merely from array-of-objects)
```

For raw `env`, descriptor shape is `Record`, so the raw object remains one record instead of being flattened.

- [ ] **Step 5: Export tooling-safe descriptor access**

`src/lib.rs` exposes read-only metadata functions that `spar-ls` can call without reaching into compiler-private modules. Do not export runtime parser objects unnecessarily.

- [ ] **Step 6: Run focused registry/adapter tests**

Run:

```bash
cargo test structured_input
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

---

### Task 13: Runtime batch/streaming execution, option evaluation, and cancellation

**Files:**
- Modify: `src/runtime.rs`
- Modify: `src/compiled.rs` if runtime-only policy fields need adjustment
- Test: `tests/scoc_structured_input.rs` (preferred new focused acceptance file)

**Interfaces:**
- Consumes: compiled decoder stage; `StructuredInputRegistry`; SCOC `parse`/`stream_parser`; existing `spar-process::ProcessStream`.
- Produces: lazy mixed-input runtime supporting codecs and SCOC through one internal decoder enum.

- [ ] **Step 1: Write end-to-end failing tests before changing runtime**

Use deterministic `printf` fixtures rather than relying on the host's `df`/`ps` formatting for most acceptance tests:

```spar
printf 'A=1\nB=2\n' | from env |> take(1) |> to jsonl | cat;
printf 'UUID=x / ext4 defaults 0 0\n' | from fstab |> to jsonl | cat;
printf '<pinned df fixture>' | from df(raw: true) |> take(1) |> to jsonl | cat;
```

Also test:

```text
from csv still uses native codec
from scoc::env selects SCOC
unknown parser diagnostic
raw wrong type at runtime through a variable expression
streaming:true on env errors
```

- [ ] **Step 2: Replace `MixedInputState.parser` with a decoder runtime enum**

Conceptually:

```rust
enum MixedDecoderRuntime {
    Codec(crate::structured_codec::StructuredParser),
    ScocStreaming(Box<dyn scoc::ScocStreamParser>),
    ScocBuffered { bytes: Vec<u8>, parser: &'static str, options: scoc::ParseOptions, shape: scoc::OutputShape },
}
```

Provide internal `push(&mut self, bytes) -> Result<Vec<Value>, SparError>` and `finish(self) -> Result<Vec<Value>, SparError>` methods so `mixed_input_next` no longer cares which decoder implementation is active.

- [ ] **Step 3: Evaluate named decoder arguments before starting the decoder**

For every `CompiledDecoderArg`, evaluate the Spar expression in the current frame/module, convert only supported scalar types into typed decoder option values, and consume `streaming` separately as `StreamingMode::{Auto,Enabled,Disabled}`.

Reject `List`, `Object`, `Table`, `Stream`, resource, and other non-option values with the argument span.

- [ ] **Step 4: Implement automatic streaming policy**

Because the left side of a mixed shell pipeline is a live process stream:

```text
Auto + SCOC supports streaming -> ScocStreaming
Auto + no streaming -> ScocBuffered
Enabled + supports streaming -> ScocStreaming
Enabled + no streaming -> error
Disabled -> ScocBuffered
codec -> retain existing codec streaming/document behavior
```

For a future buffered non-live source, the registry API should still expose policy metadata, but this milestone's shell mixed input is process-backed.

- [ ] **Step 5: Preserve lazy pull/cancel behavior**

Keep the existing `StreamResource::with_cancel` architecture. `mixed_input_cancel` must cancel `spar_process`, discard buffered stdout, retain already-arrived stderr, clear pending values, and drop the SCOC stream parser without calling `finish()` after intentional cancellation.

- [ ] **Step 6: Add a real streaming cancellation acceptance test**

Use a deterministic shell script that prints a valid ping fixture header followed by reply lines forever (or a finite high count with sleeps), then:

```spar
... | from ping |> take(2) |> to jsonl | cat;
```

Assert two records, prompt return within a bounded timeout, and success status despite upstream intentional cancellation.

- [ ] **Step 7: Run acceptance and regression suites**

Run:

```bash
cargo test --test scoc_structured_input
cargo test --test acceptance_plan3
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all previous codec mixed-pipeline tests remain green.

---

### Task 14: Sparsh terminal integration regression coverage

**Files:**
- Prefer Test-only modify: `../sparsh/crates/sparsh-core/src/session.rs`
- No UI product-code changes unless a test proves SCOC output shape exposes a renderer bug.

**Interfaces:**
- Consumes: Spar runtime returning structured previews.
- Produces: regression proof that SCOC-derived tables/records use existing Sparsh structured presentation.

- [ ] **Step 1: Add a session test using fixture-backed `from env`/`from df`**

Example assertion shape:

```rust
let result = session.submit("printf 'A=1\\nB=2\\n' | from env").unwrap();
let ShellResult::Structured(preview) = result else { panic!("expected structured result") };
let spar::Value::Table(table) = preview.value else { panic!("expected table") };
assert_eq!(table.len(), 2);
```

Use the actual existing `ShellResult`/preview variant names in this source snapshot rather than inventing a parallel type.

- [ ] **Step 2: Run Sparsh core structured tests**

Run:

```bash
cd ../sparsh
cargo test -p sparsh-core structured
cargo test -p sparsh-core direct_mixed_pipeline_without_to_returns_a_structured_table
```

Expected: PASS without renderer changes.

- [ ] **Step 3: Only if a real rendering regression appears, fix the existing renderer**

Any fix must remain generic to `Value::Table`/`Value::Object`; do not add SCOC-specific branches to Sparsh UI.

---

### Task 15: `spar-ls` decoder completion, hover, diagnostics, and semantic tokens

**Files:**
- Create: `../spar-ls/src/decoder_intelligence.rs`
- Modify: `../spar-ls/src/main.rs`
- Modify: `../spar-ls/src/completion.rs`
- Modify: `../spar-ls/src/hover.rs`
- Modify: `../spar-ls/src/shell_semantic.rs`
- Modify: `../spar-ls/src/diagnostics.rs` only if compiler diagnostics do not already cover a required case
- Test: `../spar-ls/src/main.rs` test module

**Interfaces:**
- Consumes: Spar's tooling-safe `structured_decoder_descriptors()` and parsed decoder AST.
- Produces: editor intelligence without duplicated SCOC parser tables.

- [ ] **Step 1: Write failing completion tests**

Pin:

```text
`| from ` -> codec + SCOC canonical decoders
`| from scoc::` -> SCOC canonical names + `ping-s` alias
`| from codec::` -> native codec names/aliases
`| from custom::` -> no parser items yet, but namespace recognized
`| from df(` -> `raw:` and `streaming:` where valid
`| from ping(` -> `raw:`, `ignoreErrors:`, `streaming:`
```

Do not offer `ignoreErrors` for `df`; do not offer `raw` for a decoder without that capability.

- [ ] **Step 2: Write failing hover tests**

Hover over `df` must include at least:

```text
SCOC command-output parser
Output: Table
Platforms: Linux, macOS
JC baseline: 1.26.0
Raw mode: yes
Streaming: no
```

Hover over `ping-s` must identify it as a JC compatibility alias and point to canonical `ping`.

- [ ] **Step 3: Implement cursor context detection backed by source spans**

`decoder_intelligence.rs` should identify namespace/name/argument-name positions using the parsed `ShellDecodeStage` when the document parses, with a small tolerant text fallback only for incomplete forms such as `| from scoc::` and `| from ping(`. Do not parse all shell syntax a second time.

- [ ] **Step 4: Implement semantic token classification**

Replace `collect_codec_stage` for decoders with decode-specific token collection. Required semantic distinctions:

```text
from -> keyword
codec/scoc/custom namespace -> namespace/type-like token using an existing supported semantic kind
decoder name -> shell argument or dedicated existing suitable semantic kind
option name -> property
true/false/numbers/strings -> existing expression semantic tokens
```

A recognized SCOC decoder must not be run through executable PATH resolution.

- [ ] **Step 5: Ensure compiler diagnostics surface through LSP**

Tests must cover unknown decoder, unknown namespace, unknown option, wrong option type, duplicate arg, unsupported streaming, and unsupported platform where statically knowable.

- [ ] **Step 6: Run focused and full LSP verification**

Run:

```bash
cd ../spar-ls
cargo test decoder
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
python3 scripts/lsp_smoke.py ./target/release/spar-ls
```

Expected: all pass.

---

### Task 16: Full compatibility gate, installation, and interactive verification

**Files:**
- Modify: `../scoc/compatibility/parser-matrix.toml` only to reflect actually proven results.
- Modify docs only if implementation behavior differs from the approved design in a way that must be documented.

**Interfaces:**
- Consumes: all previous tasks.
- Produces: verified first milestone and an evidence-based status report; no commits.

- [ ] **Step 1: Run SCOC format/lint/test suite**

```bash
cd ../scoc
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 2: Run differential compatibility for all six parsers**

```bash
.compat-venv/bin/python compatibility/diff.py --parser env --all-fixtures
.compat-venv/bin/python compatibility/diff.py --parser fstab --all-fixtures
.compat-venv/bin/python compatibility/diff.py --parser ps --all-fixtures
.compat-venv/bin/python compatibility/diff.py --parser df --all-fixtures
.compat-venv/bin/python compatibility/diff.py --parser ls --all-fixtures
.compat-venv/bin/python compatibility/diff.py --parser ping --all-fixtures
.compat-venv/bin/python compatibility/diff.py --parser ping --streaming --all-fixtures
```

Expected: PASS for every matrix claim. If any fixture differs, leave that parser `in-progress` or mark a fully documented deviation; never promote it just to finish the milestone.

- [ ] **Step 3: Run Spar regression suite**

```bash
cd ../spar
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 4: Run `spar-ls` regression suite**

```bash
cd ../spar-ls
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 5: Run Sparsh regression suite**

```bash
cd ../sparsh
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: PASS.

- [ ] **Step 6: Reinstall all user-facing binaries affected by Spar changes**

```bash
cargo install --path ../spar --force
cargo install --path ../spar-ls --force
cargo install --path ../sparsh --force
```

If the working directory differs, use the equivalent absolute local paths; do not install crates.io copies.

- [ ] **Step 7: Run direct Spar smoke examples**

Use fixture-backed commands so results are deterministic:

```spar
printf 'A=1\nB=2\n' | from env
printf 'UUID=x / ext4 defaults 0 0\n' | from fstab
```

Also run one real local command for ergonomics only (not compatibility evidence), e.g.:

```spar
df | from df
ps aux | from ps |> take(5)
```

Expected: structured output with JC-compatible snake_case keys.

- [ ] **Step 8: Run installed Sparsh PTY smoke verification**

```bash
cd ../sparsh
python3 scripts/verify_structured_output_pty.py "$(command -v sparsh)"
```

Then manually/PTY-submit at least:

```spar
df | from df
ping 1.1.1.1 | from ping |> take(3)
```

For the ping smoke, use a bounded host/network test only if network access is appropriate; the deterministic fixture-based streaming acceptance test remains the correctness evidence.

- [ ] **Step 9: Capture exact final status without committing**

Report:

```text
SCOC cargo fmt: <actual>
SCOC cargo test: <actual counts>
SCOC clippy -D warnings: <actual>
JC differential env/fstab/ps/df/ls/ping/ping-stream: <actual per parser>
Spar tests/clippy: <actual>
spar-ls tests/clippy/LSP smoke: <actual>
Sparsh workspace tests/clippy/PTY smoke: <actual>
Installed binaries: <actual paths/versions>
Parser matrix statuses: <actual>
Unverified items: <explicitly list any>
```

Do not use “complete”, “compatible”, or “verified” for anything that did not actually pass its command/harness.

---

## Plan Self-Review Notes

- **Spec coverage:** Core SCOC API, separate crate boundary, direct `serde_json::Value`, compatibility baseline/matrix, MIT provenance, native + differential tests, six first parsers, native codec precedence, namespaces, named decoder args, automatic streaming, `ping-s` alias, cancellation, Sparsh reuse, and `spar-ls` intelligence all have owning tasks.
- **Deliberate deferrals:** full custom `parser Identifier { ... }` declarations, Windows parity, standalone SCOC CLI, parser long tail, package distribution, and native plugins remain outside this plan exactly as specified.
- **Type consistency refinement:** `ParserOutput` uses `normalized`, optional `raw`, and optional `stream_item` shapes so `env(raw: true)` can be represented without guessing from JSON structure. All later tasks use the same shape model.
- **No placeholder steps:** parser tasks point to the exact pinned upstream parser/tests and require oracle-generated fixtures before matrix promotion. Unknown fixture counts are discovered from those exact pinned test files during the parser task rather than invented here.
- **Environment caveat:** this planning container currently lacks Rust tooling; Task 1 makes toolchain presence a hard execution preflight, and the final report must preserve any resulting verification limitation.
