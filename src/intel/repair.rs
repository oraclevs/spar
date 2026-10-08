//! Blanking of statements that do not parse, so the rest of a buffer can still
//! be analysed. Offsets never move.

use crate::error::SparError;
use crate::lexer::Lexer;
use crate::parser::Parser;

/// How many broken statements [`repair_source`] blanks before giving up; a
/// task-runner file can have one bad line per task.
pub const REPAIR_ATTEMPTS: usize = 48;

/// The text of `source` after blanking, in place, each statement the lexer or
/// parser rejects, until the rest lexes and parses. `None` when it cannot be
/// made to parse. Offsets never move.
pub fn repair_source(source: &str) -> Option<String> {
    let mut text = source.to_string();
    for _ in 0..REPAIR_ATTEMPTS {
        let failure = match Lexer::new(&text).tokenize() {
            Ok(tokens) => match Parser::new(tokens).parse() {
                Ok(_) => return Some(text),
                Err(SparError::ParseError { span, .. }) => (false, span.start),
                Err(SparError::LexError { span, .. }) => (true, span.start),
                Err(_) => return None,
            },
            Err(SparError::LexError { span, .. }) => (true, span.start),
            Err(SparError::ParseError { span, .. }) => (false, span.start),
            Err(_) => return None,
        };
        // A lexical error can sit inside a string, where statement boundaries
        // are unreliable: blank the whole physical line.
        let repaired = if failure.0 {
            blank_line_around(&text, failure.1)
        } else {
            blank_statement_around(&text, failure.1)
        };
        if repaired == text {
            return None;
        }
        text = repaired;
    }
    None
}

pub fn floor_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn blank_range(text: &str, start: usize, end: usize) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, ch) in text.char_indices() {
        if index >= start && index < end && ch != '\n' {
            out.extend(std::iter::repeat_n(' ', ch.len_utf8()));
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn blank_statement_around(text: &str, at: usize) -> String {
    let bytes = text.as_bytes();
    let at = floor_boundary(text, at);
    let mut start = at;
    while start > 0 && !matches!(bytes[start - 1], b';' | b'{' | b'}') {
        start -= 1;
    }
    let mut end = at;
    while end < bytes.len() && !matches!(bytes[end], b';' | b'}') {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b';' {
        end += 1;
    }
    blank_range(text, start, end)
}

pub fn blank_line_around(text: &str, at: usize) -> String {
    let at = floor_boundary(text, at);
    let start = text[..at].rfind('\n').map_or(0, |index| index + 1);
    let end = text[at..].find('\n').map_or(text.len(), |index| at + index);
    blank_range(text, start, end)
}

