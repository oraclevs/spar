use crate::ast::{
    ShellCommandExpr, ShellEnvironmentEntry, ShellExpr, ShellJoin, ShellRedirect, ShellStep,
    ShellWord,
};
use crate::error::{Span, SparError};
use crate::token::{SpannedToken, Token};
use spar_command::RedirectMode;

pub(crate) fn parse_shell_block(tokens: &[SpannedToken]) -> Result<(ShellExpr, usize), SparError> {
    let Some(start) = tokens.first() else {
        return Err(parse_error("expected a shell block", Span::dummy()));
    };
    if start.token != Token::ShellBlockStart {
        return Err(parse_error(
            format!(
                "expected {}, found {}",
                Token::ShellBlockStart.human_name(),
                start.token.human_name()
            ),
            start.span.clone(),
        ));
    }

    let end = tokens[1..]
        .iter()
        .position(|token| token.token == Token::ShellBlockEnd)
        .map(|position| position + 1)
        .ok_or_else(|| {
            parse_error(
                "unterminated shell block — expected '}'",
                start.span.clone(),
            )
        })?;
    let end_span = tokens[end].span.clone();
    let steps = BodyParser::new(&tokens[1..end]).parse()?;
    Ok((
        ShellExpr {
            steps,
            span: joined_span(&start.span, &end_span),
        },
        end + 1,
    ))
}

pub(crate) fn parse_command_expression(
    tokens: &[SpannedToken],
) -> Result<(ShellExpr, usize), SparError> {
    let Some(start) = tokens.first() else {
        return Err(parse_error("expected 'command'", Span::dummy()));
    };
    if start.token != Token::KwCommand {
        return Err(parse_error(
            format!(
                "expected {}, found {}",
                Token::KwCommand.human_name(),
                start.token.human_name()
            ),
            start.span.clone(),
        ));
    }

    let end = tokens[1..]
        .iter()
        .position(|token| token.token == Token::Semicolon)
        .map(|position| position + 1)
        .ok_or_else(|| {
            parse_error(
                "unterminated command expression — expected ';'",
                start.span.clone(),
            )
        })?;
    let end_span = tokens[end].span.clone();
    let steps = BodyParser::new(&tokens[1..end]).parse()?;
    if steps.len() != 1 {
        return Err(parse_error(
            "'command' must contain exactly one command or pipeline",
            start.span.clone(),
        ));
    }
    Ok((
        ShellExpr {
            steps,
            span: joined_span(&start.span, &end_span),
        },
        end + 1,
    ))
}

struct BodyParser<'a> {
    tokens: &'a [SpannedToken],
    pos: usize,
}

impl<'a> BodyParser<'a> {
    fn new(tokens: &'a [SpannedToken]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn parse(mut self) -> Result<Vec<(ShellJoin, ShellStep)>, SparError> {
        let mut steps = Vec::new();
        let mut join = ShellJoin::Always;
        while self.pos < self.tokens.len() {
            if self.at(&Token::Semicolon) || self.at(&Token::AndAnd) || self.at(&Token::OrOr) {
                return Err(self.error("expected a command before control operator"));
            }
            let step = self.parse_pipeline()?;
            steps.push((join, step));

            if self.pos == self.tokens.len() {
                break;
            }
            let separator = self.advance().clone();
            join = match separator.token {
                Token::Semicolon => ShellJoin::OnSuccess,
                Token::AndAnd => ShellJoin::OnSuccess,
                Token::OrOr => ShellJoin::OnFailure,
                token => {
                    return Err(parse_error(
                        format!("unexpected {} after command", token.human_name()),
                        separator.span,
                    ));
                }
            };
            if self.pos == self.tokens.len() {
                if separator.token == Token::Semicolon {
                    break;
                }
                return Err(parse_error(
                    format!("expected a command after {}", separator.token.human_name()),
                    separator.span,
                ));
            }
        }
        Ok(steps)
    }

    fn parse_pipeline(&mut self) -> Result<ShellStep, SparError> {
        let mut commands = vec![self.parse_command()?];
        while self.at(&Token::ShellPipe) {
            let pipe_span = self.advance().span.clone();
            if self.pos == self.tokens.len() || self.at(&Token::Semicolon) {
                return Err(parse_error("expected a command after pipe", pipe_span));
            }
            commands.push(self.parse_command()?);
        }
        if commands.len() == 1 {
            Ok(ShellStep::Command(Box::new(
                commands.pop().expect("one command"),
            )))
        } else {
            if commands
                .iter()
                .skip(1)
                .any(|command| command.stdin.is_some())
            {
                return Err(self.error("pipeline input already comes from previous stage"));
            }
            Ok(ShellStep::Pipeline(commands))
        }
    }

