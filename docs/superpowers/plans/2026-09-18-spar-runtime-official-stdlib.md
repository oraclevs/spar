# Spar Runtime and Official Stdlib Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish Spar v1's executable runtime foundation, bundled official `std` package, package/import resolver, shell/runtime integration, and matching `spar-ls` behavior, then package the changes for local Rust verification.

**Architecture:** Evolve the existing `CompiledProgram` interpreter in `spar` rather than introducing a second executor or runtime crate. Introduce explicit runtime `Value`, `RuntimeContext`, typed `NativeRegistry`, resource ownership, trusted `@native/*` providers, and a reserved bundled `std` package implemented primarily in Spar. `spar-ls` consumes compiler resolution APIs; Spar remains independent from Sparsh.

**Tech Stack:** Rust 2021, existing Spar compiler/runtime, `spar-command`, `spar-process`, `serde`/`serde_json`, package lock/store infrastructure, LSP server in `spar-ls`.

**Spec:** `spar/docs/superpowers/specs/2026-09-18-spar-runtime-official-stdlib-design.md`

## Global Constraints

- `CompiledProgram` remains the v1 execution boundary; do not add a second interpreter, VM, JIT, or `spar-runtime` crate.
- `import pkg` is the only package-namespace syntax; normal `import` never silently falls back to a package.
- `.spar` suffixes are optional for source imports and package submodules.
- `std` is a reserved bundled package, available offline without a manifest dependency or lockfile edge.
- Public stdlib APIs are written in Spar where possible; Rust is limited to private trusted `@native/*` host/runtime capability providers.
- User code cannot import `@native/*`.
- Spar must not depend on Sparsh.
- `asPartOf` stays removed; only a migration diagnostic is retained.
- Native shell execution remains structural through `spar-command` + `spar-process`; do not route native Spar shell through `/bin/sh -c`.
- Existing task/config/package behavior must remain compatible unless explicitly changed by the approved design.
- TDD: every behavior change starts with a regression test; compilation verification is deferred to the user's Rust environment if this sandbox lacks `cargo`/`rustc`.

---

### Task 1: Finish and freeze shell/import repairs

**Files:**
- Modify: `spar/src/lexer.rs`
- Modify: `spar/src/parser.rs`
- Modify: `spar/src/runtime.rs`
- Modify: `spar/src/engine.rs`
- Modify: `spar/src/loader.rs`
- Modify: `spar/src/package/locator.rs`
- Modify: `spar-process/src/exec.rs`
- Test: `spar/src/tests/parser_tests.rs`
- Test: `spar/tests/engine_runtime.rs`
- Test: `spar/tests/package_imports.rs`

**Interfaces:**
- Consumes: existing mixed `shell {}` AST/lowering and `ModuleLocator`.
- Produces: stable multiline shell parsing, logical `$()` capture, runtime cwd, source-aware execution errors, explicit local-vs-package import resolution, scoped transitive package locators.

- [ ] Add/retain failing parser/runtime tests for multiline arrays, multiline named function calls, multiline native commands, explicit `\\` continuation, `$()` with `&&`/`||`, background rejection in `$()`, native `cd`, cwd-aware redirects, and pure-shell source spans.
- [ ] Verify the tests fail against the pre-fix behavior (or preserve the already-recorded regressions in the uploaded working copy).
- [ ] Finish the statement-aware shell body normalizer and command-substitution chain execution without changing command semantics.
- [ ] Finish native `cd` as runtime cwd state and ensure `spar-process` resolves relative redirect files against `CommandPlan.cwd`.
- [ ] Remove any remaining `Span::dummy()` fallback used for returned `main() -> shell` execution errors when a real source span exists.
- [ ] Keep normal imports local-only and package imports explicit; preserve scoped locators on recursively loaded modules.
- [ ] Run `git diff --check` and, where available, the targeted Rust tests.

### Task 2: Finalize package namespace and `std` reservation

**Files:**
- Modify: `spar/src/package/locator.rs`
- Modify: `spar/src/package/resolver.rs`
- Modify: `spar/src/package/manifest.rs`
- Modify: `spar/src/loader.rs`
- Modify: `spar/src/compiler.rs`
- Test: `spar/tests/package_resolution.rs`
- Test: `spar/tests/package_imports.rs`

