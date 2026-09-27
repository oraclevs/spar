use spar::{CompileOptions, Engine, Value};

fn engine() -> Engine {
    Engine::new(CompileOptions::default())
}

#[test]
fn list_mutation_and_safe_reads_are_first_class() {
    let outcome = engine().execute_source(r#"
        fn main() -> int {
            var mut values: List<int> = [1, 3];
            values.insert(index: 1, value: 2);
            values.append(value: 4);
            values.set(index: 0, value: 9);
            var removed: bool = values.remove(value: 3);
            var second: int = values.get(index: 1).unwrap();
            var mut bonus: int = 0;
            if removed { bonus = 10; }
            return values.length() + second + bonus;
        };
    "#).expect("list collection program");
    assert_eq!(outcome.exit_status, 15);
}

#[test]
fn list_mutation_writes_back_through_nested_struct_field() {
    let outcome = engine().execute_source(r#"
        struct Holder { items: List<int> = [1]; };
        fn main() -> int {
            var mut holder: Holder = Holder();
            holder.items.append(value: 2);
            holder.items.append(value: 3);
            return holder.items.length();
        };
    "#).expect("nested list mutation");
    assert_eq!(outcome.exit_status, 3);
}

#[test]
fn map_insert_get_remove_and_clear_are_mutating_and_safe() {
    let outcome = engine().execute_source(r#"
        fn main() -> int {
            var mut values: Map<str, int> = {};
            var old: Option<int> = values.insert(key: "one", value: 1);
            values.insert(key: "two", value: 2);
            var one: int = values.get(key: "one").unwrap();
            var removed: Option<int> = values.remove(key: "two");
            var mut bonus: int = 0;
            if old.isNone() { bonus = 10; }
            return values.length() + one + removed.unwrap() + bonus;
        };
    "#).expect("map collection program");
    assert_eq!(outcome.exit_status, 14);
}

#[test]
fn list_get_pop_and_map_get_return_option() {
    let mut engine = engine();
    let checked = engine.check_source(r#"
        fn probe(values: List<int>, mapping: Map<str, int>) -> int {
            var a: Option<int> = values.get(index: 0);
            var b: Option<int> = mapping.get(key: "x");
            return 0;
        };
    "#);
    assert!(checked.is_ok(), "safe collection reads should typecheck: {checked:?}");
}

#[test]
fn list_search_predicates_and_transforms_cover_the_standard_surface() {
    let outcome = engine().execute_source(r#"
        fn main() -> int {
            var values: List<int> = [1, 2, 3, 4];
            var found: int = values.find(predicate: |value: int| value > 2).unwrap();
            var index: int = values.findIndex(predicate: |value: int| value == 4).unwrap();
            var anyLarge: bool = values.any(predicate: |value: int| value >= 4);
            var everyPositive: bool = values.every(predicate: |value: int| value > 0);
            var reversed: List<int> = values.reversed();
            var mapped: List<int> = values.map(transform: |value: int| value * 2);
            var filtered: List<int> = values.filter(predicate: |value: int| value >= 3);
            if !anyLarge || !everyPositive { return 90; }
            return found + index + reversed.get(index: 0).unwrap()
                + mapped.get(index: 1).unwrap() + filtered.length();
        };
    "#).expect("list search and transforms");
    assert_eq!(outcome.exit_status, 16);
}

#[test]
fn map_fallback_helpers_are_lazy_and_preserve_existing_values() {
    let outcome = engine().execute_source(r#"
        fn main() -> int {
            var values: Map<str, int> = { present: 7; };
            var existing: int = values.getOr(key: "present", fallback: 99);
            var missing: int = values.getOr(key: "missing", fallback: 5);
            var lazyMissing: int = values.getOrElse(key: "other", fallback: || 8);
            return existing + missing + lazyMissing;
        };
    "#).expect("map fallback helpers");
    assert_eq!(outcome.exit_status, 20);
}

#[test]
fn option_and_result_combinators_are_typed_and_callable() {
    let outcome = engine().execute_source(r#"
        fn main() -> int {
            var someValue: Option<int> = some(value: 3);
            var noneValue: Option<int> = none();
            var mapped: Option<int> = someValue.map(transform: |value: int| value * 2);
            var chained: Option<int> = someValue.andThen(transform: |value: int| some(value: value + 4));
            var recovered: int = noneValue.unwrapOrElse(fallback: || 9);
            var resultValue: Result<int, str> = someValue.okOr(error: "missing");
            var resultMapped: Result<int, str> = resultValue.map(transform: |value: int| value + 1);
            if !someValue.filter(predicate: |value: int| value == 3).isSome() { return 80; }
            if !noneValue.or(other: some(value: 5)).isSome() { return 81; }
            return mapped.unwrap() + chained.unwrap() + recovered + resultMapped.unwrap();
        };
    "#).expect("Option and Result combinators");
    assert_eq!(outcome.exit_status, 26);
}

#[test]
fn list_remove_at_join_and_option_mutation_follow_core_contracts() {
    let outcome = engine().execute_source(r#"
        fn main() -> int {
            var mut numbers: List<int> = [10, 20, 30];
            var removed: int = numbers.removeAt(index: 1);
            var words: List<str> = ["spar", "core"];
            if words.join(separator: "-") != "spar-core" { return 90; }

            var mut maybe: Option<int> = some(value: 7);
            var taken: Option<int> = maybe.take();
            if maybe.isSome() || taken.unwrap() != 7 { return 91; }
            var previous: Option<int> = maybe.replace(value: 9);
            if previous.isSome() || maybe.unwrap() != 9 { return 92; }
            return removed + numbers.length();
        };
    "#).expect("removeAt/join/Option mutation core contracts");
    assert_eq!(outcome.exit_status, 22);
}

#[test]
fn core_string_methods_use_named_arguments_and_character_indices() {
    let outcome = engine().execute_source(r#"
        fn main() -> int {
            var text: str = "  Héllo Spar  ";
            if !text.contains(needle: "Spar") { return 1; }
            if !text.trimStart().startsWith(prefix: "Héllo") { return 2; }
            if !text.trimEnd().endsWith(suffix: "Spar") { return 3; }
            if "A,B".toLowerCase() != "a,b" { return 4; }
            if "a,b".toUpperCase() != "A,B" { return 5; }
            if "a,b,c".split(separator: ",").length() != 3 { return 6; }
            if "abcabc".replace(from: "ab", to: "x") != "xcxc" { return 7; }
            if "héllo".indexOf(needle: "llo").unwrap() != 2 { return 8; }
            if "héllo".substring(start: 1, end: some(value: 4)) != "éll" { return 9; }
            return 0;
        };
    "#).expect("core string methods");
    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn map_object_literals_keep_map_identity_in_nested_and_call_contexts() {
    let outcome = engine().execute_source(r#"
        struct Holder { values: Map<str, int> = { base: 1; }; };

        fn score(values: Map<str, int>) -> int {
            return values.get(key: "passed").unwrapOr(fallback: 0);
        };

        fn main() -> int {
            var maps: List<Map<str, int>> = [{ nested: 2; }];
            var mut current: Map<str, int> = { start: 3; };
            current = { passed: 4; };
            var holder: Holder = Holder(values: { base: 5; });
            return maps.get(index: 0).unwrap().get(key: "nested").unwrap()
                + score(values: { passed: 4; })
                + current.get(key: "passed").unwrap()
                + holder.values.get(key: "base").unwrap();
        };
    "#).expect("typed Map literals should lower to runtime maps in every expected-type context");

    assert_eq!(outcome.exit_status, 15);
}

#[test]
fn map_object_literals_validate_key_and_value_types() {
    let mut engine = engine();

    let wrong_key = engine.check_source(r#"
        fn main() -> int {
            var values: Map<int, int> = { one: 1; };
            return 0;
        };
    "#);
    assert!(wrong_key.is_err(), "object-literal Map keys are strings and must reject Map<int, _>");

    let wrong_value = engine.check_source(r#"
        fn main() -> int {
            var values: Map<str, int> = { one: "not-an-int"; };
            return 0;
        };
    "#);
    assert!(wrong_value.is_err(), "Map object-literal values must match V");
}

#[test]
fn maps_survive_module_initialization_and_struct_defaults() {
    let outcome = engine().execute_source(r#"
        var mut globalValues: Map<str, int> = { one: 1; };

        struct Holder { values: Map<str, int> = { base: 5; }; };

        fn main() -> int {
            globalValues.insert(key: "two", value: 2);
            var holder: Holder = Holder();
            return globalValues.get(key: "one").unwrap()
                + globalValues.get(key: "two").unwrap()
                + holder.values.get(key: "base").unwrap();
        };
    "#).expect("Map identity should survive evaluator/runtime configuration bridging");

    assert_eq!(outcome.exit_status, 8);
}

#[test]
fn generic_maps_with_non_string_keys_survive_global_writeback() {
    let outcome = engine().execute_source(r#"
        var mut numbers: Map<int, str> = {};

        fn main() -> int {
            numbers.insert(key: 1, value: "one");
            numbers.insert(key: 2, value: "two");
            if numbers.get(key: 1).unwrap() != "one" { return 1; }
            if numbers.get(key: 2).unwrap() != "two" { return 2; }
            return numbers.length();
        };
    "#).expect("generic Map<K,V> values should survive module/global writeback without string-key coercion");

    assert_eq!(outcome.exit_status, 2);
}
