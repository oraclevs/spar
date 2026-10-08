use super::*;
use crate::ast::{SparType, TopLevelItem};
use crate::compiler::{CompileOptions, Compiler};
use crate::loader::ImportLoader;
use crate::resolver::{FunctionEntry, GlobalEntry};
use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

/// Import resolution at the prompt must not stall the editor.
const BUDGET: Duration = Duration::from_millis(500);

/// Names `target` offers to an `import { }`, sorted by name.
///
/// Candidates are the declarations physically owned by the resolved file:
/// `exported` vars, structs, enums and types, plus non-private functions and
/// function groups. The compiler's symbol table also holds prelude and
/// transitively spliced symbols, so it is used for metadata only.
/// `type_only` keeps structs, enums and types.
pub fn exports_of(
    target: &ImportTarget,
    base_dir: &Path,
    type_only: bool,
    already: &HashSet<String>,
) -> Result<Vec<ExportItem>, IntelError> {
    exports_with_budget(target, base_dir, type_only, already, BUDGET)
}

pub(crate) fn exports_with_budget(
    target: &ImportTarget,
    base_dir: &Path,
    type_only: bool,
    already: &HashSet<String>,
    budget: Duration,
) -> Result<Vec<ExportItem>, IntelError> {
    let started = Instant::now();
    let check = || {
        if started.elapsed() >= budget {
            Err(IntelError::Timeout)
        } else {
            Ok(())
        }
    };
    check()?;

    let (path, package) = match target {
        ImportTarget::File(path) => (path.as_str(), false),
        ImportTarget::Package(path) => (path.as_str(), true),
        ImportTarget::Missing => return Err(IntelError::NotFound("no import target".into())),
    };

    let mut loader = ImportLoader::new(base_dir);
    if let Some(locator) = package_locator(base_dir) {
        loader = loader.with_locator(locator);
    }
    let decl = crate::ast::ImportDecl {
        path: path.to_string(),
        package,
        kind: crate::ast::ImportKind::Selective(Vec::new()),
        span: crate::error::Span::dummy(),
    };
    let resolved = loader
        .resolve_import(&decl)
        .map_err(|error| IntelError::NotFound(error.to_string()))?;
    check()?;

    let bytes = std::fs::read(&resolved.path)
        .map_err(|error| IntelError::NotFound(format!("{}: {error}", resolved.path.display())))?;
    let source = String::from_utf8(bytes)
        .map_err(|_| IntelError::Parse(format!("{} is not valid UTF-8", resolved.path.display())))?;
    check()?;

    let raw = parse_with_statement_repair(&source)
        .ok_or_else(|| IntelError::Parse(format!("{} does not parse", resolved.path.display())))?;
    check()?;

    let mut options = CompileOptions::for_path(&resolved.path);
    options.evaluate = false;
    options.locator = resolved.locator;
    let compilation = Compiler::new(options).compile(&source);
    check()?;
    // Metadata only: a file that fails to compile still lists its names.
    let symbols = compilation.symbols;

    let mut items = Vec::new();
    let mut push = |name: &str,
                    kind: ExportKind,
                    detail: Option<String>,
                    span: &crate::error::Span,
                    decl_start: usize| {
        if already.contains(name) {
            return;
        }
        items.push(ExportItem {
            name: name.to_string(),
            kind,
            detail,
            doc: leading_documentation(&source, decl_start),
            span: span.clone(),
            file: resolved.path.clone(),
        });
    };

    for item in &raw.items {
        check()?;
        match item {
            TopLevelItem::Var(decl) if decl.exported && !type_only => {
                let entry = symbols.as_ref().and_then(|s| s.globals.get(&decl.name));
                let detail = match entry {
                    Some(GlobalEntry::Var { ty, .. }) => Some(crate::typechecker::display_type(ty)),
                    _ => None,
                };
                let callable = matches!(
                    entry,
                    Some(GlobalEntry::Var { ty: SparType::Function { .. }, .. })
                );
                let kind = if callable { ExportKind::Function } else { ExportKind::Variable };
                push(&decl.name, kind, detail, &decl.span, decl.span.start);
            }
            TopLevelItem::Struct(decl) if decl.exported && !decl.private => {
                push(&decl.name, ExportKind::Struct, Some("struct".into()), &decl.span, decl.span.start);
            }
            TopLevelItem::Function(decl) if !decl.is_private && !type_only => {
                match symbols.as_ref().and_then(|s| s.functions.get(&decl.name)) {
                    Some(entry) => {
                        let label = signature_label(&decl.name, entry, Some(decl));
                        push(&decl.name, ExportKind::Function, Some(label), &entry.span, decl.span.start);
                    }
                    None => push(&decl.name, ExportKind::Function, None, &decl.span, decl.span.start),
                }
            }
            TopLevelItem::Type(decl) if decl.exported => {
                let span = symbols
                    .as_ref()
                    .and_then(|s| s.types.get(&decl.name))
                    .map(|entry| &entry.span)
                    .unwrap_or(&decl.span);
                push(&decl.name, ExportKind::Type, Some("type".into()), span, decl.span.start);
            }
            TopLevelItem::Enum(decl) if decl.exported => {
                let span = symbols
                    .as_ref()
                    .and_then(|s| s.enums.get(&decl.name))
                    .map(|entry| &entry.span)
                    .unwrap_or(&decl.span);
                push(&decl.name, ExportKind::Enum, Some("enum".into()), span, decl.span.start);
            }
            TopLevelItem::FunctionGroup(decl) if !decl.is_private && !type_only => {
                let span = symbols
                    .as_ref()
                    .and_then(|s| s.function_groups.get(&decl.name))
                    .map(|entry| &entry.span)
                    .unwrap_or(&decl.span);
                push(&decl.name, ExportKind::Group, Some("function group".into()), span, decl.span.start);
            }
            _ => {}
        }
    }

    items.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(items)
}

