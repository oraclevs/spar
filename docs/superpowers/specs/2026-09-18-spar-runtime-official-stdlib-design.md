# Spar Runtime and Official Standard Library Design

**Status:** Approved in chat; awaiting written-spec review
**Date:** 2026-09-18
**Scope:** Spar v1 runtime foundation, official `std` package, private native capability layer, package resolution, LSP integration, and the boundary with future Sparsh work.

## 1. Goals

Spar v1 must execute ordinary typed Spar programs without relying on a foreign shell, while preserving `shell {}` as one runtime capability for native process orchestration.

The implementation must:

- evolve the existing `CompiledProgram` interpreter instead of creating a second executor;
- make runtime state explicit through `RuntimeContext`;
- introduce a central runtime `Value` model able to represent non-config runtime values;
- replace the current string-keyed host callback model with a typed `NativeRegistry`;
- ship the official standard library as a real, bundled Spar package named `std`;
- reserve `import pkg` for package-namespace imports, including the bundled `std` package;
- implement public stdlib APIs primarily in Spar, backed by a small private Rust-native capability layer;
- keep Spar independent from Sparsh;
- finish compiler/package/LSP behavior before deeper Sparsh development resumes.

## 2. Product and Dependency Boundary

Spar owns the language, compiler pipeline, optimized runtime, package manager, official standard library, shell-plan language feature, native process integration, and embedding API.

Sparsh remains a separate interactive shell host. It may consume Spar, `spar-command`, `spar-process`, the official stdlib, and future public shell packages, but Spar must never depend on Sparsh.

```text
spar-command
     ^
spar-process
     ^
spar runtime + compiler
     ^
std package / user packages
     ^
user Spar programs

sparsh -> spar + spar-command + spar-process
spar   -X-> sparsh
```

Interactive-only concerns remain in Sparsh: history, prompt, aliases, persistent job control, `fg`/`bg`/`jobs`, completion, and interactive session UX.

## 3. Runtime Architecture

Spar already has an initial runtime in `spar/src/runtime.rs`. v1 extends and modularizes that runtime rather than introducing a competing interpreter.

The execution pipeline remains:

```text
Source
  -> Lexer
  -> Parser
  -> Resolver
  -> Type Checker
  -> Lowerer
  -> CompiledProgram / CompiledModule
  -> Spar Runtime
  -> NativeRegistry when host/OS capability is required
```

`CompiledProgram` is the execution boundary for v1. Hot-path execution must use resolved IDs/slots rather than repeated string lookup where practical.

The current single runtime file should be split by responsibility as implementation pressure requires, targeting modules equivalent to:

```text
spar/src/runtime/
├── mod.rs
├── execute.rs
├── context.rs
├── value.rs
├── frame.rs
├── native.rs
├── resource.rs
├── async_runtime.rs
├── module.rs
└── error.rs
```

Exact filenames may follow existing project conventions, but responsibilities must remain separated.

## 4. RuntimeContext

All runtime-native operations receive explicit runtime/session state instead of relying on mutable global process state.

`RuntimeContext` owns or references at least:

```text
RuntimeContext
- script arguments
- current working directory
- environment view
- stdin
- stdout
- stderr
- scheduler / async executor
- resource table
- native registry / capabilities
- cancellation state
- host services
```

This context is shared by ordinary Spar execution, native stdlib functions, native shell execution, tests, embedding, and later Sparsh sessions.

A `cd` performed by native Spar shell execution updates runtime/session cwd. Child processes and relative redirections inherit that cwd. The implementation must not use process-global `std::env::set_current_dir()` as the language's session state.

## 5. Runtime Value Model

The runtime must stop using configuration-oriented values as the only representation for executable-language state.

A central `Value` representation must accommodate at least:

```text
Void
Int
Float
Bool
String
Bytes
List
Object / Struct
Error
Shell
Promise
Stream
Process / child-process handle
NativeHandle / ResourceId
```

Additional strongly typed runtime wrappers such as path/time values may be added where the type system already exposes them.

Configuration serialization remains a conversion target:

```text
Runtime Value -> ConfigValue / JSON / TOML / YAML
```

not the representation that constrains the runtime.

## 6. Native Capability ABI

The existing `HostRegistry` evolves into a typed internal `NativeRegistry`.

A native function has stable metadata equivalent to:

```text
NativeFunction
- NativeFunctionId
- module
- name
- parameter types
- return type
- execution kind: Sync | Async
- handler
```

Compilation/lowering resolves native calls to `NativeFunctionId` so normal execution does not repeatedly look functions up by string.

Conceptual handlers are:

```text
sync:  (args, RuntimeContext) -> Value | SparError
async: (args, RuntimeContext) -> Future<Value | SparError>
```

Rust futures are never exposed directly to Spar; async native operations surface through Spar's typed `Promise<T>` model.

## 7. Private Native Namespace and Trust

Low-level Rust providers are exposed internally under the reserved `@native/*` namespace, including modules such as:

```text
@native/io
@native/fs
@native/env
@native/process
@native/time
@native/terminal
@native/http
@native/regex
@native/random
```

The loader tracks at least these trust contexts:

