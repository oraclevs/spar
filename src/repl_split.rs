//! REPL statement splitter: classifies each statement of a submission as
//! Spar or a native command.

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplKind {
    Spar,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplStatement {
    pub text: String,
    pub kind: ReplKind,
    /// Byte ranges in `text` that were inserted by the block rewrite (`~ `
    /// markers and `;` terminators). Removing them yields the user's text.
    /// An offset `o` in `text` maps to the original offset `o` minus the
    /// total length of the inserted ranges that end at or before it.
    pub inserted: Vec<(usize, usize)>,
}

/// Splits a REPL submission into statements. `names` are the variable,
/// function and parameter names visible in the session.
pub fn split_statements(source: &str, names: &HashSet<String>) -> Vec<ReplStatement> {
    split_inner(source, names)
        .into_iter()
        .map(|item| {
            let original = &source[item.start..item.end];
            match item.kind {
                ReplKind::Command => ReplStatement {
                    text: original.to_string(),
                    kind: ReplKind::Command,
                    inserted: Vec::new(),
                },
                ReplKind::Spar => {
                    let mut ins = Vec::new();
                    if let Some(scope) = &item.scope {
                        spar_insertions(original, scope, 0, &mut ins);
                    }
                    let (text, inserted) = apply_insertions(original, ins);
                    ReplStatement { text, kind: ReplKind::Spar, inserted }
                }
            }
        })
        .collect()
}

struct Item {
    kind: ReplKind,
    /// Trimmed range in the scanned source.
    start: usize,
    end: usize,
    segs: Vec<SegInfo>,
    /// Names in scope for this statement (control-flow Spar statements only).
    scope: Option<HashSet<String>>,
}

struct SegInfo {
    start: usize,
    /// End of the code, excluding a trailing comment and whitespace.
    code_end: usize,
    semi: bool,
}

fn is_comment_segment(text: &str) -> bool {
    text.starts_with("//") || (text.starts_with('#') && !text.starts_with("#["))
}

fn split_inner(source: &str, names: &HashSet<String>) -> Vec<Item> {
    let mut names = names.clone();
    let scan = scan(source);
    let mut out: Vec<Item> = Vec::new();
    let mut run: Option<Item> = None;
    let mut joinable = false;

    for seg in &scan.segments {
        let raw = &source[seg.start..seg.end];
        let text = raw.trim();
        if text.is_empty() || is_comment_segment(text) {
            joinable = false;
            continue;
        }
        let start = seg.start + (raw.len() - raw.trim_start().len());
        let end = start + text.len();
        let code_end = start + source[start..seg.code_end.max(start)].trim_end().len();
        let info = SegInfo { start, code_end, semi: seg.ended_by_semicolon };
        if classify(text, &names) == ReplKind::Command {
            match run.as_mut() {
                Some(item) if joinable => {
                    item.end = end;
                    item.segs.push(info);
                }
                _ => {
                    out.extend(run.take());
                    run = Some(Item {
                        kind: ReplKind::Command,
                        start,
                        end,
                        segs: vec![info],
                        scope: None,
                    });
                }
            }
            joinable = seg.ended_by_semicolon;
        } else {
            out.extend(run.take());
            joinable = false;
            record_declaration(text, &mut names);
            let scope = is_control(text).then(|| names.clone());
            out.push(Item { kind: ReplKind::Spar, start, end, segs: vec![info], scope });
        }
    }
    out.extend(run.take());
    out
}

const DECLARATIONS: &[&str] = &[
    "var", "const", "fn", "function", "functionGroup", "struct", "impl", "type", "enum",
];
const MODIFIERS: &[&str] = &["export", "private", "dynamic", "async"];

fn first_word(text: &str) -> &str {
    let end = text
        .char_indices()
        .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '_'))
        .map_or(text.len(), |(index, _)| index);
    &text[..end]
}

