use spar::{CompileOptions, Engine};

#[test]
fn async_all_composes_spar_promises_without_shelling_out() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { all } from "std/async";
            async function value(n: int) -> int { return n + 1; };
            async function main() -> int {
                var pending: [Promise<int>] = [value(n: 1), value(n: 2), value(n: 3)];
                var values: [int] = await all<int>(promises: pending);
                return values[0] + values[1] + values[2];
            };
            "#,
        )
        .expect("std/async all should execute on the Spar scheduler");
    assert_eq!(outcome.exit_status, 9);
}

#[test]
fn async_race_returns_the_first_scheduler_result() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { race } from "std/async";
            async function value(n: int) -> int { return n; };
            async function main() -> int {
                var pending: [Promise<int>] = [value(n: 7), value(n: 9)];
                return await race<int>(promises: pending);
            };
            "#,
        )
        .expect("std/async race should run on the Spar scheduler");
    assert_eq!(outcome.exit_status, 7);
}

#[test]
fn async_timeout_returns_completed_value_before_deadline() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { timeout } from "std/async";
            async function value() -> int { return 5; };
            async function main() -> int {
                return await timeout<int>(promise: value(), millis: 1000);
            };
            "#,
        )
        .expect("std/async timeout should return a completed promise");
    assert_eq!(outcome.exit_status, 5);
}

#[test]
fn async_race_rejects_an_empty_list() {
    let errors = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { race } from "std/async";
            async function main() -> int {
                var pending: [Promise<int>] = [];
                return await race<int>(promises: pending);
            };
            "#,
        )
        .expect_err("empty race must fail deterministically");
    assert!(errors.iter().any(|error| error.to_string().contains("race requires at least one promise")));
}

#[test]
fn async_timeout_reports_deadline_after_slow_task_finishes() {
    let errors = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { timeout } from "std/async";
            import pkg { sleepMillis } from "std/time";
            async function slow() -> int {
                sleepMillis(millis: 5);
                return 8;
            };
            async function main() -> int {
                return await timeout<int>(promise: slow(), millis: 1);
            };
            "#,
        )
        .expect_err("slow promise must report timeout");
    assert!(errors.iter().any(|error| error.to_string().contains("timed out")));
}

#[test]
fn empty_list_var_takes_its_declared_type() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            "function main() -> int { var values: [int] = []; return 3; };",
        )
        .expect("a declared empty list should type-check");
    assert_eq!(outcome.exit_status, 3);
}
