use spar::{CompileOptions, Engine};

#[test]
fn str_methods_match_legacy_std_text_functions() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { trim, upper, contains, replace, split } from "std/text";

            function main() -> int {
                var raw: str = "  spar  ";
                var legacy: str = upper(value: trim(value: raw));
                var modern: str = raw.trim().upper();
                if legacy != modern { return 1; }
                if !modern.contains("PAR") { return 2; }
                if contains(value: modern, needle: "PAR") != modern.contains("PAR") { return 3; }
                if replace(value: modern, from: "SP", to: "St") != modern.replace("SP", "St") { return 4; }
                var pieces: [str] = "a,b,c".split(",");
                if pieces.length() != 3 { return 5; }
                return 0;
            };
            "#,
        )
        .expect("str methods should preserve std/text free-function behavior");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn bytes_and_list_methods_live_on_values_without_removing_free_functions() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { bytes } from "std/random";
            import pkg { map, filter, take, count, get } from "std/data";

            function main() -> int {
                var blob: Bytes = bytes(count: 4);
                if blob.length() != 4 { return 1; }
                if blob.isEmpty() { return 2; }

                var values: [int] = [1, 2, 3, 4, 5];
                var legacy: [int] = values
                    |> filter(fn(value: int) -> bool => value >= 3)
                    |> map(fn(value: int) -> int => value * 2)
                    |> take(2);
                var modern: [int] = values
                    .filter(fn(value: int) -> bool => value >= 3)
                    .map(fn(value: int) -> int => value * 2)
                    .take(2);

                if count(source: legacy) != modern.count() { return 3; }
                if get(source: legacy, key: 0) != modern.get(0) { return 4; }
                if modern.first() != 6 || modern.last() != 8 { return 5; }
                if modern.length() != 2 || modern.isEmpty() { return 6; }
                return 0;
            };
            "#,
        )
        .expect("Bytes/List methods should execute while legacy free functions remain available");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn map_methods_cover_length_keys_values_and_canonical_get() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { collectTable, groupBy, get } from "std/data";

            struct User { name: str = ""; team: str = ""; };

            function main() -> int {
                var users: Table<User> = [
                    User(name: "Obi", team: "red"),
                    User(name: "Ada", team: "blue"),
                    User(name: "Ngozi", team: "red")
                ] |> collectTable();
                var groups: Map<str, Table<User>> = users
                    |> groupBy(fn(user: User) -> str => user.team);

                if groups.length() != 2 || groups.isEmpty() { return 1; }
                if groups.keys().length() != 2 || groups.values().length() != 2 { return 2; }
                if groups.get("red").count() != get(source: groups, key: "red").count() { return 3; }
                if !groups.containsKey("blue") { return 4; }
                return 0;
            };
            "#,
        )
        .expect("Map methods should be available on Map<K,V>");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn option_and_result_are_first_class_generic_values_with_core_methods() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { some, none, ok, err } from "std";

            function main() -> int {
                var present: Option<int> = some<int>(value: 7);
                var absent: Option<int> = none<int>();
                if !present.isSome() || present.isNone() { return 1; }
                if !absent.isNone() || absent.isSome() { return 2; }
                if present.unwrap() != 7 { return 3; }
                if absent.unwrapOr(9) != 9 { return 4; }

                var success: Result<int, str> = ok<int, str>(value: 11);
                var failure: Result<int, str> = err<int, str>(error: "boom");
                if !success.isOk() || success.isErr() { return 5; }
                if !failure.isErr() || failure.isOk() { return 6; }
                if success.unwrap() != 11 { return 7; }
                if failure.unwrapErr() != "boom" { return 8; }
                if failure.unwrapOr(13) != 13 { return 9; }
                return 0;
            };
            "#,
        )
        .expect("Option<T> and Result<T,E> constructors/methods should compile and execute");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn option_and_result_unwrap_fail_strictly_on_the_wrong_variant() {
    let engine = Engine::new(CompileOptions::default());

    let option_error = engine
        .execute_source(
            r#"
            import pkg { none } from "std";
            function main() -> int {
                var value: Option<int> = none<int>();
                return value.unwrap();
            };
            "#,
        )
        .expect_err("unwrapping None must fail");
    assert!(option_error
        .iter()
        .any(|error| error.to_string().contains("cannot unwrap None")));

    let result_error = engine
        .execute_source(
            r#"
            import pkg { err } from "std";
            function main() -> int {
                var value: Result<int, str> = err<int, str>(error: "boom");
                return value.unwrap();
            };
            "#,
        )
        .expect_err("unwrapping Err must fail");
    assert!(result_error
        .iter()
        .any(|error| error.to_string().contains("cannot unwrap Err")));
}

#[test]
fn option_and_result_type_arities_are_checked() {
    let option_errors = Engine::new(CompileOptions::default())
        .check_source(
            r#"
            function badOption(value: Option<int, str>) -> int { return 0; };
            "#,
        )
        .expect_err("Option with two type arguments must be rejected");
    let option_rendered = option_errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(option_rendered.contains("Option") && option_rendered.contains("1 type argument"));

    let result_errors = Engine::new(CompileOptions::default())
        .check_source(
            r#"
            function badResult(value: Result<int>) -> int { return 0; };
            "#,
        )
        .expect_err("Result with one type argument must be rejected");
    let result_rendered = result_errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(result_rendered.contains("Result") && result_rendered.contains("2 type arguments"));
}

#[test]
fn generic_list_methods_infer_untyped_closure_parameters_and_results() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            function main() -> int {
                var values: [int] = [1, 2, 3];
                var doubled: [int] = values.map(fn(value) => value * 2);
                var selected: [int] = doubled.filter(fn(value) => value >= 4);
                return selected.first() + selected.last();
            };
            "#,
        )
        .expect("generic List methods should infer closure input/output types from the receiver");

    assert_eq!(outcome.exit_status, 10);
}
