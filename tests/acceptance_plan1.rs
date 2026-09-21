//! Acceptance sweep for roadmap Tasks 1-9: callable types and closures,
//! canonical structs, `impl` methods, built-in methods, and the `|>` operator.
//! Each test runs a whole program and asserts on `main`'s exit status, or on
//! the error the compiler must report.

use spar::Engine;

fn status(source: &str) -> i32 {
    Engine::default()
        .execute_source(source)
        .unwrap_or_else(|errors| panic!("program should run: {errors:?}\n{source}"))
        .exit_status
}

fn error(source: &str) -> String {
    match Engine::default().execute_source(source) {
        Ok(outcome) => panic!(
            "expected a compile error, got status {}",
            outcome.exit_status
        ),
        Err(errors) => format!("{errors:?}"),
    }
}

// ── Task 1-3: callable types, closures, capture ─────────────────────────────

#[test]
fn closure_forms_expression_typed_and_block() {
    assert_eq!(
        status(
            r#"
            function main() -> int {
                var double: fn(int) -> int = fn(x) => x * 2;
                var typed: fn(int) -> int = fn(x: int) -> int => x + 1;
                var block: fn(int) -> int = fn(x: int) -> int {
                    var y: int = x * 10;
                    return y;
                };
                return double(4) + typed(1) + block(1);
            };
            "#
        ),
        8 + 2 + 10
    );
}

#[test]
fn named_functions_are_first_class_values() {
    assert_eq!(
        status(
            r#"
            function inc(n: int) -> int { return n + 1; };
            function apply(f: fn(int) -> int, v: int) -> int { return f(v); };
            function main() -> int {
                var f: fn(int) -> int = inc;
                return apply(f: f, v: 41) + apply(f: inc, v: 0);
            };
            "#
        ),
        43
    );
}

#[test]
fn closures_capture_by_value_and_outlive_their_scope() {
    assert_eq!(
        status(
            r#"
            function makeAdder(n: int) -> fn(int) -> int {
                return fn(x: int) -> int => x + n;
            };
            function main() -> int {
                var add5: fn(int) -> int = makeAdder(n: 5);
                var add10: fn(int) -> int = makeAdder(n: 10);
                return add5(1) * 100 + add10(1);
            };
            "#
        ),
        611
    );
}

#[test]
fn captured_value_is_a_snapshot_not_a_reference() {
    assert_eq!(
        status(
            r#"
            function main() -> int {
                var mut base: int = 1;
                var read: fn() -> int = fn() -> int => base;
                base = 99;
                return read();
            };
            "#
        ),
        1
    );
}

#[test]
fn generic_callable_types_infer_through_calls() {
    assert_eq!(
        status(
            r#"
            function twice<T>(f: fn(T) -> T, v: T) -> T { return f(f(v)); };
            function main() -> int {
                var s: str = twice(f: fn(x: str) -> str => x + "!", v: "a");
                if s != "a!!" { return 1; }
                return twice(f: fn(x: int) -> int => x * 3, v: 2);
            };
            "#
        ),
        18
    );
}

#[test]
fn closure_type_errors_are_reported() {
    let message = error(
        r#"
        function main() -> int {
            var f: fn(int) -> int = fn(x: int) -> int => x;
            return f("nope");
        };
        "#,
    );
    assert!(
        message.contains("TypeError") || message.contains("ParseError"),
        "{message}"
    );
    let message = error(
        r#"
        function main() -> int {
            var f: fn(int) -> str = fn(x: int) -> int => x;
            return 0;
        };
        "#,
    );
    assert!(message.contains("TypeError"), "{message}");
}

// ── Task 4: canonical structs, overrides, mutability ────────────────────────

const USER: &str = r#"
    struct User { name: str = "Unknown"; age: int = 18; active: bool = true; };
"#;

#[test]
fn struct_call_clones_defaults_and_keeps_originals_independent() {
    assert_eq!(
        status(&format!(
            r#"{USER}
            function main() -> int {{
                var a: User = User(name: "OCC");
                var mut b: User = User();
                b.age = 40;
                if a.age != 18 {{ return 1; }}
                if a.name != "OCC" {{ return 2; }}
                if b.name != "Unknown" {{ return 3; }}
                return b.age;
            }};
            "#
        )),
        40
    );
}

#[test]
fn struct_unknown_and_duplicate_overrides_are_errors() {
    let unknown = error(&format!(
        r#"{USER}
        function main() -> int {{ var u: User = User(nmae: "x"); return 0; }};
        "#
    ));
    assert!(
        unknown.contains("nmae") || unknown.contains("Error"),
        "{unknown}"
    );
    let duplicate = error(&format!(
        r#"{USER}
        function main() -> int {{ var u: User = User(age: 1, age: 2); return 0; }};
        "#
    ));
    assert!(
        duplicate.to_lowercase().contains("duplicate") || duplicate.contains("Error"),
        "{duplicate}"
    );
}

#[test]
fn field_mutation_requires_a_mutable_binding() {
    let message = error(&format!(
        r#"{USER}
        function main() -> int {{
            var u: User = User();
            u.age = 5;
            return u.age;
        }};
        "#
    ));
    assert!(
        message.contains("mut") || message.contains("mutable"),
        "{message}"
    );
}

