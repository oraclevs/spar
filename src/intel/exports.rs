use super::*;
use crate::ast::{SparType, TopLevelItem};
use crate::compiler::{CompileOptions, Compiler};
use crate::loader::ImportLoader;
use crate::resolver::GlobalEntry;
use super::repair::floor_boundary;
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
/// `type_only` keeps structs, enums and types. (`type_only` and
/// `ExportKind::Type` mirror old spar-ls branches; the grammar no longer has `type`.)
pub fn exports_of(
    target: &ImportTarget,
    base_dir: &Path,
    type_only: bool,
    already: &HashSet<String>,
) -> Result<Vec<ExportItem>, IntelError> {
    exports_with_budget(target, base_dir, type_only, already, BUDGET)
}

/// Like [`exports_of`] with a caller-chosen time budget (a language server can
/// afford more than the 500 ms used at the shell prompt).
pub fn exports_of_with_budget(
    target: &ImportTarget,
    base_dir: &Path,
    type_only: bool,
    already: &HashSet<String>,
    budget: Duration,
) -> Result<Vec<ExportItem>, IntelError> {
    exports_with_budget(target, base_dir, type_only, already, budget)
}

/// Runs the work on a worker thread and gives up after `budget`. The worker
/// owns all its data and may finish on its own after a timeout.
pub(crate) fn exports_with_budget(
    target: &ImportTarget,
    base_dir: &Path,
    type_only: bool,
    already: &HashSet<String>,
    budget: Duration,
) -> Result<Vec<ExportItem>, IntelError> {
    let (target, base_dir, already) = (target.clone(), base_dir.to_path_buf(), already.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("spar-intel-exports".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let _ = tx.send(exports_worker(&target, &base_dir, type_only, &already, budget));
        });
    if spawned.is_err() {
        return Err(IntelError::Timeout);
    }
    match rx.recv_timeout(budget) {
        Ok(result) => result,
        Err(_) => Err(IntelError::Timeout),
    }
}

fn import_loader(path: &str, package: bool, base_dir: &Path) -> (ImportLoader, crate::ast::ImportDecl) {
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
    (loader, decl)
}

/// The file `target` resolves to from `base_dir` (`"lib"` finds `lib.spar`,
/// packages go through the lockfile), or `None` when it does not resolve. A
/// missing local file still gives the path it would have.
/// Cheap: no reading, parsing or compiling. Callers use it to stamp caches.
pub fn resolve_import_path(target: &ImportTarget, base_dir: &Path) -> Option<std::path::PathBuf> {
    let (path, package) = match target {
        ImportTarget::File(path) => (path.as_str(), false),
        ImportTarget::Package(path) => (path.as_str(), true),
        ImportTarget::Missing => return None,
    };
    let (loader, decl) = import_loader(path, package, base_dir);
    loader.resolve_import(&decl).ok().map(|resolved| resolved.path)
}

