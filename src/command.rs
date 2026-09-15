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
    use spar_command::{Join, RedirectMode, Redirection, Step};

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
}
