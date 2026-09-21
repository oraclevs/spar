use spar::Engine;

#[test]
fn data_pipeline_maps_filters_slices_and_collects_lists() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            import pkg { map, filter, take, collect, count } from "std/data";

            function main() -> int {
                var values: [int] = [1, 2, 3, 4, 5, 6]
                    |> filter(fn(value: int) -> bool => value == 2 || value == 4 || value == 6)
                    |> map(fn(value: int) -> int => value * 10)
                    |> take(2)
                    |> collect();
                return values[0] + values[1] + count(values);
            };
            "#,
        )
        .expect("std/data list pipeline should execute");

    assert_eq!(outcome.exit_status, 62);
}

#[test]
fn data_first_last_skip_sort_unique_flatten_and_get_are_composable() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            import pkg { first, last, skip, sortBy, unique, flatten, get } from "std/data";

            function main() -> int {
                var values: [int] = [3, 1, 3, 2, 2]
                    |> unique()
                    |> sortBy(fn(value: int) -> int => value)
                    |> skip(1);
                var nested: [[int]] = [[4, 5], [6]];
                var flat: [int] = nested |> flatten();
                return first(source: values) + last(source: values) + get(source: flat, key: 2);
            };
            "#,
        )
        .expect("std/data materialized list transforms should execute");

    assert_eq!(outcome.exit_status, 11);
}

#[test]
fn collect_table_filter_select_schema_and_methods_share_semantics() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            import pkg { collectTable, filter, select, schema } from "std/data";

            struct User { name: str = ""; age: int = 0; active: bool = false; };

            function main() -> int {
                var table: Table<User> = [
                    User(name: "Obi", age: 24, active: true),
                    User(name: "Ada", age: 31, active: false),
                    User(name: "Ngozi", age: 28, active: true)
                ] |> collectTable();

                var active: Table<User> = table
                    |> filter(fn(user: User) -> bool => user.active);
                var names: Table<Record> = active |> select(["name"]);
                var tableSchema: Schema = schema(names);

                if active.count() != 2 { return 1; }
                if names.count() != 2 { return 2; }
                if names.take(1).count() != 1 { return 3; }
                if names.skip(1).count() != 1 { return 4; }
                return 20;
            };
            "#,
        )
        .expect("Table transforms and methods should execute through std/data");

    assert_eq!(outcome.exit_status, 20);
}

#[test]
fn group_by_returns_map_of_tables_and_get_is_strict_lookup() {
    let outcome = Engine::default()
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
                var red: Table<User> = get(source: groups, key: "red");
                var blue: Table<User> = get(source: groups, key: "blue");
                return red.count() * 10 + blue.count();
            };
            "#,
        )
        .expect("groupBy should return Map<K, Table<T>>");

    assert_eq!(outcome.exit_status, 21);
}

#[test]
fn where_unique_by_and_inspect_preserve_table_shape() {
    let outcome = Engine::default()
        .execute_source(
            r#"
            import pkg { collectTable, where, uniqueBy, inspect } from "std/data";

            struct User { name: str = ""; age: int = 0; };

            function main() -> int {
                var users: Table<User> = [
                    User(name: "Obi", age: 24),
                    User(name: "Obi", age: 25),
                    User(name: "Ada", age: 31)
                ] |> collectTable();
                var result: Table<User> = users
                    |> where(fn(user: User) -> bool => user.age >= 24)
                    |> uniqueBy(fn(user: User) -> str => user.name)
                    |> inspect();
                return result.count();
            };
            "#,
        )
        .expect("where/uniqueBy/inspect should preserve Table<T>");

    assert_eq!(outcome.exit_status, 2);
}
