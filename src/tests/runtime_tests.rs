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
