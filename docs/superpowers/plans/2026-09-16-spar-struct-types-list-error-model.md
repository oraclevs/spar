# Spar Struct/Generic Collections/Error Binding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make canonical `struct`, reusable generic `type`, `List<T>`, compatibility syntax, structural conformance, and usable try/catch error bindings complete before Phase 5.

**Architecture:** Keep one runtime representation: canonical `struct` and legacy sections lower to the existing section/value path; canonical and legacy list syntax lower to the existing `SparType::List`. Reuse existing generic substitution for type-backed field validation. Catch handlers use an optional AST binding and the existing `ConfigValue::Error` value.

**Tech Stack:** Rust 2021, hand-written lexer/parser, public AST, resolver/typechecker, compiled lowerer/runtime, compatibility evaluator, formatter, Cargo tests, `spar` and `spar-ls` binaries.

**Spec:** `docs/superpowers/specs/2026-09-16-spar-v1-phase-3-generics-design.md`, `docs/superpowers/specs/2026-09-16-spar-v1-phase-4-errors-try-catch-design.md`, and approved user checkpoint requirements.

## Global Constraints

- Do not implement `Promise<T>`, `async`, `await`, HTTP async APIs, or Phase 5 work.
- Use TDD: add one focused failing test, observe the failure, implement the minimum, rerun focused and regression tests.
- Preserve old `[Name]` sections, `type [Name]`, `[T]` list types, and `catch error` while canonical formatter output uses new syntax.
- No second section/struct or list runtime implementation.
- No warning subsystem; emit deprecation diagnostics only through existing warning plumbing, if available.
- Resolve generic substitutions during checking/lowering; do not add repeated runtime type-name lookup.
- Commit each logical task with a small Conventional Commit.

### Task 1: Parser and AST canonical forms

**Files:**
- Modify: `src/token.rs`, `src/lexer.rs`, `src/ast.rs`, `src/parser.rs`
- Test: `src/tests/parser_tests.rs`, `tests/phase4_structs.rs`

**Interfaces:** `TopLevelItem::Struct(StructDecl)` is the canonical declaration. `StructDecl` contains name, visibility, optional `SparType` target, fields, and span. `TryStmt.catch_name` becomes `Option<String>`; `catch {}` stores `None`. Legacy section parsing constructs the same struct payload or an explicitly marked compatibility form that lowers immediately.

- [ ] Add failing parser tests for `struct`, `private struct`, `export struct`, colon target, generic `type Name<T>`, `List<T>`, nested applications, ignored catch, and legacy forms.
- [ ] Run `cargo test -p spar parser_tests -- --nocapture`; confirm failures are syntax/AST mismatches.
- [ ] Add `KwStruct`, recognize it in lexer/parser, parse canonical type names without brackets, parse colon type targets, reject struct type parameters, and accept type-field defaults.
- [ ] Parse `List<T>` to `SparType::List`; retain `[T]` as the same semantic node. Preserve legacy section/type parsing compatibility.
- [ ] Parse `catch <identifier>` and `catch {}` while preserving `catch error` as binding `error`.
- [ ] Run focused parser tests, then `cargo fmt --all -- --check`.
- [ ] Commit: `test: specify canonical struct and list syntax`, then `feat: parse canonical structs and catch forms`.

### Task 2: Resolver and symbol model

**Files:**
- Modify: `src/resolver.rs`, `src/loader.rs`, `src/ast.rs`
- Test: `src/tests/resolver_tests.rs` or existing resolver test module, `tests/phase4_structs.rs`

**Interfaces:** Struct names enter the existing section/value namespace and target types use existing named/applied type resolution. Generic type symbols retain parameter lists and visibility. Catch-local binding is created only when `catch_name: Some`.

