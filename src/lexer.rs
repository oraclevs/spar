use crate::error::{Span, SparError};
use crate::token::{keyword_or_ident, SpannedToken, Token};

#[derive(Debug, Clone)]
pub struct CommentTrivia {
    pub text: String,
    pub line: u32,
    pub is_trailing: bool,
}

pub struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
    last_token_line: u32,
    comments: Vec<CommentTrivia>,
    shebang: Option<String>,
}

/// Turns command statements in a native shell block into the existing
/// `command ...;` expression form while leaving ordinary Spar statements
/// untouched.  A shell command is terminated by its top-level `;`, not by a
/// physical newline, so both of these are one command:
///
/// ```text
/// printf "%s\\n"
///     one
///     two;
///
/// printf "%s\\n" \\
///     one \\
///     two;
/// ```
///
/// The previous implementation classified every physical line independently.
/// That made continuation lines of perfectly ordinary Spar constructs (list
/// literals, named calls, multi-line conditions) turn into shell words.  This
/// normalizer tracks an in-progress Spar statement or command until its real
/// syntactic terminator is reached.
fn normalize_shell_body(body: &str) -> String {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum PendingKind {
        SparStatement,
        SparControlHeader,
        NativeCommand,
    }

    let mut output = String::with_capacity(body.len() + 32);
    let mut pending = String::new();
    let mut pending_kind: Option<PendingKind> = None;
    let mut pending_indent = String::new();

    for raw_line in body.split_inclusive('\n') {
        let (line, had_newline) = raw_line
            .strip_suffix('\n')
            .map_or((raw_line, false), |line| (line, true));
        let trimmed = line.trim_start();

        match pending_kind {
            Some(PendingKind::SparStatement) => {
                pending.push_str(line);
                if had_newline {
                    pending.push('\n');
                }
                if spar_statement_complete(&pending) {
                    output.push_str(&pending);
                    pending.clear();
                    pending_kind = None;
                }
                continue;
            }
            Some(PendingKind::SparControlHeader) => {
                pending.push_str(line);
                if had_newline {
                    pending.push('\n');
                }
                if spar_control_header_complete(&pending) {
                    output.push_str(&pending);
                    pending.clear();
                    pending_kind = None;
                }
                continue;
            }
            Some(PendingKind::NativeCommand) => {
                append_native_command_line(&mut pending, line);
                if native_command_complete(&pending) {
                    output.push_str(&pending_indent);
                    output.push_str(&prefix_native_command_segments(&pending));
                    if had_newline {
                        output.push('\n');
                    }
                    pending.clear();
                    pending_indent.clear();
                    pending_kind = None;
                }
                continue;
            }
            None => {}
        }

        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with('#')
            || trimmed.starts_with('}')
        {
            output.push_str(line);
            if had_newline {
                output.push('\n');
            }
            continue;
        }

        if is_spar_control_header(trimmed) {
            pending.push_str(line);
            if had_newline {
                pending.push('\n');
            }
            if spar_control_header_complete(&pending) {
                output.push_str(&pending);
                pending.clear();
            } else {
                pending_kind = Some(PendingKind::SparControlHeader);
            }
            continue;
        }

        if is_spar_shell_statement_start(trimmed) {
            pending.push_str(line);
            if had_newline {
                pending.push('\n');
            }
            if spar_statement_complete(&pending) {
                output.push_str(&pending);
                pending.clear();
            } else {
                pending_kind = Some(PendingKind::SparStatement);
            }
            continue;
        }

        let indent_len = line.len() - trimmed.len();
        pending_indent.push_str(&line[..indent_len]);
        append_native_command_line(&mut pending, trimmed);
        if native_command_complete(&pending) {
            output.push_str(&pending_indent);
            output.push_str(&prefix_native_command_segments(&pending));
            if had_newline {
                output.push('\n');
            }
            pending.clear();
            pending_indent.clear();
        } else {
            pending_kind = Some(PendingKind::NativeCommand);
        }
    }

    // Leave malformed/incomplete source for the ordinary lexer/parser to
    // diagnose rather than silently dropping it from the normalized block.
    if !pending.is_empty() {
        match pending_kind {
            Some(PendingKind::NativeCommand) => {
                output.push_str(&pending_indent);
                output.push_str("command ");
                output.push_str(pending.trim());
            }
            _ => output.push_str(&pending),
        }
    }

    output
}

/// Maps byte offsets in a normalized shell body back to the original body.
///
/// Normalization only ever inserts text (`command ` prefixes, `;`/`&`
/// terminators), re-flows whitespace, and drops explicit `\` line
/// continuations; every other non-whitespace character survives in order.
/// That lets the two texts be aligned on their non-whitespace characters.
struct NormalizedOffsets {
    /// For each byte of the normalized text, the original offset of that
    /// character, or `None` for inserted text and whitespace.
    mapped: Vec<Option<usize>>,
}

impl NormalizedOffsets {
    fn new(original: &str, normalized: &str, inserted: &[(usize, usize)]) -> Self {
        let mut mapped = vec![None; normalized.len() + 1];
        let mut source = original
            .char_indices()
            .filter(|(_, ch)| !ch.is_whitespace())
            .peekable();
        for (index, ch) in normalized.char_indices() {
            if ch.is_whitespace()
                || inserted
                    .iter()
                    .any(|(start, end)| index >= *start && index < *end)
            {
                continue;
            }
            while let Some(&(source_index, source_ch)) = source.peek() {
                if source_ch == ch {
                    mapped[index] = Some(source_index);
                    source.next();
                    break;
                }
                if ch == ';' {
                    break; // an inserted terminator
                }
                source.next(); // a dropped character, e.g. a `\` continuation
            }
        }
        Self { mapped }
    }

    /// Original `[start, end)` for a token spanning `[start, end)` of the
    /// normalized text.
    fn original_range(&self, original: &str, start: usize, end: usize) -> (usize, usize) {
        let first = (start..end.min(self.mapped.len()))
            .find_map(|index| self.mapped[index])
            .unwrap_or(original.len());
        let last = (start..end.min(self.mapped.len()))
            .rev()
            .find_map(|index| self.mapped[index])
            .map(|index| {
                index
                    + original[index..]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8)
            })
            .unwrap_or(first);
        (first, last.max(first))
    }

    fn line_col(&self, original: &str, offset: usize, body_line: u32, body_col: u32) -> (u32, u32) {
        let before = &original[..offset.min(original.len())];
        let newlines = before.matches('\n').count() as u32;
        let line = body_line + newlines;
        let col = match before.rfind('\n') {
            Some(at) => before[at + 1..].chars().count() as u32 + 1,
            None => body_col + before.chars().count() as u32,
        };
        (line, col)
    }
}

/// Appends one physical line to an in-progress native command.  Native Spar
/// commands are semicolon terminated, so an ordinary newline is whitespace.
/// A Bash-style `\\` immediately before the newline is accepted as explicit
/// continuation sugar and is removed instead of becoming a literal argv word.
fn append_native_command_line(command: &mut String, line: &str) {
    let mut text = line.trim();
    if let Some(without_slash) = trailing_unquoted_backslash(text) {
        text = without_slash.trim_end();
    }
    if !command.is_empty() && !command.ends_with(char::is_whitespace) {
        command.push(' ');
    }
    command.push_str(text);
    command.push(' ');
}

fn trailing_unquoted_backslash(text: &str) -> Option<&str> {
    let trimmed = text.trim_end();
    if !trimmed.ends_with('\\') {
        return None;
    }
    let mut quote = None;
    let mut escaped = false;
    let mut last_backslash = None;
    for (index, ch) in trimmed.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            if quote == Some('\'') {
                continue;
            }
            escaped = true;
            last_backslash = Some(index);
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
        }
    }
    (quote.is_none() && last_backslash == Some(trimmed.len() - 1))
        .then(|| &trimmed[..trimmed.len() - 1])
}

