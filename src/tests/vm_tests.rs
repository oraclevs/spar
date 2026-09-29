//! Differential tests: every program runs on the register VM and on the
//! tree-walking runtime (VM disabled) and the observable results must match.

use std::sync::Arc;

use crate::runtime::execute_self_contained_entry;
use crate::vm::VmProgram;
use crate::{Engine, Value};

fn run(source: &str, vm: bool) -> (Result<Value, String>, usize) {
    let mut program = Engine::default().compile_source(source).expect("compiles");
    let lowered = (0..64)
        .filter(|i| program.vm.get(crate::compiled::FunctionId(*i)).is_some())
        .count();
    if !vm {
        Arc::get_mut(&mut program).expect("unique").vm = VmProgram::empty();
    }
    let result = execute_self_contained_entry(&program).map_err(|errors| {
        errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    });
    (result, lowered)
}

/// Like `run`, but through the real engine entry point (module state is set
/// up, so prelude functions such as `range` work). `main` must return `int`.
fn run_engine(source: &str, vm: bool) -> Result<i32, String> {
    let engine = Engine::default();
    let mut program = engine.compile_source(source).expect("compiles");
    if !vm {
        Arc::get_mut(&mut program).expect("unique").vm = VmProgram::empty();
    }
    engine
        .execute_compiled(&program)
        .map(|outcome| outcome.exit_status)
        .map_err(|errors| {
            errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        })
}

fn differential_engine(source: &str) -> Result<i32, String> {
    let with_vm = run_engine(source, true);
    let without_vm = run_engine(source, false);
    assert_eq!(with_vm, without_vm, "VM and tree walker disagree");
    with_vm
}

/// Asserts both tiers agree and that the VM actually lowered something.
fn differential(source: &str) -> Result<Value, String> {
    let (with_vm, lowered) = run(source, true);
    let (without_vm, _) = run(source, false);
    assert!(
        lowered > 0,
        "expected at least one function to lower to bytecode"
    );
    assert_eq!(with_vm, without_vm, "VM and tree walker disagree");
    with_vm
}

#[test]
fn recursive_fib_matches_tree_walker() {
    let v = differential(
        r#"
        fn fib(n: int) -> int {
            if n <= 1 { return n; }
            return fib(n: n - 1) + fib(n: n - 2);
        };
        fn main() -> int { return fib(n: 20); };
        "#,
    );
    assert_eq!(v, Ok(Value::Int(6765)));
}

#[test]
fn float_bool_and_short_circuit_match() {
    let v = differential(
        r#"
        fn mix(x: float, y: float, flag: bool) -> float {
            var mut acc: float = x * y - 1.5;
            if flag && acc > 0.0 || !flag { acc = acc / 2.0; } else { acc = -acc; }
            return acc + 0.25;
        };
        fn main() -> float { return mix(x: 3.0, y: 2.0, flag: true) + mix(x: 1.0, y: 1.0, flag: false); };
        "#,
    );
    assert!(matches!(v, Ok(Value::Float(_))));
}

#[test]
fn mutual_recursion_and_nested_call_arguments_match() {
    let v = differential(
        r#"
        fn isEven(n: int) -> bool { if n == 0 { return true; } return isOdd(n: n - 1); };
        fn isOdd(n: int) -> bool { if n == 0 { return false; } return isEven(n: n - 1); };
        fn add3(a: int, b: int, c: int) -> int { return a + b * 10 + c * 100; };
        fn main() -> int {
            if isEven(n: 10) && isOdd(n: 10) { return 0 - 1; }
            var r: int = add3(a: add3(a: 1, b: 2, c: 3), b: 1, c: 4);
            return r;
        };
        "#,
    );
    assert!(v.is_ok());
}

#[test]
fn division_by_zero_reports_the_same_error() {
    let v = differential(
        r#"
        fn div(a: int, b: int) -> int { return a / b; };
        fn main() -> int { return div(a: 1, b: 0); };
        "#,
    );
    assert!(v.unwrap_err().contains("division by zero"));
}

