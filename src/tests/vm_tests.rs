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
