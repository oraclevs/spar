use crate::error::SparError;
use crate::lexer::Lexer;
use crate::token::Token;

pub fn parse_shell_plan(source: &str) -> Result<spar_command::ShellPlan, SparError> {
    let wrapped = format!("shell {{ {source} }}");
    let tokens = Lexer::new(&wrapped).tokenize()?;
    let (expression, consumed) = crate::shell_lang::parse_shell_block(&tokens)?;
    debug_assert!(matches!(
        tokens.get(consumed).map(|token| &token.token),
        Some(Token::Eof)
    ));
    Ok(crate::evaluator::lower_shell_expr(&expression))
}

#[cfg(test)]
mod tests {
    use spar_command::{EnvironmentOverride, Join, RedirectMode, Redirection, Step};

    #[test]
    fn command_text_lowers_through_the_native_shell_grammar() {
        let plan =
            super::parse_shell_plan(r#"printf "%s" "hello world" | grep hello > result.txt"#)
                .unwrap();

        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].0, Join::Always);
        let Step::Pipeline(pipeline) = &plan.steps[0].1 else {
            panic!("expected a pipeline");
        };
        assert_eq!(pipeline.commands[0].program, "printf");
        assert_eq!(
            pipeline.commands[0].args,
            vec!["%s".to_string(), "hello world".to_string()]
        );
        assert_eq!(pipeline.commands[1].program, "grep");
        assert_eq!(
            pipeline.commands[1].stdout,
            Some(Redirection::File {
                path: "result.txt".into(),
                mode: RedirectMode::Truncate,
            })
        );
    }

    #[test]
    fn malformed_command_text_is_rejected_before_execution() {
        let error = super::parse_shell_plan("printf hello |").unwrap_err();
        assert!(error.to_string().contains("pipe"), "{error}");
    }

    #[test]
    fn parses_environment_input_and_logical_joins() {
        let plan = super::parse_shell_plan(
            "RUST_LOG=debug cargo run < input.txt && echo ok || echo failed",
        )
        .unwrap();

        let Step::Command(first) = &plan.steps[0].1 else {
            panic!("expected a command");
        };
        assert_eq!(first.program, "cargo");
        assert_eq!(
            first.env,
            [EnvironmentOverride {
                key: "RUST_LOG".into(),
                value: "debug".into(),
            }]
        );
        assert_eq!(
            first.stdin,
            Some(Redirection::File {
                path: "input.txt".into(),
                mode: RedirectMode::Truncate,
            })
        );
        assert_eq!(plan.steps[1].0, Join::OnSuccess);
        assert_eq!(plan.steps[2].0, Join::OnFailure);
    }

    #[test]
    fn assignment_only_input_has_actionable_diagnostic() {
        let error = super::parse_shell_plan("FOO=bar").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("export FOO=bar"), "{message}");
        assert!(message.contains("var foo: str"), "{message}");
    }

    #[test]
    fn rejects_invalid_control_and_redirection_forms() {
        for (source, expected) in [
            ("BAD-NAME=value", "invalid environment assignment"),
            ("echo ok &&", "expected a command after '&&'"),
            ("echo ok ||", "expected a command after '||'"),
            ("cat < one < two", "stdin may only be redirected once"),
            (
                "printf x | cat < input",
                "pipeline input already comes from previous stage",
            ),
        ] {
            let error = super::parse_shell_plan(source).unwrap_err();
            assert!(error.to_string().contains(expected), "{source}: {error}");
        }
    }

    #[test]
    fn options_aware_executor_uses_shared_plan_runner() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("environment.txt");
        let plan =
            super::parse_shell_plan(&format!("/usr/bin/env > {}", output.display())).unwrap();
        let options = spar_process::ExecutionOptions {
            environment: Some(vec![("SPAR_SHARED_RUNNER".into(), "yes".into())]),
            ..spar_process::ExecutionOptions::default()
        };

        let outcome = crate::execute_shell_plan_with_options(&plan, &options).unwrap();

        assert!(outcome.success);
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            "SPAR_SHARED_RUNNER=yes\n"
        );
    }
}