**Interfaces:**
- Consumes: `ModuleLocator::resolve_import_scoped`, package lock graph, `ImportDecl.package`.
- Produces: `std` special-case resolver that behaves as a package but requires no dependency edge; third-party packages continue through scoped lock edges.

- [ ] Add tests proving `import pkg { println } from "std"` resolves without `Dependencies.std` or a lock entry, while a project-declared dependency named `std` is rejected.
- [ ] Add tests proving `std/fs` resolves relative to the bundled std entry directory and cannot escape with `..` or absolute paths.
- [ ] Add tests proving dependency A's `import pkg ...` resolves against A's own dependency edges, not the application root.
- [ ] Add a `BundledPackage`/equivalent branch in module resolution for the reserved alias `std` while leaving ordinary dependency resolution unchanged.
- [ ] Ensure `std` cannot be overridden by manifest aliases or package store contents.

### Task 3: Introduce central runtime `Value`

**Files:**
- Create: `spar/src/runtime/value.rs`
- Create: `spar/src/runtime/frame.rs`
- Modify: `spar/src/runtime.rs` (transition module root)
- Modify: `spar/src/lib.rs`
- Modify: `spar/src/emit.rs`
- Modify: `spar/src/de.rs`
- Test: `spar/tests/engine_runtime.rs`

**Interfaces:**
- Consumes: existing `ConfigValue`, `PromiseHandle`, shell values, compiled local slots.
- Produces: `runtime::Value` and conversion helpers `Value::from_config`, `Value::try_into_config`; frame stores runtime values rather than config-only values.

- [ ] Add tests for `Value::{Void,Int,Float,Bool,String,Bytes,List,Object,Error,Shell,Promise,Resource}` equality/debug/conversion behavior.
- [ ] Implement the runtime `Value` enum without deleting `ConfigValue`; configuration APIs continue to expose `ConfigValue` through explicit conversion.
- [ ] Move `Frame` into `runtime/frame.rs` and store `Option<Value>` slots.
- [ ] Convert compiled execution arithmetic/control flow to use `Value` internally while preserving existing public execute results through conversion at the engine boundary.
- [ ] Add explicit errors when non-config runtime values are passed to JSON/TOML/YAML/config deserialization.

### Task 4: Add `RuntimeContext` and resource ownership

**Files:**
- Create: `spar/src/runtime/context.rs`
- Create: `spar/src/runtime/resource.rs`
- Modify: `spar/src/runtime/mod.rs`
- Modify: `spar/src/engine.rs`
- Modify: `spar/src/session.rs`
- Test: `spar/tests/runtime_context.rs`

**Interfaces:**
- Produces: `RuntimeContext`, `ResourceId`, `ResourceTable`, context builders for CLI/tests/embedding.

- [ ] Add tests creating two contexts with different cwd/env/args and prove they do not mutate process-global cwd/environment.
- [ ] Add tests for resource insertion, typed lookup, invalid/stale IDs, cleanup hooks, and shutdown cleanup.
- [ ] Implement `RuntimeContext` with cwd, args, environment overlay, stdin/stdout/stderr handles, resources, cancellation flag, scheduler/native registry references.
- [ ] Route native shell cwd/environment and owned background process cleanup through the runtime context.

### Task 5: Replace host callbacks with typed `NativeRegistry`

**Files:**
- Create: `spar/src/runtime/native.rs`
- Modify: `spar/src/host.rs`
- Modify: `spar/src/compiler.rs`
- Modify: `spar/src/compiled.rs`
- Modify: `spar/src/lowerer.rs`
- Modify: `spar/src/resolver.rs`
- Modify: `spar/src/typechecker.rs`
- Test: `spar/tests/native_registry.rs`
- Test: `spar/tests/compiled_program.rs`

**Interfaces:**
- Produces: `NativeFunctionId`, `NativeExecutionKind::{Sync,Async}`, `NativeFunction`, `NativeRegistry`, compatibility adapter for public `HostRegistry` embedding API.

- [ ] Add tests for duplicate registration, resolved numeric IDs, named argument reordering, sync dispatch, async dispatch metadata, missing capability, and native error translation.
- [ ] Implement indexed native registration while preserving `HostRegistry` as a compatibility façade over the new registry.
- [ ] Extend lowering so known native calls compile to `NativeFunctionId` rather than repeated `(namespace,name)` lookup at runtime.
- [ ] Dispatch native calls with `&mut RuntimeContext` and runtime `Value` arguments.