fn exports_worker(
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

    let (loader, decl) = import_loader(path, package, base_dir);
    let resolved = loader
        .resolve_import(&decl)
        .map_err(|error| IntelError::NotFound(error.to_string()))?;
    check()?;

    let bytes = std::fs::read(&resolved.path)
        .map_err(|error| IntelError::NotFound(format!("{}: {error}", resolved.path.display())))?;
    let source = String::from_utf8(bytes)
        .map_err(|_| IntelError::Parse(format!("{} is not valid UTF-8", resolved.path.display())))?;
    check()?;

    let comments = crate::lexer::Lexer::new(&source)
        .tokenize_with_comments()
        .map(|(_, comments)| comments)
        .unwrap_or_default();
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
            doc: leading_documentation_with(&comments, &source, decl_start),
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
                // The declared type decides when the library does not compile
                // and there is no symbol entry to ask.
                let callable = matches!(decl.ty, SparType::Function { .. })
                    || matches!(
                        entry,
                        Some(GlobalEntry::Var { ty: SparType::Function { .. }, .. })
                    );
                let kind = if callable { ExportKind::Callable } else { ExportKind::Variable };
                push(&decl.name, kind, detail, &decl.span, decl.span.start);
            }
            TopLevelItem::Struct(decl) if decl.exported && !decl.private => {
                push(&decl.name, ExportKind::Struct, Some("struct".into()), &decl.span, decl.span.start);
            }
            TopLevelItem::Function(decl) if !decl.is_private && !type_only => {
                match symbols.as_ref().and_then(|s| s.functions.get(&decl.name)) {
                    Some(entry) => {
                        let label = super::signature::from_entry(&decl.name, entry, Some(decl), None).label();
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
pub(super) fn package_locator(base_dir: &Path) -> Option<crate::package::ModuleLocator> {
    let project_dir = base_dir
        .ancestors()
        .find(|directory| directory.join(crate::package::PACKAGE_MANIFEST_FILE).is_file())?;
    let lockfile =
        crate::package::Lockfile::read(&project_dir.join(crate::package::PACKAGE_LOCK_FILE)).ok()?;
    let store = crate::package::PackageStore::new(crate::package::StorePaths::from_env());
    Some(crate::package::ModuleLocator::for_root(lockfile, store))
}

// ── Parsing with statement repair ───────────────────────────────────────────

/// Parse `source`; if it does not, blank the statements the parser complains
/// about (see [`repair_source`]). Offsets never move.
fn parse_with_statement_repair(source: &str) -> Option<crate::ast::Program> {
    let text = super::repair::repair_source(source)?;
    let tokens = crate::lexer::Lexer::new(&text).tokenize().ok()?;
    crate::parser::Parser::new(tokens).parse().ok()
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

fn leading_documentation_with(
    comments: &[crate::lexer::CommentTrivia],
    source: &str,
    declaration_start: usize,
) -> Option<String> {
    let mut cursor = documentation_cursor(source, declaration_start);
    let mut parts = Vec::new();
    // Comments are in source order: skip those at or after the declaration.
    let upto = comments.partition_point(|comment| comment.start < cursor);
    for comment in comments[..upto].iter().filter(|comment| !comment.is_trailing).rev() {
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
    let mut current = None;
    let body = &masked[open + 1..body_end];
    let cursor_in_body = offset - (open + 1);
    let mut seg_start = 0usize;
    for part in body.split(',') {
        let seg_end = seg_start + part.len();
        let holds_cursor = cursor_in_body >= seg_start && cursor_in_body <= seg_end;
        if holds_cursor {
            current = part.split_whitespace().next().map(str::to_string);
        } else if let Some(name) = part.split_whitespace().next() {
            already.insert(name.to_string());
        }
        seg_start = seg_end + 1;
    }

    let target = match close {
        Some(close) => import_target(source, &masked, close + 1, end, package),
        None => ImportTarget::Missing,
    };
    Some(ImportCursor { target, already, current, typed, replace_start, type_only, package, close_at: close })
}

/// The `from "x"` (or bare `from x`) clause after the closing brace.
fn import_target(source: &str, masked: &str, from: usize, end: usize, package: bool) -> ImportTarget {
    let end = end.min(masked.len());
    let Some(tail) = masked.get(from..end) else {
        return ImportTarget::Missing;
    };
    let trimmed = tail.trim_start();
    let at = from + (tail.len() - trimmed.len());
    let Some(after_from) = trimmed.strip_prefix("from").filter(|r| !r.bytes().next().is_some_and(is_word_byte)) else {
        return ImportTarget::Missing;
    };
    let value = after_from.trim_start();
    let value_at = at + 4 + (after_from.len() - value.len());
    let text = if let Some(quoted) = value.strip_prefix('"') {
        let Some(close_rel) = quoted.find('"') else {
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

    /// Tests use a generous budget so a loaded parallel test run cannot trip the
    /// 500 ms prompt budget; the budget itself is tested with explicit values.
    fn exports_of(
        target: &ImportTarget,
        base_dir: &Path,
        type_only: bool,
        already: &HashSet<String>,
    ) -> Result<Vec<ExportItem>, IntelError> {
        exports_with_budget(target, base_dir, type_only, already, Duration::from_secs(30))
    }

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
    fn resolve_import_path_finds_extensionless_files_and_and_missing_files() {
        let (dir, path) = temp_module("export var a: int = 1;\n");
        assert_eq!(resolve_import_path(&file("lib"), dir.path()), Some(path.clone()));
        assert_eq!(resolve_import_path(&file("lib.spar"), dir.path()), Some(path));
        // A missing local file still resolves to where it would be (callers stat it).
        assert_eq!(resolve_import_path(&file("nope"), dir.path()), Some(dir.path().join("nope.spar")));
        assert_eq!(resolve_import_path(&ImportTarget::Missing, dir.path()), None);
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
    fn function_typed_export_var_is_callable() {
        let (dir, _) = temp_module(
            "export var transform: fn(value: int) -> int = |value: int| value + 1;\nexport var plain: int = 1;\nfunction f() -> int { return 1; };\n");
        let items = exports_of(&file("lib.spar"), dir.path(), false, &Default::default()).unwrap();
        let kind = |n: &str| items.iter().find(|i| i.name == n).map(|i| i.kind);
        assert_eq!(kind("transform"), Some(ExportKind::Callable));
        assert_eq!(kind("plain"), Some(ExportKind::Variable));
        assert_eq!(kind("f"), Some(ExportKind::Function));
    }

    #[test]
    fn library_with_type_error_still_lists_names_without_details() {
        let (dir, _) = temp_module(
            "export var transform: fn(value: int) -> int = |value: int| value + 1;\nfunction broken() -> int { return \"no\"; };\nexport var bad: int = \"x\";\n");
        let items = exports_of(&file("lib.spar"), dir.path(), false, &Default::default()).unwrap();
        let kind = |n: &str| items.iter().find(|i| i.name == n).map(|i| i.kind);
        assert_eq!(kind("transform"), Some(ExportKind::Callable));
        assert_eq!(kind("broken"), Some(ExportKind::Function));
        assert_eq!(kind("bad"), Some(ExportKind::Variable));
        let broken = items.iter().find(|i| i.name == "broken").unwrap();
        assert!(broken.span.end > broken.span.start, "declaration span is used");
    }

    #[test]
    fn public_budget_parameter_is_honored() {
        let mut src = String::new();
        for i in 0..20_000 {
            src.push_str(&format!("export var v{i}: int = {i};\n"));
        }
        let (dir, _) = temp_module(&src);
        let tiny = exports_of_with_budget(
            &file("lib.spar"), dir.path(), false, &Default::default(),
            std::time::Duration::from_millis(1),
        );
        assert_eq!(tiny.unwrap_err(), IntelError::Timeout);
        let (dir, _) = temp_module("export var a: int = 1;\n");
        let roomy = exports_of_with_budget(
            &file("lib.spar"), dir.path(), false, &Default::default(),
            std::time::Duration::from_secs(5),
        );
        assert_eq!(roomy.unwrap().len(), 1);
    }

    #[test]
    fn cursor_reports_current_segment_and_package_flag() {
        let src = "import pkg { a, b as | } from \"std/fs\";";
        let at = src.find('|').unwrap();
        let cursor = import_context(&src.replace('|', ""), at).unwrap();
        assert!(cursor.package);
        assert_eq!(cursor.current.as_deref(), Some("b"));
        assert!(cursor.already.contains("a") && !cursor.already.contains("b"));
        assert_eq!(cursor.target, ImportTarget::Package("std/fs".into()));
        let src = "import { | }";
        let cursor = import_context(&src.replace('|', ""), 9).unwrap();
        assert!(!cursor.package && cursor.current.is_none());
        assert_eq!(cursor.target, ImportTarget::Missing);
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
        assert!(matches!(r, Err(IntelError::NotFound(_))), "{r:?}");
    }

    #[test]
    fn non_utf8_content_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.spar"), [0xff, 0xfe, 0x00, 0x80]).unwrap();
        let r = exports_of(&file("lib.spar"), dir.path(), false, &Default::default());
        assert!(matches!(r, Err(IntelError::Parse(_))), "{r:?}");
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
        let items = exports_of(&file("a.spar"), dir.path(), false, &Default::default()).unwrap();
        let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);
    }

    #[test]
    fn parse_error_recovers_names_from_the_good_statements() {
        let (dir, _) = temp_module("export var ok: int = 1;\nexport var broken: int = ;\n");
        let items = exports_of(&file("lib.spar"), dir.path(), false, &Default::default()).unwrap();
        let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["ok"]);
    }

    #[test]
    fn private_and_unexported_structs_and_enums_are_not_listed() {
        let (dir, _) = temp_module(
            "struct Plain { a: int = 1; };\nprivate struct Priv { a: int = 1; };\nenum Hidden { A, B };\nexport struct Shown { a: int = 1; };\n");
        let items = exports_of(&file("lib.spar"), dir.path(), false, &Default::default()).unwrap();
        let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["Shown"]);
    }

    #[test]
    fn many_documented_exports_are_fast_and_complete() {
        let mut src = String::new();
        for i in 0..10_000 {
            src.push_str(&format!("/// doc {i}\nexport var v{i}: int = {i};\n"));
        }
        let (dir, _) = temp_module(&src);
        let started = std::time::Instant::now();
        let items = exports_with_budget(
            &file("lib.spar"), dir.path(), false, &Default::default(),
            std::time::Duration::from_secs(60),
        )
        .unwrap();
        assert_eq!(items.len(), 10_000);
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
        let v7 = items.iter().find(|i| i.name == "v7").unwrap();
        assert_eq!(v7.doc.as_deref(), Some("doc 7"));
    }

    #[test]
    fn tiny_budget_times_out_promptly_mid_work() {
        let mut src = String::new();
        for i in 0..20_000 {
            src.push_str(&format!("export var v{i}: int = {i};\n"));
        }
        let (dir, _) = temp_module(&src);
        let started = std::time::Instant::now();
        let r = exports_with_budget(
            &file("lib.spar"), dir.path(), false, &Default::default(),
            std::time::Duration::from_millis(1),
        );
        assert_eq!(r.unwrap_err(), IntelError::Timeout);
        assert!(started.elapsed() < std::time::Duration::from_millis(400), "{:?}", started.elapsed());
    }

    #[test]
    fn blocking_fifo_target_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.spar");
        let ok = std::process::Command::new("mkfifo").arg(&fifo).status().map(|s| s.success()).unwrap_or(false);
        if !ok {
            return;
        }
        let started = std::time::Instant::now();
        let r = exports_with_budget(
            &file("pipe.spar"), dir.path(), false, &Default::default(),
            std::time::Duration::from_millis(100),
        );
        assert_eq!(r.unwrap_err(), IntelError::Timeout);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
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

    #[test]
    fn import_context_cursor_segment_exclusion() {
        let src = "import { a, b, c }";
        let c = import_context(src, src.find('c').unwrap() + 1).unwrap();
        assert_eq!(c.typed, "c");
        assert_eq!(c.already, ["a", "b"].iter().map(|s| s.to_string()).collect());
        let mid = import_context(src, src.find('b').unwrap() + 1).unwrap();
        assert_eq!(mid.typed, "b");
        assert_eq!(mid.already, ["a", "c"].iter().map(|s| s.to_string()).collect());
    }

    #[test]
    fn import_context_multiline_from_after_comment() {
        let src = "import { a, }\n  // where from\n  from /* x */ \"lib.spar\";";
        let c = import_context(src, src.find("a,").unwrap() + 3).unwrap();
        assert_eq!(c.target, ImportTarget::File("lib.spar".into()));
    }

    #[test]
    fn import_context_braces_and_semicolons_in_strings() {
        let src = "var s: str = \"import { x; }\";\nimport { a, } from \"we};ird.spar\";";
        let off = src.rfind("a,").unwrap() + 3;
        let c = import_context(src, off).unwrap();
        assert_eq!(c.target, ImportTarget::File("we};ird.spar".into()));
        // inside the string literal itself there is no import context
        assert!(import_context(src, src.find("x;").unwrap()).is_none());
    }

    #[test]
    fn import_context_alias_lists_original_name() {
        let src = "import { a as z, b";
        let c = import_context(src, src.len()).unwrap();
        assert_eq!(c.typed, "b");
        assert!(c.already.contains("a") && !c.already.contains("z"));
    }

    #[test]
    fn import_context_nested_braces_do_not_confuse() {
        let src = "import { a } from \"x\";\nvar r = { k: 1 };";
        assert!(import_context(src, src.find("k:").unwrap()).is_none());
    }

    #[test]
    fn import_target_requires_word_boundary_after_from() {
        let src = "import { a,  } fromx \"lib.spar\";";
        let c = import_context(src, "import { a, ".len()).unwrap();
        assert_eq!(c.target, ImportTarget::Missing);
    }
}
