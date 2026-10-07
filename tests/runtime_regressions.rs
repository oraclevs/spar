use spar::Shared;
use spar::{CompileOptions, Engine, Value};
use std::time::Instant;

// Regression: every native method call used to deep-clone the whole
// NativeRegistry (~140us/call), making collection-heavy scripts unusable.
#[test]
fn native_method_calls_do_not_clone_the_registry() {
    let started = Instant::now();
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            fn main() -> int {
                var mut total: int = 0;
                for i in range(end: 20000) { total = total + "abc".length(); }
                if total != 60000 { return 1; } return 0;
            };
            "#,
        )
        .expect("native method loop");
    assert_eq!(outcome.exit_status, 0);
    assert!(
        started.elapsed().as_millis() < 1500,
        "20k native calls took {:?}",
        started.elapsed()
    );
}

#[test]
fn exec_passes_program_args_to_process_args() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("a.spar");
    std::fs::write(
        &script,
        r#"
        import pkg { args } from "std/process";
        fn main() -> int { return args().length(); };
        "#,
    )
    .unwrap();
    let outcome = Engine::new(CompileOptions::default())
        .execute_path_with_args(&script, vec!["one".into(), "--two".into()])
        .expect("script with args");
    assert_eq!(outcome.exit_status, 2);
}

// Regression: `&&`/`||` used to eagerly evaluate both operands in the
// compiled runtime, so a length-guarded index access like
// `xs.length() > 0 && xs[0] > 0` panicked instead of short-circuiting.
#[test]
fn boolean_operators_short_circuit_in_compiled_runtime() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            fn guarded(values: List<int>) -> bool {
                return values.length() > 0 && values[0] > 0;
            };
            fn main() -> int {
                if guarded(values: []) { return 1; }
                if !guarded(values: [5]) { return 2; }
                return 0;
            };
            "#,
        )
        .expect("length-guarded index access must not evaluate the right operand");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn boolean_operators_do_not_evaluate_unneeded_side_effects() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            var mut calls: int = 0;
            fn sideEffect(v: bool) -> bool { calls = calls + 1; return v; };
            fn main() -> int {
                if false && sideEffect(v: true) { }
                if true || sideEffect(v: true) { }
                if calls != 0 { return 1; }
                var mut hit: int = 0;
                if true && sideEffect(v: true) { hit = hit + 1; }
                if false || sideEffect(v: true) { hit = hit + 1; }
                if calls != 2 || hit != 2 { return 2; }
                return 0;
            };
            "#,
        )
        .expect("&&/|| should evaluate the right operand only when needed");
    assert_eq!(outcome.exit_status, 0);
}

// Regression: Value::Map moved from a linear-scan Vec<(Value,Value)> to a
// hash-indexed MapValue for O(1) average get/insert/remove/containsKey.
// These pin down the exact semantics that must survive the swap: insertion
// order, replace-in-place on an existing key, and non-str/int/bool/float
// keys still working correctly (just without the fast path).
#[test]
fn map_value_preserves_insertion_order_and_replace_in_place() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            fn main() -> int {
                var mut m: Map<str, int> = {};
                m.insert(key: "a", value: 1);
                m.insert(key: "b", value: 2);
                m.insert(key: "c", value: 3);
                m.insert(key: "b", value: 20);
                var keys: List<str> = m.keys();
                if keys.get(index: 0).unwrap() != "a" { return 1; }
                if keys.get(index: 1).unwrap() != "b" { return 2; }
                if keys.get(index: 2).unwrap() != "c" { return 3; }
                if m.get(key: "b").unwrap() != 20 { return 4; }
                if m.length() != 3 { return 5; }
                var removed: Option<int> = m.remove(key: "a");
                if removed.unwrap() != 1 { return 6; }
                if m.length() != 2 { return 7; }
                if m.containsKey(key: "a") { return 8; }
                if !m.containsKey(key: "b") { return 9; }
                return 0;
            };
            "#,
        )
        .expect("MapValue insert/remove/keys semantics");
    assert_eq!(outcome.exit_status, 0);
}