### Task 6: Add trusted `@native/*` module boundary

**Files:**
- Create: `spar/src/stdlib/mod.rs`
- Create: `spar/src/stdlib/native.rs`
- Modify: `spar/src/loader.rs`
- Modify: `spar/src/compiler.rs`
- Modify: `spar/src/resolver.rs`
- Test: `spar/tests/native_imports.rs`

**Interfaces:**
- Produces: `LoadTrust::{UserCode,StandardLibrary,HostLibrary}` and synthetic/private native module signatures.

- [ ] Add a test where user code imports `@native/fs` and receives a source-aware trust diagnostic.
- [ ] Add a test where the same import inside bundled `std` sources resolves successfully.
- [ ] Track trust level through recursive import loading and package resolution.
- [ ] Register only private native modules needed by the stdlib and expose their signatures to resolver/typechecker without making them public package modules.

### Task 7: Create bundled std package and core pure-Spar modules

**Files:**
- Create: `spar/stdlib/spar.package.spar`
- Create: `spar/stdlib/src/lib.spar`
- Create: `spar/stdlib/src/io.spar`
- Create: `spar/stdlib/src/path.spar`
- Create: `spar/stdlib/src/text.spar`
- Create: `spar/stdlib/src/math.spar`
- Create: `spar/stdlib/src/json.spar`
- Modify: `spar/src/stdlib/mod.rs`
- Test: `spar/tests/stdlib_core.rs`

**Interfaces:**
- Produces public modules `std`, `std/io`, `std/path`, `std/text`, `std/math`, `std/json`.

- [ ] Write package-resolution tests for root and submodule imports without manifest entries.
- [ ] Implement `std/io` public wrappers over `@native/io` and export canonical `print`/`println`.
- [ ] Implement path/text/math helpers in Spar where the language supports them; use native helpers only for primitives not expressible efficiently/correctly in v1.
- [ ] Implement JSON parse/stringify through `@native/json` with runtime-value conversion.
- [ ] Add focused tests for every exported v1 function introduced in these modules.

### Task 8: Implement filesystem/environment/time/terminal/random std modules

**Files:**
- Create: `spar/stdlib/src/fs.spar`
- Create: `spar/stdlib/src/env.spar`
- Create: `spar/stdlib/src/time.spar`
- Create: `spar/stdlib/src/terminal.spar`
- Create: `spar/stdlib/src/random.spar`
- Create: `spar/src/stdlib/fs.rs`
- Create: `spar/src/stdlib/env.rs`
- Create: `spar/src/stdlib/time.rs`
- Create: `spar/src/stdlib/terminal.rs`
- Create: `spar/src/stdlib/random.rs`
- Test: `spar/tests/stdlib_os.rs`

**Interfaces:**
- Produces private native modules `@native/fs`, `@native/env`, `@native/time`, `@native/terminal`, `@native/random` and corresponding public `std/*` wrappers.

- [ ] Add tempfile-based tests for text/byte read/write/append, create/remove/copy/move, metadata and existence.
- [ ] Add isolated context environment tests for get/has/set/unset/enumerate.
- [ ] Add deterministic duration/elapsed tests using injectable clock hooks where necessary.
- [ ] Add terminal capability tests that do not require an interactive TTY by injecting context I/O capability metadata.
- [ ] Add deterministic random tests through injectable entropy for tests and OS entropy for production secure bytes.
- [ ] Implement the native providers and Spar wrappers.

### Task 9: Implement process, async, regex and HTTP std modules

**Files:**
- Create: `spar/stdlib/src/process.spar`
- Create: `spar/stdlib/src/async.spar`
- Create: `spar/stdlib/src/regex.spar`
- Create: `spar/stdlib/src/http.spar`
- Create: `spar/src/stdlib/process.rs`
- Create: `spar/src/stdlib/regex.rs`
- Create: `spar/src/stdlib/http.rs`
- Modify: `spar/src/async_runtime.rs`
- Modify: `spar/Cargo.toml`
- Test: `spar/tests/stdlib_process.rs`
- Test: `spar/tests/stdlib_async.rs`
- Test: `spar/tests/stdlib_regex.rs`
- Test: `spar/tests/stdlib_http.rs`

