use spar::Engine;

fn status(source: &str) -> i32 {
    Engine::default()
        .execute_source(source)
        .unwrap_or_else(|errors| panic!("should run: {errors:?}\n{source}"))
        .exit_status
}

fn error(source: &str) -> String {
    match Engine::default().execute_source(source) {
        Ok(outcome) => panic!("expected an error, got status {}", outcome.exit_status),
        Err(errors) => format!("{errors:?}"),
    }
}

#[test]
fn generic_constructors_take_their_type_from_the_return_type() {
    assert_eq!(
        status(
            r#"
            import pkg { ok, err, some, none } from "std";
            function safeDiv(a: int, b: int) -> Result<int, str> {
                if b == 0 { return err(error: "div by zero"); }
                return ok(value: a / b);
            };
            function firstOrNone(flag: bool) -> Option<str> {
                if flag { return some(value: "x"); }
                return none();
            };
            function main() -> int {
                var good: Result<int, str> = safeDiv(a: 9, b: 3);
                var bad: Result<int, str> = safeDiv(a: 1, b: 0);
                if !good.isOk() { return 1; }
                if !bad.isErr() { return 2; }
                if !firstOrNone(flag: false).isNone() { return 3; }
                return good.unwrap();
            };
            "#
        ),
        3
    );
}

#[test]
fn generic_constructors_take_their_type_from_declared_variables_and_assignments() {
    assert_eq!(
        status(
            r#"
            import pkg { ok, err, some, none } from "std";
            function main() -> int {
                var a: Option<int> = none();
                var b: Result<int, str> = err(error: "boom");
                var mut c: Option<int> = none();
                c = some(value: 7);
                if !a.isNone() { return 1; }
                if !b.isErr() { return 2; }
                return c.unwrapOr(0);
            };
            "#
        ),
        7
    );
}

#[test]
fn an_expected_type_that_disagrees_is_still_an_error() {
    let message = error(
        r#"
        import pkg { ok } from "std";
        function main() -> int {
            var r: Result<int, str> = ok(value: "not an int");
            return 0;
        };
        "#,
    );
    assert!(message.contains("TypeError"), "{message}");
}

#[test]
fn without_any_expected_type_inference_still_asks_for_explicit_arguments() {
    let message = error(
        r#"
        import pkg { none } from "std";
        function main() -> int {
            var x: int = none().unwrapOr(1);
            return x;
        };
        "#,
    );
    assert!(message.contains("cannot infer"), "{message}");
}
