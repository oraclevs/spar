use spar::Engine;

#[test]
fn unified_struct_json_evaluates_defaults_for_each_missing_field() {
    let source = r#"
        import pkg { parse } from "std/json";
        var mut counter: int = 0;
        fn next() -> int { counter = counter + 1; return counter; };
        struct Inner { value: int = next(); };
        struct Row<T> { value: T; inner: Inner = Inner(); };
        fn main() -> int {
            var rows: List<Row<int>> = parse<List<Row<int>>>(text: "[{\"value\":1},{\"value\":2},{\"value\":3,\"inner\":{\"value\":99}}]");
            if rows[0].inner.value != 1 { return 1; }
            if rows[1].inner.value != 2 { return 2; }
            if rows[2].inner.value != 99 { return 3; }
            if counter != 2 { return 4; }
            return 0;
        };
    "#;
    assert_eq!(
        Engine::default()
            .execute_source(source)
            .unwrap()
            .exit_status,
        0
    );
}

#[test]
fn unified_struct_json_substitutes_generic_fields_and_requires_options() {
    let engine = Engine::default();
    let source = r#"
        import pkg { parse } from "std/json";
        struct Box<T> { value: T; note: Option<str>; };
        fn main() -> int {
            var box: Box<int> = parse<Box<int>>(text: "{\"value\":9,\"note\":null}");
            return box.value;
        };
    "#;
    assert_eq!(engine.execute_source(source).unwrap().exit_status, 9);
    let missing = source.replace(",\\\"note\\\":null", "");
    let errors = engine.execute_source(&missing).unwrap_err();
    assert!(format!("{errors:?}").contains("note"));
}

#[test]
fn unified_struct_schema_from_ordinary_import() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        "export struct Shape { port: int; };",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("schema.spar"),
        "import { Shape } from \"./models.spar\"; schema Server from Shape;",
    )
    .unwrap();
    Engine::default()
        .with_base_dir(dir.path())
        .check_source(
            r#"
        import schema "./schema.spar";
        struct Server { port: int = 8080; };
    "#,
        )
        .unwrap();
}

#[test]
fn unified_struct_emission_requires_concrete_default_instance() {
    let good = Engine::default().emit_source("#[emit] struct Settings { enabled: bool = true; };");
    assert!(good.errors.is_empty(), "{:?}", good.errors);
    for source in [
        "#[emit] struct Missing { value: int; };",
        "#[emit] struct Generic<T> { value: List<T> = []; };",
    ] {
        assert!(Engine::default().check_source(source).is_err(), "{source}");
    }
}