#[test]
fn runaway_recursion_reports_the_depth_error() {
    let v = differential(
        r#"
        fn down(n: int) -> int { return down(n: n + 1); };
        fn main() -> int { return down(n: 0); };
        "#,
    );
    assert!(v.unwrap_err().contains("maximum function call depth"));
}

#[test]
fn skipped_default_parameter_falls_back_to_the_tree_walker() {
    let v = differential(
        r#"
        fn scale(n: int, factor: int = 3) -> int { return n * factor; };
        fn main() -> int { return scale(n: 5) + scale(n: 2, factor: 10); };
        "#,
    );
    assert_eq!(v, Ok(Value::Int(35)));
}

#[test]
fn range_loops_with_break_continue_and_nesting_match() {
    let v = differential_engine(
        r#"
        fn sumTo(n: int) -> int {
            var mut total: int = 0;
            for i in range(end: n) {
                if i == 7 { continue; }
                if i > 500 { break; }
                total = total + i;
            }
            return total;
        };
        fn nested(n: int) -> int {
            var mut c: int = 0;
            for a in rangeFrom(start: 2, end: n) {
                for (j, b) in rangeFrom(start: a, end: n) {
                    c = c + j + b - a;
                }
            }
            return c;
        };
        fn empty(n: int) -> int {
            var mut c: int = 5;
            for i in rangeFrom(start: 10, end: n) { c = c + i; }
            return c;
        };
        fn main() -> int { return sumTo(n: 1000) + nested(n: 40) + empty(n: 3); };
        "#,
    );
    assert!(v.is_ok());
}

#[test]
fn loop_bodies_calling_lowered_functions_match() {
    let v = differential_engine(
        r#"
        fn sq(x: int) -> int { return x * x; };
        fn total(n: int) -> int {
            var mut acc: int = 0;
            for i in range(end: n) { acc = acc + sq(x: i); }
            return acc;
        };
        fn main() -> int { return total(n: 100); };
        "#,
    );
    assert_eq!(v, Ok(328350));
}

/// `Value` is moved and returned by value on every evaluator step; keep it
/// small. Large payloads live behind `Shared`/`Box`.
#[test]
fn value_stays_small() {
    let size = std::mem::size_of::<Value>();
    assert!(size <= 40, "Value grew to {size} bytes");
}

// ── Tier 2 (universal bytecode) vs the tree walker ──────────────────────────

fn run_engine_tier2(source: &str, bytecode: bool) -> Result<i32, String> {
    let engine = Engine::default();
    let mut program = engine.compile_source(source).expect("compiles");
    {
        let p = Arc::get_mut(&mut program).expect("unique");
        p.vm = VmProgram::empty();
        if !bytecode {
            p.bc = crate::runtime::bytecode::BcProgram::empty();
        }
    }
    engine
        .execute_compiled(&program)
        .map(|outcome| outcome.exit_status)
        .map_err(|errors| {
            errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        })
}

fn tier2_differential(source: &str) -> Result<i32, String> {
    let with_bc = run_engine_tier2(source, true);
    let without = run_engine_tier2(source, false);
    assert_eq!(with_bc, without, "bytecode tier and tree walker disagree");
    with_bc
}

#[test]
fn tier2_strings_records_and_lists_match() {
    let v = tier2_differential(
        r#"
        struct Point { x: int; y: int; };
        fn norm2(p: Point) -> int { return p.x * p.x + p.y * p.y; };
        fn label(i: int) -> str { return "n" + i.toString(); };
        fn main() -> int {
            var mut total: int = 0;
            for i in range(end: 50) {
                var p: Point = Point(x: i, y: i + 1);
                total = total + norm2(p: p) + label(i: i).length();
            }
            var items: [int] = [3, 1, 2];
            for (index, item) in items { total = total + index * item; }
            return total - 40000;
        };
        "#,
    );
    assert!(v.is_ok());
}