// `Map<int,_>`/`Map<bool,_>`/`Map<float,_>` can only be constructed via an
// *empty* `{}` object-literal (fixed in core_collections.rs's
// generic_maps_with_non_string_keys_survive_global_writeback) — object-
// literal keys are identifier/string tokens, so a non-empty literal has no
// way to spell a non-str key at all. Exercise `MapValue`'s own Rust API
// directly here instead, to pin down that its hashing/equality is correct
// for every primitive key type once populated via `.insert()`, not just str.
#[test]
fn map_value_supports_non_string_keys_including_bool_and_float() {
    let mut by_int = Value::Map(vec![].into());
    let Value::Map(map) = &mut by_int else {
        unreachable!()
    };
    assert_eq!(map.insert(Value::Int(1), Value::String("one".into())), None);
    assert_eq!(map.insert(Value::Int(2), Value::String("two".into())), None);
    assert_eq!(map.get(&Value::Int(1)), Some(&Value::String("one".into())));
    assert_eq!(map.get(&Value::Int(3)), None);

    let mut by_bool = Value::Map(vec![].into());
    let Value::Map(map) = &mut by_bool else {
        unreachable!()
    };
    map.insert(Value::Bool(true), Value::Int(1));
    map.insert(Value::Bool(false), Value::Int(0));
    assert_eq!(map.get(&Value::Bool(true)), Some(&Value::Int(1)));
    assert_eq!(map.get(&Value::Bool(false)), Some(&Value::Int(0)));

    let mut by_float = Value::Map(vec![].into());
    let Value::Map(map) = &mut by_float else {
        unreachable!()
    };
    map.insert(Value::Float(1.5), Value::String("a".into()));
    map.insert(Value::Float(2.5), Value::String("b".into()));
    assert_eq!(
        map.get(&Value::Float(1.5)),
        Some(&Value::String("a".into()))
    );

    // A key type with no fast-path index (Other) must still round-trip
    // correctly, just without the O(1) lookup.
    let mut by_list = Value::Map(vec![].into());
    let Value::Map(map) = &mut by_list else {
        unreachable!()
    };
    let list_key = Value::List(Shared::from(vec![Value::Int(1), Value::Int(2)]));
    map.insert(list_key.clone(), Value::String("listy".into()));
    assert_eq!(map.get(&list_key), Some(&Value::String("listy".into())));
    assert_eq!(
        map.get(&Value::List(Shared::from(vec![Value::Int(9)]))),
        None,
        "a different Other key must not collide despite sharing a hash bucket"
    );
}

#[test]
fn map_value_map_literal_dedupes_duplicate_keys_last_wins() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            fn main() -> int {
                var m: Map<str, int> = { a: 1; b: 2; };
                if m.length() != 2 { return 1; }
                var spread: Map<str, int> = { ...m; a: 9; };
                if spread.get(key: "a").unwrap() != 9 { return 2; }
                if spread.get(key: "b").unwrap() != 2 { return 3; }
                if spread.length() != 2 { return 4; }
                return 0;
            };
            "#,
        )
        .expect("Map literal and spread construction");
    assert_eq!(outcome.exit_status, 0);
}

// Regression: every method call clones its whole receiver value before
// dispatch, even for a read-only call on a value that never changes size.
// `.length()` on a *stable* map (built once, never mutated in the loop)
// used to cost O(n) per call purely from that clone — 8,000 calls against
// an 8,000-entry map took 5.4s. Fixed for the common case (`x.method(...)`
// on a plain local, calling a native method) by moving the value out of
// its slot instead of cloning it, and always moving it back — including on
// an error from either the arguments or the call itself.
#[test]
fn native_method_calls_on_a_local_receiver_do_not_clone_it() {
    let started = std::time::Instant::now();
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            fn main() -> int {
                var mut m: Map<str, int> = {};
                for i in range(end: 4000) { m.insert(key: "k${i}", value: i); }
                var mut total: int = 0;
                for i in range(end: 4000) { total = total + m.length(); }
                if total != 4000 * 4000 { return 1; }
                return 0;
            };
            "#,
        )
        .expect("stable-receiver method calls");
    assert_eq!(outcome.exit_status, 0);
    assert!(
        started.elapsed().as_millis() < 500,
        "4000 inserts + 4000 reads of a stable receiver took {:?}",
        started.elapsed()
    );
}