/// The package locator for `base_dir`'s project, if it has a manifest and lockfile.
/// Static only: nothing is loaded or executed.
fn package_locator(base_dir: &Path) -> Option<crate::package::ModuleLocator> {
    let project_dir = base_dir
        .ancestors()
        .find(|directory| directory.join(crate::package::PACKAGE_MANIFEST_FILE).is_file())?;
    let lockfile =
        crate::package::Lockfile::read(&project_dir.join(crate::package::PACKAGE_LOCK_FILE)).ok()?;
    let store = crate::package::PackageStore::new(crate::package::StorePaths::from_env());
    Some(crate::package::ModuleLocator::for_root(lockfile, store))
}

fn signature_label(
    name: &str,
    entry: &FunctionEntry,
    raw_decl: Option<&crate::ast::FunctionDecl>,
) -> String {
    let params = entry
        .params
        .iter()
        .map(|(param_name, ty)| {
            let mut rendered = format!("{param_name}: {}", crate::typechecker::display_type(ty));
            if entry.default_params.contains(param_name) {
                let default = raw_decl
                    .and_then(|decl| decl.params.iter().find(|p| &p.name == param_name))
                    .and_then(|p| p.default.as_ref())
                    .map(crate::formatter::format_expression);
                rendered.push_str(" = ");
                rendered.push_str(default.as_deref().unwrap_or("…"));
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(", ");
    let prefix = if entry.is_async { "async " } else { "" };
    format!(
        "{prefix}{name}({params}) -> {}",
        crate::typechecker::display_type(&entry.ret)
    )
}

// ── Parsing with statement repair ───────────────────────────────────────────

const REPAIR_ATTEMPTS: usize = 48;

/// Parse `source`; if it does not, blank the statement the parser complains
/// about and retry a few times. Offsets never move.
fn parse_with_statement_repair(source: &str) -> Option<crate::ast::Program> {
    use crate::error::SparError;
    let mut text = source.to_string();
    for _ in 0..REPAIR_ATTEMPTS {
        let (is_lex, at) = match crate::lexer::Lexer::new(&text).tokenize() {
            Ok(tokens) => match crate::parser::Parser::new(tokens).parse() {
                Ok(program) => return Some(program),
                Err(SparError::ParseError { span, .. }) => (false, span.start),
                Err(SparError::LexError { span, .. }) => (true, span.start),
                Err(_) => return None,
            },
            Err(SparError::LexError { span, .. }) => (true, span.start),
            Err(SparError::ParseError { span, .. }) => (false, span.start),
            Err(_) => return None,
        };
        let repaired = if is_lex { blank_line_around(&text, at) } else { blank_statement_around(&text, at) };
        if repaired == text {
            return None;
        }
        text = repaired;
    }
    None
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
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

fn blank_statement_around(text: &str, at: usize) -> String {
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

fn blank_line_around(text: &str, at: usize) -> String {
    let at = floor_boundary(text, at);
    let start = text[..at].rfind('\n').map_or(0, |index| index + 1);
    let end = text[at..].find('\n').map_or(text.len(), |index| at + index);
    blank_range(text, start, end)
}

// ── Leading documentation ───────────────────────────────────────────────────

/// Only a `///` line (or `/** ... */` block) counts as documentation.
fn normalize_documentation_comment(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if let Some(line) = trimmed.strip_prefix("///") {
        return Some(line.trim_start().to_string());
    }
    if let Some(block) = trimmed.strip_prefix("/**").and_then(|value| value.strip_suffix("*/")) {
        return Some(
            block
                .lines()
                .map(|line| line.trim().trim_start_matches('*').trim_start())
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string(),
        );
    }
    None
}

fn documentation_cursor(source: &str, declaration_start: usize) -> usize {
    let declaration_start = floor_boundary(source, declaration_start);
    let line_start = source[..declaration_start].rfind('\n').map_or(0, |index| index + 1);
    let prefix = source[line_start..declaration_start].trim();
    if prefix.is_empty() {
        return line_start;
    }
    let allowed = [
        "export", "private", "async", "struct", "fn", "function", "var", "const", "dynamic",
        "type", "enum", "schema", "task",
    ];
    if prefix
        .split_whitespace()
        .all(|word| allowed.contains(&word.trim_matches(|ch: char| !ch.is_ascii_alphabetic())))
    {
        line_start
    } else {
        declaration_start
    }
}

fn leading_documentation(source: &str, declaration_start: usize) -> Option<String> {
    let (_, comments) = crate::lexer::Lexer::new(source).tokenize_with_comments().ok()?;
    let mut cursor = documentation_cursor(source, declaration_start);
    let mut parts = Vec::new();
    for comment in comments.iter().filter(|comment| !comment.is_trailing).rev() {
        if comment.start >= cursor {
            continue;
        }
        let end = comment.start.saturating_add(comment.text.len()).min(source.len());
        if end > cursor {
            continue;
        }
        let gap = source.get(end..cursor)?;
        if !gap.chars().all(char::is_whitespace) || gap.chars().filter(|ch| *ch == '\n').count() > 1 {
            break;
        }
        let Some(text) = normalize_documentation_comment(&comment.text) else {
            break;
        };
        if !text.is_empty() {
            parts.push(text);
        }
        cursor = comment.start;
    }
    if parts.is_empty() {
        None
    } else {
        parts.reverse();
        Some(parts.join("\n"))
    }
}

// ── Import cursor ───────────────────────────────────────────────────────────

/// `source` with comments and string contents replaced by spaces (same byte
/// length; quote marks stay), plus the statement-ending `;` offsets.
fn mask(source: &str) -> (String, Vec<usize>) {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut semis = Vec::new();
    let (mut i, mut block, mut line, mut string, mut escaped) = (0usize, 0usize, false, false, false);
    fn blank(out: &mut [u8], i: usize) {
        if i < out.len() && out[i] != b'\n' {
            out[i] = b' ';
        }
    }
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        if line {
            if b == b'\n' {
                line = false;
            } else {
                blank(&mut out, i);
            }
            i += 1;
        } else if block > 0 {
            blank(&mut out, i);
            if b == b'/' && next == Some(b'*') {
                blank(&mut out, i + 1);
                block += 1;
                i += 2;
            } else if b == b'*' && next == Some(b'/') {
                blank(&mut out, i + 1);
                block -= 1;
                i += 2;
            } else {
                i += 1;
            }
        } else if string {
            if escaped {
                escaped = false;
                blank(&mut out, i);
            } else if b == b'\\' {
                escaped = true;
                blank(&mut out, i);
            } else if b == b'"' {
                string = false;
            } else {
                blank(&mut out, i);
            }
            i += 1;
        } else if b == b'/' && next == Some(b'/') {
            blank(&mut out, i);
            blank(&mut out, i + 1);
            line = true;
            i += 2;
        } else if b == b'/' && next == Some(b'*') {
            blank(&mut out, i);
            blank(&mut out, i + 1);
            block = 1;
            i += 2;
        } else {
            if b == b'"' {
                string = true;
            }
            if b == b';' {
                semis.push(i);
            }
            i += 1;
        }
    }
    // Blanking is byte-wise, so a multi-byte character inside a comment or
    // string becomes several spaces: the length is preserved and the result is
    // valid UTF-8 (ASCII spaces only replace whole characters).
    (String::from_utf8_lossy(&out).into_owned(), semis)
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The cursor sits between the braces of an `import { }` or `import pkg { }`
/// statement. `source` is the text typed so far (it may span lines) and
/// `offset` the cursor byte offset. Never panics on odd offsets.
pub fn import_context(source: &str, offset: usize) -> Option<ImportCursor> {
    let offset = floor_boundary(source, offset);
    let (masked, semis) = mask(source);
    if masked.len() != source.len() {
        return None;
    }
    let start = semis.iter().copied().filter(|at| *at < offset).max().map_or(0, |at| at + 1);
    let end = semis.iter().copied().find(|at| *at >= offset).unwrap_or(source.len());
    let stmt = masked.get(start..end)?;

    // `import [pkg] [type] {`
    let mut rest = stmt;
    let mut cursor_at = start;
    let mut words = Vec::new();
    loop {
        let trimmed = rest.trim_start();
        cursor_at += rest.len() - trimmed.len();
        let word_len = trimmed.bytes().take_while(|b| is_word_byte(*b)).count();
        if word_len == 0 {
            break;
        }
        words.push(&trimmed[..word_len]);
        cursor_at += word_len;
        rest = &trimmed[word_len..];
        if words.len() > 3 {
            return None;
        }
    }
    if words.first() != Some(&"import") {
        return None;
    }
    if words[1..].iter().any(|w| *w != "pkg" && *w != "type") {
        return None;
    }
    let package = words.contains(&"pkg");
    let type_only = words.contains(&"type");
    let open = cursor_at;
    if masked.as_bytes().get(open) != Some(&b'{') || offset <= open {
        return None;
    }
    let close = masked.get(open + 1..end).and_then(|s| s.find('}')).map(|rel| open + 1 + rel);
    if close.is_some_and(|close| offset > close) {
        return None;
    }

    let body_end = close.unwrap_or(end).max(offset);
    let before = &masked[open + 1..offset];

    // The word under the cursor: identifier bytes immediately before it.
    let typed_len = before.bytes().rev().take_while(|b| is_word_byte(*b)).count();
    let replace_start = offset - typed_len;
    let typed = source[replace_start..offset].to_string();

    // Names already listed: every comma segment except the one holding the
    // cursor. A segment's name is its first word (`a as b` lists `a`).
    let mut already = HashSet::new();
    let body = &masked[open + 1..body_end];
    let cursor_in_body = offset - (open + 1);
    let mut seg_start = 0usize;
    for part in body.split(',') {
        let seg_end = seg_start + part.len();
        let holds_cursor = cursor_in_body >= seg_start && cursor_in_body <= seg_end;
        if !holds_cursor {
            if let Some(name) = part.split_whitespace().next() {
                already.insert(name.to_string());
            }
        }
        seg_start = seg_end + 1;
    }

    let target = match close {
        Some(close) => import_target(source, &masked, close + 1, end, package),
        None => ImportTarget::Missing,
    };
    Some(ImportCursor { target, already, typed, replace_start, type_only, close_at: close })
}

/// The `from "x"` (or bare `from x`) clause after the closing brace.
fn import_target(source: &str, masked: &str, from: usize, end: usize, package: bool) -> ImportTarget {
    let end = end.min(masked.len());
    let Some(tail) = masked.get(from..end) else {
        return ImportTarget::Missing;
    };
    let trimmed = tail.trim_start();
    let at = from + (tail.len() - trimmed.len());
    let Some(after_from) = trimmed.strip_prefix("from") else {
        return ImportTarget::Missing;
    };
    let value = after_from.trim_start();
    let value_at = at + 4 + (after_from.len() - value.len());
    let text = if value.starts_with('"') {
        let Some(close_rel) = value[1..].find('"') else {
            return ImportTarget::Missing;
        };
        source.get(value_at + 1..value_at + 1 + close_rel)
    } else {
        let len = value
            .bytes()
            .take_while(|b| is_word_byte(*b) || *b == b'/' || *b == b'.')
            .count();
        source.get(value_at..value_at + len)
    };
    match text {
        Some(text) if !text.is_empty() => {
            if package {
                ImportTarget::Package(text.to_string())
            } else {
                ImportTarget::File(text.to_string())
            }
        }
        _ => ImportTarget::Missing,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn temp_module(src: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.spar");
        std::fs::write(&path, src).unwrap();
        (dir, path)
    }

    fn file(name: &str) -> ImportTarget {
        ImportTarget::File(name.into())
    }

    #[test]
    fn lists_exported_and_public_names_only() {
        let (dir, _) = temp_module(
            "export var port: int = 80;\nvar hidden: int = 1;\nfunction greet(name: str) -> str { return name; };\nprivate function secret() -> int { return 1; };\nexport struct Server { host: str = \"x\"; };\n");
        let items = exports_of(&file("lib.spar"), dir.path(), false, &Default::default()).unwrap();
        let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"port") && names.contains(&"greet") && names.contains(&"Server"));
        assert!(!names.contains(&"hidden") && !names.contains(&"secret"));
        let greet = items.iter().find(|i| i.name == "greet").unwrap();
        assert!(greet.detail.as_deref().unwrap().contains("name"));
        assert_eq!(greet.kind, ExportKind::Function);
    }

    #[test]
    fn covers_enums_and_groups_and_docs() {
        let (dir, _) = temp_module(
            "/// the colours\nexport enum Color { Red, Blue };\nfunctionGroup Tools { function a() -> int { return 1; } };\nprivate functionGroup Hidden { function b() -> int { return 1; } };\n");
        let items = exports_of(&file("lib.spar"), dir.path(), false, &Default::default()).unwrap();
        let kind = |n: &str| items.iter().find(|i| i.name == n).map(|i| i.kind);
        assert_eq!(kind("Color"), Some(ExportKind::Enum));
        assert_eq!(kind("Tools"), Some(ExportKind::Group));
        assert_eq!(kind("Hidden"), None);
        let color = items.iter().find(|i| i.name == "Color").unwrap();
        assert_eq!(color.doc.as_deref(), Some("the colours"));
        assert!(color.file.ends_with("lib.spar"));
    }

    #[test]
    fn type_only_keeps_types_structs_and_enums() {
        let (dir, _) = temp_module(
            "export var v: int = 1;\nfunction f() -> int { return 1; };\nexport struct S { a: int = 1; };\nexport enum E { A, B };\n");
        let items = exports_of(&file("lib.spar"), dir.path(), true, &Default::default()).unwrap();
        let mut names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["E", "S"]);
    }

    #[test]
    fn skips_already_listed_names() {
        let (dir, _) = temp_module("export var a: int = 1;\nexport var b: int = 2;\n");
        let already = ["a".to_string()].into_iter().collect();
        let items = exports_of(&file("lib.spar"), dir.path(), false, &already).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "b");
    }

    #[test]
    fn missing_target_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            exports_of(&file("nope.spar"), dir.path(), false, &Default::default()),
            Err(IntelError::NotFound(_))
        ));
    }

    #[test]
    fn directory_target_does_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub.spar")).unwrap();
        let r = exports_of(&file("sub.spar"), dir.path(), false, &Default::default());
        assert!(r.is_err());
    }

    #[test]
    fn non_utf8_content_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.spar"), [0xff, 0xfe, 0x00, 0x80]).unwrap();
        let r = exports_of(&file("lib.spar"), dir.path(), false, &Default::default());
        assert!(r.is_err());
    }

    #[test]
    fn cyclic_import_terminates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.spar"),
            "import { b } from \"b.spar\";\nexport var a: int = 1;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b.spar"),
            "import { a } from \"a.spar\";\nexport var b: int = 2;\n",
        )
        .unwrap();
        let _ = exports_of(&file("a.spar"), dir.path(), false, &Default::default());
    }

    #[test]
    fn parse_error_still_returns_recovered_names_or_an_error() {
        let (dir, _) = temp_module("export var ok: int = 1;\nexport var broken: int = ;\n");
        let r = exports_of(&file("lib.spar"), dir.path(), false, &Default::default());
        assert!(r.is_ok() || matches!(r, Err(IntelError::Parse(_))));
        if let Ok(items) = r {
            assert!(items.iter().any(|i| i.name == "ok"));
        }
    }

    #[test]
    fn zero_budget_times_out() {
        let (dir, _) = temp_module("export var a: int = 1;\n");
        let r = exports_with_budget(
            &file("lib.spar"),
            dir.path(),
            false,
            &Default::default(),
            std::time::Duration::ZERO,
        );
        assert_eq!(r.unwrap_err(), IntelError::Timeout);
    }

    #[test]
    fn std_package_exports_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let items = exports_of(
            &ImportTarget::Package("std/fs".into()),
            dir.path(),
            false,
            &Default::default(),
        )
        .unwrap();
        assert!(items.iter().any(|i| i.name == "writeText"));
    }

    #[test]
    fn import_context_finds_cursor_between_braces() {
        let src = "import { a, ";
        let c = import_context(src, src.len()).unwrap();
        assert_eq!(c.already.len(), 1);
        assert!(c.already.contains("a"));
        assert_eq!(c.typed, "");
        assert_eq!(c.target, ImportTarget::Missing);
        assert_eq!(c.close_at, None);
    }

    #[test]
    fn import_context_reads_target_after_the_cursor_on_the_same_or_later_line() {
        let src = "import { a, ap }\n    from \"lib.spar\";";
        let off = src.find("ap").unwrap() + 2;
        let c = import_context(src, off).unwrap();
        assert!(matches!(c.target, ImportTarget::File(ref f) if f == "lib.spar"));
        assert_eq!(c.typed, "ap");
        assert_eq!(c.replace_start, src.find("ap").unwrap());
        assert!(c.already.contains("a") && !c.already.contains("ap"));
        assert_eq!(c.close_at, Some(src.find('}').unwrap()));
    }

    #[test]
    fn import_context_none_outside_braces() {
        assert!(import_context("import \"lib.spar\" as l;", 10).is_none());
        assert!(import_context("var x: int = 1;", 5).is_none());
        let src = "import { a } from \"lib.spar\";";
        assert!(import_context(src, src.len() - 3).is_none());
        assert!(import_context(src, 3).is_none());
    }

    #[test]
    fn import_context_handles_package_form() {
        let src = "import pkg {  } from \"std/fs\";";
        let c = import_context(src, "import pkg { ".len()).unwrap();
        assert!(matches!(c.target, ImportTarget::Package(ref p) if p == "std/fs"));
    }

    #[test]
    fn import_context_ignores_earlier_statements_and_comments() {
        let src = "var x: int = 1;\n// import { no }\nimport { a, } from \"m.spar\";";
        let off = src.find("a,").unwrap() + 3;
        let c = import_context(src, off).unwrap();
        assert!(matches!(c.target, ImportTarget::File(ref f) if f == "m.spar"));
        assert!(c.already.contains("a"));
    }

    #[test]
    fn import_context_never_panics_on_odd_offsets() {
        let src = "import { é, ü";
        for off in 0..src.len() + 4 {
            let _ = import_context(src, off);
        }
        let _ = import_context("", 0);
        let _ = import_context("\"unterminated import { ", 5);
    }
}
