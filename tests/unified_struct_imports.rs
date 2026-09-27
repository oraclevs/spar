use spar::Engine;

#[test]
fn unified_struct_repeated_imports_share_module_helpers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        r#"
        var mut counter: int = 0;
        fn next() -> int { counter = counter + 1; return counter; };
        export struct First { value: int = next(); };
        export struct Second { value: int = next(); };
    "#,
    )
    .unwrap();
    let result = Engine::default()
        .with_base_dir(dir.path())
        .execute_source(
            r#"
        import { First } from "./models.spar";
        import { Second } from "./models.spar";
        fn main() -> int {
            var first: First = First();
            var second: Second = Second();
            return first.value + second.value;
        };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 3);
}

#[test]
fn unified_struct_aliased_recursive_type_keeps_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        "export struct Node { value: int; next: Option<Node> = none(); };",
    )
    .unwrap();
    let result = Engine::default()
        .with_base_dir(dir.path())
        .execute_source(
            r#"
        import { Node as Link } from "./models.spar";
        fn main() -> int {
            var node: Link = Link(value: 9, next: some(value: Link(value: 2)));
            return node.next.unwrapOr(fallback: Link(value: 3)).value;
        };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 2);
}

#[test]
fn unified_struct_imports_generic_constructor_and_nested_dependency() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        r#"
        export struct Person { name: str; };
        export struct Box<T> { value: T; };
        export struct Team { owner: Person; };
    "#,
    )
    .unwrap();
    let result = Engine::default()
        .with_base_dir(dir.path())
        .execute_source(
            r#"
        import { Box, Team, Person } from "./models.spar";
        fn main() -> int {
            var box: Box<int> = Box<int>(value: 9);
            var team: Team = Team(owner: Person(name: "Ada"));
            return box.value;
        };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 9);
}

#[test]
fn unified_struct_import_rejects_private_declaration() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        "private struct Hidden { value: int; };",
    )
    .unwrap();
    assert!(Engine::default()
        .with_base_dir(dir.path())
        .check_source("import { Hidden } from \"./models.spar\";")
        .is_err());
}

#[test]
fn unified_struct_import_defaults_resolve_in_declaring_module() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        r#"
        var seed: int = 10;
        fn label() -> int { return seed; };
        export struct Item { value: int = label(); };
    "#,
    )
    .unwrap();
    let result = Engine::default()
        .with_base_dir(dir.path())
        .execute_source(
            r#"
        import { Item } from "./models.spar";
        var seed: int = 99;
        fn main() -> int { var item: Item = Item(); return item.value; };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 10);
}

#[test]
fn unified_struct_aliased_imports_keep_distinct_shapes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.spar"),
        "export struct Item { number: int; };",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.spar"),
        "export struct Item { text: str; };",
    )
    .unwrap();
    let result = Engine::default()
        .with_base_dir(dir.path())
        .execute_source(
            r#"
        import { Item as NumberItem } from "./a.spar";
        import { Item as TextItem } from "./b.spar";
        fn main() -> int {
            var a: NumberItem = NumberItem(number: 9);
            var b: TextItem = TextItem(text: "nine");
            return a.number;
        };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 9);
}

#[test]
fn unified_struct_nested_imports_keep_module_type_identity() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.spar"), "private struct Detail { value: int = 7; }; export struct Item { detail: Detail = Detail(); };").unwrap();
    std::fs::write(dir.path().join("b.spar"), "private struct Detail { text: str = \"b\"; }; export struct Item { detail: Detail = Detail(); };").unwrap();
    let result = Engine::default()
        .with_base_dir(dir.path())
        .execute_source(
            r#"
        import { Item as NumberItem } from "./a.spar";
        import { Item as TextItem } from "./b.spar";
        fn main() -> int {
            var a: NumberItem = NumberItem();
            var b: TextItem = TextItem();
            if b.detail.text != "b" { return 1; }
            return a.detail.value;
        };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 7);
}

