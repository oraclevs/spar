use crate::ast::{
    ShellCommandExpr, ShellEnvironmentEntry, ShellExpr, ShellFdRedirect, ShellFdRedirectTarget,
    ShellJoin, ShellRedirect, ShellStep, ShellWord, ShellWordPart, TopLevelItem,
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
            statements: Vec::new(),
            steps,
            span: joined_span(&start.span, &end_span),
            foreign_shell: None,
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
            statements: Vec::new(),
            steps,
            span: joined_span(&start.span, &end_span),
            foreign_shell: None,
        },
        end + 1,
    ))
}

pub(crate) fn parse_bare_command_statement(
    tokens: &[SpannedToken],
) -> Result<(ShellExpr, usize), SparError> {
    let Some(start) = tokens.first() else {
        return Err(parse_error("expected a command", Span::dummy()));
    };
    let end = tokens
        .iter()
        .position(|token| token.token == Token::Semicolon)
        .ok_or_else(|| parse_error("unterminated command — expected ';'", start.span.clone()))?;
    let steps = BodyParser::new(&tokens[..end]).parse()?;
    let end_span = tokens[end].span.clone();
    Ok((
        ShellExpr {
            statements: Vec::new(),
            steps,
            span: joined_span(&start.span, &end_span),
            foreign_shell: None,
        },
        end + 1,
    ))
}

