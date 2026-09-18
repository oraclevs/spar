use spar::{CompileOptions, Engine};

#[test]
fn regex_module_matches_finds_replaces_and_splits() {
    let outcome = Engine::new(CompileOptions::default())
        .execute_source(
            r#"
            import pkg { isMatch, find, findAll, replace, split } from "std/regex";

            function main() -> int {
                if !isMatch(pattern: "^[a-z]+[0-9]+$", text: "spar42") { return 1; }
                if find(pattern: "[0-9]+", text: "spar42") != "42" { return 2; }
                var matches: [str] = findAll(pattern: "[a-z]+", text: "one 2 three");
                if matches[0] != "one" { return 3; }
                if replace(pattern: "[0-9]+", text: "spar42", replacement: "7") != "spar7" { return 4; }
                var parts: [str] = split(pattern: ",+", text: "a,,b");
                if parts[1] != "b" { return 5; }
                return 0;
            };
            "#,
        )
        .expect("std/regex should execute through the private regex provider");
    assert_eq!(outcome.exit_status, 0);
}