#[test]
fn native_method_calls_survive_an_error_inside_the_call_with_receiver_intact() {
    // The receiver is "checked out" of its slot only around the call
    // itself; an error raised *by the native method* (not just by
    // argument evaluation) must not leave the slot empty for anything
    // that reads it afterward, e.g. inside the enclosing `try`'s `catch`.
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            fn main() -> int {
                var mut xs: List<int> = [1, 2, 3];
                try {
                    var bad: int = xs.removeAt(index: 99);
                } catch e {
                }
                if xs.length() != 3 { return 1; }
                xs.append(value: 4);
                if xs.length() != 4 { return 2; }
                return 0;
            };
            "#,
        )
        .expect("receiver must survive an error raised inside the call");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn native_method_calls_survive_an_error_in_argument_evaluation() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { parse } from "std/json";
            fn main() -> int {
                var mut m: Map<str, int> = {};
                m.insert(key: "a", value: 1);
                try {
                    var bad: int = parse<int>(text: "not json");
                    m.insert(key: "b", value: bad);
                } catch e {
                }
                if m.length() != 1 { return 1; }
                if m.get(key: "a").unwrap() != 1 { return 2; }
                m.insert(key: "c", value: 3);
                if m.length() != 2 { return 3; }
                return 0;
            };
            "#,
        )
        .expect("receiver must survive an error raised while evaluating an argument");
    assert_eq!(outcome.exit_status, 0);
}

// Regression: two structs whose default field values construct each other
// via `Other()` constructor calls (the only way to reach another struct's
// fields now that bare `Other.field` access requires an instance) used to
// recurse with no cycle guard and blow the stack instead of reporting a
// cyclic reference — `eval_struct_constructor` had no equivalent of the
// `evaluating_structs` guard `eval_struct_by_path` already used.
#[test]
fn struct_constructor_cycle_reports_cyclic_reference_not_stack_overflow() {
    // check_source only runs lex/parse/resolve/typecheck; the cycle only
    // manifests during evaluation, so this must go through emit_source
    // (which runs the full pipeline, including eager evaluation of
    // `#[emit]` structs) to actually exercise the constructor recursion.
    let compilation = Engine::new(CompileOptions::default()).emit_source(
        r#"
            #[emit] struct X {
                nested: Record = {
                    v: Y().inner.val;
                };
            };
            struct Y {
                inner: Record = {
                    val: X().nested.v;
                };
            };
            "#,
    );
    assert!(
        !compilation.is_ok(),
        "a genuine circular constructor reference must error, not hang or crash"
    );
    assert!(
        compilation
            .errors
            .iter()
            .any(|e| format!("{e}").to_lowercase().contains("cyclic")),
        "expected a cyclic-reference error, got {:?}",
        compilation.errors
    );
}

// Regression: `status.signal` (inside a shell block) is declared
// `Option<int>` on the synthetic ProcessStatus type, but the runtime built
// a bare int for the Some case and omitted the field entirely for None,
// so `.isSome()`/`.unwrap()` on it crashed with "Option method received
// runtime value int" instead of working like any other Option<T>.
#[test]
fn shell_status_signal_is_a_real_option_not_a_bare_int_or_missing_field() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            fn main() -> __shell {
                return __shell {
                    true;
                    if status.signal.isSome() { exit 1; }
                    true | sh -c "kill -TERM $$";
                    if !status.signal.isSome() { exit 2; }
                    if status.signal.unwrap() != 15 { exit 3; }
                    exit 0;
                };
            };
            "#,
        )
        .expect("status.signal must behave like an ordinary Option<int>");
    assert_eq!(outcome.exit_status, 0);
}

// Regression: `await receiver.asyncMethod()` — an async `impl` method,
// called through method syntax rather than as a free function — didn't
// typecheck at all (infer_method_call never wrapped an async method's
// return type in Promise<T>, the same wrapping instantiate_named_fn_call
// already does for a plain async function call, so `await` saw the bare
// return type and rejected it as "not a Promise"). Fixing the typechecker
// exposed a second gap one layer down: CompiledExpression::MethodCall's
// function-target branch always called the method synchronously with no
// async check at all (DirectCall already had one), so an async method's
// eagerly-computed result got hit with an `await` expecting a real
// Promise and crashed with "expected Promise, received <T>" instead.
#[test]
fn await_on_an_async_impl_method_call_works_like_an_async_function() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            struct Doubler { factor: int = 2; };
            impl Doubler {
                async fn apply(self, value: int) -> int {
                    return self.factor * value;
                };
            };
            async fn main() -> int {
                var d: Doubler = Doubler();
                var result: int = await d.apply(value: 21);
                if result != 42 { return 1; }
                return 0;
            };
            "#,
        )
        .expect("await on an async impl-method call should typecheck and run");
    assert_eq!(outcome.exit_status, 0);
}

