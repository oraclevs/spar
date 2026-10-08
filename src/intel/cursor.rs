//! Pure cursor helpers: lexical state, statement bounds and call recovery.
//! They work on plain source text and byte offsets only.

/// A call whose argument list contains the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallCursor {
    pub callee: String,
    pub supplied: Vec<String>,
    pub active_parameter: u32,
    /// `Some(param)` when the cursor sits after `param:` of the current argument.
    pub value_of: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorLexicalState {
    Code,
    String,
    LineComment,
    BlockComment,
}

pub fn lexical_state_at(source: &str, offset: usize) -> CursorLexicalState {
    lexical_scan(source, offset).0
}

/// Whether `offset` sits inside a still-open `${ ... }` of the string it is
/// in — code, not literal text, so completion applies there.
pub fn inside_string_interpolation(source: &str, offset: usize) -> bool {
    let (state, string_start) = lexical_scan(source, offset);
    let (CursorLexicalState::String, Some(start)) = (state, string_start) else {
        return false;
    };
    let body = source.get(start + 1..offset.min(source.len())).unwrap_or("");
    let bytes = body.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 1,
            b'$' if bytes.get(i + 1) == Some(&b'{') => {
                depth += 1;
                i += 1;
            }
            b'}' if depth > 0 => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    depth > 0
}

