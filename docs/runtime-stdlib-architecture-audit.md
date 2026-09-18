# Spar v1 Runtime and Standard Library Entry Audit

Date: 2026-09-16

This audit records the implementation boundary before Spar v1 runtime work starts. It supports the approved architecture in `2026-09-16-spar-v1-runtime-stdlib-architecture.md`. Source code remains authoritative when this document becomes stale.

## Verified baseline

The isolated feature worktree builds successfully. `cargo test` passes 969 tests: 815 library tests, 41 binary tests, and 113 integration tests. No v1 grammar or runtime behavior changed during this audit.

The current pipeline is:

```text
source
  -> Lexer
  -> Parser / Program
  -> import expansion and collection
  -> Resolver / SymbolTable
  -> TypeChecker
  -> Evaluator / EvalResult
  -> emit, execute main, or task lowering
```

`Compiler::compile` in `src/compiler.rs` owns this sequence. It can stop after type checking through `CompileOptions::evaluate`, but successful compilation has no separate lowered execution artifact.

## Existing boundaries to preserve

### Language and modes

- `spar run` and task shorthand select task execution.
- `spar exec` selects program execution and invokes only entry-module `main`.
- `spar repl` uses one `Session`.
- `Engine::check_source` performs semantic validation without evaluation.
- `Engine::emit_source` evaluates configuration without calling `main`.
- `Engine::execute_source` evaluates initialization, calls `main`, and maps `int`, `void`, or `shell` to a process status.
- Configuration declarations, tasks, `shell { ... }`, `command ...`, and `exec shell { ... }` use the same frontend.

These contracts are frozen in `tests/v1_compatibility.rs` before runtime changes.

### Frontend

`src/ast.rs` exposes one `Program` containing ordered top-level items. Function statements already support local variables, assignment, expression statements, `if`, `for`, `break`, `continue`, and `return`. `SparType` currently contains `str`, `int`, `float`, `bool`, `section`, `void`, `shell`, lists, and named types.

No generic parameter representation exists. No `async`, `await`, `try`, or `catch` token or AST node exists. `bytes`, `path`, `datetime`, `duration`, `error`, `Promise<T>`, `Stream<T>`, and `Process` are not runtime types.

### Loading and packages

`src/loader.rs` expands selective imports, collects aliased imports, validates schemas, and retains source-local spans. `package::ModuleLocator` resolves bare package aliases through the package lock/store. The loader has no trust context and no reserved `std/*`, `shell/*`, or `@native/*` resolution path.

### Type checking

`src/resolver.rs` builds a `SymbolTable`; `src/typechecker.rs` checks the AST against it. Host signatures are copied into the symbol table so host calls use normal named-call checking. The type system has named object types and homogeneous lists, but no type variables, generic applications, promise typing, native execution kind, or checked operation IDs.

### Evaluation and values

`src/evaluator.rs` interprets the checked AST directly. `ConfigValue` is the only runtime value representation and contains strings, integers, floats, booleans, lists, sections, and shell plans. Function locals use `HashMap<String, ConfigValue>` and AST statements are cloned during calls. Arithmetic dispatches through AST operators and runtime value matches.

There is no lowerer, `CompiledProgram`, `CompiledModule`, indexed local frame, resolved function/native ID, specialized operation instruction, central opaque-resource table, or cancellation state.

### Embedding and native calls

`src/host.rs` provides synchronous `HostRegistry` and `HostFunction`. Each function declares a namespace, name, parameter list, return type, and callback. Callbacks receive `&[ConfigValue]` and return `Result<ConfigValue, String>`.

This is a useful embedding seam, but it is not the approved native capability ABI. It has no stable native ID, async handler, Spar error value, `RuntimeContext`, trust policy, resource handles, capability availability, or public-module/private-provider separation.

### Sessions

`src/session.rs` preserves successful REPL fragments by recompiling accumulated source. `EffectLedger` prevents replayed process effects from running twice. Session state is not a persistent compiled module graph or indexed runtime frame yet.

## Architecture gap map

| Approved area | Current state | Required phase |
| --- | --- | --- |
| `CompiledProgram` execution boundary | Missing; `Compilation` stores AST, symbols, imports, evaluation, and tasks | 2 |
| Indexed locals and function IDs | String-keyed local maps and AST lookup | 2 |
| Typed primitive operations | AST operator plus runtime value matching | 2 |
| Parametric generics | Missing | 3 |
| Spar `error` and `try`/`catch` | Runtime failures become `SparError`; no language value/control flow | 4 |
| `Promise<T>` and async/await | Missing | 5 |
| Typed `NativeRegistry` and `RuntimeContext` | Synchronous `HostRegistry` only | 6 |
| Trusted `@native/*` | Missing loader trust contexts | 7 |
| Bundled lazy `std/*` | Missing | 8-9 |
| Opaque resources and streaming | Missing | 10 |
| User compilation cache | Missing | 11 |
| Performance benchmarks | No v1 benchmark suite | 12 |
| `shell/*` host modules | Missing | 13 |

## Phase 2 entry seam

Phase 2 should split semantic compilation from execution without changing public mode behavior:

```text
Compiler frontend
  -> checked Program + SymbolTable + imports
  -> Lowerer
  -> CompiledProgram
  -> Runtime
```

The first `CompiledProgram` may retain checked AST nodes internally while introducing stable module/function identities and one execution entry point. That permits incremental lowering without rewriting every expression in one change. Public `Compiler`, `Compilation`, `Engine`, `Evaluator`, `from_str`, and `from_eval` must keep compatibility adapters until callers migrate.

Phase 2 must first characterize these mutations:

- `check` accidentally evaluating source;
- `emit` accidentally invoking `main`;
- imported-module `main` being invoked;
- entry initialization executing more than once;
- integer, void, or shell `main` mapping to the wrong status;
- public/private configuration visibility changing;
- task named `exec` becoming unreachable;
- shell-plan values executing before explicit execution.

## Risks

- `Compilation` currently combines frontend, evaluation, and task products. Replacing it directly would break the CLI, deserialization, tests, and embedders.
- Import expansion splices AST items while aliased imports retain separate programs. Lowering needs stable module identity before cross-module IDs can be reliable.
- Runtime values and emitted configuration share `ConfigValue`. Opaque resources and promises must never leak into emit or serde deserialization.
- Host calls are already public embedding API. The native registry should grow beside it or provide an adapter; a rename-only migration would break users without adding trust or context.
- `Session` recompilation is correct but not the target performance model. Optimizing it before `CompiledProgram` exists would duplicate work.

## Phase gate

Phase 1 is complete when this audit and black-box compatibility tests pass with unchanged production code. Phase 2 may then introduce the lowering/runtime boundary behind those tests.