// Regression: an env-prefix assignment whose value starts with a quote or
// `${` (e.g. `SPAR_A="${secret}"`) lexes the `SPAR_A=` and the value as two
// separate tokens (`"` breaks bare-word scanning like whitespace does), so
// the naive text.split_once('=') on just the first token saw an empty
// value and fabricated a spurious empty-string literal fragment that was
// never in the source. Formatting rendered it as a leading `''`, and
// re-parsing that `''` hit the exact same empty-value case, adding
// another `''` on every subsequent format pass -- non-idempotent, growing
// output. Fixed in shell_lang.rs's parse_command by only seeding the
// parts list from the split-off value when it's actually non-empty.
#[test]
fn env_prefix_value_starting_with_interpolation_formats_stably() {
    let source = "task Show {\n    run {\n        SPAR_A=${secret} printenv SPAR_A;\n    };\n};\n";
    let once = spar::formatter::format_source(source).expect("format");
    let twice = spar::formatter::format_source(&once).expect("format again");
    assert_eq!(once, twice, "formatting must be idempotent");
    assert!(
        !once.contains("''"),
        "must not fabricate an empty-literal fragment: {once}"
    );
}

// Regression: ModuleState.results moved from a plain HashMap to an
// Arc<Mutex<...>> shared across every per-task Runtime (Phase 2, real
// concurrency). A module's top-level state (`hits` here) must still be
// initialized exactly once and shared correctly across two separate calls
// into the same imported module, not silently re-initialized or copied.
#[test]
fn imported_module_state_persists_across_two_calls_into_it() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("counter.spar"),
        concat!(
            "export var mut hits: int = 0;\n",
            "fn bump() -> int { hits = hits + 1; return hits; };\n",
        ),
    )
    .unwrap();
    std::fs::write(
        temp.path().join("main.spar"),
        concat!(
            "import \"counter.spar\" as counter;\n",
            "async fn a() -> int { return counter::bump(); };\n",
            "async fn b() -> int { return counter::bump(); };\n",
            "async fn main() -> int {\n",
            "    var x = await a();\n",
            "    var y = await b();\n",
            "    return x + y;\n",
            "};\n",
        ),
    )
    .unwrap();

    let outcome = Engine::default()
        .execute_path(&temp.path().join("main.spar"))
        .expect("imported module state should persist across calls");
    // If the module were double-initialized (hits reset to 0 between calls)
    // this would read 1 + 1 = 2 instead of the correct 1 + 2 = 3.
    assert_eq!(outcome.exit_status, 3);
}

// Regression: the flagship proof for Phase 2 (real multi-threaded async
// concurrency) — spawning several async tasks and awaiting them together
// must actually overlap on real OS threads, not run one at a time on a
// single-threaded cooperative scheduler (the old TaskTable behavior).
#[test]
fn concurrent_async_delays_overlap_not_serialize() {
    let start = Instant::now();
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { all } from "std/async";
            import pkg { sleepMillis } from "std/time";
            async fn slow() -> int {
                sleepMillis(millis: 200);
                return 1;
            };
            async fn main() -> int {
                var mut ps: [Promise<int>] = [];
                for i in range(end: 5) { ps.append(value: slow()); }
                var rs = await all<int>(promises: ps);
                return rs.length();
            };
            "#,
        )
        .expect("concurrent delays should execute");
    let elapsed = start.elapsed();
    assert_eq!(outcome.exit_status, 5);
    // Serial would take ~1000ms (5 x 200ms); concurrent should land near
    // 200ms. 600ms leaves generous headroom for scheduling/test-machine
    // jitter while still failing hard on a regression back to serial.
    assert!(
        elapsed < std::time::Duration::from_millis(600),
        "expected concurrent execution (~200ms), took {elapsed:?} — looks serial"
    );
}

