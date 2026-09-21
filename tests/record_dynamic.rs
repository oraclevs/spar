use spar::Engine;

fn run(body: &str) -> Result<i32, String> {
    let source = format!(
        r#"
        function main() -> int {{
            var payload: Record = {{ name: "Ada"; active: true; age: 3; ratio: 2; }};
            {body}
            return 0;
        }};
        "#
    );
    Engine::default()
        .execute_source(&source)
        .map(|outcome| outcome.exit_status)
        .map_err(|errors| format!("{errors:?}"))
}

#[test]
fn record_fields_are_dynamic_and_compare_with_primitives() {
    let status = run(r#"
        if payload.name != "Ada" { return 1; }
        if payload.age == 4 { return 2; }
        if payload.active != true { return 3; }
    "#)
    .unwrap();
    assert_eq!(status, 0);
}

#[test]
fn as_methods_bridge_dynamic_values_to_static_types() {
    let status = run(r#"
        var name: str = payload.name.asStr();
        var age: int = payload.age.asInt();
        var active: bool = payload.active.asBool();
        var ratio: float = payload.ratio.asFloat();
        if name != "Ada" { return 1; }
        if age + 1 != 4 { return 2; }
        if !active { return 3; }
        if ratio != 2.0 { return 4; }
        if !payload.has("name") { return 5; }
        if payload.has("missing") { return 6; }
        if payload.keys().length() != 4 { return 7; }
    "#)
    .unwrap();
    assert_eq!(status, 0);
}

#[test]
fn a_dynamic_value_is_not_silently_a_static_type() {
    let error = run(r#"var name: str = payload.name;"#).unwrap_err();
    assert!(error.contains("TypeError"), "{error}");
    let error = run(r#"var age: int = payload.age + 1;"#).unwrap_err();
    assert!(error.contains("TypeError"), "{error}");
}

#[test]
fn a_wrong_conversion_fails_at_runtime_with_a_clear_message() {
    let error = run(r#"var age: int = payload.name.asInt();"#).unwrap_err();
    assert!(error.contains("asInt() expected a int value"), "{error}");
}

#[test]
fn dynamic_values_order_against_numbers_and_strings() {
    let status = run(r#"
        if !(payload.age > 2) { return 1; }
        if payload.age >= 4 { return 2; }
        if !(payload.age < 3.5) { return 3; }
        if !(payload.name < "Bob") { return 4; }
        if !(payload.ratio <= 2) { return 5; }
    "#)
    .unwrap();
    assert_eq!(status, 0);
    let error = run(r#"if payload.name > 1 { return 1; }"#).unwrap_err();
    assert!(error.contains("cannot order"), "{error}");
}
