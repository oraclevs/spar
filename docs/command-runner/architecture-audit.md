# Command Runner Architecture Audit

**Audit date:** 2026-08-29

**Spar revision audited:** `137a020` baseline worktree (before command-runner changes)

**Design authority:** [`../superpowers/specs/2026-08-29-command-runner-design.md`](../superpowers/specs/2026-08-29-command-runner-design.md)

## Spar baseline

The baseline suite is **599 passed, 0 failed**. The command runner must retain
the existing configuration pipeline and its error-accumulation behavior.

```text
CLI (src/main.rs)
  check -> Compiler::compile with evaluate=false -> ErrorRenderer
  emit  -> Compiler::compile -> evaluator result + SymbolTable -> emit JSON
  fmt   -> formatter::format_source (separate formatter path)

Compiler::compile (src/compiler.rs)
  Lexer::tokenize (src/lexer.rs; token definitions in src/token.rs)
  -> Parser::parse (src/parser.rs -> src/ast.rs::Program)
  -> loader::expand_imports (src/loader.rs)
  -> loader::collect_imports
  -> Resolver::resolve_with_imports (src/resolver.rs -> SymbolTable)
  -> loader::validate_schema_imports
  -> TypeChecker::check_with_schema (src/typechecker.rs)
  -> Evaluator::evaluate_with_imports_and_base
       (only when CompileOptions.evaluate and no prior errors; src/evaluator.rs -> EvalResult)
  -> emit::build_emit_json (src/emit.rs; only the `emit` CLI path)
```

`Compiler` preserves the expanded `Program`, `SymbolTable`, imports,
`EvalResult`, and every accumulated `SparError` in `Compilation`. Lexer and
parser failures return immediately; resolver failures preserve the program;
type-check errors accumulate. Evaluation runs only when
`CompileOptions.evaluate` is true and every prior stage is error-free; any
evaluation errors then accumulate in `Compilation`. `renderer.rs` renders
those source-spanned errors. `de.rs` is a separate serde bridge over the same
compiler/evaluation result.

## Approved insertion boundary

```text
Spar source
  -> existing lexer/parser/task AST
  -> existing import expansion, resolution, type checking, evaluation
  -> src/task_lowering.rs                 (the sole Spar-to-runner adapter)
  -> src/runner/{task,graph,executor,shell,environment,error}.rs
  -> std::process::Command -> operating-system shell
```

`src/runner/` owns parser-independent, owned task data and may not import
Spar AST, parser, resolver, or evaluator types. Runner tests construct that
IR directly. `task_lowering` is the only module that translates
`ast::TaskDecl` and evaluated Spar values into runner types. Lowered tasks are
available only after normal compilation succeeds; task declarations never
appear in emitted JSON.

## Behavioral decisions fixed by the audit

- The Spar lexer enters raw shell mode only for `run { ... }` in a `task`;
  `${expr}` returns temporarily to Spar expressions, `$NAME` remains shell
  syntax, and `$${...}` lowers to literal `${...}`.
- Graph validation is Spar-owned: validate all requested tasks and their
  dependencies before spawning, depth-first order dependencies first, report
  explicit cycles, and execute a shared dependency once per invocation.
- The executor runs each raw block as one script: Unix `sh -cu`; Windows
  `cmd /S /C`. It inherits stdin/stdout/stderr and the parent environment,
  then applies task environment overrides. Relative `cwd` is based on the
  source `.spar` file directory.
- Dry-run performs graph validation and interpolation, prints ordered
  commands, and starts no child. Quiet affects command echo only. A failing
  child stops dependents.
- The CLI remains file-first: `spar tasks <file.spar>` and
  `spar run <file.spar> [task] [args...] [--dry-run]`. With no explicit task,
  exactly one default is required.

## Non-goals protected by this boundary

Do not import Justfile grammar, recipes, Just expression evaluation, module
semantics, aliases, settings, dotenv handling, caches, shebang scripts,
parallel scheduling, formatting, completion, or Just CLI parsing. Spar tasks
are command-runner tasks, not Make-style timestamp/file targets.

See [just-extraction-map.md](just-extraction-map.md) for the source-unit
decisions and [UPSTREAM-JUST.md](UPSTREAM-JUST.md) for donor provenance.