```text
UserCode
StandardLibrary
HostLibrary
```

Only trusted standard-library or host-library code may resolve `@native/*`. Direct user imports from `@native/*` produce a source-aware diagnostic.

## 8. Official `std` Package

The official standard library is a real Spar package named `std`.

It ships with the Spar distribution, is version-matched to the runtime, is available offline, is reserved from third-party use, and does not require the user to declare a `std` dependency in their project manifest or lockfile.

The std package itself has normal package metadata so it is exercised through the same package/module conventions as other Spar libraries.

Target layout:

```text
spar/stdlib/
├── spar.package.spar
└── src/
    ├── lib.spar
    ├── io.spar
    ├── fs.spar
    ├── path.spar
    ├── env.spar
    ├── process.spar
    ├── time.spar
    ├── terminal.spar
    ├── http.spar
    ├── async.spar
    ├── regex.spar
    ├── json.spar
    ├── text.spar
    ├── math.spar
    └── random.spar
```

The std package is resolved from a runtime-known bundled location, not from the network or package store.

## 9. Import Semantics

Package namespace imports use `import pkg` exclusively.

```spar
// local source module
import { helper } from "./utils/helper";

// bundled official package
import pkg { println } from "std";
import pkg { readText } from "std/fs";

// ordinary project dependency
import pkg { Client } from "http";
import pkg { parse } from "json/parser";
```

Rules:

1. normal `import` resolves local/source modules and never silently falls back to a package;
2. `.spar` extensions are optional and normally omitted;
3. `import pkg` resolves the package namespace;
4. the first package component is an alias/name and the remainder is a submodule path relative to that package's entry-module directory;
5. `std` is a reserved bundled package alias and resolves without an explicit dependency or lockfile edge;
6. ordinary third-party/package-manager aliases resolve through the current package's dependency edges in the lock graph;
7. transitive imports inside dependency packages resolve using that dependency's own scoped edges, not the root project's aliases;
8. package submodule paths may not escape the package root with `..`, absolute paths, or prefixes;
9. `asPartOf` remains removed. Ordinary imports are the only source-module composition mechanism.

## 10. Package Metadata and Lockfiles

Existing Spar package declarations remain canonical, e.g.:

```spar
struct Package: SparPackage {
    name = "phase0-showcase";
    version = "0.1.0";
    kind = "application";
    entry = "src/main.spar";
};

struct Dependencies {
    toolkit: str = "path:../phase0-toolkit";
};
```

The official stdlib is not declared in `Dependencies`.

Ordinary dependencies remain represented in the lock graph. A package's `dependencies` entries provide scoped alias edges for imports inside that package.

The `std` package is version-coupled to the installed Spar runtime and does not need a project lock entry.

## 11. Prelude

The public implementations of core user-facing helpers live in the std package rather than as unrelated interpreter magic.

A deliberately tiny prelude re-exports a small approved subset so common scripts remain ergonomic. The initial prelude includes at least:

```text
print
println
len
assert
panic
```

The implementation of an implicitly available function must remain canonical in std/core or std/io; the prelude is an exposure mechanism, not a duplicate implementation.

Large APIs are never injected into the prelude.

## 12. Initial v1 Stdlib Modules

The v1 stdlib ships these modules:

```text
std/io
std/fs
std/path
std/env
std/process
std/time
std/terminal
std/http
std/async
std/regex
std/json
std/text
std/math
std/random
```

Responsibilities:

- `std/io`: stdin/stdout/stderr, print/println, line and byte-oriented I/O;
- `std/fs`: text/bytes read/write/append, existence, metadata, file/directory create/remove/copy/move;
- `std/path`: pure path joining, basename/dirname/extension, normalization, absolute/relative calculations;
- `std/env`: read/test/set/unset/enumerate runtime environment;
- `std/process`: script args, command/process spawn, run/wait, typed process result, stream/child interaction, explicit exit;
- `std/time`: now, duration helpers, sleep, elapsed timing, basic parse/format;
- `std/terminal`: TTY detection, terminal dimensions, ANSI color/style helpers, clear/cursor operations;
- `std/http`: typed async HTTP request plus GET/POST/PUT/PATCH/DELETE convenience wrappers;
- `std/async`: `all`, `race`, `timeout` and task/promise composition;
- `std/regex`: match/find/find-all/replace/split;
- `std/json`: parse/stringify and typed conversion aligned with Spar's data model;
- `std/text`: trim, prefix/suffix/contains, replace, split/join, case operations;
- `std/math`: abs/min/max/round/floor/ceil/pow/sqrt;
- `std/random`: random int/float/bytes/selection; OS randomness for secure bytes.

Higher-level logic is written in Spar where possible. Rust native functions are limited to host/OS/runtime operations.

## 13. Runtime Resources

Opaque host resources are stored in a runtime-owned table addressed by safe IDs/handles.

```text
ResourceId -> Process | Stream | File/descriptor | HTTP body/client | host resource
```

Runtime shutdown must release owned resources, cancel pending async operations, clean up child processes according to documented ownership semantics, and run host cleanup hooks when present.

User code cannot inspect Rust internals or manufacture valid resource handles.

