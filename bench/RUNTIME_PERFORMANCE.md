# Spar runtime performance

Living record of runtime architecture, measurements and next steps.
Reproduce with `bench/run.py` (release binary, min/median/mean/sd over N runs).

## Architecture (as of this change)

```
Lexer → Parser → Loader → Resolver → TypeChecker → lowerer (CompiledProgram)
                                                      │
                          ┌───────────────────────────┴──────────────┐
                          │ every function: slot-indexed tree         │
                          │ (runtime.rs, `Runtime::eval_expression`)  │
                          │                                           │
                          │ primitive-typed subset: register bytecode │
                          │ (vm.rs, built once in from_compilation)   │
                          └───────────────────────────────────────────┘
```

Findings that shaped this (all measured, see below):

* The tree walker was *already* slot-resolved: locals are `LocalSlot`
  indexes, calls are `FunctionId`, arguments positional. There were no
  `HashMap` environments, no name lookups and no reparsing on the call path.
  The 1 µs/call cost came from elsewhere.
* `Value` is 160 bytes (it inlines `ShellPlan`, `TableValue`, `Schema`, …).
  Every `Result<Value, RuntimeFault>` return is a 160-byte memcpy.

### VM tier (`src/vm.rs`)

Functions whose params/locals/return are `int`/`float`/`bool` (`void` return)
and whose bodies use only `if`, local stores, `return`, typed primitive
operations and direct calls to other such functions are lowered to register
bytecode at compile time. Everything else stays on the tree walker; the tiers
interoperate at `Runtime::call_direct_inline` (arguments → register bits,
result → `Value`).

Invariants (also in the module docs): untagged `u64` registers; slot `i` is
register `i`; args occupy consecutive caller registers that become the
callee's parameters (register windows); call depth check mirrors the tree
walker; `+ - * /` wrap on overflow.

Tools: `spar dis file.spar` (bytecode listing), `SPAR_DISABLE_VM=1` (force the
tree walker, for A/B and differential runs), `--features runtime-stats`
(allocation counters), `--features profile` + `SPAR_PROFILE=1 spar exec …`
(sampling profiler, self/inclusive hot symbols).

## Baseline (before any change)

Release build, this machine. fib(38) itself was not re-run at baseline; it is
extrapolated from fib(32) (7.05 M calls → ~1.0 µs/call ⇒ ~128 s) and agrees
with the reported ~145.7 s.

| bench  | time    |
|--------|---------|
| fib(27)| 0.73 s  |
| fib(30)| 2.67 s  |
| fib(32)| 7.18 s  |

Allocation counters (fib(30), 2.69 M calls): 5.44 M allocs (≈2/call),
2.16 GB (≈800 B/call): one `Vec<Value>` for args + one frame slot vector.

## Benchmark history

| step | fib(30) | fib(32) | fib(38) | notes |
|------|---------|---------|---------|-------|
| baseline tree walker | 2.67 s | 7.18 s | ~128 s (extrap.) | |
| + no args `Vec`, scalar-constant fast path, inline int ops | 0.86 s | 2.37 s | | −68 % |
| + no `Arc` clone per call, skip generic return-type resolution | 0.77 s | 2.06 s | | profiler showed atomics ≈11 % |
| slot-vector pool (tried) | 0.89 s | 2.39 s | | **reverted**: no gain, malloc is not the bottleneck |
| + register VM tier | 0.056 s | 0.137 s | **2.36 s** | −93 % vs previous, ≈52× vs baseline at fib(32) |

Reference on the same machine, fib(38): CPython 3.x **6.15 s**, native Rust
`-O` **0.11 s**. Spar VM is 2.6× faster than CPython and ~21× slower than
native Rust (fraction of the CPython→Rust gap closed: ≈ 24 % in log-scale
terms: log(6.15/2.36) / log(6.15/0.11)).

Other microbenchmarks (min of N, VM vs tree walker via `SPAR_DISABLE_VM=1`):