#[test]
fn tier2_try_catch_break_and_continue_inside_loops_match() {
    let v = tier2_differential(
        r#"
        fn boom(n: int) -> int { return 10 / n; };
        fn main() -> int {
            var mut acc: int = 0;
            for i in range(end: 10) {
                try {
                    if i == 8 { break; }
                    if i == 2 { continue; }
                    acc = acc + boom(n: i - 3);
                } catch error {
                    acc = acc + 1000;
                }
            }
            return acc;
        };
        "#,
    );
    assert!(v.is_ok());
}

#[test]
fn tier2_exit_and_early_return_match() {
    let v = tier2_differential(
        r#"
        import pkg { exit } from "std/process";
        fn stop() -> int { exit(code: 17); return 1; };
        fn main() -> int {
            var mut n: int = 0;
            for i in range(end: 5) { n = n + i; }
            var r: int = stop();
            return r + n + 99;
        };
        "#,
    );
    assert_eq!(v, Ok(17));
}

// ── Native (Cranelift) backend vs the bytecode interpreter ───────────────────

/// Runs every lowered function of `source` on both backends over a grid of
/// integer arguments and requires identical results or identical errors.
#[cfg(not(target_arch = "wasm32"))]
fn native_matches_interpreter(source: &str, args: &[&[u64]]) {
    use crate::compiled::FunctionId;
    use crate::vm::VmState;
    let program = Engine::default().compile_source(source).expect("compiles");
    let mut checked = 0;
    for index in 0..64u32 {
        let id = FunctionId(index);
        let Some(function) = program.vm.get(id) else {
            continue;
        };
        for call_args in args.iter().filter(|a| a.len() == function.params.len()) {
            let native = program.vm.run_native(id, call_args, 0);
            let Some(native) = native else { return }; // no native backend on this host
            let mut state = VmState::default();
            let interpreted = program.vm.run_interpreted(&mut state, id, call_args, 0);
            assert_eq!(
                native.map_err(|e| e.to_string()),
                interpreted.map_err(|e| e.to_string()),
                "function {index} args {call_args:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "no function was compared");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_backend_matches_interpreter_on_integer_code() {
    native_matches_interpreter(
        r#"
        fn fib(n: int) -> int { if n <= 1 { return n; } return fib(n: n - 1) + fib(n: n - 2); };
        fn collatz(n: int) -> int {
            var mut steps: int = 0;
            var mut x: int = n;
            for i in range(end: 200) {
                if x <= 1 { break; }
                if x - (x / 2) * 2 == 0 { x = x / 2; } else { x = x * 3 + 1; }
                steps = steps + 1;
            }
            return steps;
        };
        fn div(a: int, b: int) -> int { return a / b; };
        fn logic(a: int, b: int) -> int {
            if a < b && b != 0 || a == 7 { return a - b; }
            if !(a > b) { return a * b; }
            return -a;
        };
        fn down(n: int) -> int { return down(n: n + 1); };
        fn add(a: int, b: int) -> int { return a + b; };
        fn sub(a: int, b: int) -> int { return a - b; };
        fn mul(a: int, b: int) -> int { return a * b; };
        fn neg(a: int) -> int { return -a; };
        fn add1(a: int) -> int { return a + 1; };
        fn sub1(a: int) -> int { return a - 1; };
        fn main() -> int { return 0; };
        "#,
        &[
            &[0],
            &[1],
            &[7],
            &[20],
            &[(-5i64) as u64],
            &[10, 3],
            &[10, 0],
            &[i64::MIN as u64, (-1i64) as u64],
            &[7, 7],
            &[3, 9],
            &[9, 3],
            &[(-4i64) as u64, 0],
            &[i64::MAX as u64, 1],
            &[i64::MIN as u64, 1],
            &[i64::MAX as u64, 2],
            &[i64::MIN as u64, 2],
            &[i64::MAX as u64, (-1i64) as u64],
            &[3_037_000_500, 3_037_000_500],
            &[i64::MAX as u64],
            &[i64::MIN as u64],
        ],
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_backend_matches_interpreter_on_float_and_bool_code() {
    native_matches_interpreter(
        r#"
        fn mix(x: float, y: float, flag: bool) -> float {
            var mut acc: float = x * y - 1.5;
            if flag && acc > 0.0 || !flag { acc = acc / 2.0; } else { acc = -acc; }
            return acc + 0.25;
        };
        fn fdiv(a: float, b: float) -> float { return a / b; };
        fn cmp(a: float, b: float) -> bool { return a < b || a == b; };
        fn main() -> int { return 0; };
        "#,
        &[
            &[1.5f64.to_bits(), 2.0f64.to_bits(), 1],
            &[3.0f64.to_bits(), 2.0f64.to_bits(), 0],
            &[(-1.0f64).to_bits(), 0.5f64.to_bits(), 1],
            &[1.0f64.to_bits(), 0.0f64.to_bits()],
            &[f64::NAN.to_bits(), 1.0f64.to_bits()],
            &[2.0f64.to_bits(), 2.0f64.to_bits()],
        ],
    );
}

#[test]
fn integer_overflow_is_an_error_on_every_tier() {
    let source = r#"
        fn add(a: int, b: int) -> int { return a + b; };
        fn main() -> int {
            var big: int = 9223372036854775807;
            return add(a: big, b: 1);
        };
    "#;
    for bytecode in [true, false] {
        let result = run_engine_tier2(source, bytecode);
        let error = result.expect_err("overflow must not wrap");
        assert!(error.contains("integer overflow in addition"), "{error}");
    }
    // The primitive tiers (interpreter and native) agree with the tree walker.
    let with_vm = run_engine(source, true).expect_err("vm overflow");
    let without = run_engine(source, false).expect_err("tree overflow");
    assert_eq!(with_vm, without);
}

#[test]
fn explicit_wrapping_saturating_and_checked_methods_behave() {
    let v = differential_engine(
        r#"
        fn main() -> int {
            var big: int = 9223372036854775807;
            var small: int = 0 - 9223372036854775807 - 1;
            if big.wrappingAdd(other: 1) != small { return 1; }
            if big.saturatingAdd(other: 10) != big { return 2; }
            if !big.checkedAdd(other: 1).isNone() { return 3; }
            if big.checkedSub(other: 1).isNone() { return 4; }
            if small.wrappingNeg() != small { return 5; }
            return 0;
        };
        "#,
    );
    assert_eq!(v, Ok(0));
}

// ── while / loop ────────────────────────────────────────────────────────────

const WHILE_PROGRAM: &str = r#"
    fn countdown(n: int) -> int {
        var mut i: int = n;
        var mut steps: int = 0;
        while i > 0 {
            i = i - 1;
            if i == 3 { continue; }
            steps = steps + 1;
        }
        return steps;
    };
    fn firstSquareOver(limit: int) -> int {
        var mut n: int = 0;
        loop {
            n = n + 1;
            if n * n > limit { return n; }
        }
    };
    fn collatz(start: int) -> int {
        var mut n: int = start;
        var mut steps: int = 0;
        while n != 1 {
            if n - (n / 2) * 2 == 0 { n = n / 2; } else { n = 3 * n + 1; }
            steps = steps + 1;
        }
        return steps;
    };
    fn nested(n: int) -> int {
        var mut total: int = 0;
        var mut a: int = 0;
        while a < n {
            var mut b: int = 0;
            loop {
                if b >= a { break; }
                total = total + b;
                b = b + 1;
            }
            a = a + 1;
        }
        return total;
    };
    fn neverRuns() -> int {
        var mut x: int = 7;
        while false { x = 0; }
        return x;
    };
    fn main() -> int {
        return countdown(n: 10) * 1000000 + firstSquareOver(limit: 50) * 10000
            + collatz(start: 27) + nested(n: 6) * 100000000 / 100000000 + neverRuns();
    };
"#;

#[test]
fn while_and_loop_match_across_tiers_and_compute_the_right_value() {
    // countdown: 10 iterations, one `continue` -> 9; firstSquareOver(50) = 8; collatz(27) = 111;
    // nested(6) = 0+0+1+3+6+10 = 20; neverRuns = 7
    let expected = 9 * 1_000_000 + 8 * 10_000 + 111 + 20 + 7;
    assert_eq!(differential_engine(WHILE_PROGRAM), Ok(expected));
    assert_eq!(tier2_differential(WHILE_PROGRAM), Ok(expected));
}

#[test]
fn while_with_non_primitive_state_matches_on_the_bytecode_tier() {
    let v = tier2_differential(
        r#"
        fn main() -> int {
            var mut text: str = "";
            var mut i: int = 0;
            while i < 5 {
                text = text + i.toString();
                i = i + 1;
            }
            var mut items: [int] = [];
            loop {
                if items.length() == 4 { break; }
                items.append(value: items.length());
            }
            return text.length() * 10 + items.length();
        };
        "#,
    );
    assert_eq!(v, Ok(54));
}

#[test]
fn while_bodies_run_the_tree_walker_when_forced_off_every_tier() {
    // Same source through the plain tree walker (both fast tiers disabled).
    let engine = Engine::default();
    let mut program = engine.compile_source(WHILE_PROGRAM).expect("compiles");
    {
        let p = Arc::get_mut(&mut program).expect("unique");
        p.vm = VmProgram::empty();
        p.bc = crate::runtime::bytecode::BcProgram::empty();
    }
    let outcome = engine.execute_compiled(&program).expect("runs");
    assert_eq!(outcome.exit_status, 9 * 1_000_000 + 8 * 10_000 + 111 + 20 + 7);
}

// ── % modulo ────────────────────────────────────────────────────────────────

const REM_PROGRAM: &str = r#"
    fn rem(a: int, b: int) -> int { return a % b; };
    fn remf(a: float, b: float) -> float { return a % b; };
    fn main() -> int {
        var mut score: int = 0;
        // Rust semantics: the result takes the sign of the dividend.
        if rem(a: 7, b: 3) == 1 { score = score + 1; }
        if rem(a: -7, b: 3) == -1 { score = score + 2; }
        if rem(a: 7, b: -3) == 1 { score = score + 4; }
        if rem(a: -7, b: -3) == -1 { score = score + 8; }
        if rem(a: 6, b: 3) == 0 { score = score + 16; }
        if remf(a: 7.5, b: 2.0) == 1.5 { score = score + 32; }
        if remf(a: -7.5, b: 2.0) == -1.5 { score = score + 64; }
        var mut i: int = 0;
        var mut evens: int = 0;
        while i < 100 {
            if i % 2 == 0 { evens = evens + 1; }
            i = i + 1;
        }
        return score * 1000 + evens;
    };
"#;

#[test]
fn modulo_follows_the_dividend_sign_on_every_tier() {
    let expected = 127 * 1000 + 50;
    assert_eq!(differential_engine(REM_PROGRAM), Ok(expected));
    assert_eq!(tier2_differential(REM_PROGRAM), Ok(expected));
}

#[test]
fn modulo_by_zero_and_overflow_are_runtime_errors_on_every_tier() {
    for (body, needle) in [
        ("fn f(a: int, b: int) -> int { return a % b; }; fn main() -> int { return f(a: 5, b: 0); };", "division by zero"),
        ("fn f(a: float, b: float) -> float { return a % b; }; fn main() -> int { var x: float = f(a: 5.0, b: 0.0); return 0; };", "division by zero"),
        ("fn f(a: int, b: int) -> int { return a % b; }; fn main() -> int { return f(a: -9223372036854775807 - 1, b: -1); };", "overflow"),
    ] {
        let vm = run_engine(body, true).unwrap_err();
        let tree = run_engine(body, false).unwrap_err();
        let bytecode = run_engine_tier2(body, true).unwrap_err();
        for message in [&vm, &tree, &bytecode] {
            assert!(message.contains(needle), "{needle}: {message}");
        }
    }
}