    fn parse_command(&mut self) -> Result<ShellCommandExpr, SparError> {
        let mut environment = Vec::new();
        while let Some(Token::ShellWord(text)) = self.tokens.get(self.pos).map(|token| &token.token)
        {
            let Some((name, value)) = text.split_once('=') else {
                break;
            };
            let token = self.tokens[self.pos].clone();
            if !valid_environment_name(name) {
                return Err(parse_error(
                    format!("invalid environment assignment: `{text}`"),
                    token.span,
                ));
            }
            self.pos += 1;
            environment.push(ShellEnvironmentEntry {
                name: name.to_string(),
                value: value.to_string(),
                span: token.span,
            });
        }
        if self.pos == self.tokens.len()
            || self.at(&Token::Semicolon)
            || self.at(&Token::AndAnd)
            || self.at(&Token::OrOr)
            || self.at(&Token::ShellPipe)
        {
            let assignment = environment
                .first()
                .map(|entry| format!("{}={}", entry.name, entry.value))
                .unwrap_or_default();
            let variable = environment
                .first()
                .map(|entry| entry.name.to_ascii_lowercase())
                .unwrap_or_else(|| "name".to_string());
            return Err(self.error(format!(
                "environment assignment requires a command; use `export {assignment}` or Spar `var {variable}: str = \"...\";`"
            )));
        }
        let program = self.expect_word("expected an executable name")?;
        let start_span = environment
            .first()
            .map(|entry| entry.span.clone())
            .unwrap_or_else(|| program.span.clone());
        let mut args = Vec::new();
        let mut stdin = None;
        let mut stdout = None;
        let mut stderr = None;
        let mut end_span = start_span.clone();

        while self.pos < self.tokens.len()
            && !self.at(&Token::Semicolon)
            && !self.at(&Token::ShellPipe)
            && !self.at(&Token::AndAnd)
            && !self.at(&Token::OrOr)
        {
            match self.peek() {
                Token::ShellWord(_) => {
                    let word = self.expect_word("expected an argument")?;
                    end_span = word.span.clone();
                    args.push(word);
                }
                Token::Lt | Token::Gt | Token::ShellRedirectAppend | Token::ShellRedirectStderr => {
                    let operator = self.advance().clone();
                    let target = self.expect_word("expected a redirect target")?;
                    end_span = target.span.clone();
                    let redirect = ShellRedirect {
                        target,
                        mode: if operator.token == Token::ShellRedirectAppend {
                            RedirectMode::Append
                        } else {
                            RedirectMode::Truncate
                        },
                        span: joined_span(&operator.span, &end_span),
                    };
                    if operator.token == Token::Lt {
                        if stdin.replace(redirect).is_some() {
                            return Err(parse_error(
                                "stdin may only be redirected once per command",
                                operator.span,
                            ));
                        }
                    } else if operator.token == Token::ShellRedirectStderr {
                        if stderr.replace(redirect).is_some() {
                            return Err(parse_error(
                                "stderr may only be redirected once per command",
                                operator.span,
                            ));
                        }
                    } else if stdout.replace(redirect).is_some() {
                        return Err(parse_error(
                            "stdout may only be redirected once per command",
                            operator.span,
                        ));
                    }
                }
                token => {
                    return Err(self.error(format!(
                        "unexpected {} in shell command",
                        token.human_name()
                    )));
                }
            }
        }

        Ok(ShellCommandExpr {
            environment,
            program,
            args,
            stdin,
            stdout,
            stderr,
            span: joined_span(&start_span, &end_span),
        })
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos].token
    }

    fn at(&self, expected: &Token) -> bool {
        self.tokens
            .get(self.pos)
            .is_some_and(|token| &token.token == expected)
    }

    fn advance(&mut self) -> &SpannedToken {
        let token = &self.tokens[self.pos];
        self.pos += 1;
        token
    }

    fn expect_word(&mut self, message: &str) -> Result<ShellWord, SparError> {
        let Some(token) = self.tokens.get(self.pos).cloned() else {
            return Err(self.error(message));
        };
        let Token::ShellWord(text) = token.token else {
            return Err(parse_error(message, token.span));
        };
        self.pos += 1;
        Ok(ShellWord {
            text,
            span: token.span,
        })
    }

    fn error(&self, message: impl Into<String>) -> SparError {
        let span = self
            .tokens
            .get(self.pos)
            .map(|token| token.span.clone())
            .or_else(|| self.tokens.last().map(|token| token.span.clone()))
            .unwrap_or_else(Span::dummy);
        parse_error(message, span)
    }
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first == b'_' || first.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn joined_span(start: &Span, end: &Span) -> Span {
    Span::new(start.start, end.end, start.line, start.col)
}

