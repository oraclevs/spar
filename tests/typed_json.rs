use spar::{CompileOptions, Engine};

fn engine() -> Engine {
    Engine::new(CompileOptions::default())
}

fn run(source: &str) -> Result<i32, String> {
    engine()
        .execute_source(source)
        .map(|outcome| outcome.exit_status)
        .map_err(|errors| format!("{errors:?}"))
}

#[test]
fn typed_json_decodes_scalars_lists_maps_records_and_null_options() {
    let status = run(r#"
        import pkg { parse } from "std/json";

        fn main() -> int {
            var text: str = parse<str>(text: "\"spar\"");
            var enabled: bool = parse<bool>(text: "true");
            var count: int = parse<int>(text: "42");
            var ratio: float = parse<float>(text: "2.5");
            var absent: Option<int> = parse<Option<int>>(text: "null");
            var numbers: List<int> = parse<List<int>>(text: "[1,2,3]");
            var mapping: Map<str, int> = parse<Map<str, int>>(text: "{\"one\":1,\"two\":2}");
            var record: Record = parse<Record>(text: "{\"name\":\"Ada\",\"active\":true}");

            if text != "spar" { return 1; }
            if !enabled { return 2; }
            if count != 42 { return 3; }
            if ratio != 2.5 { return 4; }
            if !absent.isNone() { return 5; }
            if numbers.get(index: 2).unwrap() != 3 { return 6; }
            if mapping.get(key: "two").unwrap() != 2 { return 7; }
            if record.name.asStr() != "Ada" { return 8; }
            if !record.active.asBool() { return 9; }
            return 0;
        };
    "#)
    .expect("typed JSON primitives and collections should decode");

    assert_eq!(status, 0);
}

#[test]
fn typed_json_materializes_nested_structs_and_option_fields() {
    let status = run(r#"
        import pkg { parse } from "std/json";

        struct Author {
            id: int = 0;
            login: str = "";
        };

        struct Release {
            tagName: str = "";
            author: Author = Author();
            note: Option<str> = none();
        };

        fn main() -> int {
            var present: Release = parse<Release>(
                text: "{\"tagName\":\"v1.0.0\",\"author\":{\"id\":7,\"login\":\"obi\"},\"note\":\"stable\"}"
            );
            var missing: Release = parse<Release>(
                text: "{\"tagName\":\"v1.1.0\",\"author\":{\"id\":8,\"login\":\"ada\"}}"
            );
            var explicitNull: Release = parse<Release>(
                text: "{\"tagName\":\"v1.2.0\",\"author\":{\"id\":9,\"login\":\"lin\"},\"note\":null}"
            );

            if present.author.id != 7 { return 1; }
            if present.note.unwrap() != "stable" { return 2; }
            if !missing.note.isNone() { return 3; }
            if !explicitNull.note.isNone() { return 4; }
            return 0;
        };
    "#)
    .expect("typed JSON should recursively materialize declared structs");

    assert_eq!(status, 0);
}

#[test]
fn typed_json_error_reports_the_full_nested_path_for_wrong_field_type() {
    let error = run(r#"
        import pkg { parse } from "std/json";

        struct Author { id: int = 0; };
        struct Release { author: Author = Author(); };

        fn main() -> int {
            var releases: List<Release> = parse<List<Release>>(
                text: "[{\"author\":{\"id\":1}},{\"author\":{\"id\":\"bad\"}}]"
            );
            return releases.length();
        };
    "#)
    .expect_err("wrong nested JSON field type must fail");

    assert!(error.contains("[1].author.id"), "{error}");
    assert!(error.contains("expected int"), "{error}");
    assert!(error.contains("str") || error.contains("string"), "{error}");
}

#[test]
fn typed_json_error_reports_missing_required_field_path() {
    let error = run(r#"
        import pkg { parse } from "std/json";

        struct Address { city: str; };
        struct User { name: str; address: Address; };

        fn main() -> int {
            var user: User = parse<User>(
                text: "{\"name\":\"Ada\",\"address\":{}}"
            );
            return 0;
        };
    "#)
    .expect_err("missing required JSON field must fail");

    assert!(error.contains("address.city"), "{error}");
    assert!(error.contains("missing") || error.contains("required"), "{error}");
}

#[test]
fn typed_json_error_reports_root_type_mismatch() {
    let error = run(r#"
        import pkg { parse } from "std/json";

        fn main() -> int {
            var values: List<int> = parse<List<int>>(text: "{\"value\":1}");
            return values.length();
        };
    "#)
    .expect_err("root JSON shape mismatch must fail");

    assert!(error.contains("expected List<int>") || error.contains("expected List"), "{error}");
    assert!(error.contains("object") || error.contains("Record"), "{error}");
}


#[test]
fn json_stringify_accepts_any_and_encodes_option_values() {
    let status = run(r#"
        import pkg { stringify } from "std/json";

        fn main() -> int {
            var missing: Option<int> = none();
            var present: Option<int> = some(value: 7);
            if stringify(value: missing) != "null" { return 1; }
            if stringify(value: present) != "7" { return 2; }
            if stringify(value: [1, 2]) != "[1,2]" { return 3; }
            return 0;
        };
    "#)
    .expect("JSON stringify should accept ordinary Spar values through Any");

    assert_eq!(status, 0);
}
