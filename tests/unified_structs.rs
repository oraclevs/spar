use spar::Engine;

#[test]
fn unified_struct_instances_can_be_spread_into_dynamic_records() {
    assert_eq!(
        run(r#"
        struct Item { value: int = 7; };
        var item: Item = Item();
        var record: Record = { ...item; extra: 2; };
        fn main() -> int {
            var merged: Record = { ...Item(); value: 9; };
            if record.value != 7 { return 1; }
            if merged.value != 9 { return 2; }
            return 0;
        };
    "#),
        0
    );
}

fn run(source: &str) -> i32 {
    Engine::default()
        .execute_source(source)
        .unwrap()
        .exit_status
}

#[test]
fn unified_struct_requires_only_fields_without_defaults() {
    assert_eq!(
        run(r#"
        struct User { name: str; age: int = 0; };
        fn main() -> int {
            var user: User = User(name: "Ada");
            return user.age;
        };
    "#),
        0
    );
}

#[test]
fn unified_struct_rejects_removed_type_declaration() {
    let errors = Engine::default()
        .check_source("type User { name: str; };")
        .unwrap_err();
    assert!(format!("{errors:?}").contains("struct"));
}

#[test]
fn unified_struct_supports_generic_constructors() {
    assert_eq!(
        run(r#"
        struct Page<T> { items: List<T>; total: int = 7; };
        fn main() -> int {
            var page: Page<str> = Page<str>(items: ["Ada"]);
            return page.total;
        };
    "#),
        7
    );
}

#[test]
fn unified_struct_rejects_binding_and_untyped_fields() {
    for source in [
        "struct User { name = \"Ada\"; };",
        "struct User: Other { name = \"Ada\"; };",
    ] {
        assert!(Engine::default().check_source(source).is_err(), "{source}");
    }
}

#[test]
fn unified_struct_format_is_idempotent() {
    let once =
        spar::formatter::format_source("struct Page<T>{items:List<T>;total:int=0;};").unwrap();
    assert!(once.contains("items: List<T>;"));
    assert!(once.contains("total: int = 0;"));
    assert_eq!(spar::formatter::format_source(&once).unwrap(), once);
}

#[test]
fn unified_struct_defaults_run_per_instance_in_module_scope() {
    assert_eq!(
        run(r#"
        var mut counter: int = 0;
        var seed: int = 10;
        fn next() -> int { counter = counter + 1; return counter; };
        struct Item { id: int = next(); value: int = seed; };
        fn main() -> int {
            var seed: int = 99;
            var first: Item = Item();
            var second: Item = Item();
            if first.id != 1 { return 1; }
            if second.id != 2 { return 2; }
            if first.value != 10 { return 3; }
            return 0;
        };
    "#),
        0
    );
}

#[test]
fn unified_struct_rejects_implicit_instances_and_missing_options() {
    for source in [
        "struct Item { value: int = 1; }; var item: Item = Item;",
        "struct Item { value: int = 1; }; var n: int = Item.value;",
        "struct Item { value: Option<int>; }; var item: Item = Item();",
        "import type { Item } from \"./models.spar\";",
    ] {
        assert!(Engine::default().check_source(source).is_err(), "{source}");
    }
}

#[test]
fn unified_struct_generic_method_format_preserves_lexical_parameters() {
    let source = "struct Box<T> { value: T; }; impl<T> Box<T> { fn read(self) -> T { return self.value; }; };";
    let formatted = spar::formatter::format_source(source).unwrap();
    assert!(formatted.contains("fn read(self)"), "{formatted}");
    assert_eq!(
        spar::formatter::format_source(&formatted).unwrap(),
        formatted
    );
    Engine::default().check_source(&formatted).unwrap();
}

#[test]
fn unified_struct_default_comprehension_preserves_caller_locals() {
    assert_eq!(
        run(r#"
        struct Batch { values: List<int> = for value in [1, 2, 3] { value }; };
        fn main() -> int {
            var sentinel: int = 47;
            var batch: Batch = Batch();
            if batch.values[2] != 3 { return 1; }
            if sentinel != 47 { return 2; }
            return 0;
        };
    "#),
        0
    );
}

#[test]
fn unified_struct_top_level_constructor_accepts_required_fields() {
    assert_eq!(
        run(r#"
        struct Item { value: int; };
        var item: Item = Item(value: 9);
        fn main() -> int { return item.value; };
    "#),
        9
    );
}

#[test]
fn unified_struct_mutable_defaults_are_independent() {
    assert_eq!(
        run(r#"
        struct Bucket { values: List<int> = []; };
        fn main() -> int {
            var mut first: Bucket = Bucket();
            var second: Bucket = Bucket();
            first.values.append(value: 7);
            if first.values.length() != 1 { return 1; }
            return second.values.length();
        };
    "#),
        0
    );
}

#[test]
fn unified_struct_constructor_reports_invalid_arguments() {
    for arguments in ["", "value: \"bad\"", "value: 1, value: 2", "other: 1"] {
        let source = format!("struct Item {{ value: int; }}; var item: Item = Item({arguments});");
        assert!(Engine::default().check_source(&source).is_err(), "{source}");
    }
}

#[test]
fn unified_struct_rejects_recursive_default_construction() {
    let source = "struct Node { next: Node = Node(); };";
    assert!(Engine::default().check_source(source).is_err());
}

#[test]
fn unified_struct_generic_instance_and_static_methods() {
    assert_eq!(
        run(r#"
        struct Box<T> { value: T; };
        impl<T> Box<T> {
            fn read(self) -> T { return self.value; };
        };
        struct Factory {};
        impl Factory {
            fn create(value: int) -> Box<int> { return Box<int>(value: value); };
        };
        fn main() -> int {
            var box: Box<int> = Factory.create(value: 9);
            return box.read();
        };
    "#),
        9
    );
}

#[test]
fn typed_closure_in_loop_iterable_infers_its_expression_return() {
    let result = Engine::default()
        .execute_source(
            r#"
        import pkg { sortBy } from "std/data";
        fn main() -> int {
            var mut total: int = 0;
            for value in sortBy(source: [3, 1, 2], key: |value: int| value) {
                total += value;
            }
            return total;
        };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 6);
}

#[test]
fn enum_values_compare_and_generic_sequence_pipes_execute() {
    let result = Engine::default()
        .execute_source(
            r#"
        import pkg { collectTable, count } from "std/data";
        enum Kind { Credit, Adjustment };
        struct Event { amount: int; };
        fn main() -> int {
            var a: Kind = Kind::Credit;
            if a != Kind::Credit { return 91; }
            if a == Kind::Adjustment { return 2; }
            var events: List<Event> = [Event(amount: 7)];
            var table: Table<Event> = events |> collectTable();
            return count(source: table);
        };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 1);
}

#[test]
fn generic_struct_defaults_substitute_constructor_type_arguments() {
    let result = Engine::default()
        .execute_source(
            r#"
        import pkg { parse } from "std/json";
        struct Box<T> { items: List<T> = []; };
        struct Row<T> { box: Box<T> = Box<T>(); };
        fn main() -> int {
            var row: Row<int> = Row<int>();
            var decoded: Row<int> = parse<Row<int>>(text: "{}");
            return row.box.items.length() + decoded.box.items.length();
        };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 0);
}