| bench | VM | tree walker |
|-------|----|-------------|
| float_tree | 0.067 s | 0.979 s |
| int_arith_tree | 0.042 s | 0.631 s |
| bool_branch_tree | 0.014 s | 0.126 s |
| fib32 | 0.126 s | 1.967 s |

Startup (`hello.spar`, includes process start + compile): ~7 ms.

## VM loops (`for i in range(...)` / `rangeFrom(...)`)

The lowerer compiles `for i in range(end: e)` and `rangeFrom(start:, end:)`
into counting loops over registers (`break`/`continue` supported, optional
`(index, value)` binding). The list is never materialised. The prelude names
`range`/`rangeFrom` are reserved (user declarations are rejected), so
recognising them is a compile-time intrinsic, not benchmark special-casing.
Other iterables (lists etc.) still make the function fall back to the tree
walker.

`loop_sum.spar` (100 M iterations of `total = total + i * 3 - (i / 7)`):
Spar VM **3.55 s**, CPython **16.7 s**, Rust `-O` **0.16 s**. The tree walker
cannot run this at all: `range(end: 100000000)` materialises 100 M
160-byte `Value`s (~16 GB) and the process is OOM-killed.

## Tier 2: universal bytecode (`src/runtime/bytecode.rs`)

Real scripts are dominated by strings, records, lists, async and shell, so the
primitive tier alone never applies to them (`spar dis` on the sample scripts:
every function was "non-primitive parameter/return"). Tier 2 lowers **every**
function body to bytecode that runs against the ordinary `Frame` (slot `i` =
register `i`, temporaries after the locals):

* real instructions: locals, constants, control flow (`if`, range and list
  `for`, `break`, `continue`, `return`), typed binary/unary operations with
  int/float fast paths, direct calls (arguments moved straight into the
  callee frame, no argument vector);
* everything else is a *fallback node*: the original compiled tree is kept and
  executed by the tree walker against the same frame (`Tree`/`Eval`/`TreeStmt`),
  so no construct is unsupported and semantics cannot drift;
* `CheckExit` reproduces the tree walker's post-statement `exit()`/shell-exit
  checks.

Dispatch order at a call: tier 1 (untagged registers, primitive-only) →
tier 2 → tree walker. `SPAR_DISABLE_VM=1` / `SPAR_DISABLE_BYTECODE=1` turn the
tiers off for A/B runs; `vm_tests.rs` compares each tier against the tree
walker.

## `Value` representation

`Value` was 160 bytes (large payloads inline), so every evaluator step moved
and cloned huge values, and reading a record deep-copied its `IndexMap`.
Large payloads now sit behind `Shared<T>` (`Arc` with copy-on-write through
`DerefMut`/`Arc::make_mut`): `List`, `Object`, `Table`, `Schema`, `Shell`,
`MixedShell`, `ShellProgram`, `Closure`, and the map inside `MapValue`;
`Error` is boxed (`ErrorValue`). Result: `Value` is <= 40 bytes (enforced by
the `value_stays_small` test), clones are reference-count bumps, and value
semantics are unchanged.

API note for embedders: `Value::List`/`Value::Object` now hold
`Shared<Vec<Value>>`/`Shared<IndexMap<..>>` (build with `.into()`,
`Shared::from`), and `Value::Error` holds `Box<ErrorValue>`. `spar::Shared` and
`spar::ErrorValue` are exported. `sparsh` was adjusted; `spar-ls` needed no
change.

| bench (release, min of 5) | before Shared/shrink | after |
|---|---|---|
| mixed.spar (records+strings, 300 k iters) | 0.91 s | ~0.6 s |
| fib(30), tier 2 only (VM off) | 0.86-0.94 s | 0.74 s |

Remaining profile of `mixed.spar`: tree-fallback nodes (struct construction,
field access, method and native calls) dominate; SipHash inside
`IndexMap<String, Value>` records is ~10 %; `Result<Value, RuntimeFault>`
returns ~10 %.

