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
}

/// Splits a REPL submission into statements. `names` are the variable,
/// function and parameter names visible in the session.
pub fn split_statements(source: &str, names: &HashSet<String>) -> Vec<ReplStatement> {
    let mut names = names.clone();
    let scan = scan(source);
    let mut out: Vec<ReplStatement> = Vec::new();
    // Start/end of the Command run being accumulated, and whether the
    // previous segment ended on a plain `;` (so the next one may join it).
    let mut run: Option<(usize, usize)> = None;
    let mut joinable = false;

    let flush = |run: &mut Option<(usize, usize)>, out: &mut Vec<ReplStatement>| {
        if let Some((start, end)) = run.take() {
            out.push(ReplStatement {
                text: source[start..end].trim().to_string(),
                kind: ReplKind::Command,
            });
        }
    };

    for seg in &scan.segments {
        let raw = &source[seg.start..seg.end];
        let text = raw.trim();
        if text.is_empty() || text.starts_with("//") || text.starts_with('#') {
            continue;
        }
        let lead = raw.len() - raw.trim_start().len();
        let start = seg.start + lead;
        let end = start + text.len();
        if classify(text, &names) == ReplKind::Command {
            match run {
                Some((_, ref mut run_end)) if joinable => *run_end = end,
                _ => {
                    flush(&mut run, &mut out);
                    run = Some((start, end));
                }
            }
            joinable = seg.ended_by_semicolon;
        } else {
            flush(&mut run, &mut out);
            joinable = false;
            record_declaration(text, &mut names);
            out.push(ReplStatement {
                text: rewrite_blocks(text, &names),
                kind: ReplKind::Spar,
            });
        }
    }
    flush(&mut run, &mut out);
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

fn classify(text: &str, names: &HashSet<String>) -> ReplKind {
    if is_spar(text, names) {
        ReplKind::Spar
    } else {
        ReplKind::Command
    }
}

fn is_spar(text: &str, names: &HashSet<String>) -> bool {
    let word = first_word(text);
    let rest = text[word.len()..].trim_start();

    // `~` followed by whitespace is the shell-statement marker; `~/x`,
    // `~user` and a lone `~` are home-directory words.
    if let Some(after) = text.strip_prefix('~') {
        return after.starts_with(char::is_whitespace) && !after.trim().is_empty();
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
            return rest.is_empty()
                || text[1..].starts_with(['.', '[', '(', ' '])
                || text.len() == 1;
        }
        _ => {}
    }

    let first = text.chars().next().unwrap_or(' ');
    if first.is_ascii_digit() || matches!(first, '(' | '{' | '"' | '\'') {
        return true;
    }
    if first == '[' && !text[1..].starts_with(char::is_whitespace) {
        return true;
    }
    if text.contains("|>") {
        return true;
    }
    if !word.is_empty() && names.contains(word) {
        return true;
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
            let after = rest[word.len()..].trim_start();
            let name = first_word(after);
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
        break;
    }
}

/// Rewrites bare commands inside the `{ }` blocks of a control-flow
/// statement into `~ cmd;`. Declarations (function bodies and the like) and
/// statements without blocks are returned verbatim.
fn rewrite_blocks(text: &str, names: &HashSet<String>) -> String {
    if !matches!(first_word(text), "if" | "for" | "while" | "loop" | "try") {
        return text.to_string();
    }
    let blocks = scan(text).blocks;
    if blocks.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut last = 0;
    for (open, close) in blocks {
        out.push_str(&text[last..=open]);
        out.push_str(&rewrite_interior(&text[open + 1..close], names));
        last = close;
    }
    out.push_str(&text[last..]);
    out
}

fn rewrite_interior(interior: &str, names: &HashSet<String>) -> String {
    let statements = split_statements(interior, names);
    if statements.is_empty() {
        return interior.to_string();
    }
    let multiline = interior.contains('\n');
    let sep = if multiline { "\n" } else { " " };
    let mut parts: Vec<String> = Vec::new();
    for statement in statements {
        match statement.kind {
            ReplKind::Command => {
                let (marked, _) =
                    crate::lexer::normalize_shell_body_tracked(&format!("{};", statement.text));
                parts.push(marked.trim().to_string());
            }
            ReplKind::Spar => {
                let text = statement.text;
                if text.ends_with(';') || text.ends_with('}') {
                    parts.push(text);
                } else {
                    parts.push(format!("{text};"));
                }
            }
        }
    }
    format!("{sep}{}{sep}", parts.join(sep))
}

struct Segment {
    start: usize,
    end: usize,
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
    let mut segments = Vec::new();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (idx, ch) = chars[i];
        let next = chars.get(i + 1).map(|(_, c)| *c);
        match stack.last_mut() {
            Some(Ctx::Dq) => match ch {
                '\\' => i += 1,
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
                '\\' => i += 1,
                '"' => stack.push(Ctx::Dq),
                '\'' => stack.push(Ctx::Sq),
                '$' if next == Some('{') => {
                    stack.push(Ctx::Interp(0));
                    i += 1;
                }
                '/' if next == Some('/')
                    && (i == 0 || chars[i - 1].1.is_whitespace() || chars[i - 1].1 == ';') =>
                {
                    while chars.get(i + 1).is_some_and(|(_, c)| *c != '\n') {
                        i += 1;
                    }
                }
                '#' if src[..idx].rsplit('\n').next().is_some_and(|l| l.trim().is_empty()) => {
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
                    segments.push(Segment { start: seg_start, end: idx, ended_by_semicolon: true });
                    seg_start = idx + 1;
                }
                '\n' if stack.is_empty()
                    && nest == 0
                    && brace == 0
                    && !continues_on_next_line(src, seg_start, idx) =>
                {
                    segments.push(Segment { start: seg_start, end: idx, ended_by_semicolon: false });
                    seg_start = idx + 1;
                }
                _ => {}
            },
        }
        i += 1;
    }
    if seg_start < src.len() {
        segments.push(Segment { start: seg_start, end: src.len(), ended_by_semicolon: false });
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
}