fn is_declaration_keyword(word: &str) -> bool {
    DECLARATIONS.contains(&word) || MODIFIERS.contains(&word)
}

fn is_control(text: &str) -> bool {
    matches!(first_word(text), "if" | "for" | "while" | "loop" | "try")
}

fn classify(text: &str, names: &HashSet<String>) -> ReplKind {
    if is_spar(text, names) {
        ReplKind::Spar
    } else {
        ReplKind::Command
    }
}

fn contains_outside_quotes(text: &str, needle: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if text[index..].starts_with(needle) {
            return true;
        }
    }
    false
}

fn is_spar(text: &str, names: &HashSet<String>) -> bool {
    let word = first_word(text);
    let rest = text[word.len()..].trim_start();

    // `~` followed by whitespace is the shell-statement marker; `~/x`,
    // `~user` and a lone `~` are home-directory words.
    if let Some(after) = text.strip_prefix('~') {
        return after.starts_with(char::is_whitespace) && !after.trim().is_empty();
    }
    if text.starts_with("#[") {
        return true;
    }

    match word {
        "" => {}
        "if" | "for" | "while" | "loop" | "try" | "return" | "break" | "continue" | "await"
        | "__shell" => return true,
        "var" | "const" | "fn" | "function" | "functionGroup" | "struct" | "impl" | "enum" => {
            return true
        }
        // `type ls` is a command; `type A = int` declares an alias.
        "type" => {
            let name = first_word(rest);
            let after = rest[name.len()..].trim_start();
            if !name.is_empty() && (after.starts_with('=') || after.starts_with('<')) {
                return true;
            }
        }
        // `export FOO=bar` is the shell builtin; `export var x` is Spar.
        "export" | "private" | "dynamic" | "async" => {
            if is_declaration_keyword(first_word(rest)) {
                return true;
            }
        }
        "import" => {
            if rest.starts_with('"') || rest.starts_with("schema") {
                return true;
            }
        }
        "exec" => {
            return rest.starts_with('{') || crate::lexer::starts_with_keyword(rest, "__shell");
        }
        "_" => {
            return rest.is_empty() || text[1..].starts_with(['.', '[', '(', ' ']);
        }
        _ => {}
    }

    let first = text.chars().next().unwrap_or(' ');
    if first.is_ascii_digit() {
        // `7z x f` is a command; `1 + 2`, `1.5`, `0x10`, `1e5` are numbers.
        let digits = text.trim_start_matches(|c: char| c.is_ascii_digit());
        let next = digits.chars().next();
        let numeric_suffix = match next {
            Some('x' | 'b' | 'o') => text.starts_with('0'),
            Some('e' | 'E') => digits[1..].starts_with(|c: char| c.is_ascii_digit() || c == '+' || c == '-'),
            Some(c) => !(c.is_ascii_alphabetic() || c == '_'),
            None => true,
        };
        return numeric_suffix;
    }
    if matches!(first, '(' | '{' | '"' | '\'') {
        return true;
    }
    if first == '[' && !text[1..].starts_with(|c: char| c.is_whitespace() || c == '[') {
        return true;
    }
    if contains_outside_quotes(text, "|>") {
        return true;
    }
    if !word.is_empty() && names.contains(word) {
        let next = text[word.len()..].chars().next();
        if !matches!(next, Some('-') | Some('/')) {
            return true;
        }
    }
    crate::lexer::is_spar_shell_statement_start(text)
}

/// Remembers names a Spar statement declares so later statements in the
/// same submission see them.
fn record_declaration(text: &str, names: &mut HashSet<String>) {
    let mut rest = text;
    loop {
        let word = first_word(rest);
        if MODIFIERS.contains(&word) {
            rest = rest[word.len()..].trim_start();
            continue;
        }
        if matches!(
            word,
            "var" | "const" | "fn" | "function" | "functionGroup" | "struct" | "enum" | "type"
        ) {
            let mut after = rest[word.len()..].trim_start();
            if matches!(word, "var" | "const") && crate::lexer::starts_with_keyword(after, "mut") {
                after = after["mut".len()..].trim_start();
            }
            let name = first_word(after);
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
        break;
    }
}

fn identifiers_in(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty() && *w != "mut" && !w.starts_with(|c: char| c.is_ascii_digit()))
}