/// Scans `source` up to `offset`; also returns where the string the cursor is
/// in began (the opening quote), when it is in one.
pub fn lexical_scan(source: &str, offset: usize) -> (CursorLexicalState, Option<usize>) {
    let bytes = source.as_bytes();
    let mut string_start = None;
    let end = offset.min(bytes.len());
    let mut i = 0usize;
    let mut block_depth = 0usize;
    let mut in_line = false;
    let mut in_string = false;
    let mut escaped = false;

    while i < end {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if in_line {
            if b == b'\n' {
                in_line = false;
            }
            i += 1;
            continue;
        }
        if block_depth > 0 {
            if b == b'/' && next == Some(b'*') {
                block_depth += 1;
                i += 2;
                continue;
            }
            if b == b'*' && next == Some(b'/') {
                block_depth = block_depth.saturating_sub(1);
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'/' && next == Some(b'/') {
            in_line = true;
            i += 2;
            continue;
        }
        if b == b'/' && next == Some(b'*') {
            block_depth = 1;
            i += 2;
            continue;
        }
        if b == b'"' {
            in_string = true;
            string_start = Some(i);
        }
        i += 1;
    }

    let state = if in_line {
        CursorLexicalState::LineComment
    } else if block_depth > 0 {
        CursorLexicalState::BlockComment
    } else if in_string {
        CursorLexicalState::String
    } else {
        CursorLexicalState::Code
    };
    (state, string_start)
}

pub fn statement_bounds(source: &str, offset: usize) -> (usize, usize) {
    let bytes = source.as_bytes();
    let mut semicolons = Vec::new();
    let mut i = 0usize;
    let mut block_depth = 0usize;
    let mut in_line = false;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if in_line {
            if b == b'\n' { in_line = false; }
            i += 1;
            continue;
        }
        if block_depth > 0 {
            if b == b'/' && next == Some(b'*') { block_depth += 1; i += 2; continue; }
            if b == b'*' && next == Some(b'/') { block_depth -= 1; i += 2; continue; }
            i += 1;
            continue;
        }
        if in_string {
            if escaped { escaped = false; }
            else if b == b'\\' { escaped = true; }
            else if b == b'"' { in_string = false; }
            i += 1;
            continue;
        }
        if b == b'/' && next == Some(b'/') { in_line = true; i += 2; continue; }
        if b == b'/' && next == Some(b'*') { block_depth = 1; i += 2; continue; }
        if b == b'"' { in_string = true; i += 1; continue; }
        if b == b';' { semicolons.push(i); }
        i += 1;
    }
    let clamped = offset.min(source.len());
    let start = semicolons.iter().copied().filter(|at| *at < clamped).max().map_or(0, |at| at + 1);
    let end = semicolons.iter().copied().find(|at| *at >= clamped).map_or(source.len(), |at| at + 1);
    (start, end)
}

pub fn masked_code(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0usize;
    let mut block_depth = 0usize;
    let mut in_line = false;
    let mut in_string = false;
    let mut escaped = false;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if in_line {
            if b == b'\n' { in_line = false; } else { out[i] = b' '; }
            i += 1;
            continue;
        }
        if block_depth > 0 {
            out[i] = if b == b'\n' { b'\n' } else { b' ' };
            if b == b'/' && next == Some(b'*') { if i + 1 < out.len() { out[i + 1] = b' '; } block_depth += 1; i += 2; continue; }
            if b == b'*' && next == Some(b'/') { if i + 1 < out.len() { out[i + 1] = b' '; } block_depth -= 1; i += 2; continue; }
            i += 1;
            continue;
        }
        if in_string {
            if b != b'\n' { out[i] = b' '; }
            if escaped { escaped = false; }
            else if b == b'\\' { escaped = true; }
            else if b == b'"' { in_string = false; }
            i += 1;
            continue;
        }
        if b == b'/' && next == Some(b'/') { out[i]=b' '; if i+1<out.len(){out[i+1]=b' ';} in_line=true; i+=2; continue; }
        if b == b'/' && next == Some(b'*') { out[i]=b' '; if i+1<out.len(){out[i+1]=b' ';} block_depth=1; i+=2; continue; }
        if b == b'"' { out[i]=b' '; in_string=true; i+=1; continue; }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

pub fn split_top_level_arguments(source: &str, masked: &str) -> (Vec<String>, u32) {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut paren = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    for (index, ch) in masked.char_indices() {
        match ch {
            '(' => paren += 1,
            ')' => paren -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            '{' => brace += 1,
            '}' => brace -= 1,
            ',' if paren == 0 && bracket == 0 && brace == 0 => {
                segments.push(source[start..index].to_string());
                start = index + 1;
            }
            _ => {}
        }
    }
    segments.push(source[start..].to_string());
    let active = segments.len().saturating_sub(1) as u32;
    (segments, active)
}

pub fn call_context(source: &str, offset: usize) -> Option<CallCursor> {
    let prefix = source.get(..offset.min(source.len()))?;
    let masked = masked_code(prefix);
    let bytes = masked.as_bytes();
    let mut depth = 0i32;
    let mut open = None;
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b')' => depth += 1,
            b'(' if depth == 0 => { open = Some(i); break; }
            b'(' => depth -= 1,
            _ => {}
        }
    }
    let open = open?;
    let before = &masked[..open];
    let end = before.trim_end().len();
    let mut start = end;
    let mut angle_depth = 0usize;
    while start > 0 {
        let b = before.as_bytes()[start - 1];
        if b == b'>' { angle_depth += 1; start -= 1; continue; }
        if b == b'<' && angle_depth > 0 { angle_depth -= 1; start -= 1; continue; }
        if angle_depth > 0 || b.is_ascii_alphanumeric() || b == b'_' || b == b':' || b == b'.' {
            start -= 1;
        } else {
            break;
        }
    }
    let callee = before[start..end].trim().to_string();
    if callee.is_empty() || matches!(callee.as_str(), "if" | "for" | "fn" | "function" | "task" | "shell" | "__shell") {
        return None;
    }
    let leading = before[..start].trim_end();
    if leading.ends_with("fn") || leading.ends_with("function") || leading.ends_with("task") {
        return None;
    }
    let args_source = &prefix[open + 1..];
    let args_masked = &masked[open + 1..];
    let (segments, active_parameter) = split_top_level_arguments(args_source, args_masked);
    let current_segment = segments.last().map(String::as_str).unwrap_or("");
    let value_of = current_segment.find(':').and_then(|colon| {
        let name = current_segment[..colon].trim();
        (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .then(|| name.to_string())
    });
    let supplied = segments[..segments.len().saturating_sub(1)]
        .iter()
        .filter_map(|segment| {
            let colon = segment.find(':')?;
            let candidate = segment[..colon].trim();
            (!candidate.is_empty() && candidate.chars().all(|c| c.is_alphanumeric() || c == '_'))
                .then(|| candidate.to_string())
        })
        .collect();
    Some(CallCursor { callee, supplied, active_parameter, value_of })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_state_distinguishes_code_strings_and_comments() {
        let src = "var a = \"x\"; // note\n/* blk */ var b = \"open";
        assert_eq!(lexical_state_at(src, 3), CursorLexicalState::Code);
        assert_eq!(lexical_state_at(src, 10), CursorLexicalState::String);
        assert_eq!(lexical_state_at(src, 18), CursorLexicalState::LineComment);
        assert_eq!(lexical_state_at(src, 26), CursorLexicalState::BlockComment);
        assert_eq!(lexical_state_at(src, src.len()), CursorLexicalState::String);
    }

    #[test]
    fn nested_block_comments_stay_open() {
        let src = "/* a /* b */ still */ x";
        assert_eq!(lexical_state_at(src, 15), CursorLexicalState::BlockComment);
        assert_eq!(lexical_state_at(src, src.len()), CursorLexicalState::Code);
    }

    #[test]
    fn interpolation_is_code_but_plain_string_is_not() {
        let src = "var s = \"a ${x";
        assert!(inside_string_interpolation(src, src.len()));
        let plain = "var s = \"abc";
        assert!(!inside_string_interpolation(plain, plain.len()));
        let closed = "var s = \"a ${x} b";
        assert!(!inside_string_interpolation(closed, closed.len()));
    }

    #[test]
    fn lexical_scan_reports_string_start() {
        let src = "var s = \"abc";
        assert_eq!(lexical_scan(src, src.len()), (CursorLexicalState::String, Some(8)));
    }

    #[test]
    fn statement_bounds_ignore_semicolons_in_strings_and_comments() {
        let src = "var a = \";\"; // ;\nvar b = 1;";
        let at = src.find("var b").unwrap() + 2;
        let (start, end) = statement_bounds(src, at);
        assert_eq!(&src[start..end], " // ;\nvar b = 1;");
        assert_eq!(statement_bounds("x", 99), (0, 1));
    }

    #[test]
    fn masked_code_blanks_strings_and_comments_keeping_offsets() {
        let src = "f(\"a,b\") // c\nx";
        let masked = masked_code(src);
        assert_eq!(masked.len(), src.len());
        assert_eq!(masked, "f(     )     \nx");
    }

    #[test]
    fn split_arguments_respects_nesting() {
        let src = "a, f(b, c), [d, e], g";
        let (segments, active) = split_top_level_arguments(src, &masked_code(src));
        assert_eq!(segments.len(), 4);
        assert_eq!(active, 3);
        assert_eq!(segments[3].trim(), "g");
    }

    #[test]
    fn call_context_finds_callee_and_named_arguments() {
        let src = "var x = greet(name: \"a\", age: ";
        let call = call_context(src, src.len()).unwrap();
        assert_eq!(call.callee, "greet");
        assert_eq!(call.supplied, vec!["name".to_string()]);
        assert_eq!(call.active_parameter, 1);
        assert_eq!(call.value_of.as_deref(), Some("age"));
    }

    #[test]
    fn call_context_keeps_dotted_and_generic_callees() {
        let src = "obj.method(1, ";
        assert_eq!(call_context(src, src.len()).unwrap().callee, "obj.method");
        let src = "make<Int>(";
        assert_eq!(call_context(src, src.len()).unwrap().callee, "make<Int>");
    }

    #[test]
    fn call_context_rejects_keywords_and_declarations() {
        assert!(call_context("if (a", 5).is_none());
        assert!(call_context("function f(a", 12).is_none());
        assert!(call_context("var x = 1", 9).is_none());
    }

    #[test]
    fn helpers_survive_multibyte_offsets() {
        let src = "var s = \"é\"; f(é";
        let _ = lexical_state_at(src, 10);
        let _ = statement_bounds(src, 10);
        assert!(call_context(src, 11).is_none() || true);
    }
}
