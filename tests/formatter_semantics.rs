use spar::formatter::format_source;
use spar::{Lexer, Parser};

fn assert_reparse_and_idempotent(source: &str) -> String {
    let formatted = format_source(source).expect("first format should succeed");
    let tokens = Lexer::new(&formatted)
        .tokenize()
        .expect("formatted source should lex");
    Parser::new(tokens)
        .parse()
        .expect("formatted source should parse");
    let twice = format_source(&formatted).expect("second format should succeed");
    assert_eq!(twice, formatted, "formatting must be idempotent");
    formatted
}

#[test]
fn canonical_function_keyword_is_fn() {
    let formatted = assert_reparse_and_idempotent(
        "function greet(name: str) -> str { return name; };\n",
    );
    assert!(formatted.starts_with("fn greet("), "{formatted}");
}

#[test]
fn shell_literal_dollar_does_not_turn_into_environment_interpolation() {
    let formatted = assert_reparse_and_idempotent(
        "fn main() -> shell { return shell { printf '%s\\n' '$HOME'; }; };\n",
    );
    assert!(
        formatted.contains("'$HOME'") || formatted.contains("'\u{24}HOME'"),
        "literal dollar must remain literal: {formatted}"
    );
}

#[test]
fn shell_word_preserves_literal_and_expression_parts() {
    let formatted = assert_reparse_and_idempotent(
        "fn main() -> shell { var name: str = \"spar\"; return shell { printf '%s\\n' 'literal-$HOME-'${name}; }; };\n",
    );
    assert!(formatted.contains("${name}"), "expression interpolation lost: {formatted}");
    assert!(formatted.contains("$HOME"), "literal fragment lost: {formatted}");
}

#[test]
fn shell_args_interpolation_remains_a_whole_word() {
    let formatted = assert_reparse_and_idempotent(
        "fn main() -> shell { var args: List<str> = [\"one\", \"two\"]; return shell { printf '%s\\n' ${args.asArgs()}; }; };\n",
    );
    assert!(formatted.contains("${args.asArgs()}"), "Args interpolation changed: {formatted}");
}

#[test]
fn shell_command_substitution_and_redirects_reparse() {
    let formatted = assert_reparse_and_idempotent(
        "fn main() -> shell { return shell { printf '%s\\n' pre$(printf mid)post > 'out file'; }; };\n",
    );
    assert!(formatted.contains("$(printf"), "command substitution lost: {formatted}");
    assert!(formatted.contains(">"), "redirect lost: {formatted}");
}

#[test]
fn long_calls_and_signatures_use_vertical_arguments() {
    let source = r#"fn summarize(accountName: str, serviceName: str, acceptedEventCount: int, rejectedEventCount: int, duplicateEventCount: int) -> str { return buildReport(accountName: accountName, serviceName: serviceName, acceptedEventCount: acceptedEventCount, rejectedEventCount: rejectedEventCount); };"#;
    let formatted = assert_reparse_and_idempotent(source);
    assert!(formatted.contains("fn summarize(\n    accountName: str,"), "{formatted}");
    assert!(formatted.contains("buildReport(\n        accountName: accountName,"), "{formatted}");
    assert!(formatted.lines().all(|line| line.chars().count() <= 100), "{formatted}");
}

#[test]
fn nested_method_calls_wrap_and_preserve_execution() {
    let source = r#"fn main() -> int { var mut values: List<int> = []; values.append(value: some<int>(value: 7).map(transform: |value: int| value + 1).unwrapOr(fallback: 100)); return values[0]; };"#;
    let formatted = assert_reparse_and_idempotent(source);
    assert!(formatted.contains("values.append(\n"), "{formatted}");
    assert_eq!(spar::Engine::default().execute_source(&formatted).unwrap().exit_status, 8);
    assert!(formatted.lines().all(|line| line.chars().count() <= 100), "{formatted}");
}