- [ ] Add failing tests for generic type arity, unknown target type, generic struct rejection, private/exported generic type visibility, catch scope, and catch-without-binding.
- [ ] Run focused resolver tests and record expected failures.
- [ ] Route `StructDecl` through existing section symbol registration and type declaration checks. Reject `struct Name<T>` with the locked diagnostic.
- [ ] Resolve `List<T>` and nested `Applied` types through existing generic machinery. Resolve typed struct targets with import/privacy rules.
- [ ] Add catch scope only for `Some(name)` and reject catch binding leakage.
- [ ] Run resolver and import regression tests. Commit: `feat: resolve canonical structs and generic targets`.

### Task 3: Generic structural type checking

**Files:**
- Modify: `src/typechecker.rs`, `src/ast.rs`
- Test: `src/tests/typechecker_tests.rs`, `tests/phase4_structs.rs`

**Interfaces:** `TypeChecker::check_struct(&StructDecl)` validates inferred structs or an instantiated target. Existing `type_fields_for`, `substitute_type`, and shape checking remain the single conformance path.

- [ ] Add failing tests for self-described structs, one/multiple generic parameters, nested generics, `List<T>` fields, generic type inside list, wrong field, missing required field, default field, optional field, extra field, and imported/private/exported generic types.
- [ ] Run tests and confirm current checker either ignores declarations or reports old arrow/bracket assumptions.
- [ ] Implement type-field defaults as metadata. For typed structs, substitute target fields before checking values; apply explicit values over defaults; reject missing required and extra fields with field-focused diagnostics.
- [ ] Validate nested anonymous objects against expected structural fields. Keep anonymous object literals lightweight.
- [ ] Validate untyped struct fields from annotations/inferred values and derive internal shape without adding runtime tags.
- [ ] Run all typechecker tests plus package metadata tests. Commit: `feat: type-check generic struct conformance`.

### Task 4: Runtime/lowering unification and defaults

**Files:**
- Modify: `src/lowerer.rs`, `src/runtime.rs`, `src/evaluator.rs`, `src/compiled.rs`, `src/emit.rs`
- Test: `src/tests/runtime_tests.rs`, `tests/engine_runtime.rs`, `tests/compiled_program.rs`

**Interfaces:** Canonical and legacy declarations evaluate through existing `ConfigValue::Section` and section caches. Defaults are materialized once during section evaluation/lowering-compatible preparation. `ConfigValue::List` remains intrinsic runtime storage.

- [ ] Add failing runtime tests for canonical/legacy equivalence, generic typed struct execution, defaults, field access, `List<T>`, and nested object validation.
- [ ] Run focused runtime tests and confirm failures.
- [ ] Make compiled and compatibility evaluators consume the unified declaration payload. Preserve field lookup and cache behavior.
- [ ] Materialize type defaults before explicit struct fields, without dynamic generic lookup on field access.
- [ ] Ensure list operations continue using concrete `ConfigValue::List` without per-element generic dispatch.
- [ ] Run runtime, compiled, serde, package, and compatibility tests. Commit: `feat: execute canonical structs and defaults`.

### Task 5: Formatter and compatibility migration

**Files:**
- Modify: `src/formatter.rs`, `src/typechecker.rs`, `src/error.rs`, `src/renderer.rs`
- Test: `src/tests/formatter_tests.rs`, `tests/v1_compatibility.rs`, `tests/conformance_corpus.rs`

**Interfaces:** Formatter emits `type Name<T>`, `struct Name: Type<...>`, `List<T>`, and optional catch binding. Legacy AST/input remains accepted and formats canonically.

- [ ] Add failing round-trip tests for every canonical example and legacy section/list input.
- [ ] Run formatter tests and capture non-idempotent or parse failures.
- [ ] Emit canonical syntax for new nodes; never emit bracket list/type syntax for canonical AST. Keep task brackets unchanged because task syntax is separate.
- [ ] Add deprecation warning only if existing diagnostic collection supports warnings; otherwise document compatibility without inventing warning infrastructure.
- [ ] Update parser/type display diagnostics from `[T]` to `List<T>` for canonical semantic types. Commit: `feat: format canonical struct and list syntax`.