#[test]
fn override_type_mismatch_is_rejected() {
    let message = error(&format!(
        r#"{USER}
        function main() -> int {{ var u: User = User(age: "old"); return 0; }};
        "#
    ));
    assert!(message.contains("TypeError"), "{message}");
}

// ── Task 5: impl, self, mut self, privacy ───────────────────────────────────

#[test]
fn impl_instance_static_and_mut_self_methods() {
    assert_eq!(
        status(&format!(
            r#"{USER}
            impl User {{
                function isAdult(self) -> bool {{ return self.age >= 18; }};
                function birthday(mut self) -> void {{ self.age = self.age + 1; }};
                function child(name: str) -> User {{ return User(name: name, age: 5); }};
            }};
            impl User {{
                function label(self) -> str {{ return self.name + "!"; }};
            }};
            function main() -> int {{
                var mut kid: User = User.child("Tobi");
                if kid.isAdult() {{ return 1; }}
                kid.birthday();
                kid.birthday();
                if kid.label() != "Tobi!" {{ return 2; }}
                return kid.age;
            }};
            "#
        )),
        7
    );
}

#[test]
fn mut_self_method_on_an_immutable_binding_is_rejected() {
    let message = error(&format!(
        r#"{USER}
        impl User {{ function birthday(mut self) -> void {{ self.age = self.age + 1; }}; }};
        function main() -> int {{
            var u: User = User();
            u.birthday();
            return 0;
        }};
        "#
    ));
    assert!(message.contains("mut"), "{message}");
}

#[test]
fn self_is_read_only_inside_plain_self_methods() {
    let message = error(&format!(
        r#"{USER}
        impl User {{ function bad(self) -> void {{ self.age = 1; }}; }};
        function main() -> int {{ return 0; }};
        "#
    ));
    assert!(
        message.contains("mut") || message.contains("read-only") || message.contains("self"),
        "{message}"
    );
}

#[test]
fn private_methods_are_only_callable_from_the_impl() {
    assert_eq!(
        status(&format!(
            r#"{USER}
            impl User {{
                private function secret(self) -> int {{ return self.age; }};
                function reveal(self) -> int {{ return self.secret() + 1; }};
            }};
            function main() -> int {{ var u: User = User(age: 9); return u.reveal(); }};
            "#
        )),
        10
    );
    let message = error(&format!(
        r#"{USER}
        impl User {{ private function secret(self) -> int {{ return self.age; }}; }};
        function main() -> int {{ var u: User = User(); return u.secret(); }};
        "#
    ));
    assert!(
        message.contains("private") || message.contains("secret"),
        "{message}"
    );
}

#[test]
fn unknown_method_is_a_clear_error() {
    let message = error(&format!(
        r#"{USER}
        function main() -> int {{ var u: User = User(); return u.nope(); }};
        "#
    ));
    assert!(message.contains("nope"), "{message}");
}

// ── Task 6: built-in methods use the same mechanism ─────────────────────────

#[test]
fn builtin_methods_and_free_functions_agree() {
    assert_eq!(
        status(
            r#"
            function main() -> int {
                var s: str = "hello";
                var xs: [int] = [1, 2, 3];
                if s.length() != 5 { return 1; }
                if s.isEmpty() { return 2; }
                return xs.length();
            };
            "#
        ),
        3
    );
}

// ── Task 7: structured pipe ─────────────────────────────────────────────────

#[test]
fn structured_pipe_forms_and_chaining() {
    assert_eq!(
        status(
            r#"
            function add(value: int, by: int) -> int { return value + by; };
            function double(value: int) -> int { return value * 2; };
            function main() -> int {
                var viaClosure: int = 3 |> fn(x: int) -> int => x + 100;
                var chained: int = 1 |> add(by: 2) |> double |> add(by: 1);
                if viaClosure != 103 { return 1; }
                return chained;
            };
            "#
        ),
        7
    );
}

#[test]
fn structured_pipe_type_mismatch_names_the_problem() {
    let message = error(
        r#"
        function needsInt(value: int) -> int { return value; };
        function main() -> int { return "text" |> needsInt; };
        "#,
    );
    assert!(message.contains("TypeError"), "{message}");
}

#[test]
fn unix_pipe_is_not_the_structured_pipe() {
    // `|` outside a shell block is not a value operator.
    let message = error(
        r#"
        function main() -> int { var x: int = 1 | 2; return x; };
        "#,
    );
    assert!(message.contains("Error"), "{message}");
}

// ── a shell value that is created but never run is an error ─────────────────

#[test]
fn a_bare_shell_statement_in_a_function_is_rejected_with_a_hint() {
    let message = error(
        r#"
        function main() -> int {
            shell { echo hi; };
            return 0;
        };
        "#,
    );
    assert!(message.contains("never run"), "{message}");
    assert!(message.contains("exec shell"), "{message}");
}

#[test]
fn exec_shell_return_and_assignment_are_the_ways_to_use_a_shell_value() {
    assert_eq!(
        status(
            r#"
            function plan() -> shell { return shell { echo hi; }; };
            function main() -> int {
                var kept: shell = shell { echo kept; };
                var ran: ExecResult = exec shell { true; };
                if !ran.success { return 1; }
                return 0;
            };
            "#
        ),
        0
    );
}