pub(crate) fn parse_command_substitution(
    tokens: &[SpannedToken],
) -> Result<(ShellExpr, usize), SparError> {
    let Some(start) = tokens.first() else {
        return Err(parse_error("expected command substitution", Span::dummy()));
    };
    if start.token != Token::CommandSubStart {
        return Err(parse_error("expected '$('", start.span.clone()));
    }
    let end = tokens[1..]
        .iter()
        .position(|token| token.token == Token::CommandSubEnd)
        .map(|position| position + 1)
        .ok_or_else(|| parse_error("unterminated command substitution", start.span.clone()))?;
    let steps = BodyParser::new(&tokens[1..end]).parse()?;
    if steps.len() != 1 {
        return Err(parse_error(
            "command substitution must contain one command or pipeline",
            start.span.clone(),
        ));
    }
    Ok((
        ShellExpr {
            statements: Vec::new(),
            steps,
            span: joined_span(&start.span, &tokens[end].span),
            foreign_shell: None,
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
                let message = match self
                    .pos
                    .checked_sub(1)
                    .and_then(|position| self.tokens.get(position))
                {
                    Some(token) if token.token == Token::AndAnd => "expected a command after '&&'",
                    Some(token) if token.token == Token::OrOr => "expected a command after '||'",
                    _ => "expected a command before control operator",
                };
                return Err(self.error(message));
            }
            let step = self.parse_pipeline()?;
            steps.push((join, step));

            if self.pos == self.tokens.len() {
                break;
            }
            let separator = self.advance().clone();
            join = match separator.token {
                Token::Semicolon => ShellJoin::Always,
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
        let mut redirections = Vec::new();
        let mut background = false;
        let mut end_span = start_span.clone();

        while self.pos < self.tokens.len()
            && !self.at(&Token::Semicolon)
            && !self.at(&Token::ShellPipe)
            && !self.at(&Token::AndAnd)
            && !self.at(&Token::OrOr)
            && !self.at(&Token::ShellBackground)
        {
            match self.peek().clone() {
                Token::ShellWord(_) | Token::ShellLiteralWord(_) => {
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
                        redirections.push(ShellFdRedirect {
                            fd: 0,
                            target: ShellFdRedirectTarget::File(redirect.clone()),
                            span: redirect.span.clone(),
                        });
                        stdin = Some(redirect);
                    } else if operator.token == Token::ShellRedirectStderr {
                        redirections.push(ShellFdRedirect {
                            fd: 2,
                            target: ShellFdRedirectTarget::File(redirect.clone()),
                            span: redirect.span.clone(),
                        });
                        stderr = Some(redirect);
                    } else {
                        redirections.push(ShellFdRedirect {
                            fd: 1,
                            target: ShellFdRedirectTarget::File(redirect.clone()),
                            span: redirect.span.clone(),
                        });
                        stdout = Some(redirect);
                    }
                }
                Token::ShellFdRedirect { fd, append } => {
                    let operator = self.advance().clone();
                    let target = self.expect_word("expected a redirect target")?;
                    let redirect = ShellRedirect {
                        target,
                        mode: if append {
                            RedirectMode::Append
                        } else {
                            RedirectMode::Truncate
                        },
                        span: operator.span.clone(),
                    };
                    redirections.push(ShellFdRedirect {
                        fd,
                        target: ShellFdRedirectTarget::File(redirect),
                        span: operator.span,
                    });
                }
                Token::ShellFdDuplicate { fd, target } => {
                    let operator = self.advance().clone();
                    redirections.push(ShellFdRedirect {
                        fd,
                        target: ShellFdRedirectTarget::Duplicate(target),
                        span: operator.span,
                    });
                }
                Token::ShellRedirectBoth { append } => {
                    let operator = self.advance().clone();
                    let target = self.expect_word("expected a redirect target")?;
                    let redirect = ShellRedirect {
                        target,
                        mode: if append {
                            RedirectMode::Append
                        } else {
                            RedirectMode::Truncate
                        },
                        span: operator.span.clone(),
                    };
                    redirections.push(ShellFdRedirect {
                        fd: 1,
                        target: ShellFdRedirectTarget::File(redirect),
                        span: operator.span.clone(),
                    });
                    redirections.push(ShellFdRedirect {
                        fd: 2,
                        target: ShellFdRedirectTarget::Duplicate(1),
                        span: operator.span,
                    });
                }
                token => {
                    return Err(self.error(format!(
                        "unexpected {} in shell command",
                        token.human_name()
                    )));
                }
            }
        }

        if self.at(&Token::ShellBackground) {
            self.advance();
            background = true;
        }

        Ok(ShellCommandExpr {
            environment,
            program,
            args,
            stdin,
            stdout,
            stderr,
            redirections,
            background,
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
        let (text, literal) = match token.token {
            Token::ShellWord(text) => (text, false),
            Token::ShellLiteralWord(text) => (text, true),
            _ => return Err(parse_error(message, token.span)),
        };
        self.pos += 1;
        let parts = if literal {
            vec![ShellWordPart::Literal(text.clone())]
        } else {
            parse_word_parts(&text, &token.span)?
        };
        Ok(ShellWord {
            text,
            parts,
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

fn parse_word_parts(text: &str, span: &Span) -> Result<Vec<ShellWordPart>, SparError> {
    let mut parts = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find('$') {
        let dollar = cursor + relative;
        if dollar > cursor {
            parts.push(ShellWordPart::Literal(text[cursor..dollar].to_string()));
        }
        if text[dollar..].starts_with("${") {
            let expression_start = dollar + 2;
            let Some(relative_end) = text[expression_start..].find('}') else {
                return Err(parse_error(
                    "unterminated '${...}' interpolation",
                    span.clone(),
                ));
            };
            let expression_end = expression_start + relative_end;
            parts.push(ShellWordPart::Expr(parse_word_expression(
                &text[expression_start..expression_end],
                span,
            )?));
            cursor = expression_end + 1;
        } else if text[dollar..].starts_with("$!") || text[dollar..].starts_with("$?") {
            parts.push(ShellWordPart::Environment(
                text[dollar + 1..dollar + 2].to_string(),
            ));
            cursor = dollar + 2;
        } else {
            let name_start = dollar + 1;
            let name_len = text[name_start..]
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .map(char::len_utf8)
                .sum::<usize>();
            if name_len == 0 {
                parts.push(ShellWordPart::Literal("$".into()));
                cursor = name_start;
            } else {
                parts.push(ShellWordPart::Environment(
                    text[name_start..name_start + name_len].to_string(),
                ));
                cursor = name_start + name_len;
            }
        }
    }
    if cursor < text.len() {
        parts.push(ShellWordPart::Literal(text[cursor..].to_string()));
    }
    if parts.is_empty() {
        parts.push(ShellWordPart::Literal(text.to_string()));
    }
    Ok(parts)
}

fn parse_word_expression(source: &str, span: &Span) -> Result<crate::ast::Expr, SparError> {
    let wrapped = format!("var __shell_interpolation: str = {source};");
    let tokens = crate::lexer::Lexer::new(&wrapped).tokenize()?;
    let program = crate::parser::Parser::new(tokens).parse()?;
    let Some(TopLevelItem::Var(variable)) = program.items.into_iter().next() else {
        return Err(parse_error("invalid shell interpolation", span.clone()));
    };
    variable
        .value
        .ok_or_else(|| parse_error("empty shell interpolation", span.clone()))
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
    fn shell_lang_sequences_commands_unconditionally() {
        let expression = parse_block("shell { a; b; c; }");
        assert_eq!(expression.steps.len(), 3);
        assert!(matches!(expression.steps[0].0, ShellJoin::Always));
        assert!(matches!(expression.steps[1].0, ShellJoin::Always));
        assert!(matches!(expression.steps[2].0, ShellJoin::Always));
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