### Task 6: Catch error binding semantics

**Files:**
- Modify: `src/parser.rs`, `src/typechecker.rs`, `src/lowerer.rs`, `src/runtime.rs`, `src/evaluator.rs`, `src/formatter.rs`
- Test: `src/tests/parser_tests.rs`, `src/tests/typechecker_tests.rs`, `src/tests/runtime_tests.rs`, `tests/try_catch.rs`

**Interfaces:** `catch_name: None` means ignored error. `Some(name)` creates a local `SparType::Error`. Field access `message` and `kind` type-checks as `str`. Runtime caught value is the same logical error produced at the failure boundary.

- [ ] Add failing tests for `catch err`, `catch error`, `catch {}`, `err.message`, `err.kind`, invalid error fields, and error identity/message/kind preservation.
- [ ] Run focused tests and confirm ignored catch currently cannot parse or error fields are unsupported.
- [ ] Type-check only bound catches; expose fixed error field types. Preserve optional `code`/cause behavior already present where compatible.
- [ ] Lower optional catch slots safely. Convert runtime failures into one `ConfigValue::Error` and write it to the catch slot without replacing message/kind.
- [ ] Add fixture that triggers recoverable failure, prints both fields, and add no-binding fixture. Commit: `feat: expose bound Spar errors in catch`.

### Task 7: Integration fixtures and language surface migration

**Files:**
- Modify: `tests/fixtures/**/*.spar`, `tests/conformance/**/*.spar`, `examples/**/*.spar`, `README.md`
- Create: `tests/fixtures/phase4/struct_generics.spar`, `tests/fixtures/phase4/try_catch.spar`
- Test: `tests/conformance_corpus.rs`, `tests/script_cli.rs`, `tests/engine_runtime.rs`

- [ ] Add real `.spar` fixtures for Pair, nested Container/List, defaults, missing/extra/type errors, generic imports, private/exported types, and try/catch output.
- [ ] Run each fixture through check and execute commands; confirm failures before code or fixture migration.
- [ ] Update in-scope Spar files to canonical `struct`, `type Name`, and `List<T>` syntax. Leave task names in existing `task [Name]` syntax.
- [ ] Update README and examples to document schema deferral and compatibility syntax.
- [ ] Commit: `docs: migrate Spar examples to canonical struct syntax`.

### Task 8: Full verification, docs, install, and checkpoint

**Files:**
- Modify: `2026-09-16-spar-v1-runtime-stdlib-architecture.md`, `docs/superpowers/specs/2026-09-16-spar-v1-phase-3-generics-design.md`, `docs/superpowers/specs/2026-09-16-spar-v1-phase-4-errors-try-catch-design.md`
- Test: all relevant Cargo crates and fixtures

- [ ] Add architecture decisions: struct/type split, concrete non-generic structs, anonymous objects, deferred schemas, canonical `List<T>`, legacy migrations, catch error fields.
- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] Run `cargo test --workspace --all-targets --all-features` and focused language/integration tests. Record exact counts/results.
- [ ] Run relevant `spar-ls` tests/build and formatter round trips.
- [ ] Inspect `git diff --check`, `git status`, and ensure no Promise/async/await implementation started.
- [ ] Install both binaries with `cargo install --path spar --force` and `cargo install --path spar-ls --force`; verify `spar --version` and `spar-ls --version` or equivalent help output.
- [ ] Commit: `test: verify Phase 4 struct and error checkpoint`.

## Self-review

- Canonical syntax, legacy section/list compatibility, generic substitution, defaults, nested objects, optional fields, visibility/imports, diagnostics, formatting, runtime equivalence, catch binding, ignored catch, and error field access each have an explicit task.
- Phase 5 is explicitly excluded.
- No task introduces a second runtime representation or hot-path generic lookup.
- Plan contains no unresolved placeholders.