## 14. Shell Integration

Native `shell {}` remains a Spar language/runtime feature.

It uses `spar-command` for structural plans and `spar-process` for OS execution. Sparsh is not involved.

The runtime context supplies cwd, environment, stdio and process ownership to shell execution. The existing shell redesign requirements remain binding, including multiline Spar statements, multiline commands, explicit `\\` continuation, interpolation, pipelines, logical operators, redirections, background jobs, `status`, `lastJob`, `cd`, `exec {}`, and source-aware errors.

A future public `shell` package may provide ergonomic programmatic process APIs, but it is explicitly deferred until Sparsh work resumes. It must sit above the same native process capabilities rather than calling Sparsh.

## 15. Sparsh Boundary

No new Sparsh feature work is required for this runtime/stdlib milestone beyond compatibility updates needed by compiler/runtime API changes.

When deeper Sparsh work resumes, Sparsh may register additional host-native modules for jobs/history/aliases/prompt/completion and expose stable public shell packages. Those modules are host capabilities, not dependencies of core Spar.

## 16. Standard Library Loading and Distribution

The std package is always discoverable by the package resolver but is loaded lazily by module import. Tiny scripts must not initialize unrelated filesystem/network/regex subsystems.

Development/test builds may load `.spar` stdlib sources directly. The release pipeline must support precompiling the shipped std package so ordinary startup does not reparse and re-type-check all std sources on every execution.

The precompiled artifact is version-coupled to the runtime/compiler version. A source fallback may remain for development builds and diagnostics.

## 17. LSP Integration

`spar-ls` must use compiler/package resolution APIs instead of reimplementing path logic.

It must understand:

```spar
import { helper } from "./helper";
import pkg { println } from "std";
import pkg { readText } from "std/fs";
import pkg { Client } from "http";
```

Required behaviors include diagnostics, hover, completion, go-to-definition, references and semantic tokens for local, std, and ordinary package imports.

`asPartOf` receives no semantic support beyond a parser migration diagnostic.

## 18. Testing

Implementation follows TDD.

Required test groups:

1. runtime value/frame execution and ordinary non-shell programs;
2. `RuntimeContext` cwd/env/args/stdin/stdout/stderr isolation;
3. `NativeRegistry` typed sync/async dispatch, missing capabilities and error translation;
4. trusted versus user access to `@native/*`;
5. bundled `std` package root/submodule resolution without manifest/lock entries;
6. local extensionless module resolution;
7. ordinary package and transitive package submodule resolution;
8. every public std module, with deterministic local HTTP tests instead of public network dependencies;
9. resource cleanup and child-process ownership;
10. shell regression suite discovered during real-world testing;
11. `spar-ls` diagnostics/hover/completion/definition/references for local/std/package imports;
12. existing config/task/package behavior to prevent regressions.

The end-to-end stdlib smoke program must exercise normal runtime execution rather than only shell commands. At minimum it should write/read a file through `std/fs`, build a path through `std/path`, print through the prelude/std I/O implementation, branch on the result, and return a process exit code.

## 19. Implementation Order

The implementation order for this milestone is:

1. finish and audit current shell/package/import repairs already in the working tree;
2. finish `spar-ls` compatibility for the finalized package-import grammar;
3. modularize the existing runtime boundary and introduce central `Value`/`RuntimeContext` without duplicating the executor;
4. implement typed `NativeRegistry` and resource table;
5. implement loader trust contexts and private `@native/*` providers;
6. add bundled reserved `std` package resolution through `import pkg`;
7. implement std modules in dependency-sized groups;
8. add prelude wiring to canonical std implementations;
9. add stdlib/LSP/end-to-end tests;
10. add/prep stdlib precompilation path;
11. package all changed files into the replacement ZIP for local Rust compilation/testing;
12. after Spar passes local verification, resume deeper Sparsh work as a separate project.

## 20. Explicit Non-Goals for This Milestone

Do not add a separate `spar-runtime` crate, bytecode VM, JIT, LLVM/Cranelift backend, public arbitrary FFI, user-visible OS threads, SQL/database client, WebSockets, SMTP, archive/compression ecosystem, web framework, GUI, or a public `shell` package in this pass.

## 21. Success Criteria

This milestone is complete when:

- ordinary typed Spar programs execute through the runtime without shelling out;
- `shell {}` remains one integrated runtime capability rather than the runtime itself;
- `RuntimeContext`, native dispatch and resource ownership are explicit and testable;
- the official `std` package is bundled, reserved, offline and imported with `import pkg`;
- std does not need a project dependency/lockfile entry;
- public std APIs are primarily Spar-written over a small trusted Rust-native boundary;
- ordinary user code cannot import `@native/*`;
- all 14 v1 std modules have focused tests and useful initial APIs;
- transitive package imports resolve in the correct package scope;
- `asPartOf` is removed;
- `spar-ls` understands local/std/package imports using compiler resolution;
- the real-world advanced shell stress cases remain covered;
- Spar remains independent of Sparsh;
- the replacement ZIP includes compiler/runtime/process/LSP/stdlib changes plus a patch manifest and verification instructions.