#[test]
fn multiline_argument_comments_stay_with_their_arguments() {
    let source = "fn main() -> int { return compute(\n// first argument\nfirst: 1, // important\n// second argument\nsecond: 2,\n); };";
    let formatted = assert_reparse_and_idempotent(source);
    for comment in ["// first argument", "// important", "// second argument"] {
        assert_eq!(formatted.matches(comment).count(), 1, "{formatted}");
    }
    assert!(formatted.find("// first argument").unwrap() < formatted.find("first: 1").unwrap(), "{formatted}");
    assert!(formatted.contains("first: 1, // important"), "{formatted}");
}

#[test]
fn empty_call_comments_stay_inside_parentheses() {
    let formatted = assert_reparse_and_idempotent(
        "fn main() -> int { return count(\n// keep this explanation\n); };\n",
    );
    assert!(formatted.contains("count(\n        // keep this explanation\n    )"), "{formatted}");
}

#[test]
fn long_logical_conditions_wrap_without_changing_precedence() {
    let source = "fn enabled(firstCondition: bool, secondCondition: bool, thirdCondition: bool, fourthCondition: bool) -> bool { return firstCondition && secondCondition && thirdCondition && fourthCondition && firstCondition && secondCondition; };";
    let formatted = assert_reparse_and_idempotent(source);
    assert!(formatted.contains("\n        &&"), "{formatted}");
    assert!(formatted.lines().all(|line| line.chars().count() <= 100), "{formatted}");
}

#[test]
fn long_method_signatures_keep_receivers_and_defaults() {
    let source = "struct Report { total: int = 0; }; impl Report { fn summarize(self, accountName: str, serviceName: str, acceptedEventCount: int, rejectedEventCount: int = 0) -> int { return self.total; }; };";
    let formatted = assert_reparse_and_idempotent(source);
    assert!(formatted.contains("fn summarize(\n        self,\n"), "{formatted}");
    assert!(formatted.contains("rejectedEventCount: int = 0,"), "{formatted}");
    spar::Engine::default().check_source(&formatted).unwrap();
}

#[test]
fn wrapping_inside_interpolation_keeps_exact_string_contents() {
    let source = r#"fn amount(firstAmount: int, secondAmount: int, thirdAmount: int, fourthAmount: int) -> int { return firstAmount + secondAmount + thirdAmount + fourthAmount; }; fn main() -> int { var text: str = "prefix:${amount(firstAmount: 10, secondAmount: 20, thirdAmount: 30, fourthAmount: 40)}:suffix"; if text == "prefix:100:suffix" { return 0; } return 1; };"#;
    let formatted = assert_reparse_and_idempotent(source);
    assert_eq!(spar::Engine::default().execute_source(&formatted).unwrap().exit_status, 0, "{formatted}");
}

#[test]
fn nested_config_calls_use_compact_wrappers_and_vertical_fields() {
    let source = r#"struct Config { prompt: Prompt = some(value: Prompt(slot1: some(value: Slot(text: "{duration}", color: some(value: "yellow"))), slot2: some(value: Slot(text: "{cpu}% {ram}%", color: some(value: "cyan"))))); };"#;
    let formatted = assert_reparse_and_idempotent(source);
    assert!(formatted.contains("prompt: Prompt = some(value: Prompt(\n"), "{formatted}");
    assert!(formatted.contains("Slot(\n"), "{formatted}");
    assert!(formatted.lines().all(|line| line.chars().count() <= 80), "{formatted}");
    assert!(!formatted.contains("some(\n"), "single nested wrappers should not create indentation staircases: {formatted}");
}

#[test]
fn medium_length_calls_split_each_argument_at_eighty_columns() {
    let formatted = assert_reparse_and_idempotent("fn main() -> int { return calculate(firstAccount: firstAccount, secondAccount: secondAccount, thirdAccount: thirdAccount); };");
    assert!(formatted.contains("calculate(\n        firstAccount: firstAccount,\n        secondAccount: secondAccount,\n        thirdAccount: thirdAccount,\n    )"), "{formatted}");
    assert!(formatted.lines().all(|line| line.chars().count() <= 80), "{formatted}");
}