// Regression: each spawned task runs on a fresh, short-lived, worker-side
// `Runtime` (built per task by `new_entry_runtime`'s `run` closure) that is
// dropped as soon as that one task finishes. That drop must NOT shut down
// the shared `Scheduler` — is_entry is false for every worker-side
// `Runtime` — or a sibling task still in flight (or not yet started) would
// never get to run once the first spawned task's own `Runtime` is dropped.
#[test]
fn dropping_a_nested_runtime_does_not_cancel_sibling_tasks() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { all } from "std/async";
            import pkg { sleepMillis } from "std/time";
            async fn slow(tag: int) -> int {
                sleepMillis(millis: 150);
                return tag;
            };
            async fn main() -> int {
                var mut ps: [Promise<int>] = [];
                for i in range(end: 3) { ps.append(value: slow(tag: i)); }
                var rs = await all<int>(promises: ps);
                var mut sum = 0;
                for r in rs { sum = sum + r; }
                return sum;
            };
            "#,
        )
        .expect("sibling tasks should all run to completion");
    assert_eq!(outcome.exit_status, 0 + 1 + 2);
}

// Regression: std/process's run/wait/spawn are plain synchronous natives
// (not async fn — confirmed by auditing stdlib/src/process.spar before this
// phase started), so a blocking run() call inside one async task should
// only block that one worker thread, not the whole scheduler. This is
// composition the worker pool gives for free — no production code change
// expected, this test only confirms it.
#[test]
fn blocking_process_wait_does_not_stall_concurrent_async_work() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { all } from "std/async";
            import pkg { run } from "std/process";
            import pkg { nowMillis, sleepMillis } from "std/time";
            async fn slowProcess() -> int {
                var result = run(program: "sleep", args: ["1"]);
                return result.exitCode;
            };
            async fn slowDelay() -> int {
                sleepMillis(millis: 200);
                return 1;
            };
            async fn main() -> int {
                var start = nowMillis();
                var ps: [Promise<int>] = [slowProcess(), slowDelay()];
                var rs = await all<int>(promises: ps);
                var elapsed = nowMillis() - start;
                // If the process wait (1000ms) blocked the delay task too,
                // elapsed would be >= 1000ms serial-ish either way, so this
                // alone can't prove overlap against a 1s process. Assert the
                // weaker but still meaningful property: total time is close
                // to the slower of the two (~1000ms), not their sum
                // (~1200ms).
                if elapsed < 1150 { return 0; }
                return 1;
            };
            "#,
        )
        .expect("process wait and concurrent delay should both complete");
    assert_eq!(outcome.exit_status, 0);
}

// Regression (found in final review, not the original plan): nested `await`
// — an async fn awaiting another async fn it called, the shape the stdlib
// itself uses everywhere (`std/http::get` awaits `request`, `std/async::all`
// awaits each promise in its list) — used to be able to deadlock the whole
// worker pool at realistic fan-out, well under the pool's own size limit,
// because a worker blocked in `await_handle` held its thread doing nothing
// while waiting for a `Pending` task that no *other* free worker existed to
// pick up. `Scheduler::await_handle` now claims and runs a `Pending` task
// inline on the waiting thread instead of only blocking, which makes this
// deadlock structurally impossible regardless of fan-out width.
#[test]
fn nested_await_fan_out_does_not_deadlock_the_pool() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { all } from "std/async";
            import pkg { sleepMillis } from "std/time";
            async fn inner() -> int {
                sleepMillis(millis: 20);
                return 1;
            };
            async fn outer(tag: int) -> int {
                return await inner();
            };
            async fn main() -> int {
                var mut ps: [Promise<int>] = [];
                for i in range(end: 40) { ps.append(value: outer(tag: i)); }
                var rs = await all<int>(promises: ps);
                return rs.length();
            };
            "#,
        )
        .expect("nested fan-out should not deadlock the worker pool");
    assert_eq!(outcome.exit_status, 40);
}

// Regression (found in final review): `exit(code:)` called inside a task
// that's spawned and then `await`ed used to be silently lost — the task's
// own `RuntimeContext` is an isolated `spawn_child()` copy (by design, see
// Task 1), so setting `requested_exit` on it never reached the awaiting
// side's context, and execution just continued past the `await` as if
// nothing had happened. Mirrors the existing synchronous baseline
// (`stdlib_process.rs::process_exit_requests_runtime_exit_without_terminating_embedder`)
// but across an `await` boundary.
#[test]
fn exit_inside_an_awaited_task_propagates_to_the_awaiting_context() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { exit } from "std/process";
            async fn quit() -> int {
                exit(code: 7);
                return 0;
            };
            async fn main() -> int {
                var r = await quit();
                return 1;
            };
            "#,
        )
        .expect("exit inside an awaited task should run");
    assert_eq!(outcome.exit_status, 7);
}