fn prefix_native_command_segments(line: &str) -> String {
    let mut output = String::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut paren_depth = 0_u32;
    let bytes = line.as_bytes();
    for (index, ch) in line.char_indices() {
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
        if quote.is_none() {
            if ch == '(' && index > 0 && bytes.get(index.wrapping_sub(1)) == Some(&b'$') {
                paren_depth += 1;
                continue;
            }
            if ch == ')' && paren_depth > 0 {
                paren_depth -= 1;
                continue;
            }
        }
        if quote.is_some() || paren_depth != 0 {
            continue;
        }

        if ch == ';' {
            let segment = line[start..index].trim();
            if !segment.is_empty() {
                output.push_str("command ");
                output.push_str(segment);
                output.push(';');
            }
            start = index + 1;
            continue;
        }

        // A single top-level '&' is a list terminator, just like in an
        // ordinary Unix shell.  Do not split logical '&&' or '&>'/'&>>'.
        if ch == '&'
            && bytes.get(index.wrapping_sub(1)) != Some(&b'&')
            && bytes.get(index.wrapping_sub(1)) != Some(&b'>')
            && bytes.get(index + 1) != Some(&b'&')
            && bytes.get(index + 1) != Some(&b'>')
        {
            let segment = line[start..index].trim();
            if !segment.is_empty() {
                output.push_str("command ");
                output.push_str(segment);
                output.push_str(" &;");
            }
            start = index + 1;
        }
    }
    let tail = line[start..].trim();
    if !tail.is_empty() {
        output.push_str("command ");
        output.push_str(tail);
        if !tail.ends_with(';') {
            output.push(';');
        }
    }
    output
}

fn native_command_complete(source: &str) -> bool {
    if last_top_level_semicolon(source)
        .is_some_and(|index| source[index + 1..].trim().is_empty())
    {
        return true;
    }
    top_level_background_terminator(source)
        .is_some_and(|index| source[index + 1..].trim().is_empty())
}

fn top_level_background_terminator(source: &str) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut quote = None;
    let mut escaped = false;
    let mut paren = 0_u32;
    let mut bracket = 0_u32;
    let mut brace = 0_u32;
    let mut last = None;
    for (index, ch) in source.char_indices() {
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
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            '{' => brace += 1,
            '}' => brace = brace.saturating_sub(1),
            '&' if paren == 0 && bracket == 0 && brace == 0
                && bytes.get(index.wrapping_sub(1)) != Some(&b'&')
                && bytes.get(index.wrapping_sub(1)) != Some(&b'>')
                && bytes.get(index + 1) != Some(&b'&')
                && bytes.get(index + 1) != Some(&b'>') =>
            {
                last = Some(index)
            }
            _ => {}
        }
    }
    last
}

fn spar_statement_complete(source: &str) -> bool {
    last_top_level_semicolon(source)
        .is_some_and(|index| source[index + 1..].trim().is_empty())
}

fn spar_control_header_complete(source: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    let mut paren = 0_u32;
    let mut bracket = 0_u32;
    for ch in source.chars() {
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
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            '{' if paren == 0 && bracket == 0 => return true,
            _ => {}
        }
    }
    false
}

fn last_top_level_semicolon(source: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    let mut paren = 0_u32;
    let mut bracket = 0_u32;
    let mut brace = 0_u32;
    let mut last = None;
    for (index, ch) in source.char_indices() {
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
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            '{' => brace += 1,
            '}' => brace = brace.saturating_sub(1),
            ';' if paren == 0 && bracket == 0 && brace == 0 => last = Some(index),
            _ => {}
        }
    }
    last
}

fn is_spar_control_header(line: &str) -> bool {
    ["if", "for", "try", "catch", "else"]
        .iter()
        .any(|keyword| starts_with_keyword(line, keyword))
}

fn is_spar_shell_statement_start(line: &str) -> bool {
    const SIMPLE_KEYWORDS: &[&str] = &[
        "var", "return", "break", "continue", "command", "shell", "exec",
    ];
    if SIMPLE_KEYWORDS
        .iter()
        .any(|keyword| starts_with_keyword(line, keyword))
    {
        return true;
    }

    looks_like_spar_call(line) || looks_like_spar_assignment(line)
}

fn starts_with_keyword(source: &str, keyword: &str) -> bool {
    source.starts_with(keyword)
        && source[keyword.len()..]
            .chars()
            .next()
            .map_or(true, |ch| !(ch.is_ascii_alphanumeric() || ch == '_'))
}

fn looks_like_spar_call(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut pos = 0usize;
    if !bytes
        .get(pos)
        .is_some_and(|byte| *byte == b'_' || byte.is_ascii_alphabetic())
    {
        return false;
    }
    loop {
        pos += 1;
        while bytes
            .get(pos)
            .is_some_and(|byte| *byte == b'_' || byte.is_ascii_alphanumeric())
        {
            pos += 1;
        }
        if bytes.get(pos..pos + 2) == Some(&b"::"[..]) {
            pos += 2;
            if !bytes
                .get(pos)
                .is_some_and(|byte| *byte == b'_' || byte.is_ascii_alphabetic())
            {
                return false;
            }
            continue;
        }
        break;
    }
    while bytes.get(pos).is_some_and(|byte| byte.is_ascii_whitespace()) {
        pos += 1;
    }
    bytes.get(pos) == Some(&b'(')
}

