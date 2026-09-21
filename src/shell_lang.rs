use crate::ast::{
    DecoderNamespace, DecoderRef, Expr, NamedDecoderArg, ShellCodecStage, ShellCommandExpr,
    ShellDecodeStage, ShellEnvironmentEntry, ShellExpr, ShellFdRedirect, ShellFdRedirectTarget,
    ShellJoin, ShellMixedPipeline, ShellRedirect, ShellStep, ShellWord, ShellWordPart,
    TopLevelItem,
};
use crate::error::{Span, SparError};
use crate::lexer::Lexer;
use crate::parser::parse_expression_tokens;
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
            end_line: end_span.line,
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
            end_line: end_span.line,
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
            end_line: end_span.line,
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
    if steps.is_empty() {
        return Err(parse_error(
            "command substitution must contain at least one command or pipeline",
            start.span.clone(),
        ));
    }
    if steps.iter().any(|(_, step)| match step {
        ShellStep::Command(command) => command.background,
        ShellStep::Pipeline(commands) => commands.iter().any(|command| command.background),
        ShellStep::MixedPipeline(pipeline) => pipeline
            .input
            .iter()
            .chain(pipeline.output.iter())
            .any(|command| command.background),
    }) {
        return Err(parse_error(
            "background commands are not allowed inside command substitution",
            start.span.clone(),
        ));
    }
    Ok((
        ShellExpr {
            statements: Vec::new(),
            steps,
            span: joined_span(&start.span, &tokens[end].span),
            foreign_shell: None,
            end_line: tokens[end].span.line,
        },
        end + 1,
    ))
}

fn command_has_stdout_redirect(command: &ShellCommandExpr) -> bool {
    command.stdout.is_some() || command.redirections.iter().any(|redirect| redirect.fd == 1)
}

fn command_has_stdin_redirect(command: &ShellCommandExpr) -> bool {
    command.stdin.is_some() || command.redirections.iter().any(|redirect| redirect.fd == 0)
}

fn decode_bridge_from_command(
    command: &ShellCommandExpr,
) -> Result<Option<ShellDecodeStage>, SparError> {
    if command.program.text != "from" {
        return Ok(None);
    }
    if !command.environment.is_empty()
        || command.background
        || command.stdin.is_some()
        || command.stdout.is_some()
        || command.stderr.is_some()
        || !command.redirections.is_empty()
    {
        return Err(parse_error(
            "`from FORMAT` is a byte-to-value bridge and cannot carry shell environment/redirection/background syntax",
            command.span.clone(),
        ));
    }
    if command.args.len() != 1 || !word_is_literal(&command.args[0]) {
        return Err(parse_error(
            "`from` expects exactly one literal format name, for example `| from jsonl`",
            command.span.clone(),
        ));
    }
    Ok(Some(ShellDecodeStage {
        decoder: DecoderRef {
            namespace: None,
            name: command.args[0].text.clone(),
            span: command.args[0].span.clone(),
        },
        args: Vec::new(),
        span: command.span.clone(),
    }))
}