**Interfaces:**
- `std/process` delegates process execution to existing structural process infrastructure instead of `/bin/sh`.
- `std/async` composes runtime promises/tasks.
- `std/regex` and `std/http` use focused Rust providers behind private native modules.

- [ ] Add typed process-result tests for nonzero exits, stdout/stderr bytes, cwd/env and spawn failures.
- [ ] Add `all`, `race`, `timeout` tests covering success, failure and cancellation.
- [ ] Add regex match/find/find-all/replace/split tests.
- [ ] Add HTTP tests against a local loopback test server only; no public network dependency.
- [ ] Add minimal well-supported Rust crates only where standard Rust cannot provide the required capability (`regex`; HTTP client chosen with a small synchronous/async surface compatible with current runtime constraints).
- [ ] Implement Spar wrappers over those native primitives.

### Task 10: Wire the prelude and executable entry semantics

**Files:**
- Create: `spar/src/stdlib/prelude.rs`
- Modify: `spar/src/compiler.rs`
- Modify: `spar/src/resolver.rs`
- Modify: `spar/src/typechecker.rs`
- Modify: `spar/src/runtime/mod.rs`
- Test: `spar/tests/prelude.rs`
- Test: `spar/tests/script_cli.rs`

**Interfaces:**
- Produces implicit bindings for canonical std exports: `print`, `println`, `len`, `assert`, `panic`.

- [ ] Add tests proving prelude functions work with no import and refer to the same signatures/implementations as their std exports.
- [ ] Ensure a user declaration with a reserved prelude name receives a deterministic diagnostic rather than silently shadowing runtime magic.
- [ ] Map `main() -> int` to CLI exit semantics and `main() -> void` to zero; keep `main() -> shell` runtime-owned execution behavior.

### Task 11: Finish `spar-ls` against compiler resolution APIs

**Files:**
- Modify: `spar-ls/src/document.rs`
- Modify: `spar-ls/src/backend.rs`
- Modify: `spar-ls/src/definition.rs`
- Modify: `spar-ls/src/references.rs`
- Modify: `spar-ls/src/completion.rs`
- Modify: `spar-ls/src/hover.rs`
- Modify: `spar-ls/src/diagnostics.rs`
- Modify: `spar-ls/src/semantic_tokens.rs`
- Test: add/extend `spar-ls` unit/integration tests beside these modules.

**Interfaces:**
- Consumes: compiler `ResolvedImportSource`/package locator and std bundled module resolver.
- Produces: diagnostics, completion, hover, definition, references and semantic tokens for local/std/third-party imports without duplicating path logic.

- [ ] Add tests for extensionless local import navigation.
- [ ] Add tests for `import pkg` syntax/semantic tokens and std root/submodule go-to-definition.
- [ ] Add tests for an ordinary package dependency and transitive package reference.
- [ ] Add completion/hover tests for imported std symbols.
- [ ] Remove any remaining manual `base_dir.join(import_path)` logic and consume compiler-resolved paths instead.

### Task 12: Stdlib source loading, cache/precompile hook and end-to-end verification bundle

**Files:**
- Create: `spar/src/stdlib/bundle.rs`
- Modify: `spar/src/stdlib/mod.rs`
- Modify: `spar/src/main.rs`
- Create: `spar/tests/fixtures/stdlib_smoke/main.spar`
- Create: `PATCH_MANIFEST.md` at bundle root during packaging
- Create: `VERIFY.md` at bundle root during packaging

**Interfaces:**
- Produces: lazy std source loader with a versioned compiled-cache hook; final patch archive.

- [ ] Add a smoke program importing `std/fs` and `std/path`, writing/reading a file, printing through the prelude, branching on content, and returning an exit code.
- [ ] Ensure unused std modules are not parsed/initialized when a script imports only one module.
- [ ] Add a versioned std compilation-cache interface; source fallback remains authoritative for development/diagnostics.
- [ ] Run every available test/static command: `git diff --check`; if Rust tools exist, `cargo fmt --check`, `cargo test`, `cargo clippy --all-targets -- -D warnings` for `spar`, `spar-process`, `spar-command`, and `spar-ls`.
- [ ] Record commands and exact unavailable-tool limitations in `VERIFY.md` rather than claiming unrun tests passed.
- [ ] Create a patch ZIP containing only modified/new files at repository-relative paths plus `PATCH_MANIFEST.md` and `VERIFY.md` so extraction with overwrite applies the patch safely.