fn parse_error(message: impl Into<String>, span: Span) -> SparError {
    SparError::ParseError {
        message: message.into(),
        span,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_command_expression, parse_shell_block};
    use crate::ast::{ShellJoin, ShellStep};
    use crate::lexer::Lexer;
    use spar_command::RedirectMode;

    fn parse_block(source: &str) -> crate::ast::ShellExpr {
        let tokens = Lexer::new(source).tokenize().expect("lex failed");
        let (expression, consumed) = parse_shell_block(&tokens).expect("parse failed");
        assert_eq!(consumed, tokens.len() - 1, "must stop before EOF");
        expression
    }

    #[test]
    fn shell_lang_parses_one_command() {
        let expression = parse_block("shell { echo hello; }");
        assert_eq!(expression.steps.len(), 1);
        let (ShellJoin::Always, ShellStep::Command(command)) = &expression.steps[0] else {
            panic!("expected one always-run command")
        };
        assert_eq!(command.program.text, "echo");
        assert_eq!(command.args.len(), 1);
        assert_eq!(command.args[0].text, "hello");
    }

    #[test]
    fn shell_lang_sequences_commands_with_fail_fast_joins() {
        let expression = parse_block("shell { a; b; c; }");
        assert_eq!(expression.steps.len(), 3);
        assert!(matches!(expression.steps[0].0, ShellJoin::Always));
        assert!(matches!(expression.steps[1].0, ShellJoin::OnSuccess));
        assert!(matches!(expression.steps[2].0, ShellJoin::OnSuccess));
    }

    #[test]
    fn shell_lang_parses_a_pipeline_as_one_step() {
        let expression = parse_block("shell { a | b | c; }");
        let ShellStep::Pipeline(commands) = &expression.steps[0].1 else {
            panic!("expected a pipeline")
        };
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0].program.text, "a");
        assert_eq!(commands[1].program.text, "b");
        assert_eq!(commands[2].program.text, "c");
    }

    #[test]
    fn shell_lang_attaches_redirects_with_their_modes() {
        let truncate = parse_block("shell { cargo test > test.log; }");
        let append = parse_block("shell { cargo test >> test.log; }");
        let stderr = parse_block("shell { cargo test 2> errors.log; }");

        let ShellStep::Command(truncate) = &truncate.steps[0].1 else {
            panic!("expected command")
        };
        let ShellStep::Command(append) = &append.steps[0].1 else {
            panic!("expected command")
        };
        let ShellStep::Command(stderr) = &stderr.steps[0].1 else {
            panic!("expected command")
        };

        let stdout = truncate.stdout.as_ref().expect("stdout redirect");
        assert_eq!(stdout.target.text, "test.log");
        assert_eq!(stdout.mode, RedirectMode::Truncate);
        assert_eq!(
            append.stdout.as_ref().expect("stdout redirect").mode,
            RedirectMode::Append
        );
        let stderr = stderr.stderr.as_ref().expect("stderr redirect");
        assert_eq!(stderr.target.text, "errors.log");
        assert_eq!(stderr.mode, RedirectMode::Truncate);
    }

    #[test]
    fn shell_lang_attaches_pipeline_redirect_to_the_last_stage() {
        let expression = parse_block("shell { a | b > out.log; }");
        let ShellStep::Pipeline(commands) = &expression.steps[0].1 else {
            panic!("expected pipeline")
        };
        assert!(commands[0].stdout.is_none());
        assert_eq!(
            commands[1]
                .stdout
                .as_ref()
                .expect("last-stage redirect")
                .target
                .text,
            "out.log"
        );
    }

    #[test]
    fn shell_lang_preserves_quoted_argument_boundaries_and_empty_plans() {
        let expression = parse_block(r#"shell { rm "my file.txt"; }"#);
        let ShellStep::Command(command) = &expression.steps[0].1 else {
            panic!("expected command")
        };
        assert_eq!(command.args.len(), 1);
        assert_eq!(command.args[0].text, "my file.txt");
        assert!(parse_block("shell {}").steps.is_empty());
    }

    #[test]
    fn shell_lang_rejects_a_dangling_pipeline() {
        let tokens = Lexer::new("shell { a | }").tokenize().expect("lex failed");
        let err = parse_shell_block(&tokens).expect_err("dangling pipe must fail");
        assert!(err.to_string().contains("pipe"), "got: {err}");
    }

    #[test]
    fn shell_lang_command_sugar_uses_the_same_segment_parser() {
        let tokens = Lexer::new("command echo hello;")
            .tokenize()
            .expect("lex failed");
        let (expression, consumed) =
            parse_command_expression(&tokens).expect("command parse failed");
        assert_eq!(consumed, tokens.len() - 1);
        let ShellStep::Command(command) = &expression.steps[0].1 else {
            panic!("expected command")
        };
        assert_eq!(command.program.text, "echo");
        assert_eq!(command.args[0].text, "hello");
    }
}