fn encode_bridge_from_stage(raw: &str, span: &Span) -> Result<Option<ShellCodecStage>, SparError> {
    let mut words = raw.split_whitespace();
    if words.next() != Some("to") {
        return Ok(None);
    }
    let Some(format) = words.next() else {
        return Err(parse_error(
            "`to` expects a literal format name, for example `|> to jsonl`",
            span.clone(),
        ));
    };
    if words.next().is_some()
        || format
            .chars()
            .any(|ch| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
    {
        return Err(parse_error(
            "`to` expects exactly one literal format name, for example `|> to jsonl`",
            span.clone(),
        ));
    }
    Ok(Some(ShellCodecStage {
        format: format.to_string(),
        span: span.clone(),
    }))
}

fn word_is_literal(word: &ShellWord) -> bool {
    word.parts
        .iter()
        .all(|part| matches!(part, ShellWordPart::Literal(_)))
}

fn parse_structured_stage_expression(raw: &str, span: &Span) -> Result<Expr, SparError> {
    let mut tokens = Lexer::new(raw).tokenize()?;
    for token in &mut tokens {
        let relative_line = token.span.line;
        let relative_col = token.span.col;
        token.span.start = span.start.saturating_add(token.span.start);
        token.span.end = span.start.saturating_add(token.span.end);
        token.span.line = span.line.saturating_add(relative_line.saturating_sub(1));
        token.span.col = if relative_line <= 1 {
            span.col.saturating_add(relative_col.saturating_sub(1))
        } else {
            relative_col
        };
    }
    parse_expression_tokens(tokens).map_err(|error| match error {
        SparError::LexError { message, .. } => SparError::LexError {
            message,
            span: span.clone(),
        },
        SparError::ParseError { message, .. } => SparError::ParseError {
            message,
            span: span.clone(),
        },
        other => other,
    })
}

fn decoder_name_valid(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn decoder_subspan(raw: &str, parent: &Span, start: usize, end: usize) -> Span {
    let prefix = &raw[..start.min(raw.len())];
    let line_offset = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line = parent.line.saturating_add(line_offset);
    let col = if let Some(last_newline) = prefix.rfind('\n') {
        (start - last_newline) as u32
    } else {
        parent.col.saturating_add(start as u32)
    };
    Span::new(
        parent.start.saturating_add(start),
        parent.start.saturating_add(end),
        line,
        col,
    )
}

fn top_level_positions(text: &str, needle: u8) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut positions = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut paren = 0_u32;
    let mut bracket = 0_u32;
    let mut brace = 0_u32;
    let mut i = 0usize;
    while i < bytes.len() {
        let byte = bytes[i];
        if escaped {
            escaped = false;
            i += 1;
            continue;
        }
        if byte == b'\\' && quote != Some(b'\'') {
            escaped = true;
            i += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = if quote == Some(byte) {
                None
            } else if quote.is_none() {
                Some(byte)
            } else {
                quote
            };
            i += 1;
            continue;
        }
        if quote.is_none() {
            match byte {
                b'(' => paren += 1,
                b')' => paren = paren.saturating_sub(1),
                b'[' => bracket += 1,
                b']' => bracket = bracket.saturating_sub(1),
                b'{' => brace += 1,
                b'}' => brace = brace.saturating_sub(1),
                _ => {}
            }
            if byte == needle && paren == 0 && bracket == 0 && brace == 0 {
                positions.push(i);
            }
        }
        i += 1;
    }
    positions
}

fn split_decoder_args(text: &str) -> Vec<(usize, usize)> {
    let commas = top_level_positions(text, b',');
    let mut result = Vec::new();
    let mut start = 0usize;
    for comma in commas {
        result.push((start, comma));
        start = comma + 1;
    }
    result.push((start, text.len()));
    result
}

fn parse_decoder_stage(raw: &str, span: &Span) -> Result<ShellDecodeStage, SparError> {
    let raw = raw.trim();
    let (head, args_text, args_base) = if let Some(open) = raw.find('(') {
        if !raw.ends_with(')') {
            return Err(parse_error(
                "decoder option list must end with ')'",
                span.clone(),
            ));
        }
        (&raw[..open], Some(&raw[open + 1..raw.len() - 1]), open + 1)
    } else {
        (raw, None, raw.len())
    };
    let head = head.trim();
    let (namespace, name) = if let Some((prefix, name)) = head.split_once("::") {
        if name.contains("::") {
            return Err(parse_error(
                "decoder reference may contain only one namespace separator",
                span.clone(),
            ));
        }
        let namespace = match prefix {
            "codec" => DecoderNamespace::Codec,
            "scoc" => DecoderNamespace::Scoc,
            "custom" => DecoderNamespace::Custom,
            _ => {
                return Err(parse_error(
                    format!("unknown decoder namespace `{prefix}`"),
                    span.clone(),
                ))
            }
        };
        (Some(namespace), name)
    } else {
        (None, head)
    };
    if !decoder_name_valid(name) {
        return Err(parse_error(
            "decoder name must start with a letter and contain only letters, digits, '_' or '-'",
            span.clone(),
        ));
    }
    let name_offset = raw.find(name).unwrap_or(0);
    let decoder_span = decoder_subspan(raw, span, name_offset, name_offset + name.len());
    let decoder = DecoderRef {
        namespace,
        name: name.to_string(),
        span: decoder_span,
    };

    let mut args = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Some(args_text) = args_text {
        if !args_text.trim().is_empty() {
            for (part_start, part_end) in split_decoder_args(args_text) {
                let part = &args_text[part_start..part_end];
                let leading = part.len() - part.trim_start().len();
                let trailing = part.trim_end().len();
                if trailing <= leading {
                    return Err(parse_error("empty decoder argument", span.clone()));
                }
                let trimmed = &part[leading..trailing];
                let colons = top_level_positions(trimmed, b':');
                if colons.len() != 1 {
                    return Err(parse_error(
                        "decoder arguments use `name: expression` syntax",
                        span.clone(),
                    ));
                }
                let colon = colons[0];
                let arg_name = trimmed[..colon].trim();
                if arg_name.is_empty()
                    || !arg_name
                        .chars()
                        .next()
                        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
                    || !arg_name
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                {
                    return Err(parse_error("invalid decoder option name", span.clone()));
                }
                if !seen.insert(arg_name.to_string()) {
                    return Err(parse_error(
                        format!("duplicate decoder option `{arg_name}`"),
                        span.clone(),
                    ));
                }
                let expr_source = trimmed[colon + 1..].trim();
                if expr_source.is_empty() {
                    return Err(parse_error(
                        format!("decoder option `{arg_name}` requires a value"),
                        span.clone(),
                    ));
                }
                let expr_in_trimmed =
                    trimmed[colon + 1..].find(expr_source).unwrap_or(0) + colon + 1;
                let value_start = args_base + part_start + leading + expr_in_trimmed;
                let value_span =
                    decoder_subspan(raw, span, value_start, value_start + expr_source.len());
                let value = parse_structured_stage_expression(expr_source, &value_span)?;
                let arg_start = args_base + part_start + leading;
                let arg_end = args_base + part_start + trailing;
                args.push(NamedDecoderArg {
                    name: arg_name.to_string(),
                    value,
                    span: decoder_subspan(raw, span, arg_start, arg_end),
                });
            }
        }
    }
    Ok(ShellDecodeStage {
        decoder,
        args,
        span: span.clone(),
    })
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
        let first = self.parse_command()?;
        let start_span = first.span.clone();
        let mut input = vec![first];
        let mut decoder: Option<ShellDecodeStage> = None;
        let mut stages = Vec::new();
        let mut encoder: Option<ShellCodecStage> = None;
        let mut output = Vec::new();

        loop {
            if self.at(&Token::ShellPipe) {
                let pipe_span = self.advance().span.clone();
                if self.pos == self.tokens.len() || self.at(&Token::Semicolon) {
                    return Err(parse_error("expected a command after pipe", pipe_span));
                }
                if self.at(&Token::StructuredPipe) {
                    return Err(parse_error("expected a byte command after '|'", pipe_span));
                }
                if let Some(token) = self.tokens.get(self.pos).cloned() {
                    if let Token::ShellDecoderStage(raw) = token.token {
                        if decoder.is_some() {
                            return Err(parse_error(
                                "a mixed pipeline can contain only one `from` decoder",
                                token.span,
                            ));
                        }
                        if command_has_stdout_redirect(input.last().expect("input command")) {
                            return Err(parse_error(
                                "the command before `from` cannot redirect stdout; `from` must receive that byte stream",
                                token.span,
                            ));
                        }
                        self.pos += 1;
                        decoder = Some(parse_decoder_stage(&raw, &token.span)?);
                        continue;
                    }
                }
                let command = self.parse_command()?;
                if decoder.is_none() {
                    if let Some(stage) = decode_bridge_from_command(&command)? {
                        if command_has_stdout_redirect(input.last().expect("input command")) {
                            return Err(parse_error(
                                "the command before `from` cannot redirect stdout; `from` must receive that byte stream",
                                command.span.clone(),
                            ));
                        }
                        decoder = Some(stage);
                    } else {
                        if command_has_stdin_redirect(&command) {
                            return Err(parse_error(
                                "pipeline input already comes from previous stage",
                                command.span.clone(),
                            ));
                        }
                        input.push(command);
                    }
                } else if encoder.is_none() {
                    return Err(parse_error(
                        "structured values cannot enter a Unix `|` pipeline directly; add `|> to FORMAT` first",
                        pipe_span,
                    ));
                } else {
                    if output.is_empty() && command_has_stdin_redirect(&command) {
                        return Err(parse_error(
                            "the first command after `to` receives stdin from the structured serializer",
                            command.span.clone(),
                        ));
                    }
                    output.push(command);
                }
                continue;
            }

            if self.at(&Token::StructuredPipe) {
                let pipe_span = self.advance().span.clone();
                if decoder.is_none() {
                    return Err(parse_error(
                        "Unix bytes cannot enter `|>` directly; add `| from FORMAT` first",
                        pipe_span,
                    ));
                }
                if encoder.is_some() {
                    return Err(parse_error(
                        "structured stages cannot follow `to`; use Unix `|` after `|> to FORMAT`",
                        pipe_span,
                    ));
                }
                let token = self.tokens.get(self.pos).cloned().ok_or_else(|| {
                    parse_error("expected a Spar stage after '|>'", pipe_span.clone())
                })?;
                let Token::ShellStructuredStage(raw) = token.token else {
                    return Err(parse_error("expected a Spar stage after '|>'", token.span));
                };
                self.pos += 1;
                if let Some(stage) = encode_bridge_from_stage(&raw, &token.span)? {
                    encoder = Some(stage);
                } else {
                    stages.push(parse_structured_stage_expression(&raw, &token.span)?);
                }
                continue;
            }
            break;
        }

        // `|> to FORMAT > file`: the serializer writes straight to a file.
        let mut encoder_redirect = None;
        if encoder.is_some()
            && decoder.is_some()
            && (self.at(&Token::Gt) || self.at(&Token::ShellRedirectAppend))
        {
            if !output.is_empty() {
                return Err(self
                    .error("`to FORMAT` cannot both redirect to a file and pipe into a command"));
            }
            let operator = self.advance().clone();
            let target = self.expect_word("expected a redirect target after `to FORMAT >`")?;
            let span = joined_span(&operator.span, &target.span);
            encoder_redirect = Some(ShellRedirect {
                target,
                mode: if operator.token == Token::ShellRedirectAppend {
                    RedirectMode::Append
                } else {
                    RedirectMode::Truncate
                },
                span,
            });
        }

        let Some(decoder) = decoder else {
            if input.len() == 1 {
                return Ok(ShellStep::Command(Box::new(
                    input.pop().expect("one command"),
                )));
            }
            if input.iter().skip(1).any(command_has_stdin_redirect) {
                return Err(self.error("pipeline input already comes from previous stage"));
            }
            return Ok(ShellStep::Pipeline(input));
        };

        if input.iter().any(|command| command.background)
            || output.iter().any(|command| command.background)
        {
            return Err(parse_error(
                "background execution is not supported for mixed structured pipelines",
                start_span.clone(),
            ));
        }
        let end_span = output
            .last()
            .map(|command| command.span.clone())
            .or_else(|| {
                encoder_redirect
                    .as_ref()
                    .map(|redirect| redirect.span.clone())
            })
            .or_else(|| encoder.as_ref().map(|stage| stage.span.clone()))
            .or_else(|| stages.last().and_then(|stage| stage.span().cloned()))
            .unwrap_or_else(|| decoder.span.clone());
        Ok(ShellStep::MixedPipeline(Box::new(ShellMixedPipeline {
            input,
            decoder,
            stages,
            encoder,
            output,
            encoder_redirect,
            span: joined_span(&start_span, &end_span),
        })))
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
            let value_span = Span::new(
                token.span.start + name.len() + 1,
                token.span.end,
                token.span.line,
                token.span.col + name.chars().count() as u32 + 1,
            );
            let mut parts = parse_word_parts(value, &value_span)?;
            let mut value_text = value.to_string();
            let mut end_span = token.span.clone();
            self.merge_adjacent_fragments(&mut parts, &mut value_text, &mut end_span)?;
            environment.push(ShellEnvironmentEntry {
                name: name.to_string(),
                value: ShellWord {
                    text: value_text,
                    parts,
                    span: value_span,
                },
                span: Span::new(
                    token.span.start,
                    end_span.end,
                    token.span.line,
                    token.span.col,
                ),
            });
        }
        if self.pos == self.tokens.len()
            || self.at(&Token::Semicolon)
            || self.at(&Token::AndAnd)
            || self.at(&Token::OrOr)
            || self.at(&Token::ShellPipe)
            || self.at(&Token::StructuredPipe)
        {
            let assignment = environment
                .first()
                .map(|entry| format!("{}={}", entry.name, entry.value.text))
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
        if program.text.contains('(') {
            return Err(parse_error(
                format!(
                    "'{}' looks like a Spar function call, not a shell command — function \
                     calls are not supported inside a native command chain (after ';', '&&', \
                     or '||'); capture the command's result instead: \
                     `var ok: str = $(cmd && echo \"true\" || echo \"false\"); \
                     if ok == \"true\" {{ ... }} else {{ ... }}`",
                    program.text
                ),
                program.span.clone(),
            ));
        }
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
            && !self.at(&Token::StructuredPipe)
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

    /// Appends every physically adjacent shell-word fragment to a word being
    /// built (see the note below on why the lexer emits fragments).
    fn merge_adjacent_fragments(
        &mut self,
        parts: &mut Vec<ShellWordPart>,
        combined_text: &mut String,
        end_span: &mut Span,
    ) -> Result<(), SparError> {
        // Shell quoting is compositional: adjacent fragments with no
        // intervening whitespace form one argv word.  For example:
        //
        //     duration="${duration}"
        //     "prefix-"$name
        //     pre'literal'post
        //
        // The lexer deliberately emits quoted and unquoted fragments as
        // separate tokens so each fragment keeps its interpolation rules.
        // Merge only physically-adjacent shell-word tokens here; a real
        // whitespace gap must remain an argv boundary.
        while let Some(next) = self.tokens.get(self.pos).cloned() {
            if next.span.start != end_span.end {
                break;
            }
            let (next_text, next_literal) = match next.token {
                Token::ShellWord(text) => (text, false),
                Token::ShellLiteralWord(text) => (text, true),
                _ => break,
            };
            let next_parts = if next_literal {
                vec![ShellWordPart::Literal(next_text.clone())]
            } else {
                parse_word_parts(&next_text, &next.span)?
            };
            combined_text.push_str(&next_text);
            parts.extend(next_parts);
            end_span.end = next.span.end;
            self.pos += 1;
        }
        Ok(())
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
        let mut parts = if literal {
            vec![ShellWordPart::Literal(text.clone())]
        } else {
            parse_word_parts(&text, &token.span)?
        };
        let mut combined_text = text;
        let mut end_span = token.span.clone();

        self.merge_adjacent_fragments(&mut parts, &mut combined_text, &mut end_span)?;

        Ok(ShellWord {
            text: combined_text,
            parts,
            span: end_span,
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
    // Where `text[0]` sits in the real source: a quoted fragment's token
    // span includes its surrounding quotes, its text does not.
    let origin = span.start + usize::from(span.end - span.start == text.len() + 2);
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
                origin + expression_start,
                span,
            )?));
            cursor = expression_end + 1;
        } else if text[dollar..].starts_with("$(") {
            let Some(end) = command_substitution_end(text, dollar) else {
                return Err(parse_error(
                    "unterminated '$(...)' command substitution",
                    span.clone(),
                ));
            };
            let source = &text[dollar..=end];
            let mut tokens = crate::lexer::Lexer::new(source).tokenize()?;
            relocate_tokens(&mut tokens, 0, origin + dollar, span);
            let (shell, _) = parse_command_substitution(&tokens)?;
            parts.push(ShellWordPart::CommandSubstitution(shell));
            cursor = end + 1;
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

/// Returns the byte index of the `)` closing the `$(` at `dollar`.
/// Quotes and escapes inside the native command are respected and nested
/// parenthesized text is balanced, so command substitution can safely appear
/// as one segment of a larger argv word.
fn command_substitution_end(text: &str, dollar: usize) -> Option<usize> {
    if !text[dollar..].starts_with("$(") {
        return None;
    }
    let mut depth = 1_u32;
    let mut quote = None;
    let mut escaped = false;
    for (relative, ch) in text[dollar + 2..].char_indices() {
        let index = dollar + 2 + relative;
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = if quote == Some(ch) {
                None
            } else if quote.is_none() {
                Some(ch)
            } else {
                quote
            };
            continue;
        }
        if quote.is_some() {
            continue;
        }
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// Interpolated text is lexed on its own, so its token spans start at zero.
/// Move them to where the text really sits in the file (`real_origin`), so
/// spans on the resulting AST are usable for editor features.
/// `synthetic_origin` is the offset in the lexed text at which the real text
/// begins (non-zero when it was wrapped in a synthetic declaration).
fn relocate_tokens(
    tokens: &mut [crate::token::SpannedToken],
    synthetic_origin: usize,
    real_origin: usize,
    word_span: &Span,
) {
    for token in tokens {
        if token.span.start < synthetic_origin {
            token.span = word_span.clone();
            continue;
        }
        let start = real_origin + (token.span.start - synthetic_origin);
        let end = real_origin + token.span.end.saturating_sub(synthetic_origin);
        let col = word_span.col + start.saturating_sub(word_span.start) as u32;
        token.span = Span::new(start, end.max(start), word_span.line, col);
    }
}

fn parse_word_expression(
    source: &str,
    real_origin: usize,
    span: &Span,
) -> Result<crate::ast::Expr, SparError> {
    const PREFIX: &str = "var __shell_interpolation: str = ";
    let wrapped = format!("{PREFIX}{source};");
    let mut tokens = crate::lexer::Lexer::new(&wrapped).tokenize()?;
    relocate_tokens(&mut tokens, PREFIX.len(), real_origin, span);
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
    use crate::ast::{DecoderNamespace, ShellJoin, ShellStep, ShellWordPart};
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
    fn shell_lang_concatenates_assignment_and_quoted_interpolation_into_one_argv() {
        let expression =
            parse_block(r#"shell { awk -v duration="${duration}" 'BEGIN { print duration }'; }"#);
        let ShellStep::Command(command) = &expression.steps[0].1 else {
            panic!("expected command")
        };
        assert_eq!(command.program.text, "awk");
        assert_eq!(
            command.args.len(),
            3,
            "argv fragments must not split at quotes"
        );
        assert_eq!(command.args[0].text, "-v");
        assert_eq!(command.args[1].text, "duration=${duration}");
        assert!(matches!(
            command.args[1].parts.as_slice(),
            [ShellWordPart::Literal(prefix), ShellWordPart::Expr(_)] if prefix == "duration="
        ));
        assert_eq!(command.args[2].text, "BEGIN { print duration }");
    }

    #[test]
    fn shell_lang_concatenates_mixed_quote_fragments_without_whitespace() {
        let expression = parse_block(r#"shell { printf pre'literal'"-${name}"post; }"#);
        let ShellStep::Command(command) = &expression.steps[0].1 else {
            panic!("expected command")
        };
        assert_eq!(command.args.len(), 1);
        assert_eq!(command.args[0].text, "preliteral-${name}post");
        assert!(matches!(
            command.args[0].parts.as_slice(),
            [
                ShellWordPart::Literal(prefix),
                ShellWordPart::Literal(literal),
                ShellWordPart::Literal(separator),
                ShellWordPart::Expr(_),
                ShellWordPart::Literal(suffix)
            ] if prefix == "pre" && literal == "literal" && separator == "-" && suffix == "post"
        ));
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
        let tokens = Lexer::new("shell { a |; }").tokenize().expect("lex failed");
        let err = parse_shell_block(&tokens).expect_err("dangling pipe must fail");
        assert!(err.to_string().contains("pipe"), "got: {err}");
    }

    #[test]
    fn command_substitution_accepts_logical_command_chains() {
        let tokens = Lexer::new("$(test -f file && echo true || echo false)")
            .tokenize()
            .expect("lex failed");
        let (expression, consumed) = super::parse_command_substitution(&tokens)
            .expect("logical command substitution must parse");
        assert_eq!(consumed, tokens.len() - 1);
        assert_eq!(expression.steps.len(), 3);
        assert!(matches!(expression.steps[0].0, ShellJoin::Always));
        assert!(matches!(expression.steps[1].0, ShellJoin::OnSuccess));
        assert!(matches!(expression.steps[2].0, ShellJoin::OnFailure));
    }

    #[test]
    fn command_substitution_rejects_background_commands() {
        let tokens = Lexer::new("$(sleep 1 &)").tokenize().expect("lex failed");
        let error = super::parse_command_substitution(&tokens)
            .expect_err("background substitution must fail");
        assert!(error.to_string().contains("background"), "got: {error}");
    }

    #[test]
    fn shell_word_supports_embedded_command_substitution() {
        let expression = parse_block(r#"shell { printf "%s" "prefix-$(printf value)-suffix"; }"#);
        let ShellStep::Command(command) = &expression.steps[0].1 else {
            panic!("expected command")
        };
        assert!(matches!(
            command.args[1].parts.as_slice(),
            [
                ShellWordPart::Literal(prefix),
                ShellWordPart::CommandSubstitution(_),
                ShellWordPart::Literal(suffix)
            ] if prefix == "prefix-" && suffix == "-suffix"
        ));
    }

    #[test]
    fn shell_lang_parses_explicit_mixed_structured_pipeline_boundaries() {
        let expression = parse_block(
            r#"shell { printf '%s\n' '{"name":"Obi"}' | from jsonl |> take(1) |> to jsonl | cat; }"#,
        );
        assert_eq!(expression.steps.len(), 1);
        let ShellStep::MixedPipeline(pipeline) = &expression.steps[0].1 else {
            panic!("expected mixed structured pipeline")
        };
        assert_eq!(pipeline.input.len(), 1);
        assert_eq!(pipeline.input[0].program.text, "printf");
        assert_eq!(pipeline.decoder.decoder.name, "jsonl");
        assert_eq!(pipeline.stages.len(), 1);
        assert_eq!(pipeline.encoder.as_ref().unwrap().format, "jsonl");
        assert_eq!(pipeline.output.len(), 1);
        assert_eq!(pipeline.output[0].program.text, "cat");
    }

    #[test]
    fn shell_lang_parses_scoc_decoder_namespace_and_named_args() {
        let expression =
            parse_block(r#"shell { printf x | from scoc::ping(raw: true, streaming: false); }"#);
        let ShellStep::MixedPipeline(pipeline) = &expression.steps[0].1 else {
            panic!("expected mixed structured pipeline")
        };
        assert_eq!(
            pipeline.decoder.decoder.namespace,
            Some(DecoderNamespace::Scoc)
        );
        assert_eq!(pipeline.decoder.decoder.name, "ping");
        assert_eq!(pipeline.decoder.args.len(), 2);
        assert_eq!(pipeline.decoder.args[0].name, "raw");
        assert_eq!(pipeline.decoder.args[1].name, "streaming");
    }

    #[test]
    fn shell_lang_keeps_hyphenated_decoder_names_literal() {
        let expression = parse_block(r#"shell { printf x | from scoc::ping-s; }"#);
        let ShellStep::MixedPipeline(pipeline) = &expression.steps[0].1 else {
            panic!("expected mixed structured pipeline")
        };
        assert_eq!(pipeline.decoder.decoder.name, "ping-s");
    }

    #[test]
    fn shell_lang_rejects_duplicate_decoder_args() {
        let tokens = Lexer::new(r#"shell { printf x | from df(raw: true, raw: false); }"#)
            .tokenize()
            .expect("lex failed");
        let error = parse_shell_block(&tokens).expect_err("duplicate decoder args must fail");
        assert!(
            error.to_string().contains("duplicate decoder option `raw`"),
            "got: {error}"
        );
    }

    #[test]
    fn shell_lang_requires_from_before_structured_pipe() {
        let tokens = Lexer::new("shell { printf x |> take(1) |> to lines; }")
            .tokenize()
            .expect("lex failed");
        let error = parse_shell_block(&tokens).expect_err("bytes must cross through from first");
        assert!(
            error.to_string().contains("add `| from FORMAT` first"),
            "{error}"
        );
    }

    #[test]
    fn shell_lang_requires_to_before_returning_to_unix_pipe() {
        let tokens = Lexer::new("shell { printf x | from lines | cat; }")
            .tokenize()
            .expect("lex failed");
        let error = parse_shell_block(&tokens).expect_err("values must cross through to first");
        assert!(
            error.to_string().contains("add `|> to FORMAT` first"),
            "{error}"
        );
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
