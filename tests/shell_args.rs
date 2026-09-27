use spar::{CompileOptions, Engine};

fn engine() -> Engine {
    Engine::new(CompileOptions::default())
}

#[test]
fn as_args_expands_each_list_entry_to_one_argv_value_without_retokenizing() {
    let outcome = engine()
        .execute_source(r#"
            fn main() -> shell {
                var args: List<str> = ["-x", "name with spaces", "*.definitely-not-a-real-glob"];
                return shell {
                    sh -c 'test "$#" -eq 3 && test "$1" = "-x" && test "$2" = "name with spaces" && test "$3" = "*.definitely-not-a-real-glob"' marker ${args.asArgs()};
                };
            };
        "#)
        .expect("Args should expand into exact argv entries");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn empty_args_adds_no_argv_entries() {
    let outcome = engine()
        .execute_source(r#"
            fn main() -> shell {
                var args: List<str> = [];
                return shell {
                    sh -c 'test "$#" -eq 0' marker ${args.asArgs()};
                };
            };
        "#)
        .expect("empty Args should add no argv entries");

    assert_eq!(outcome.exit_status, 0);
}

#[test]
fn args_cannot_be_embedded_inside_a_larger_shell_word() {
    let errors = engine()
        .execute_source(r#"
            fn main() -> shell {
                var args: List<str> = ["one", "two"];
                return shell { printf '%s\n' prefix-${args.asArgs()}; };
            };
        "#)
        .expect_err("Args embedded in a larger word must be rejected");

    let rendered = errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(rendered.contains("Args") && rendered.contains("whole") && rendered.contains("word"), "{rendered}");
}

#[test]
fn as_args_is_only_available_on_list_of_strings() {
    let errors = engine()
        .check_source(r#"
            fn bad(values: List<int>) -> void {
                values.asArgs();
            };
        "#)
        .expect_err("List<int>.asArgs must be rejected");

    let rendered = errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
    assert!(rendered.contains("asArgs") || rendered.contains("List<str>"), "{rendered}");
}