fn looks_like_spar_assignment(line: &str) -> bool {
    let Some(name_end) = line
        .char_indices()
        .take_while(|(_, ch)| *ch == '_' || ch.is_ascii_alphanumeric())
        .map(|(index, ch)| index + ch.len_utf8())
        .last()
    else {
        return false;
    };
    let rest = &line[name_end..];
    rest.starts_with(" = ") || rest.starts_with(" += ")
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str) -> Self {
        let shebang = if source.starts_with("#!") {
            let end = source.find('\n').unwrap_or(source.len());
            Some(source[..end].trim_end_matches('\r').to_string())
        } else {
            None
        };
        Self {
            source,
            bytes: source.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            last_token_line: 0,
            comments: Vec::new(),
            shebang,
        }
    }

    /// A leading `#!...` line, if `source` started with one. Only valid to
    /// read before/alongside `tokenize()` — `Lexer::new` computes it
    /// up front from raw source, independent of tokenizing.
    pub fn shebang(&self) -> Option<&str> {
        self.shebang.as_deref()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let b = self.bytes.get(self.pos).copied()?;
        self.pos += 1;
        if b == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(b)
    }

    /// Consumes and returns the full UTF-8 character starting at the
    /// current position (which is always on a char boundary — every path
    /// through the lexer consumes either a full multi-byte character via
    /// this method or a single-byte ASCII character, never a partial
    /// sequence). Advancing byte-by-byte keeps `pos`/`line`/`col`
    /// bookkeeping identical to the single-byte-at-a-time path; only the
    /// decoded value differs from a raw `byte as char` cast, which is
    /// wrong for any byte >= 0x80.
    fn advance_char(&mut self) -> Option<char> {
        let ch = self.source[self.pos..].chars().next()?;
        for _ in 0..ch.len_utf8() {
            self.advance();
        }
        Some(ch)
    }

    fn span_at(&self, start: usize, start_line: u32, start_col: u32) -> Span {
        Span::new(start, self.pos, start_line, start_col)
    }

    fn skip_line_comment(&mut self) {
        while let Some(b) = self.peek() {
            if b == b'\n' {
                break;
            }
            self.advance();
        }
    }

    fn collect_line_comment(&mut self, text_start: usize, comment_line: u32) {
        self.skip_line_comment();
        let text = self.source[text_start..self.pos].trim_end().to_string();
        self.comments.push(CommentTrivia {
            text,
            line: comment_line,
            is_trailing: self.last_token_line == comment_line,
        });
    }

    fn skip_block_comment(&mut self, start: usize, line: u32, col: u32) -> Result<(), SparError> {
        loop {
            match self.peek() {
                None => {
                    return Err(SparError::LexError {
                        message: "unterminated block comment".to_string(),
                        span: Span::new(start, self.pos, line, col),
                    });
                }
                Some(b'*') if self.peek_at(1) == Some(b'/') => {
                    self.advance();
                    self.advance();
                    return Ok(());
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn collect_block_comment(
        &mut self,
        text_start: usize,
        comment_line: u32,
        err_span_start: usize,
        err_line: u32,
        err_col: u32,
    ) -> Result<(), SparError> {
        let is_trailing = self.last_token_line == comment_line;
        self.skip_block_comment(err_span_start, err_line, err_col)?;
        let text = self.source[text_start..self.pos].to_string();
        self.comments.push(CommentTrivia {
            text,
            line: comment_line,
            is_trailing,
        });
        Ok(())
    }

    fn lex_string(
        &mut self,
        tokens: &mut Vec<SpannedToken>,
        start: usize,
        start_line: u32,
        start_col: u32,
    ) -> Result<(), SparError> {
        tokens.push(SpannedToken::new(
            Token::StringStart,
            Span::new(start, start, start_line, start_col),
        ));

        let mut fragment = String::new();

        loop {
            let frag_start = self.pos;
            let frag_line = self.line;
            let frag_col = self.col;

            match self.peek() {
                None => {
                    return Err(SparError::LexError {
                        message: "unterminated string".to_string(),
                        span: Span::new(frag_start, self.pos, frag_line, frag_col),
                    });
                }
                Some(b'\n') => {
                    return Err(SparError::LexError {
                        message: "unterminated string".to_string(),
                        span: Span::new(frag_start, self.pos, frag_line, frag_col),
                    });
                }
                Some(b'"') => {
                    self.advance();
                    tokens.push(SpannedToken::new(
                        Token::StringFragment(fragment),
                        Span::new(frag_start, self.pos, frag_line, frag_col),
                    ));
                    tokens.push(SpannedToken::new(
                        Token::StringEnd,
                        Span::new(self.pos, self.pos, self.line, self.col),
                    ));
                    return Ok(());
                }
                Some(b'$') if self.peek_at(1) == Some(b'{') => {
                    // emit current fragment (even if empty)
                    tokens.push(SpannedToken::new(
                        Token::StringFragment(fragment.clone()),
                        Span::new(frag_start, self.pos, frag_line, frag_col),
                    ));
                    fragment.clear();

                    self.advance(); // $
                    self.advance(); // {

                    let interp_span = Span::new(self.pos - 2, self.pos, self.line, self.col);
                    tokens.push(SpannedToken::new(Token::InterpolStart, interp_span));

                    let mut brace_depth: u32 = 1;
                    self.tokenize_interp(tokens, &mut brace_depth)?;

                    tokens.push(SpannedToken::new(
                        Token::InterpolEnd,
                        Span::new(self.pos, self.pos, self.line, self.col),
                    ));
                }
                Some(b'\\') => {
                    self.advance(); // backslash
                    match self.peek() {
                        Some(b'\\') => {
                            self.advance();
                            fragment.push('\\');
                        }
                        Some(b'"') => {
                            self.advance();
                            fragment.push('"');
                        }
                        Some(b'n') => {
                            self.advance();
                            fragment.push('\n');
                        }
                        Some(b't') => {
                            self.advance();
                            fragment.push('\t');
                        }
                        Some(_) => {
                            fragment.push('\\');
                            if let Some(ch) = self.advance_char() {
                                fragment.push(ch);
                            }
                        }
                        None => {
                            return Err(SparError::LexError {
                                message: "unterminated string".to_string(),
                                span: Span::new(frag_start, self.pos, frag_line, frag_col),
                            });
                        }
                    }
                }
                Some(_) => {
                    if let Some(ch) = self.advance_char() {
                        fragment.push(ch);
                    }
                }
            }
        }
    }

    fn tokenize_interp(
        &mut self,
        tokens: &mut Vec<SpannedToken>,
        brace_depth: &mut u32,
    ) -> Result<(), SparError> {
        loop {
            let start = self.pos;
            let line = self.line;
            let col = self.col;

            match self.peek() {
                None => {
                    return Err(SparError::LexError {
                        message: "unterminated string".to_string(),
                        span: Span::new(start, self.pos, line, col),
                    });
                }
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') => {
                    self.advance();
                }
                Some(b'"') => {
                    return Err(SparError::LexError {
                        message: "nested strings inside interpolation are not supported"
                            .to_string(),
                        span: Span::new(start, self.pos, line, col),
                    });
                }
                Some(b'{') => {
                    self.advance();
                    *brace_depth += 1;
                    tokens.push(SpannedToken::new(
                        Token::LBrace,
                        self.span_at(start, line, col),
                    ));
                }
                Some(b'}') => {
                    self.advance();
                    *brace_depth -= 1;
                    if *brace_depth == 0 {
                        return Ok(());
                    }
                    tokens.push(SpannedToken::new(
                        Token::RBrace,
                        self.span_at(start, line, col),
                    ));
                }
                Some(b'/') if self.peek_at(1) == Some(b'/') => {
                    self.advance();
                    self.advance();
                    self.collect_line_comment(start, line);
                }
                Some(b'/') if self.peek_at(1) == Some(b'*') => {
                    self.advance();
                    self.advance();
                    self.collect_block_comment(start, line, start, line, col)?;
                }
                Some(c) => {
                    let tok = self.lex_single_token(c, start, line, col)?;
                    if let Some(t) = tok {
                        tokens.push(t);
                    }
                }
            }
        }
    }

    fn lex_single_token(
        &mut self,
        c: u8,
        start: usize,
        line: u32,
        col: u32,
    ) -> Result<Option<SpannedToken>, SparError> {
        let tok = match c {
            b'+' => {
                self.advance();
                if self.peek() == Some(b'=') {
                    self.advance();
                    Token::PlusEq
                } else {
                    Token::Plus
                }
            }
            b'-' => {
                if self.peek_at(1) == Some(b'>') {
                    self.advance();
                    self.advance();
                    Token::Arrow
                } else {
                    self.advance();
                    Token::Minus
                }
            }
            b'*' => {
                self.advance();
                Token::Star
            }
            b'=' => {
                if self.peek_at(1) == Some(b'=') {
                    self.advance();
                    self.advance();
                    Token::EqEq
                } else {
                    self.advance();
                    Token::Eq
                }
            }
            b';' => {
                self.advance();
                Token::Semicolon
            }
            b',' => {
                self.advance();
                Token::Comma
            }
            b'(' => {
                self.advance();
                Token::LParen
            }
            b')' => {
                self.advance();
                Token::RParen
            }
            b'[' => {
                self.advance();
                Token::LBracket
            }
            b']' => {
                self.advance();
                Token::RBracket
            }

            b'?' => {
                self.advance();
                if self.peek() == Some(b'?') {
                    self.advance();
                    Token::QuestionQuestion
                } else {
                    Token::Question
                }
            }
            b':' => {
                self.advance();
                if self.peek() == Some(b':') {
                    self.advance();
                    Token::ColonColon
                } else {
                    Token::Colon
                }
            }
            b'.' => {
                self.advance();
                if self.peek() == Some(b'.') && self.peek_at(1) == Some(b'.') {
                    self.advance();
                    self.advance();
                    Token::DotDotDot
                } else {
                    Token::Dot
                }
            }
            b'/' => {
                self.advance();
                if self.peek() == Some(b'/') {
                    self.advance();
                    self.collect_line_comment(start, line);
                    return Ok(None);
                } else if self.peek() == Some(b'*') {
                    self.advance();
                    self.collect_block_comment(start, line, start, line, col)?;
                    return Ok(None);
                } else {
                    Token::Slash
                }
            }

            b'!' => {
                if self.peek_at(1) == Some(b'=') {
                    self.advance();
                    self.advance();
                    Token::NotEq
                } else {
                    self.advance();
                    Token::Bang
                }
            }
            b'<' => {
                if self.peek_at(1) == Some(b'=') {
                    self.advance();
                    self.advance();
                    Token::LtEq
                } else {
                    self.advance();
                    Token::Lt
                }
            }
            b'>' => {
                if self.peek_at(1) == Some(b'=') {
                    self.advance();
                    self.advance();
                    Token::GtEq
                } else {
                    self.advance();
                    Token::Gt
                }
            }
            b'&' => {
                if self.peek_at(1) == Some(b'&') {
                    self.advance();
                    self.advance();
                    Token::AndAnd
                } else {
                    return Err(SparError::LexError {
                        message: "unexpected '&' — did you mean '&&'?".into(),
                        span: self.span_at(start, line, col),
                    });
                }
            }
            b'|' => {
                if self.peek_at(1) == Some(b'|') {
                    self.advance();
                    self.advance();
                    Token::OrOr
                } else {
                    return Err(SparError::LexError {
                        message: "unexpected '|' — did you mean '||'?".into(),
                        span: self.span_at(start, line, col),
                    });
                }
            }

            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let s = self.read_ident();
                keyword_or_ident(s)
            }

            b'0'..=b'9' => self.read_number(start, line, col)?,

            b'@' => {
                self.advance();
                Token::At
            }

            _ => {
                let ch = self
                    .advance_char()
                    .expect("byte was peeked, char must decode");
                return Err(SparError::LexError {
                    message: format!("unexpected character '{ch}'"),
                    span: Span::new(start, self.pos, line, col),
                });
            }
        };

        Ok(Some(SpannedToken::new(tok, self.span_at(start, line, col))))
    }

    fn read_ident(&mut self) -> String {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.advance();
            } else {
                break;
            }
        }
        self.source[start..self.pos].to_string()
    }

    fn read_number(&mut self, start: usize, line: u32, col: u32) -> Result<Token, SparError> {
        let num_start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                self.advance();
            } else {
                break;
            }
        }

        // check for float: digit '.' digit
        if self.peek() == Some(b'.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            self.advance(); // .
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    self.advance();
                } else {
                    break;
                }
            }
            let s = &self.source[num_start..self.pos];
            s.parse::<f64>()
                .map(Token::FloatLit)
                .map_err(|_| SparError::LexError {
                    message: "invalid number literal".to_string(),
                    span: Span::new(start, self.pos, line, col),
                })
        } else {
            let s = &self.source[num_start..self.pos];
            s.parse::<i64>()
                .map(Token::IntLit)
                .map_err(|_| SparError::LexError {
                    message: "invalid number literal".to_string(),
                    span: Span::new(start, self.pos, line, col),
                })
        }
    }

    /// Called right after an `Ident("run")` token has been pushed. Scans
    /// ahead (without committing) for an optional bare-identifier OS label
    /// followed by `{` — `run { ... }` (bare/default) or
    /// `run windows { ... }` (labeled). If neither shape is found at this
    /// position, the position is left untouched and `run`/the tentative
    /// label lex as ordinary tokens on the next loop iterations — this
    /// keeps the check honest rather than assuming `run` always opens a
    /// block.
    fn maybe_enter_run_body(&mut self, tokens: &mut Vec<SpannedToken>) -> Result<(), SparError> {
        let mut offset = 0usize;
        while matches!(
            self.peek_at(offset),
            Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            offset += 1;
        }
        let label_start = offset;
        while matches!(self.peek_at(offset), Some(b) if b.is_ascii_alphanumeric() || b == b'_') {
            offset += 1;
        }
        let label_end = offset;
        while matches!(
            self.peek_at(offset),
            Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            offset += 1;
        }
        if self.peek_at(offset) != Some(b'{') {
            return Ok(());
        }

        for _ in 0..label_start {
            self.advance();
        }
        if label_end > label_start {
            let label_pos = self.pos;
            let (label_line, label_col) = (self.line, self.col);
            let label_text =
                self.source[self.pos..self.pos + (label_end - label_start)].to_string();
            for _ in label_start..label_end {
                self.advance();
            }
            tokens.push(SpannedToken::new(
                Token::Ident(label_text),
                self.span_at(label_pos, label_line, label_col),
            ));
        }
        for _ in label_end..offset {
            self.advance();
        }

        let brace_pos = self.pos;
        let (brace_line, brace_col) = (self.line, self.col);
        self.advance(); // consume '{'
        tokens.push(SpannedToken::new(
            Token::RunStart,
            self.span_at(brace_pos, brace_line, brace_col),
        ));
        self.lex_run_body(tokens)
    }

    /// Lexes the raw shell body of a `run { ... }` block: everything up to
    /// the matching `}` is copied verbatim as `ShellFragment` text, except
    /// `${expr}` islands (tokenized exactly like string interpolation via
    /// `tokenize_interp`). A `#{` escape is kept verbatim in the shell
    /// fragment while preventing its brace group from becoming a Spar
    /// interpolation; `task_lowering` later turns it into literal `${` in
    /// executable shell text. Keeping the source spelling here lets the
    /// formatter round-trip it. Brace depth is tracked over every literal
    /// `{`/`}` byte (including escaped ones) so shell brace groups don't
    /// prematurely close the block.
    fn lex_run_body(&mut self, tokens: &mut Vec<SpannedToken>) -> Result<(), SparError> {
        let mut fragment = String::new();
        let mut frag_start = self.pos;
        let mut frag_line = self.line;
        let mut frag_col = self.col;
        let mut depth: u32 = 1;

        loop {
            match self.peek() {
                None => {
                    return Err(SparError::LexError {
                        message: "unterminated run block — expected '}'".to_string(),
                        span: Span::new(frag_start, self.pos, frag_line, frag_col),
                    });
                }
                Some(b'#') if self.peek_at(1) == Some(b'{') => {
                    self.advance(); // #
                    self.advance(); // {
                    fragment.push_str("#{");
                    depth += 1;
                }
                Some(b'$') if self.peek_at(1) == Some(b'{') => {
                    tokens.push(SpannedToken::new(
                        Token::ShellFragment(std::mem::take(&mut fragment)),
                        Span::new(frag_start, self.pos, frag_line, frag_col),
                    ));
                    self.advance(); // $
                    self.advance(); // {
                    let interp_span = Span::new(self.pos - 2, self.pos, self.line, self.col);
                    tokens.push(SpannedToken::new(Token::InterpolStart, interp_span));
                    let mut brace_depth: u32 = 1;
                    self.tokenize_interp(tokens, &mut brace_depth)?;
                    tokens.push(SpannedToken::new(
                        Token::InterpolEnd,
                        Span::new(self.pos, self.pos, self.line, self.col),
                    ));
                    frag_start = self.pos;
                    frag_line = self.line;
                    frag_col = self.col;
                }
                Some(b'{') => {
                    self.advance();
                    fragment.push('{');
                    depth += 1;
                }
                Some(b'}') => {
                    self.advance();
                    depth -= 1;
                    if depth == 0 {
                        tokens.push(SpannedToken::new(
                            Token::ShellFragment(std::mem::take(&mut fragment)),
                            Span::new(frag_start, self.pos - 1, frag_line, frag_col),
                        ));
                        tokens.push(SpannedToken::new(
                            Token::RunEnd,
                            self.span_at(self.pos - 1, self.line, self.col),
                        ));
                        return Ok(());
                    }
                    fragment.push('}');
                }
                Some(_) => {
                    if let Some(ch) = self.advance_char() {
                        fragment.push(ch);
                    }
                }
            }
        }
    }

    /// Replaces a just-emitted `TypeShell` with `ShellBlockStart` when the
    /// keyword is followed by `{`, then lexes the body with the native command
    /// tokenizer. A plain `shell` type annotation is left untouched.
    fn maybe_enter_shell_body(&mut self, tokens: &mut Vec<SpannedToken>) -> Result<(), SparError> {
        let mut offset = 0usize;
        while matches!(
            self.peek_at(offset),
            Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            offset += 1;
        }
        let mut brace_offset = offset;
        let foreign_bash = self.source[self.pos + offset..].starts_with("bash")
            && !self
                .peek_at(offset + 4)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if foreign_bash {
            brace_offset += 4;
            while matches!(
                self.peek_at(brace_offset),
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
            ) {
                brace_offset += 1;
            }
        }
        if self.peek_at(brace_offset) != Some(b'{') {
            return Ok(());
        }

        for _ in 0..=brace_offset {
            self.advance();
        }
        let shell = tokens
            .pop()
            .expect("shell mode is entered immediately after emitting TypeShell");
        tokens.push(SpannedToken::new(
            if foreign_bash {
                Token::ShellForeignBlockStart("bash".into())
            } else {
                Token::ShellBlockStart
            },
            Span::new(shell.span.start, self.pos, shell.span.line, shell.span.col),
        ));
        if foreign_bash {
            self.lex_foreign_bash_block(tokens)
        } else {
            self.lex_shell_block(tokens)
        }
    }

    fn maybe_enter_exec_body(&mut self, tokens: &mut Vec<SpannedToken>) -> Result<(), SparError> {
        let mut offset = 0usize;
        while matches!(
            self.peek_at(offset),
            Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            offset += 1;
        }
        if self.peek_at(offset) != Some(b'{') {
            return Ok(());
        }
        for _ in 0..=offset {
            self.advance();
        }
        let exec = tokens
            .last()
            .expect("exec token was emitted before block lookahead");
        tokens.push(SpannedToken::new(
            Token::ShellBlockStart,
            Span::new(exec.span.start, self.pos, exec.span.line, exec.span.col),
        ));
        self.lex_shell_block(tokens)
    }

    fn lex_foreign_bash_block(&mut self, tokens: &mut Vec<SpannedToken>) -> Result<(), SparError> {
        let body_start = self.pos;
        let body_line = self.line;
        let body_col = self.col;
        let mut depth = 1_u32;
        let mut quote = None;
        let mut escaped = false;
        while let Some(byte) = self.peek() {
            if escaped {
                escaped = false;
                self.advance_char();
                continue;
            }
            if byte == b'\\' {
                escaped = true;
                self.advance();
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
                self.advance();
                continue;
            }
            if quote.is_none() {
                if byte == b'{' {
                    depth += 1;
                } else if byte == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        let body_end = self.pos;
                        let close_line = self.line;
                        let close_col = self.col;
                        self.advance();
                        let span = Span::new(body_start, body_end, body_line, body_col);
                        tokens.push(SpannedToken::new(
                            Token::ShellWord("bash".into()),
                            span.clone(),
                        ));
                        tokens.push(SpannedToken::new(
                            Token::ShellWord("-c".into()),
                            span.clone(),
                        ));
                        tokens.push(SpannedToken::new(
                            Token::ShellLiteralWord(self.source[body_start..body_end].to_string()),
                            span,
                        ));
                        tokens.push(SpannedToken::new(
                            Token::Semicolon,
                            Span::new(body_end, body_end, close_line, close_col),
                        ));
                        tokens.push(SpannedToken::new(
                            Token::ShellBlockEnd,
                            Span::new(body_end, self.pos, close_line, close_col),
                        ));
                        return Ok(());
                    }
                }
            }
            self.advance_char();
        }
        Err(SparError::LexError {
            message: "unterminated foreign Bash block — expected '}'".into(),
            span: Span::new(body_start, self.pos, body_line, body_col),
        })
    }

    fn lex_shell_block(&mut self, tokens: &mut Vec<SpannedToken>) -> Result<(), SparError> {
        let body_start = self.pos;
        let body_line = self.line;
        let body_col = self.col;
        let mut depth = 1_u32;
        let mut quote = None;
        let mut escaped = false;

        while let Some(byte) = self.peek() {
            if escaped {
                escaped = false;
                self.advance_char();
                continue;
            }
            if byte == b'\\' && quote.is_some() {
                escaped = true;
                self.advance();
                continue;
            }
            if matches!(byte, b'\'' | b'"') {
                if quote == Some(byte) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(byte);
                }
                self.advance();
                continue;
            }
            if quote.is_none() {
                if byte == b'{' {
                    depth += 1;
                } else if byte == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        let body_end = self.pos;
                        let close_line = self.line;
                        let close_col = self.col;
                        self.advance();
                        let original = &self.source[body_start..body_end];
                        let normalized = normalize_shell_body(original);
                        let nested = Lexer::new(&normalized).tokenize()?;
                        // Normalization inserts `command ` prefixes and
                        // terminators and re-flows whitespace, so nested
                        // spans are relative to text that does not exist in
                        // the file. Map them back onto the original body.
                        let inserted: Vec<(usize, usize)> = nested
                            .iter()
                            .filter(|token| token.token == Token::KwCommand)
                            .map(|token| (token.span.start, token.span.end))
                            .collect();
                        let offsets = NormalizedOffsets::new(original, &normalized, &inserted);
                        for mut token in nested.into_iter().filter(|token| {
                            token.token != Token::Eof && token.token != Token::KwCommand
                        }) {
                            let (start, end) = offsets.original_range(
                                original,
                                token.span.start,
                                token.span.end,
                            );
                            let (line, col) = offsets.line_col(original, start, body_line, body_col);
                            token.span = Span::new(body_start + start, body_start + end, line, col);
                            tokens.push(token);
                        }
                        tokens.push(SpannedToken::new(
                            Token::ShellBlockEnd,
                            Span::new(body_end, self.pos, close_line, close_col),
                        ));
                        return Ok(());
                    }
                }
            }
            self.advance_char();
        }

        Err(SparError::LexError {
            message: "unterminated shell block — expected '}' (unbalanced brace)".to_string(),
            span: Span::new(body_start, self.pos, body_line, body_col),
        })
    }

    fn should_enter_command_body(&self) -> bool {
        let mut offset = 0usize;
        while matches!(
            self.peek_at(offset),
            Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            offset += 1;
        }

        // Native `command` sugar is whitespace-delimited (`command echo ...`).
        // With no separation, `command` is an ordinary Spar identifier used
        // in an expression such as `command/2` or `command.foo`.
        if offset == 0 {
            return false;
        }
        let Some(next) = self.peek_at(offset) else {
            return false;
        };
        // `command` is a soft keyword. These tokens unambiguously continue a
        // normal Spar identifier/reference/call rather than introducing the
        // native command expression sugar.
        if matches!(
            next,
            b';' | b':' | b'?' | b'=' | b',' | b')' | b']' | b'}' | b'('
                | b'+' | b'*' | b'<' | b'>' | b'!'
        ) {
            return false;
        }
        if next == b'.' {
            let rest = &self.source[self.pos + offset..];
            return rest.starts_with("./") || rest.starts_with("../");
        }
        if next == b'-' {
            // A bare `command - ...` is not a valid native program name and
            // is overwhelmingly a Spar operator use.
            return false;
        }
        if next == b'/' && self.peek_at(offset + 1).is_some_and(|byte| byte.is_ascii_whitespace()) {
            return false;
        }
        true
    }

    fn lex_command_body(&mut self, tokens: &mut Vec<SpannedToken>) -> Result<(), SparError> {
        loop {
            let start = self.pos;
            let line = self.line;
            let col = self.col;

            match self.peek() {
                None | Some(b'\n') => {
                    return Err(SparError::LexError {
                        message: "unterminated command expression — expected ';'".to_string(),
                        span: Span::new(start, self.pos, line, col),
                    });
                }
                Some(b' ') | Some(b'\t') | Some(b'\r') => {
                    self.advance();
                }
                Some(b';') => {
                    self.advance();
                    tokens.push(SpannedToken::new(
                        Token::Semicolon,
                        self.span_at(start, line, col),
                    ));
                    return Ok(());
                }
                Some(b'{') | Some(b'}') => {
                    self.advance();
                    return Err(SparError::LexError {
                        message: "bare brace is not supported inside a shell command".to_string(),
                        span: self.span_at(start, line, col),
                    });
                }
                Some(b'|') => {
                    self.advance();
                    let token = if self.peek() == Some(b'|') {
                        self.advance();
                        Token::OrOr
                    } else {
                        Token::ShellPipe
                    };
                    tokens.push(SpannedToken::new(token, self.span_at(start, line, col)));
                }
                Some(byte) if byte.is_ascii_digit() => {
                    let mut offset = 0usize;
                    while self.peek_at(offset).is_some_and(|b| b.is_ascii_digit()) {
                        offset += 1;
                    }
                    if self.peek_at(offset) != Some(b'>') {
                        self.lex_bare_shell_word(tokens, start, line, col);
                        continue;
                    }
                    let fd = self.source[start..start + offset]
                        .parse::<u32>()
                        .map_err(|_| SparError::LexError {
                            message: "file descriptor is too large".into(),
                            span: Span::new(start, start + offset, line, col),
                        })?;
                    for _ in 0..=offset {
                        self.advance();
                    }
                    let token = if self.peek() == Some(b'&') {
                        self.advance();
                        let target_start = self.pos;
                        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                            self.advance();
                        }
                        if target_start == self.pos {
                            return Err(SparError::LexError {
                                message: "expected target fd after '>&'".into(),
                                span: self.span_at(start, line, col),
                            });
                        }
                        let target =
                            self.source[target_start..self.pos]
                                .parse::<u32>()
                                .map_err(|_| SparError::LexError {
                                    message: "file descriptor is too large".into(),
                                    span: self.span_at(start, line, col),
                                })?;
                        Token::ShellFdDuplicate { fd, target }
                    } else {
                        let append = if self.peek() == Some(b'>') {
                            self.advance();
                            true
                        } else {
                            false
                        };
                        if fd == 2 && !append {
                            Token::ShellRedirectStderr
                        } else {
                            Token::ShellFdRedirect { fd, append }
                        }
                    };
                    tokens.push(SpannedToken::new(token, self.span_at(start, line, col)));
                }
                Some(b'&') => {
                    self.advance();
                    let token = if self.peek() == Some(b'&') {
                        self.advance();
                        Token::AndAnd
                    } else if self.peek() == Some(b'>') {
                        self.advance();
                        let append = if self.peek() == Some(b'>') {
                            self.advance();
                            true
                        } else {
                            false
                        };
                        Token::ShellRedirectBoth { append }
                    } else {
                        Token::ShellBackground
                    };
                    tokens.push(SpannedToken::new(token, self.span_at(start, line, col)));
                }
                Some(b'<') => {
                    self.advance();
                    tokens.push(SpannedToken::new(Token::Lt, self.span_at(start, line, col)));
                }
                Some(b'>') => {
                    self.advance();
                    let token = if self.peek() == Some(b'>') {
                        self.advance();
                        Token::ShellRedirectAppend
                    } else {
                        Token::Gt
                    };
                    tokens.push(SpannedToken::new(token, self.span_at(start, line, col)));
                }
                Some(b'"') => self.lex_quoted_shell_word(tokens, start, line, col)?,
                Some(b'\'') => self.lex_literal_shell_word(tokens, start, line, col)?,
                Some(_) => self.lex_bare_shell_word(tokens, start, line, col),
            }
        }
    }

    fn lex_quoted_shell_word(
        &mut self,
        tokens: &mut Vec<SpannedToken>,
        start: usize,
        line: u32,
        col: u32,
    ) -> Result<(), SparError> {
        self.advance();
        let mut word = String::new();
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    return Err(SparError::LexError {
                        message: "unterminated quoted shell word".to_string(),
                        span: self.span_at(start, line, col),
                    });
                }
                Some(b'"') => {
                    self.advance();
                    tokens.push(SpannedToken::new(
                        Token::ShellWord(word),
                        self.span_at(start, line, col),
                    ));
                    return Ok(());
                }
                Some(b'\\') if matches!(self.peek_at(1), Some(b'\\') | Some(b'"')) => {
                    self.advance();
                    word.push(self.advance().expect("escaped byte was peeked") as char);
                }
                Some(_) => {
                    if let Some(ch) = self.advance_char() {
                        word.push(ch);
                    }
                }
            }
        }
    }

    fn lex_literal_shell_word(
        &mut self,
        tokens: &mut Vec<SpannedToken>,
        start: usize,
        line: u32,
        col: u32,
    ) -> Result<(), SparError> {
        self.advance();
        let content_start = self.pos;
        while let Some(byte) = self.peek() {
            if byte == b'\'' {
                let text = self.source[content_start..self.pos].to_string();
                self.advance();
                tokens.push(SpannedToken::new(
                    Token::ShellLiteralWord(text),
                    self.span_at(start, line, col),
                ));
                return Ok(());
            }
            self.advance_char();
        }
        Err(SparError::LexError {
            message: "unterminated single-quoted shell word".into(),
            span: self.span_at(start, line, col),
        })
    }

    fn lex_bare_shell_word(
        &mut self,
        tokens: &mut Vec<SpannedToken>,
        start: usize,
        line: u32,
        col: u32,
    ) {
        while let Some(byte) = self.peek() {
            if byte == b'$' && self.peek_at(1) == Some(b'{') {
                self.advance();
                self.advance();
                let mut depth = 1_u32;
                while let Some(interpolation_byte) = self.peek() {
                    self.advance_char();
                    if interpolation_byte == b'{' {
                        depth += 1;
                    } else if interpolation_byte == b'}' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                continue;
            }
            // `$(...)` is one shell-word segment even when the native command
            // contains spaces, pipes, redirects, or logical operators.  Keep
            // it inside this ShellWord token; shell_lang later parses the
            // captured segment structurally and appends its text result to
            // the surrounding argv word without Bash-style splitting.
            if byte == b'$' && self.peek_at(1) == Some(b'(') {
                self.consume_shell_word_command_substitution();
                continue;
            }
            if byte.is_ascii_whitespace()
                || matches!(
                    byte,
                    b';' | b'|' | b'&' | b'<' | b'>' | b'{' | b'}' | b'"' | b'\''
                )
            {
                break;
            }
            self.advance_char();
        }
        tokens.push(SpannedToken::new(
            Token::ShellWord(self.source[start..self.pos].to_string()),
            self.span_at(start, line, col),
        ));
    }

    fn consume_shell_word_command_substitution(&mut self) {
        debug_assert_eq!(self.peek(), Some(b'$'));
        debug_assert_eq!(self.peek_at(1), Some(b'('));
        self.advance();
        self.advance();
        let mut depth = 1_u32;
        let mut quote = None;
        let mut escaped = false;
        while let Some(byte) = self.peek() {
            if escaped {
                escaped = false;
                self.advance_char();
                continue;
            }
            if byte == b'\\' && quote != Some(b'\'') {
                escaped = true;
                self.advance();
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
                self.advance();
                continue;
            }
            if quote.is_none() {
                if byte == b'(' {
                    depth += 1;
                } else if byte == b')' {
                    depth -= 1;
                    self.advance();
                    if depth == 0 {
                        return;
                    }
                    continue;
                }
            }
            self.advance_char();
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<SpannedToken>, SparError> {
        self.tokenize_inner()
    }

    pub fn tokenize_with_comments(
        mut self,
    ) -> Result<(Vec<SpannedToken>, Vec<CommentTrivia>), SparError> {
        let tokens = self.tokenize_inner()?;
        Ok((tokens, self.comments))
    }

    fn tokenize_inner(&mut self) -> Result<Vec<SpannedToken>, SparError> {
        let mut tokens: Vec<SpannedToken> = Vec::new();

        if self.shebang.is_some() {
            self.skip_line_comment(); // stops before the '\n', same as `//`
        }

        loop {
            let start = self.pos;
            let line = self.line;
            let col = self.col;

            let c = match self.peek() {
                None => break,
                Some(c) => c,
            };

            match c {
                b' ' | b'\t' | b'\r' | b'\n' => {
                    self.advance();
                }
                b'"' => {
                    self.advance();
                    self.lex_string(&mut tokens, start, line, col)?;
                    self.last_token_line = line;
                }
                b'$' if self.peek_at(1) == Some(b'(') => {
                    self.advance();
                    self.advance();
                    tokens.push(SpannedToken::new(
                        Token::CommandSubStart,
                        self.span_at(start, line, col),
                    ));
                    self.lex_command_substitution(&mut tokens, start, line, col)?;
                    self.last_token_line = line;
                }
                b'{' => {
                    self.advance();
                    tokens.push(SpannedToken::new(
                        Token::LBrace,
                        self.span_at(start, line, col),
                    ));
                    self.last_token_line = line;
                }
                b'}' => {
                    self.advance();
                    tokens.push(SpannedToken::new(
                        Token::RBrace,
                        self.span_at(start, line, col),
                    ));
                    self.last_token_line = line;
                }
                _ => {
                    if let Some(t) = self.lex_single_token(c, start, line, col)? {
                        self.last_token_line = line;
                        let is_run = matches!(&t.token, Token::Ident(s) if s == "run");
                        let is_shell_return_type =
                            matches!(tokens.last().map(|token| &token.token), Some(Token::Arrow));
                        let is_shell =
                            matches!(&t.token, Token::TypeShell) && !is_shell_return_type;
                        let is_command = matches!(&t.token, Token::KwCommand);
                        let is_exec = matches!(&t.token, Token::KwExec);
                        tokens.push(t);
                        if is_run {
                            self.maybe_enter_run_body(&mut tokens)?;
                        } else if is_shell {
                            self.maybe_enter_shell_body(&mut tokens)?;
                        } else if is_command && self.should_enter_command_body() {
                            self.lex_command_body(&mut tokens)?;
                        } else if is_exec {
                            self.maybe_enter_exec_body(&mut tokens)?;
                        }
                    }
                }
            }
        }

        tokens.push(SpannedToken::new(
            Token::Eof,
            Span::new(self.pos, self.pos, self.line, self.col),
        ));

        Ok(tokens)
    }

    fn lex_command_substitution(
        &mut self,
        tokens: &mut Vec<SpannedToken>,
        expression_start: usize,
        expression_line: u32,
        expression_col: u32,
    ) -> Result<(), SparError> {
        let body_start = self.pos;
        let mut quote = None;
        let mut escaped = false;
        let mut depth = 1_u32;
        while let Some(byte) = self.peek() {
            if escaped {
                escaped = false;
                self.advance_char();
                continue;
            }
            if byte == b'\\' && quote.is_some() {
                escaped = true;
                self.advance();
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
                self.advance();
                continue;
            }
            if quote.is_none() {
                if byte == b'(' {
                    depth += 1;
                } else if byte == b')' {
                    depth -= 1;
                    if depth == 0 {
                        let body_end = self.pos;
                        self.advance();
                        let body = self.source[body_start..body_end].trim();
                        let mut normalized_body = String::new();
                        for line in body.lines() {
                            append_native_command_line(&mut normalized_body, line);
                        }
                        let normalized_body = normalized_body.trim();
                        let wrapped = format!("command {normalized_body};");
                        let mut nested = Lexer::new(&wrapped).tokenize()?;
                        nested.retain(|token| {
                            token.token != Token::KwCommand && token.token != Token::Eof
                        });
                        if nested
                            .last()
                            .is_some_and(|token| token.token == Token::Semicolon)
                        {
                            nested.pop();
                        }
                        for mut token in nested {
                            token.span = Span::new(
                                body_start,
                                body_end,
                                expression_line,
                                expression_col + 2,
                            );
                            tokens.push(token);
                        }
                        tokens.push(SpannedToken::new(
                            Token::CommandSubEnd,
                            Span::new(body_end, self.pos, self.line, self.col),
                        ));
                        return Ok(());
                    }
                }
            }
            self.advance_char();
        }
        Err(SparError::LexError {
            message: "unterminated command substitution — expected ')'".into(),
            span: Span::new(expression_start, self.pos, expression_line, expression_col),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Token;

    fn lex(src: &str) -> Vec<Token> {
        Lexer::new(src)
            .tokenize()
            .expect("lex failed")
            .into_iter()
            .map(|st| st.token)
            .collect()
    }

    #[test]
    fn multibyte_utf8_in_string_literal_decodes_correctly() {
        // Regression: the string-fragment scanner used to cast raw u8
        // bytes to char one at a time, which mis-decodes any multi-byte
        // UTF-8 sequence (an em-dash, an accented letter, ...) into
        // several garbage Latin-1-ish characters instead of the one real
        // character.
        let tokens = lex(r#""café — naïve""#);
        assert_eq!(tokens[1], Token::StringFragment("café — naïve".into()));
    }

    #[test]
    fn multibyte_utf8_in_run_block_decodes_correctly() {
        let tokens = lex("task [X] { run { echo café; }; }");
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t, Token::ShellFragment(s) if s.contains("café"))),
            "expected a ShellFragment containing 'café', got: {tokens:?}"
        );
    }

    #[test]
    fn test_basic_var_decl() {
        assert_eq!(
            lex("var port: int = 3000;"),
            vec![
                Token::Var,
                Token::Ident("port".into()),
                Token::Colon,
                Token::TypeInt,
                Token::Eq,
                Token::IntLit(3000),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_export_var() {
        let tokens = lex(r#"export var version: str = "1.0.0";"#);
        assert_eq!(tokens[0], Token::Export);
        assert_eq!(tokens[1], Token::Var);
        assert_eq!(tokens[2], Token::Ident("version".into()));
        assert_eq!(tokens[3], Token::Colon);
        assert_eq!(tokens[4], Token::TypeStr);
        assert_eq!(tokens[5], Token::Eq);
        assert_eq!(tokens[6], Token::StringStart);
    }

    #[test]
    fn test_plain_string() {
        assert_eq!(
            lex(r#""base.cl""#),
            vec![
                Token::StringStart,
                Token::StringFragment("base.cl".into()),
                Token::StringEnd,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_interp_single() {
        assert_eq!(
            lex(r#""http://${host}""#),
            vec![
                Token::StringStart,
                Token::StringFragment("http://".into()),
                Token::InterpolStart,
                Token::Ident("host".into()),
                Token::InterpolEnd,
                Token::StringFragment("".into()),
                Token::StringEnd,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_interp_multiple() {
        assert_eq!(
            lex(r#""${host}:${port}""#),
            vec![
                Token::StringStart,
                Token::StringFragment("".into()),
                Token::InterpolStart,
                Token::Ident("host".into()),
                Token::InterpolEnd,
                Token::StringFragment(":".into()),
                Token::InterpolStart,
                Token::Ident("port".into()),
                Token::InterpolEnd,
                Token::StringFragment("".into()),
                Token::StringEnd,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_import_with_alias() {
        assert_eq!(
            lex(r#"import "base.cl" as config;"#),
            vec![
                Token::Import,
                Token::StringStart,
                Token::StringFragment("base.cl".into()),
                Token::StringEnd,
                Token::As,
                Token::Ident("config".into()),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_section_decl() {
        assert_eq!(
            lex("[server]{"),
            vec![
                Token::LBracket,
                Token::Ident("server".into()),
                Token::RBracket,
                Token::LBrace,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_multi_char_ops() {
        assert_eq!(
            lex("?? :: ..."),
            vec![
                Token::QuestionQuestion,
                Token::ColonColon,
                Token::DotDotDot,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_namespace_ref() {
        assert_eq!(
            lex("global::port"),
            vec![
                Token::Ident("global".into()),
                Token::ColonColon,
                Token::Ident("port".into()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_float_literal() {
        #[allow(clippy::approx_constant)]
        {
            assert_eq!(lex("3.14"), vec![Token::FloatLit(3.14), Token::Eof]);
        }
    }

    #[test]
    fn test_int_not_float() {
        assert_eq!(lex("3000"), vec![Token::IntLit(3000), Token::Eof]);
    }

    #[test]
    fn test_dynamic_decl() {
        let tokens = lex(r#"dynamic var tags = [2026, "prod", true];"#);
        assert_eq!(tokens[0], Token::Dynamic);
        assert_eq!(tokens[1], Token::Var);
        assert_eq!(tokens[2], Token::Ident("tags".into()));
    }

    #[test]
    fn test_bool_keywords() {
        assert_eq!(
            lex("true false"),
            vec![Token::True, Token::False, Token::Eof]
        );
    }

    #[test]
    fn test_all_keywords() {
        assert_eq!(
            lex("var export import as dynamic str int float bool true false section"),
            vec![
                Token::Var,
                Token::Export,
                Token::Import,
                Token::As,
                Token::Dynamic,
                Token::TypeStr,
                Token::TypeInt,
                Token::TypeFloat,
                Token::TypeBool,
                Token::True,
                Token::False,
                Token::TypeSection,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_section_keyword() {
        assert_eq!(lex("section"), vec![Token::TypeSection, Token::Eof]);
    }

    #[test]
    fn test_line_comment_skipped() {
        assert_eq!(
            lex("var // this is ignored\nport"),
            vec![Token::Var, Token::Ident("port".into()), Token::Eof]
        );
    }

    #[test]
    fn test_block_comment_skipped() {
        assert_eq!(
            lex("var /* ignored */ port"),
            vec![Token::Var, Token::Ident("port".into()), Token::Eof]
        );
    }

    #[test]
    fn test_line_comment_collected_as_trivia() {
        let (tokens, comments) =
            Lexer::new("var x: int = 1; // trailing\n// standalone\nvar y: int = 2;")
                .tokenize_with_comments()
                .expect("lex failed");
        // tokens still work
        assert!(tokens.iter().any(|t| t.token == Token::Var));
        // two comments
        assert_eq!(comments.len(), 2, "got: {:?}", comments);
        assert!(comments[0].is_trailing, "first should be trailing");
        assert_eq!(comments[0].line, 1);
        assert!(!comments[1].is_trailing, "second should be standalone");
        assert_eq!(comments[1].line, 2);
        assert!(
            comments[1].text.contains("standalone"),
            "text: {:?}",
            comments[1].text
        );
    }

    #[test]
    fn test_unclosed_block_comment() {
        let result = Lexer::new("/* not closed").tokenize();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unterminated block comment"), "got: {msg}");
    }

    #[test]
    fn test_unterminated_string() {
        let result = Lexer::new("\"not closed").tokenize();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unterminated string"), "got: {msg}");
    }

    #[test]
    fn test_optional_field() {
        assert_eq!(
            lex("log_level?: str"),
            vec![
                Token::Ident("log_level".into()),
                Token::Question,
                Token::Colon,
                Token::TypeStr,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn test_arith_and_fallback() {
        assert_eq!(
            lex("30 * 3 ?? 90"),
            vec![
                Token::IntLit(30),
                Token::Star,
                Token::IntLit(3),
                Token::QuestionQuestion,
                Token::IntLit(90),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn lex_new_tokens() {
        let cases: &[(&str, Token)] = &[
            ("->", Token::Arrow),
            ("==", Token::EqEq),
            ("!=", Token::NotEq),
            ("<=", Token::LtEq),
            (">=", Token::GtEq),
            ("&&", Token::AndAnd),
            ("||", Token::OrOr),
            ("!", Token::Bang),
            ("<", Token::Lt),
            (">", Token::Gt),
        ];
        for (src, expected) in cases {
            let tokens = Lexer::new(src).tokenize().unwrap();
            assert_eq!(tokens[0].token, *expected, "failed on {src:?}");
        }
    }

    #[test]
    fn lex_keywords_function_return_if_else_for_in_break_continue() {
        let cases: &[(&str, Token)] = &[
            ("function", Token::KwFunction),
            ("async", Token::KwAsync),
            ("await", Token::KwAwait),
            ("return", Token::KwReturn),
            ("if", Token::KwIf),
            ("else", Token::KwElse),
            ("for", Token::KwFor),
            ("in", Token::KwIn),
            ("break", Token::KwBreak),
            ("continue", Token::KwContinue),
            ("try", Token::KwTry),
            ("catch", Token::KwCatch),
        ];
        for (src, expected) in cases {
            let tokens = Lexer::new(src).tokenize().unwrap();
            assert_eq!(tokens[0].token, *expected, "failed on {src:?}");
        }
    }

    #[test]
    fn minus_not_arrow() {
        let tokens = Lexer::new("x - y").tokenize().unwrap();
        assert_eq!(tokens[1].token, Token::Minus);
    }

    #[test]
    fn eq_not_eqeq() {
        let tokens = Lexer::new("x = 1").tokenize().unwrap();
        assert_eq!(tokens[1].token, Token::Eq);
    }

    #[test]
    fn lex_at_sign() {
        let tokens = Lexer::new("@").tokenize().unwrap();
        assert_eq!(tokens[0].token, Token::At);
    }

    #[test]
    fn lex_at_before_ident() {
        let tokens = Lexer::new("@SchemaFile").tokenize().unwrap();
        assert_eq!(tokens[0].token, Token::At);
        assert_eq!(tokens[1].token, Token::Ident("SchemaFile".to_string()));
    }

    #[test]
    fn run_block_preserves_quotes_pipes_redirects_braces_and_interpolation() {
        let src = r#"run {
    echo "hello world" | grep hi > out.txt;
    if [ -f x ]; then { echo nested; }; fi;
    cargo run -- --port ${port} $HOME #{HOME:-x};
};"#;
        let tokens = lex(src);

        assert_eq!(tokens[0], Token::Ident("run".into()));
        assert_eq!(tokens[1], Token::RunStart);
        assert_eq!(tokens.last(), Some(&Token::Eof));
        assert!(tokens.contains(&Token::RunEnd));

        let fragments: Vec<&str> = tokens
            .iter()
            .filter_map(|t| match t {
                Token::ShellFragment(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        let joined = fragments.join("");
        assert!(joined.contains(r#"echo "hello world" | grep hi > out.txt;"#));
        assert!(joined.contains("if [ -f x ]; then { echo nested; }; fi;"));
        assert!(joined.contains("$HOME"));
        // Escapes stay verbatim at the lexer boundary; lowering owns their semantics.
        assert!(joined.contains("#{HOME:-x}"));

        // `${port}` must have become a real interpolation island, not shell text.
        assert!(tokens.contains(&Token::InterpolStart));
        assert!(tokens.contains(&Token::Ident("port".into())));
        assert!(!joined.contains("${port}"));
    }

    #[test]
    fn run_block_does_not_swallow_the_rest_of_the_file() {
        let tokens = lex("run { echo hi; }; var x: int = 1;");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("run".into()),
                Token::RunStart,
                Token::ShellFragment(" echo hi; ".into()),
                Token::RunEnd,
                Token::Semicolon,
                Token::Var,
                Token::Ident("x".into()),
                Token::Colon,
                Token::TypeInt,
                Token::Eq,
                Token::IntLit(1),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn bare_run_identifier_without_brace_is_not_special_cased() {
        // `run` used as an ordinary identifier (no `{` immediately after) must
        // lex like any other identifier — the raw-mode heuristic only fires
        // on `run` directly followed by `{`.
        let tokens = lex("var run: int = 1;");
        assert_eq!(
            tokens,
            vec![
                Token::Var,
                Token::Ident("run".into()),
                Token::Colon,
                Token::TypeInt,
                Token::Eq,
                Token::IntLit(1),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn unterminated_run_block_is_a_lex_error() {
        let result = Lexer::new("run { echo hi;").tokenize();
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unterminated run block"), "got: {msg}");
    }

    #[test]
    fn shell_block_tokenizes_bare_words() {
        assert_eq!(
            lex("shell { echo hello; }"),
            vec![
                Token::ShellBlockStart,
                Token::ShellWord("echo".into()),
                Token::ShellWord("hello".into()),
                Token::Semicolon,
                Token::ShellBlockEnd,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn shell_block_keeps_a_quoted_argument_as_one_word() {
        let tokens = lex(r#"shell { rm "my file.txt"; }"#);
        assert_eq!(tokens[2], Token::ShellWord("my file.txt".into()));
    }

    #[test]
    fn shell_block_tokenizes_pipeline_and_redirect_operators() {
        assert_eq!(
            lex("shell { cat input | grep x > out >> log 2> err; }"),
            vec![
                Token::ShellBlockStart,
                Token::ShellWord("cat".into()),
                Token::ShellWord("input".into()),
                Token::ShellPipe,
                Token::ShellWord("grep".into()),
                Token::ShellWord("x".into()),
                Token::Gt,
                Token::ShellWord("out".into()),
                Token::ShellRedirectAppend,
                Token::ShellWord("log".into()),
                Token::ShellRedirectStderr,
                Token::ShellWord("err".into()),
                Token::Semicolon,
                Token::ShellBlockEnd,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn shell_background_command_terminates_before_following_spar_statement() {
        let tokens = Lexer::new(
            r#"shell {
                sleep 1 &
                println(message: "done");
            }"#,
        )
        .tokenize()
        .expect("background command followed by Spar call should lex");

        assert!(tokens.iter().any(|token| token.token == Token::ShellBackground));
        assert!(tokens.iter().any(|token| matches!(&token.token, Token::Ident(name) if name == "println")));
    }

    #[test]
    fn shell_background_command_can_be_followed_by_another_native_command() {
        let tokens = Lexer::new("shell { sleep 1 & echo done; }")
            .tokenize()
            .expect("background list separator should lex");
        let words = tokens
            .iter()
            .filter_map(|token| match &token.token {
                Token::ShellWord(word) => Some(word.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(words, vec!["sleep", "1", "echo", "done"]);
        assert_eq!(
            tokens
                .iter()
                .filter(|token| token.token == Token::ShellBackground)
                .count(),
            1
        );
    }

    #[test]
    fn command_keyword_stays_soft_in_field_and_call_positions() {
        assert_eq!(
            lex("type Tool { command: str; exec: str; shell: str; };")
                .into_iter()
                .filter(|token| !matches!(token, Token::Eof))
                .collect::<Vec<_>>()
                .iter()
                .filter(|token| matches!(token, Token::KwCommand))
                .count(),
            1
        );

        let tokens = lex("command(name: \"x\");");
        assert!(tokens.contains(&Token::KwCommand));
        assert!(!tokens.iter().any(|token| matches!(token, Token::ShellWord(_))));
    }

    #[test]
    fn command_sugar_uses_the_native_command_tokenizer() {
        assert_eq!(
            lex(r#"command echo "hello world";"#),
            vec![
                Token::KwCommand,
                Token::ShellWord("echo".into()),
                Token::ShellWord("hello world".into()),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn shell_return_type_does_not_enter_command_mode() {
        let tokens = lex("function main() -> shell { return shell {}; };");
        assert_eq!(tokens[5], Token::TypeShell);
        assert_eq!(tokens[6], Token::LBrace);
        assert!(tokens.contains(&Token::ShellBlockStart));
    }

    #[test]
    fn unterminated_shell_block_is_a_lex_error() {
        let err = Lexer::new("shell { echo hi;")
            .tokenize()
            .expect_err("unterminated shell block must fail");
        assert!(
            matches!(err, SparError::LexError { .. })
                && err.to_string().contains("unterminated shell block"),
            "got: {err}"
        );
    }

    #[test]
    fn bare_brace_inside_shell_block_is_a_lex_error() {
        let err = Lexer::new("shell { echo {; }")
            .tokenize()
            .expect_err("bare command-word braces must fail");
        assert!(
            matches!(err, SparError::LexError { .. }) && err.to_string().contains("brace"),
            "got: {err}"
        );
    }
}
