use crate::runtime::execute_self_contained_entry;
use crate::{ConfigValue, Engine};

fn execute(source: &str) -> Result<ConfigValue, Vec<crate::SparError>> {
    let program = Engine::default().compile_source(source)?;
    execute_self_contained_entry(&program)
}

#[test]
fn compiled_function_uses_slots_across_nested_control_flow() {
    let value = execute("function main() -> int { var mut total: int = 0; for (index, value) in [2, 4, 6] { if index == 1 { continue; } total = total + value; } return total; };").unwrap();
    assert_eq!(value, ConfigValue::Int(8));
}

#[test]
fn compiled_functions_support_recursion_defaults_and_named_arguments() {
    let value = execute("function sum(value: int, carry: int = 0) -> int { if value == 0 { return carry; } return sum(carry: carry + value, value: value - 1); }; function main() -> int { return sum(value: 4); };").unwrap();
    assert_eq!(value, ConfigValue::Int(10));
}

#[test]
fn compiled_expressions_build_lists_objects_interpolation_and_comprehensions() {
    let value = execute("type [Result] { label: str; }; function main() -> int { var values: [int] = for value in [1, 2, 3] { value }; var object: Result = { label: \"sum-${values[0] + values[2]}\"; }; if object.label == \"sum-4\" { return 0; } return 1; };").unwrap();
    assert_eq!(value, ConfigValue::Int(0));
}

#[test]
fn compiled_integer_division_by_zero_is_an_error() {
    let errors = execute("function main() -> int { return 1 / 0; };").unwrap_err();
    assert!(errors[0].to_string().contains("division by zero"));
}

#[test]
fn compiled_try_catch_exposes_error_and_supports_ignored_binding() {
    let value = execute(
        r#"
        function main() -> int {
            try { var broken: int = 1 / 0; }
            catch err {
                if err.kind == "runtime" { return 7; }
                return 1;
            }
            try { var broken: int = 1 / 0; } catch { return 9; }
            return 0;
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, ConfigValue::Int(7));
}

#[test]
fn compiled_generic_functions_are_erased_and_reusable() {
    let value = execute(
        r#"
        function identity<T>(value: T) -> T { return value; };
        function main() -> int {
            var number: int = identity(value: 7);
            var word: str = identity<str>(value: "spar");
            if word == "spar" { return number; }
            return 0;
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, ConfigValue::Int(7));
}

#[test]
fn compiled_generic_named_types_substitute_nested_fields() {
    let value = execute(
        r#"
        type [Box<T>] { value: T; };
        function unbox<T>(box: Box<T>) -> T { return box.value; };
        function main() -> int {
            var boxed: Box<int> = { value: 9; };
            return unbox(box: boxed);
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, ConfigValue::Int(9));
}

#[test]
fn compiled_generic_function_can_construct_applied_return_type() {
    let value = execute(
        r#"
        type [Box<T>] { value: T; };
        function box<T>(value: T) -> Box<T> { return { value: value; }; };
        function main() -> int {
            var boxed: Box<int> = box(value: 13);
            return boxed.value;
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, ConfigValue::Int(13));
}

#[test]
fn compiled_function_group_member_can_be_generic() {
    let value = execute(
        r#"
        functionGroup Values {
            function identity<T>(value: T) -> T { return value; }
        };
        function main() -> int { return Values::identity(value: 17); };
        "#,
    )
    .unwrap();
    assert_eq!(value, ConfigValue::Int(17));
}

#[test]
fn compiled_generic_calls_specialize_callers_and_recurse_erased() {
    let value = execute(
        r#"
        function identity<T>(value: T) -> T { return value; };
        function repeat<T>(value: T, count: int) -> T {
            if count == 0 { return value; }
            return repeat(value: value, count: count - 1);
        };
        function main() -> int {
            return identity(value: 4) + repeat(value: 5, count: 2);
        };
        "#,
    )
    .unwrap();
    assert_eq!(value, ConfigValue::Int(9));
}