## Later rounds (all measured; correctness = full suite + differential tests)

| change | effect |
|---|---|
| Box `RuntimeFault` payloads (`Result<Value, RuntimeFault>` 96 B -> ~40 B) | tier-2 fib(30) 0.74 -> 0.40 s, `mixed.spar` 0.68 -> 0.36 s (largest single win after the VM) |
| Native bytecode ops for field access, native calls/methods, interpolation, list literals | field 101 -> 68 ms / 1 M, list literal 206 -> 110 ms / 1 M |
| Flat `Record` (`Vec<(Arc<str>, Value)>`, hash index only > 16 fields) replaces `IndexMap` objects | interleaved A/B: field -20 %, mixed -8 %, struct construction -7 % |
| **Cranelift backend** for tier 1 (`src/jit.rs`) | fib(38) 1.95 -> **0.32 s**; 100 M-iteration loop 2.5 -> **0.21 s** |

Reference (same machine): fib(38) CPython 6.15 s, Rust -O 0.11 s; 100 M loop
CPython 16.7 s, Rust -O 0.16 s. Total vs the original tree walker: fib(38)
~145 s -> 0.32 s.

### Native backend (`src/jit.rs`)

Compiled on first use of any tier-1 function (hello-world startup unchanged,
~7 ms). ABI: `extern "C" fn(depth, ctx, args...) -> i64`; a non-zero `ctx.err`
after a call means division by zero (function/op recorded) or call depth
exceeded, and callees propagate by returning 0. Semantics match the bytecode
interpreter exactly (wrapping `+ - *`, `MIN / -1` wraps, same depth limit);
`native_backend_matches_interpreter_*` in `vm_tests.rs` compare results and
errors. `SPAR_NO_JIT=1` forces the interpreter; any compile failure falls back
to it. Not built for wasm32.

### Benchmarking notes

Wall-clock on this machine drifts by +-10-20 % between minutes; use
`bench/ab.py binA binB N files...` (interleaved runs) for A/B decisions and
`bench/micro.sh` for per-operation costs. The 1 kHz in-process sampler
(`--features profile`) needs >= 5 M iterations to be readable.

## Correctness status

* `src/tests/vm_tests.rs`: differential tests (VM vs tree walker) for
  recursion, float/bool/short-circuit, mutual recursion, nested call args,
  division by zero, depth-limit error text, default-argument fallback.
* Full `cargo test --no-fail-fast`: lib 1146 pass / 12 fail, plus 4
  integration failures. All 16 failures predate this work (spread-in-nested-
  field typechecker cases; `response.json<T>()` generic-method-call parse
  bug) and are unrelated to the runtime.

## Known bottlenecks / open decisions

* **Integer overflow semantics are undefined** in the language. Both tiers
  currently wrap (release-build behaviour of the old tree walker; a debug
  build of the old walker would have panicked). Needs a language decision
  (checked / wrapping / trap).
* Tier 1 only takes primitive-typed functions; tier 2 covers everything but
  runs strings/records/methods/native calls through fallback nodes. Loops need a `while`/range construct first.
* `Value` is 160 bytes; boxing the large variants would speed the tree walker
  (`Result` returns were ~14 % of its profile) but touches ~330 match sites.
* No modulo operator in the language (`%` does not lex).

## Next steps (ordered by expected impact)

1. (done) VM: fused compare+branch, 17-20% faster on fib(38) in an A/B run under identical load (fib(38) ~2.3 s).
2. Extend the VM: lists of ints, strings-as-handles, native call bridge
   (`println`), so `main` and more loop bodies can be lowered. (int `range`
   loops: done.)
3. IR optimisation passes (constant folding, dead code) once there is a
   real IR between the AST and bytecode.
4. (done for the int/float/bool subset) Extend native code to loops over other types, strings, records.
5. Compiled-artifact cache and incremental compilation.
6. Concurrency work after single-thread execution is settled.