// Regression (found in final review): if the entry point errors while a
// detached (fire-and-forget) task is still in flight, the entry `Runtime`
// drops, and `Scheduler::shutdown` cancels not-yet-started work, stops the
// pool, and joins every worker thread. Under the old blocking-only
// `await_handle`, a still-running detached task that then spawned and
// awaited its own subtask would wait forever for a free pool worker that
// would never come (every worker had already exited or was about to) —
// `shutdown`'s `join()` call, and so the whole process, hung permanently.
// The same inline-execution fix that resolves nested-await pool exhaustion
// resolves this too: the still-running task's own thread claims and runs
// its subtask itself, so it always finishes (here, within ~100ms), instead
// of waiting on a pool that's shutting down around it.
#[test]
fn spawning_a_subtask_after_shutdown_has_started_does_not_hang() {
    let start = Instant::now();
    let result = Engine::new(CompileOptions::default()).execute_source(
        r#"
            import pkg { sleepMillis } from "std/time";
            async fn helper() -> int { return 5; };
            async fn background() -> int {
                sleepMillis(millis: 100);
                return await helper();
            };
            async fn main() -> int {
                background();
                return 1 / 0;
            };
            "#,
    );
    let elapsed = start.elapsed();
    assert!(
        result.is_err(),
        "main's own division-by-zero error should still propagate"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "a subtask spawned by a task that outlives the entry point's own error \
         must not hang the process forever, took {elapsed:?}"
    );
}

// Regression: concurrent compound assignment to a module-level `var mut`
// global used to lose updates. `hits = hits + 1` compiled to a separate
// read_global call and a separate write_global call, each individually
// locked but not atomic as a *unit* — two tasks could both read the same
// value before either wrote back, dropping one increment per collision.
// This spawns 8 real tasks (via `all`, so they run concurrently on the
// worker pool, not one at a time) each incrementing the same global 2000
// times; an earlier, insufficient version of this test awaited each spawn
// before starting the next, which serializes them and never actually races.
#[test]
fn concurrent_global_increments_do_not_lose_updates() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { all } from "std/async";
            var mut hits: int = 0;
            async fn bump() -> int {
                for i in range(end: 2000) { hits = hits + 1; }
                return 0;
            };
            async fn main() -> int {
                var mut ps: [Promise<int>] = [];
                for i in range(end: 8) { ps.append(value: bump()); }
                var rs = await all<int>(promises: ps);
                return hits;
            };
            "#,
        )
        .expect("concurrent global increments should execute");
    assert_eq!(outcome.exit_status, 8 * 2000);
}

#[test]
fn conversion_builtins_execute_in_compiled_functions() {
    let source = r#"
        fn main() -> int {
            if str(value: 42) != "42" { return 1; }
            if int(value: " 41 ") != 41 { return 2; }
            if float(value: "1.5") != 1.5 { return 3; }
            if bool(value: "true") != true { return 4; }
            return 0;
        };
    "#;
    let outcome = Engine::default().execute_source(source).expect("conversions run");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn invalid_compiled_conversion_reports_the_value() {
    let source = r#"fn main() -> int { return int(value: "not a number"); };"#;
    let error = Engine::default().execute_source(source).expect_err("invalid int");
    let rendered = error.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(rendered.contains("cannot convert"), "{rendered}");
}

#[test]
fn interpolation_accepts_nested_string_literals() {
    let source = r##"fn main() -> int {
        if "hello ${"world"}" != "hello world" { return 1; }
        if "n=${int(value: "2")}" != "n=2" { return 2; }
        return 0;
    };"##;
    let outcome = Engine::default().execute_source(source).expect("nested string interpolation");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn structural_equality_compares_lists_tuples_structs_and_generic_values() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            struct Pair { left: int; right: List<int>; };
            fn same<T>(a: T, b: T) -> bool { return a == b; };
            fn main() -> int {
                if [1, 2] != [1, 2] { return 1; }
                if [1, 2] == [2, 1] { return 2; }
                if (1, "x") != (1, "x") { return 3; }
                if Pair(left: 1, right: [2]) != Pair(left: 1, right: [2]) { return 4; }
                if !same(a: [3], b: [3]) { return 5; }
                return 0;
            };
            "#,
        )
        .expect("structural equality should compile and run");
    assert_eq!(outcome.exit_status, 0);
}
