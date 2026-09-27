use spar::{CompileOptions, Engine};

fn status(source: &str) -> i32 {
    Engine::new(CompileOptions::default())
        .execute_source(source)
        .unwrap_or_else(|errors| panic!("program should run: {errors:?}\n{source}"))
        .exit_status
}

#[test]
fn named_call_can_skip_an_earlier_default_and_supply_a_later_one() {
    assert_eq!(
        status(
            r#"
            fn encode(first: int = 1, second: int = 2, third: int = 3) -> int {
                return first * 100 + second * 10 + third;
            };

            fn main() -> int {
                return encode(first: 9, third: 7);
            };
            "#,
        ),
        927,
    );
}

#[test]
fn named_method_call_preserves_default_parameter_slots() {
    assert_eq!(
        status(
            r#"
            struct Encoder {};

            impl Encoder {
                fn encode(self, first: int = 1, second: int = 2, third: int = 3) -> int {
                    return first * 100 + second * 10 + third;
                };
            };

            fn main() -> int {
                var encoder: Encoder = Encoder();
                return encoder.encode(first: 9, third: 7);
            };
            "#,
        ),
        927,
    );
}

#[test]
fn default_expression_can_still_read_an_earlier_parameter_when_middle_slot_is_omitted() {
    assert_eq!(
        status(
            r#"
            fn derive(base: int, derived: int = base + 1, tail: int = 0) -> int {
                return derived * 10 + tail;
            };

            fn main() -> int {
                return derive(base: 4, tail: 7);
            };
            "#,
        ),
        57,
    );
}