#[test]
fn unified_struct_import_keeps_method_enum_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        r#"
        export enum Kind { Credit, Adjustment };
        export struct Event { amount: int; };
        impl Event {
            fn kind(self) -> Kind { return Kind::Credit; };
        };
    "#,
    )
    .unwrap();
    let result = Engine::default()
        .with_base_dir(dir.path())
        .execute_source(
            r#"
        import { Event } from "./models.spar";
        fn main() -> int { var event: Event = Event(amount: 3); event.kind(); return 0; };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 0);
}

#[test]
fn unified_struct_diamond_imports_share_nominal_identity() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        "export struct Event { amount: int; };\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("producer.spar"),
        r#"
        import { Event } from "./models.spar";
        fn make() -> Event { return Event(amount: 7); };
    "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("consumer.spar"),
        r#"
        import { Event } from "./models.spar";
        fn read(event: Event) -> int { return event.amount; };
    "#,
    )
    .unwrap();
    let result = Engine::default()
        .with_base_dir(dir.path())
        .execute_source(
            r#"
        import { Event } from "./models.spar";
        import { make } from "./producer.spar";
        import { read } from "./consumer.spar";
        fn main() -> int { var event: Event = make(); return read(event: event); };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 7);
}

#[test]
fn unified_struct_transitive_dependencies_keep_their_methods() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        r#"
        export struct Inner { amount: int = 8; };
        impl Inner { fn read(self) -> int { return self.amount; }; };
        export struct Outer { inner: Inner = Inner(); };
        fn read(value: Outer) -> int { return value.inner.read(); };
    "#,
    )
    .unwrap();
    let result = Engine::default()
        .with_base_dir(dir.path())
        .execute_source(
            r#"
        import { Outer, read } from "./models.spar";
        fn main() -> int { return read(value: Outer()); };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 8);
}

#[test]
fn unified_struct_diamond_imports_deduplicate_source_methods() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        r#"
        export struct Event { amount: int; };
        impl Event { fn read(self) -> int { return self.amount; }; };
    "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("producer.spar"),
        r#"
        import { Event } from "./models.spar";
        fn make() -> Event { return Event(amount: 7); };
    "#,
    )
    .unwrap();
    let result = Engine::default()
        .with_base_dir(dir.path())
        .execute_source(
            r#"
        import { Event } from "./models.spar";
        import { make } from "./producer.spar";
        fn main() -> int { var event: Event = make(); return event.read(); };
    "#,
        )
        .unwrap();
    assert_eq!(result.exit_status, 7);
}

#[test]
fn private_dependency_cannot_be_constructed_by_implicit_short_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        r#"
        private struct Hidden { value: int = 7; };
        export struct Visible { hidden: Hidden = Hidden(); };
    "#,
    )
    .unwrap();
    let engine = Engine::default().with_base_dir(dir.path());
    assert!(engine
        .check_source(
            r#"
        import { Visible } from "./models.spar";
        fn main() -> int { var hidden: Hidden = Hidden(value: 19); return hidden.value; };
    "#
        )
        .is_err());
    assert_eq!(
        engine
            .execute_source(
                r#"
        import { Visible } from "./models.spar";
        fn main() -> int { var visible: Visible = Visible(); return visible.hidden.value; };
    "#
            )
            .unwrap()
            .exit_status,
        7
    );
}

#[test]
fn imported_closure_annotations_keep_module_type_identity() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("models.spar"),
        r#"
        export struct Item { value: int; };
        fn value() -> int {
            var read = |item: Item| -> int { return item.value; };
            return read(item: Item(value: 7));
        };
    "#,
    )
    .unwrap();
    assert_eq!(
        Engine::default()
            .with_base_dir(dir.path())
            .execute_source(
                r#"
        import { value } from "./models.spar";
        fn main() -> int { return value(); };
    "#
            )
            .unwrap()
            .exit_status,
        7
    );
}
