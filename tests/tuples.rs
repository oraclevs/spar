use spar::{CompileOptions, Engine};

fn run(source: &str) -> Result<i32, String> {
    Engine::new(CompileOptions::default())
        .execute_source(source)
        .map(|outcome| outcome.exit_status)
        .map_err(|errors| {
            errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })
}

#[test]
fn typed_tuple_fields_flow_through_calls_and_locals() {
    let source = r#"
        fn make(n: int) -> (int, str) { return (n + 2, "ok"); };
        fn take(pair: (int, str)) -> int {
            var copy: (int, str) = pair;
            if copy.1 == "ok" { return copy.0; }
            return 0;
        };
        fn main() -> int { return take(pair: make(n: 40)); };
    "#;
    assert_eq!(run(source), Ok(42));
}

#[test]
fn tuple_field_errors() {
    let bounds =
        run("fn main() -> int { var p: (int, str) = (1, \"x\"); return p.2; };").unwrap_err();
    assert!(bounds.contains("out of bounds"), "{bounds}");
    let wrong = run("fn main() -> int { var p: (int, str) = (1, 2); return p.0; };").unwrap_err();
    assert!(wrong.contains("expects `str`"), "{wrong}");
}

#[test]
fn tuple_literals_format_and_round_trip() {
    let source = "fn main() -> int { var pair: (int, str) = (40 + 2, \"ok\"); return pair.0; };";
    let formatted = spar::formatter::format_source(source).expect("tuple formats");
    assert!(formatted.contains("(int, str)"), "{formatted}");
    assert!(formatted.contains("(40 + 2, \"ok\")"), "{formatted}");
    assert_eq!(run(&formatted), Ok(42));
}

#[test]
fn tuple_index_is_static_and_heterogeneous() {
    let source = r#"
        fn main() -> int {
            var pair = ("ok", 40);
            if pair.0 == "ok" { return pair.1 + 2; }
            return 0;
        };
    "#;
    assert_eq!(run(source), Ok(42));
}

#[test]
fn tuple_destructuring_evaluates_source_once_and_binds_types() {
    let source = r#"
        fn make() -> (int, str) { return (40, "ok"); };
        fn main() -> int {
            var (count, label): (int, str) = make();
            if label == "ok" { return count + 2; }
            return 0;
        };
    "#;
    assert_eq!(run(source), Ok(42));
}

#[test]
fn destructuring_rejects_non_tuple_and_wrong_arity() {
    let non_tuple = run("fn main() -> int { var (a, b) = 3; return a; };").unwrap_err();
    assert!(non_tuple.contains("requires a tuple"), "{non_tuple}");
    let wrong_arity = run("fn main() -> int { var (a, b, c) = (1, 2); return a; };").unwrap_err();
    assert!(
        wrong_arity.contains("3 names") && wrong_arity.contains("2 elements"),
        "{wrong_arity}"
    );
}

#[test]
fn indexed_for_binding_keeps_index_and_tuple_value_access() {
    let source = r#"
        fn main() -> int {
            var items: [(int, str)] = [(20, "a"), (21, "b")];
            var mut total: int = 0;
            for (index, item) in items {
                total = total + index + item.0;
            }
            return total;
        };
    "#;
    assert_eq!(run(source), Ok(42));
}

#[test]
fn destructured_names_follow_block_scope() {
    let source = r#"
        fn main() -> int {
            var x: int = 10;
            if true {
                var (x, y) = (30, 2);
                if x + y != 32 { return 1; }
            }
            return x;
        };
    "#;
    assert_eq!(run(source), Ok(10));
}

#[test]
fn nested_tuple_access_uses_parentheses() {
    let source = r#"
        fn main() -> int {
            var nested: ((str, int), bool) = (("ok", 42), true);
            if nested.1 { return (nested.0).1; }
            return 0;
        };
    "#;
    assert_eq!(run(source), Ok(42));
}