/// Names a block header binds for its interior: `for i in`, `for (a, b) in`
/// and `catch e`.
fn header_binders(header: &str, names: &mut HashSet<String>) {
    let h = header.trim_start().trim_start_matches('}').trim_start();
    if crate::lexer::starts_with_keyword(h, "for") {
        let after = &h[3..];
        let pattern = after.split(" in ").next().unwrap_or(after);
        names.extend(identifiers_in(pattern).map(str::to_string));
    } else if crate::lexer::starts_with_keyword(h, "catch") {
        if let Some(name) = identifiers_in(&h[5..]).next() {
            names.insert(name.to_string());
        }
    }
}

/// Collects the `~ ` and `;` insertions that turn the bare commands inside a
/// control-flow statement's blocks into marker statements. `base` is the
/// offset of `text` in the final statement text.
fn spar_insertions(
    text: &str,
    names: &HashSet<String>,
    base: usize,
    out: &mut Vec<(usize, &'static str)>,
) {
    if !is_control(text) {
        return;
    }
    let mut header_start = 0;
    for (open, close) in scan(text).blocks {
        let mut inner = names.clone();
        header_binders(&text[header_start..open], &mut inner);
        header_start = close + 1;
        let interior = &text[open + 1..close];
        let interior_base = base + open + 1;
        for item in split_inner(interior, &inner) {
            match item.kind {
                ReplKind::Command => {
                    for seg in &item.segs {
                        out.push((interior_base + seg.start, "~ "));
                        if !seg.semi {
                            out.push((interior_base + seg.code_end, ";"));
                        }
                    }
                }
                ReplKind::Spar => {
                    if let Some(scope) = &item.scope {
                        spar_insertions(
                            &interior[item.start..item.end],
                            scope,
                            interior_base + item.start,
                            out,
                        );
                    }
                    let seg = &item.segs[0];
                    let code = &interior[item.start..seg.code_end];
                    if !seg.semi && !ends_with_block_statement(code) {
                        out.push((interior_base + seg.code_end, ";"));
                    }
                }
            }
        }
    }
}

/// Control-flow and declaration blocks end at `}` without a terminator;
/// anything else (including `var m = { a: 1 }`) needs a `;`.
fn ends_with_block_statement(code: &str) -> bool {
    if !code.ends_with('}') {
        return false;
    }
    let mut rest = code;
    loop {
        let word = first_word(rest);
        if MODIFIERS.contains(&word) {
            rest = rest[word.len()..].trim_start();
            continue;
        }
        return matches!(
            word,
            "if" | "for" | "while" | "loop" | "try" | "fn" | "function" | "functionGroup"
                | "struct" | "impl" | "enum"
        );
    }
}

fn apply_insertions(
    original: &str,
    mut insertions: Vec<(usize, &'static str)>,
) -> (String, Vec<(usize, usize)>) {
    insertions.sort_by_key(|(pos, _)| *pos);
    let mut text = String::with_capacity(original.len() + insertions.len() * 2);
    let mut ranges = Vec::with_capacity(insertions.len());
    let mut last = 0;
    for (pos, ins) in insertions {
        text.push_str(&original[last..pos]);
        ranges.push((text.len(), text.len() + ins.len()));
        text.push_str(ins);
        last = pos;
    }
    text.push_str(&original[last..]);
    (text, ranges)
}

struct Segment {
    start: usize,
    end: usize,
    code_end: usize,
    ended_by_semicolon: bool,
}

struct Scan {
    segments: Vec<Segment>,
    /// Byte offsets of each top-level `{` and its matching `}`.
    blocks: Vec<(usize, usize)>,
}

enum Ctx {
    Dq,
    Sq,
    Interp(u32),
}

fn continues_on_next_line(src: &str, seg_start: usize, newline: usize) -> bool {
    let before = src[seg_start..newline].trim_end();
    if ["&&", "||", "|", "|>", ","].iter().any(|op| before.ends_with(op)) {
        return true;
    }
    let after = src[newline + 1..].trim_start();
    crate::lexer::starts_with_keyword(after, "else")
        || crate::lexer::starts_with_keyword(after, "catch")
        || after.starts_with("|>")
}

fn scan(src: &str) -> Scan {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut stack: Vec<Ctx> = Vec::new();
    let mut nest = 0_u32; // parens and brackets
    let mut brace = 0_u32;
    let mut open_at: Option<usize> = None;
    let mut seg_start = 0;
    let mut comment_at: Option<usize> = None;
    let mut segments = Vec::new();
    let mut blocks = Vec::new();
    let mut i = 0;
    // Skips the char after a backslash; `\r\n` goes as a unit.
    let skip_escaped = |i: &mut usize| {
        if chars.get(*i + 1).map(|c| c.1) == Some('\r') && chars.get(*i + 2).map(|c| c.1) == Some('\n') {
            *i += 2;
        } else {
            *i += 1;
        }
    };
    while i < chars.len() {
        let (idx, ch) = chars[i];
        let next = chars.get(i + 1).map(|(_, c)| *c);
        let prev_ws = i == 0 || chars[i - 1].1.is_whitespace();
        match stack.last_mut() {
            Some(Ctx::Dq) => match ch {
                '\\' => skip_escaped(&mut i),
                '"' => {
                    stack.pop();
                }
                '$' if next == Some('{') => {
                    stack.push(Ctx::Interp(0));
                    i += 1;
                }
                _ => {}
            },
            Some(Ctx::Sq) => {
                if ch == '\'' {
                    stack.pop();
                }
            }
            _ => match ch {
                '\\' => skip_escaped(&mut i),
                '"' => stack.push(Ctx::Dq),
                '\'' => stack.push(Ctx::Sq),
                '$' if next == Some('{') => {
                    stack.push(Ctx::Interp(0));
                    i += 1;
                }
                '/' if next == Some('/') && (prev_ws || chars[i - 1].1 == ';') => {
                    comment_at.get_or_insert(idx);
                    while chars.get(i + 1).is_some_and(|(_, c)| *c != '\n') {
                        i += 1;
                    }
                }
                '#' if prev_ws && next != Some('[') => {
                    comment_at.get_or_insert(idx);
                    while chars.get(i + 1).is_some_and(|(_, c)| *c != '\n') {
                        i += 1;
                    }
                }
                '(' | '[' => nest += 1,
                ')' | ']' => nest = nest.saturating_sub(1),
                '{' => {
                    if let Some(Ctx::Interp(depth)) = stack.last_mut() {
                        *depth += 1;
                    } else {
                        if brace == 0 && nest == 0 {
                            open_at = Some(idx);
                        }
                        brace += 1;
                    }
                }
                '}' => {
                    if let Some(Ctx::Interp(depth)) = stack.last_mut() {
                        if *depth == 0 {
                            stack.pop();
                        } else {
                            *depth -= 1;
                        }
                    } else {
                        brace = brace.saturating_sub(1);
                        if brace == 0 {
                            if let Some(open) = open_at.take() {
                                blocks.push((open, idx));
                            }
                        }
                    }
                }
                ';' if stack.is_empty() && nest == 0 && brace == 0 => {
                    segments.push(Segment {
                        start: seg_start,
                        end: idx,
                        code_end: comment_at.take().unwrap_or(idx),
                        ended_by_semicolon: true,
                    });
                    seg_start = idx + 1;
                }
                '\n' if stack.is_empty()
                    && nest == 0
                    && brace == 0
                    && !continues_on_next_line(src, seg_start, idx) =>
                {
                    segments.push(Segment {
                        start: seg_start,
                        end: idx,
                        code_end: comment_at.take().unwrap_or(idx),
                        ended_by_semicolon: false,
                    });
                    seg_start = idx + 1;
                }
                _ => {}
            },
        }
        i += 1;
    }
    if seg_start < src.len() {
        segments.push(Segment {
            start: seg_start,
            end: src.len(),
            code_end: comment_at.take().unwrap_or(src.len()),
            ended_by_semicolon: false,
        });
    }
    Scan { segments, blocks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn names(list: &[&str]) -> HashSet<String> { list.iter().map(|s| s.to_string()).collect() }
    fn kinds(src: &str, n: &[&str]) -> Vec<(ReplKind, String)> {
        split_statements(src, &names(n)).into_iter().map(|s| (s.kind, s.text)).collect()
    }

    #[test]
    fn mixes_command_and_spar_on_one_line() {
        assert_eq!(
            kinds("echo hi; var a: int = 2; a + 1", &[]),
            vec![
                (ReplKind::Command, "echo hi".into()),
                (ReplKind::Spar, "var a: int = 2".into()),
                (ReplKind::Spar, "a + 1".into()),
            ]
        );
    }

    #[test]
    fn adjacent_commands_stay_one_statement() {
        assert_eq!(
            kinds("echo a && echo b; ls | grep x", &[]),
            vec![(ReplKind::Command, "echo a && echo b; ls | grep x".into())]
        );
    }

    #[test]
    fn existing_names_win_over_commands() {
        assert_eq!(kinds("ls", &["ls"]), vec![(ReplKind::Spar, "ls".into())]);
        assert_eq!(kinds("ls", &[]), vec![(ReplKind::Command, "ls".into())]);
    }

    #[test]
    fn bare_commands_inside_blocks_get_the_marker() {
        let out = kinds("if true { echo yes }", &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, ReplKind::Spar);
        assert!(out[0].1.contains("~ echo yes;"), "{}", out[0].1);
    }

    #[test]
    fn blocks_keep_spar_statements_and_mark_only_commands() {
        let out = kinds("for i in [1, 2] { echo ${i}\n var x: int = i }", &[]);
        assert!(out[0].1.contains("~ echo ${i};") && out[0].1.contains("var x: int = i"), "{}", out[0].1);
    }

    #[test]
    fn explicit_marker_and_underscore_and_pipelines_are_spar() {
        assert_eq!(kinds("~ ls", &[])[0].0, ReplKind::Spar);
        assert_eq!(kinds("_.name", &[])[0].0, ReplKind::Spar);
        assert_eq!(kinds("ls |> where(f: true)", &[])[0].0, ReplKind::Spar);
    }

    #[test]
    fn separators_inside_quotes_and_braces_do_not_split() {
        assert_eq!(kinds("echo \"a; b\"; var x: int = 1", &[]).len(), 2);
        assert_eq!(kinds("var m = { a: 1, b: 2 }; m.a", &[]).len(), 2);
    }

    #[test]
    fn declarations_earlier_in_the_submission_shadow_commands() {
        assert_eq!(
            kinds("var ls = [1]; ls", &[]),
            vec![(ReplKind::Spar, "var ls = [1]".into()), (ReplKind::Spar, "ls".into())]
        );
    }

    #[test]
    fn tilde_paths_are_commands_not_markers() {
        assert_eq!(kinds("cd ~", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("ls ~/projects", &[])[0].0, ReplKind::Command);
    }

    #[test]
    fn multi_line_input_splits_on_newlines() {
        assert_eq!(kinds("echo a\nvar x: int = 1\nx", &[]).len(), 3);
    }

    #[test]
    fn interpolation_and_quoted_semicolons_do_not_split() {
        assert_eq!(kinds("echo \"a ${m[\"k;\"]} b\"; var y: int = 1", &[]).len(), 2);
        assert_eq!(kinds("echo 'a;b'", &[]), vec![(ReplKind::Command, "echo 'a;b'".into())]);
        assert_eq!(kinds("echo ${x;y}", &[]).len(), 1);
    }

    #[test]
    fn shell_command_and_export_are_ordinary_words() {
        assert_eq!(kinds("shell ls", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("command -v ls", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("export FOO=bar", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("type ls", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("__shell { ls }", &[])[0].0, ReplKind::Spar);
        assert_eq!(kinds("export var a: int = 1", &[])[0].0, ReplKind::Spar);
        assert_eq!(kinds("type A = int", &[])[0].0, ReplKind::Spar);
    }

    #[test]
    fn exec_forms() {
        assert_eq!(kinds("exec printf ok", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("exec { ls }", &[])[0].0, ReplKind::Spar);
        assert_eq!(kinds("exec __shell { ls }", &[])[0].0, ReplKind::Spar);
    }

    #[test]
    fn else_branch_stays_with_its_if_and_nested_blocks_are_marked() {
        let out = kinds("if a {\n echo x\n} else {\n echo y\n}", &["a"]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].1.contains("~ echo x;") && out[0].1.contains("~ echo y;"), "{}", out[0].1);
        let out = kinds("if a { if b { echo z } }", &["a", "b"]);
        assert!(out[0].1.contains("~ echo z;"), "{}", out[0].1);
    }

    #[test]
    fn function_bodies_are_not_rewritten() {
        let out = kinds("function f() -> int { return 1; }", &[]);
        assert_eq!(out, vec![(ReplKind::Spar, "function f() -> int { return 1; }".into())]);
    }

    #[test]
    fn shadowing_name_inside_block_is_spar() {
        let out = kinds("if true { var ls = 1\n ls }", &[]);
        assert!(!out[0].1.contains("~ ls"), "{}", out[0].1);
    }

    #[test]
    fn rewritten_blocks_run_through_a_session() {
        let mut session = crate::Engine::default().session();
        let out = kinds("if true { echo yes }", &[]);
        session.eval(&out[0].1).expect("block with marker evaluates");
        let out = kinds("for i in [1, 2] { echo n${i} }", &[]);
        session.eval(&out[0].1).expect("loop with marker evaluates");
    }

    #[test]
    fn rewritten_block_commands_actually_run() {
        let path = std::env::temp_dir().join(format!("spar_repl_split_{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut session = crate::Engine::default().session();
        let out = kinds(&format!("if true {{ touch {} }}", path.display()), &[]);
        session.eval(&out[0].1).expect("evaluates");
        let ran = path.exists();
        let _ = std::fs::remove_file(&path);
        assert!(ran, "marked command did not run: {}", out[0].1);
    }

    // ---- fix round 1 ----

    fn strip_inserted(st: &ReplStatement) -> String {
        let mut out = String::new();
        let mut at = 0;
        for &(a, b) in &st.inserted {
            out.push_str(&st.text[at..a]);
            at = b;
        }
        out.push_str(&st.text[at..]);
        out
    }

    fn first(src: &str, n: &[&str]) -> ReplStatement {
        split_statements(src, &names(n)).remove(0)
    }

    #[test]
    fn rewrite_is_insert_only() {
        let src = "for i in [1, 2] {\n  echo ${i} // c\n  var x: int = i\n  if i > 1 { ls -l; pwd }\n}";
        let st = first(src, &[]);
        assert!(!st.inserted.is_empty());
        assert_eq!(strip_inserted(&st), src);
        for &(a, b) in &st.inserted {
            assert!(matches!(&st.text[a..b], "~ " | ";"), "{:?}", &st.text[a..b]);
        }
        assert!(st.text.contains("~ ls -l;") && st.text.contains("~ pwd;"), "{}", st.text);
    }

    #[test]
    fn terminator_after_closing_brace_value_and_before_comment() {
        let st = first("if x { var m = { a: 1 }\n m.a }", &["x"]);
        assert!(st.text.contains("var m = { a: 1 };"), "{}", st.text);
        let st = first("if x {\n var y = 1 // c\n}", &["x"]);
        assert!(st.text.contains("var y = 1; // c"), "{}", st.text);
        let st = first("if x { if y { echo a } }", &["x", "y"]);
        assert!(!st.text.contains("};"), "{}", st.text);
    }

    #[test]
    fn loop_and_catch_binders_stay_spar_in_blocks() {
        let st = first("for p in xs { p * 2 }", &["xs"]);
        assert!(!st.text.contains("~ p"), "{}", st.text);
        let st = first("for (a, b) in xs { a + b }", &["xs"]);
        assert!(!st.text.contains("~ a"), "{}", st.text);
        let st = first("try { f() } catch e { e }", &["f"]);
        assert!(!st.text.contains("~ e"), "{}", st.text);
    }

    #[test]
    fn declarations_with_modifiers_shadow_commands() {
        for decl in ["var mut ls = 1", "export var ls = 1", "private const ls = 1", "async fn ls() {}", "export function ls() {}"] {
            let out = kinds(&format!("{decl}; ls"), &[]);
            assert_eq!(out.last().unwrap().0, ReplKind::Spar, "{decl}");
            assert_eq!(out.len(), 2, "{decl}");
        }
    }

    #[test]
    fn scope_names_from_fresh_session_do_not_shadow_commands() {
        let mut session = crate::Engine::default().session();
        let n = session.scope_names();
        for cmd in ["sleep 1", "env", "read x", "which ls", "exit", "print hi"] {
            let out = split_statements(cmd, &n);
            assert_eq!(out[0].kind, ReplKind::Command, "{cmd}");
        }
        session.eval("var env: int = 1;").unwrap();
        let n = session.scope_names();
        assert_eq!(split_statements("env", &n)[0].kind, ReplKind::Spar);
    }

    #[test]
    fn comment_and_quote_edge_cases() {
        // attribute survives; mid-line comment apostrophe does not swallow
        assert_eq!(kinds("#[emit]\nvar x: int = 1", &[])[0].1, "#[emit]");
        assert_eq!(kinds("echo hi # don't\nvar x: int = 1", &[]).len(), 2);
        // CRLF with backslash continuation stays one command
        assert_eq!(kinds("echo a \\\r\n b\r\nvar x: int = 1", &[]).len(), 2);
        // a comment segment between two commands separated by newline: two statements
        assert_eq!(kinds("echo a;\n// c\necho b", &[]).len(), 2);
        // comment containing ; and {
        assert_eq!(kinds("echo hi // a; b {\nvar x: int = 1", &[]).len(), 2);
        assert_eq!(kinds("ls # a; {\nvar x: int = 1", &[]).len(), 2);
    }

    #[test]
    fn classification_edge_cases() {
        assert_eq!(kinds("docker-compose up", &["docker"])[0].0, ReplKind::Command);
        assert_eq!(kinds("ls/foo", &["ls"])[0].0, ReplKind::Command);
        assert_eq!(kinds("echo \"a |> b\"", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("[[ -f x ]]", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("[ -f x ]", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("7z x f", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("1 + 2", &[])[0].0, ReplKind::Spar);
        assert_eq!(kinds("[1, 2]", &[])[0].0, ReplKind::Spar);
        assert_eq!(kinds("$(ls; pwd)", &[]).len(), 1);
        assert_eq!(kinds("a=1", &[])[0].0, ReplKind::Command);
        assert_eq!(kinds("FOO=bar cmd", &[])[0].0, ReplKind::Command);
    }
}
