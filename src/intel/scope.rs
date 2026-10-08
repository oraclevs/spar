//! Completion of the names visible at the cursor: locals declared before it,
//! parameters, loop variables, `catch` binders, globals, functions, struct
//! constructors, enum types, imports, built-ins, keywords and `_`.
//!
//! [`complete_scope`] compiles the request source; [`complete_scope_with`]
//! reuses a [`MemberAnalysis`] computed earlier. [`local_items`] is the piece a
//! language server shares when it supplies its own symbol tables.

use super::chain::{local_names_at, ScopeName, ScopeNameKind};
use super::cursor::{inside_string_interpolation, lexical_state_at, CursorLexicalState};
use super::member::{analyze_session, MemberAnalysis};
use super::{IntelItem, IntelKind, IntelRequest};
use std::collections::HashSet;

/// Keywords valid where an expression or statement may start.
pub const KEYWORDS: &[&str] = &[
    // declaration keywords
    "var", "const", "export", "private", "import", "dynamic", "as", "struct", "type", "fn",
    "function", "schema", "task", "try", "catch", // control keywords
    "if", "else", "for", "while", "loop", "in", "break", "continue", "return",
    "mut", // literals
    "true", "false",
];

/// Type names, offered in expression position as well.
pub const TYPE_KEYWORDS: &[&str] = &[
    "int", "float", "str", "bool", "Any", "Record", "List", "Map", "Option", "Result", "ShellResult",
];

/// Built-in functions that need no import, with their signatures.
pub const BUILTIN_FUNCTIONS: &[(&str, &str)] = &[
    ("env", "(name: str) -> str"),
    ("int", "(value: int | float | str) -> int"),
    ("float", "(value: int | float | str) -> float"),
    ("str", "(value: int | float | bool | str) -> str"),
    ("bool", "(value: str | bool) -> bool"),
];

fn is_internal_dependency_name(name: &str) -> bool {
    name.starts_with("sparModule") || name.starts_with("SparModule")
}

fn tiered(label: &str, tier: u8) -> Option<String> {
    Some(format!("{tier}_{label}"))
}

/// Locals as completion items, innermost declaration first-wins on shadowing.
/// `kind` is `Parameter` for parameters and `Variable` otherwise; sorted tier 0.
pub fn local_items(names: &[ScopeName]) -> Vec<IntelItem> {
    let mut seen = HashSet::new();
    names
        .iter()
        .rev()
        .filter(|scope| seen.insert(scope.name.clone()))
        .map(|scope| IntelItem {
            label: scope.name.clone(),
            kind: match scope.kind {
                ScopeNameKind::Parameter => IntelKind::Parameter,
                ScopeNameKind::Variable => IntelKind::Variable,
            },
            detail: scope.ty.clone(),
            insert_text: String::new(),
            doc: None,
            sort_text: tiered(&scope.name, 0),
        })
        .collect()
}

/// Names visible at `req.offset`, compiling `req.source` for the file-level
/// declarations. Without a usable compilation only the lexical names remain.
pub fn complete_scope(req: &IntelRequest) -> Vec<IntelItem> {
    match analyze_session(req.source, req.base_dir) {
        Some(analysis) => complete_scope_with(&analysis, req.source, req.offset),
        None => scope_items(None, req.source, req.offset),
    }
}

/// [`complete_scope`] against an analysis of (a prefix of) `source`.
pub fn complete_scope_with(analysis: &MemberAnalysis, source: &str, offset: usize) -> Vec<IntelItem> {
    scope_items(Some(analysis), source, offset)
}

fn scope_items(analysis: Option<&MemberAnalysis>, source: &str, offset: usize) -> Vec<IntelItem> {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    match lexical_state_at(source, offset) {
        CursorLexicalState::Code => {}
        CursorLexicalState::String if inside_string_interpolation(source, offset) => {}
        _ => return Vec::new(),
    }
    let bytes = source.as_bytes();
    let mut start = offset;
    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }
    // After `.` / `::` the names are members, not scope.
    if start > 0 && (bytes[start - 1] == b'.' || (start > 1 && &bytes[start - 2..start] == b"::")) {
        return Vec::new();
    }
    // `var |`, `fn |`, `struct |`...: the user is naming something new.
    let before_word = source[..start].trim_end();
    let previous = before_word
        .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .next()
        .unwrap_or("");
    if start > before_word.len()
        && matches!(
            previous,
            "var" | "const" | "mut" | "fn" | "function" | "struct" | "enum" | "type" | "export" | "private"
                | "schema" | "task" | "functionGroup"
        )
    {
        return Vec::new();
    }
    let typed = source[start..offset].to_ascii_lowercase();

    // An unfinished `${ }` (often inside a command) is not lexable Spar; the
    // names visible where it opens are the ones visible inside it.
    let names_at = match source[..offset].rfind("${") {
        Some(open) if !source[open..offset].contains('}') => open,
        _ => offset,
    };
    let mut items = local_items(&local_names_at(source, names_at));
    if let Some(analysis) = analysis {
        let symbols = analysis.symbols();
        let mut push = |label: &str, kind: IntelKind, detail: Option<String>, tier: u8| {
            items.push(IntelItem {
                label: label.to_string(),
                kind,
                detail,
                insert_text: String::new(),
                doc: None,
                sort_text: tiered(label, tier),
            });
        };
        for (name, entry) in &symbols.globals {
            if is_internal_dependency_name(name) {
                continue;
            }
            let detail = match entry {
                crate::resolver::GlobalEntry::Var { ty, .. } => Some(crate::typechecker::display_type(ty)),
                crate::resolver::GlobalEntry::Dynamic { .. } => Some("dynamic".to_string()),
            };
            push(name, IntelKind::Variable, detail, 1);
        }
        for (name, entry) in &symbols.functions {
            if is_internal_dependency_name(name) {
                continue;
            }
            let params = entry
                .params
                .iter()
                .map(|(param, ty)| format!("{param}: {}", crate::typechecker::display_type(ty)))
                .collect::<Vec<_>>()
                .join(", ");
            let detail = format!("({params}) -> {}", crate::typechecker::display_type(&entry.ret));
            let tier = if BUILTIN_FUNCTIONS.iter().any(|(builtin, _)| builtin == name) { 3 } else { 1 };
            push(name, IntelKind::Function, Some(detail), tier);
        }
        for path in symbols.structs.keys() {
            if path.len() == 1 && !is_internal_dependency_name(&path[0]) {
                push(&path[0], IntelKind::Struct, None, 1);
            }
        }
        for name in symbols.enums.keys() {
            if !is_internal_dependency_name(name) {
                push(name, IntelKind::Enum, None, 1);
            }
        }
        for alias in symbols.imports.keys() {
            push(alias, IntelKind::Module, None, 2);
        }
    }
    for (name, signature) in BUILTIN_FUNCTIONS {
        items.push(IntelItem {
            label: (*name).to_string(),
            kind: IntelKind::Function,
            detail: Some((*signature).to_string()),
            insert_text: String::new(),
            doc: None,
            sort_text: tiered(name, 3),
        });
    }
    for (names, tier) in [(TYPE_KEYWORDS, 4u8), (KEYWORDS, 9u8)] {
        for name in names {
            items.push(IntelItem {
                label: (*name).to_string(),
                kind: IntelKind::Keyword,
                detail: None,
                insert_text: String::new(),
                doc: None,
                sort_text: tiered(name, tier),
            });
        }
    }
    items.push(IntelItem {
        label: "_".to_string(),
        kind: IntelKind::Variable,
        detail: Some("previous result".to_string()),
        insert_text: String::new(),
        doc: None,
        sort_text: tiered("_", 5),
    });

    let mut seen = HashSet::new();
    items.retain(|item| {
        item.label.to_ascii_lowercase().starts_with(&typed) && seen.insert(item.label.clone())
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(text: &str) -> (String, usize) {
        let offset = text.find('|').expect("marker");
        (text.replacen('|', "", 1), offset)
    }

    fn labels_at(text: &str) -> Vec<String> {
        let (source, offset) = marked(text);
        complete_scope(&IntelRequest { source: &source, offset, base_dir: std::path::Path::new(".") })
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    fn has(labels: &[String], name: &str) -> bool {
        labels.iter().any(|l| l == name)
    }

    #[test]
    fn params_locals_and_loop_bindings_are_in_scope() {
        let labels = labels_at(concat!(
            "function g(p: int, q: str) -> int {\n",
            "    var loc: int = 2;\n",
            "    for item in [1, 2] {\n",
            "        var inner: int = item;\n",
            "        return |;\n",
            "    };\n",
            "    return 0;\n",
            "};\n",
        ));
        for expected in ["p", "q", "loc", "item", "inner"] {
            assert!(has(&labels, expected), "missing {expected}: {labels:?}");
        }
    }

    #[test]
    fn closed_blocks_and_other_functions_are_not_in_scope() {
        let labels = labels_at(concat!(
            "function a(x: int) -> int { var hidden: int = 1; return x; };\n",
            "function b(y: int) -> int { return |; };\n",
        ));
        assert!(has(&labels, "y"));
        assert!(!has(&labels, "x"), "{labels:?}");
        assert!(!has(&labels, "hidden"), "{labels:?}");
    }

    #[test]
    fn locals_before_the_cursor_are_visible_and_later_ones_are_not() {
        let labels = labels_at(concat!(
            "function f() -> int {\n",
            "    var early: int = 1;\n",
            "    return |;\n",
            "    var late: int = 2;\n",
            "};\n",
        ));
        assert!(has(&labels, "early"));
        assert!(!has(&labels, "late"), "{labels:?}");
    }

    #[test]
    fn for_variable_is_visible_inside_the_loop_only() {
        let inside = labels_at("function f() -> int {\n    for i in [1, 2] {\n        var n: int = |;\n    };\n    return 0;\n};\n");
        assert!(has(&inside, "i"));
        let after = labels_at("function f() -> int {\n    for i in [1, 2] {\n        var n: int = 1;\n    };\n    return |;\n};\n");
        assert!(!has(&after, "i"), "{after:?}");
    }

    #[test]
    fn catch_binder_is_visible_in_the_handler() {
        let labels = labels_at("function f() -> int {\n    try { var a: int = 1; } catch err {\n        var m: str = |;\n    };\n    return 0;\n};\n");
        assert!(has(&labels, "err"), "{labels:?}");
    }

    #[test]
    fn inner_declaration_shadows_the_outer_name() {
        let (source, offset) = marked(concat!(
            "function f(v: int) -> int {\n",
            "    if true {\n",
            "        var v: str = \"x\";\n",
            "        return |;\n",
            "    };\n",
            "    return 0;\n",
            "};\n",
        ));
        let items = complete_scope(&IntelRequest { source: &source, offset, base_dir: std::path::Path::new(".") });
        let hits: Vec<_> = items.iter().filter(|i| i.label == "v").collect();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].detail.as_deref(), Some("str"));
        assert_eq!(hits[0].sort_text.as_deref(), Some("0_v"));
    }

    #[test]
    fn local_items_rank_first_with_type_and_kind() {
        let names = vec![
            ScopeName::new("loc", ScopeNameKind::Variable, Some("int".into())),
            ScopeName::new("arg", ScopeNameKind::Parameter, None),
        ];
        let items = local_items(&names);
        let loc = items.iter().find(|i| i.label == "loc").unwrap();
        assert_eq!(loc.sort_text.as_deref(), Some("0_loc"));
        assert_eq!(loc.detail.as_deref(), Some("int"));
        assert_eq!(loc.kind, IntelKind::Variable);
        assert_eq!(items.iter().find(|i| i.label == "arg").unwrap().kind, IntelKind::Parameter);
    }

    #[test]
    fn file_level_names_keywords_and_underscore_are_offered() {
        let labels = labels_at(concat!(
            "var count: int = 1;\n",
            "function helper(a: int) -> int { return a; };\n",
            "struct Point { x: int; };\n",
            "enum Color { Red; };\n",
            "|",
        ));
        for expected in ["count", "helper", "Point", "Color", "if", "return", "_", "env"] {
            assert!(has(&labels, expected), "missing {expected}: {labels:?}");
        }
    }

    #[test]
    fn typed_prefix_filters_case_insensitively() {
        let labels = labels_at("var count: int = 1;\nvar x = Co|");
        assert!(has(&labels, "count"));
        assert!(!has(&labels, "if"));
    }

    #[test]
    fn nothing_is_offered_inside_strings_comments_or_after_a_dot() {
        assert!(labels_at("var a: int = 1;\nvar s = \"he|llo\";").is_empty());
        assert!(labels_at("var a: int = 1;\n// note a|\n").is_empty());
        assert!(labels_at("var a: int = 1;\nvar s = a.|").is_empty());
    }

    #[test]
    fn nothing_is_offered_where_a_new_name_is_declared() {
        assert!(labels_at("var count: int = 1;\nvar |").is_empty());
        assert!(labels_at("var count: int = 1;\nfunction co|").is_empty());
        assert!(!labels_at("var count: int = 1;\nvar x = |").is_empty());
    }

    #[test]
    fn loop_variable_is_visible_in_a_command_interpolation_on_the_same_line() {
        let labels = labels_at("for i in [1, 2] { echo ${i|");
        assert!(has(&labels, "i"), "{labels:?}");
    }

    #[test]
    fn interpolation_inside_a_string_is_scope() {
        let labels = labels_at("var count: int = 1;\nvar s = \"n=${co|}\";");
        assert!(has(&labels, "count"), "{labels:?}");
    }

    #[test]
    fn broken_sibling_statement_still_offers_names() {
        let labels = labels_at("var count: int = 1;\nvar = = ;\nvar y = co|");
        assert!(has(&labels, "count"), "{labels:?}");
    }

    #[test]
    fn out_of_range_and_multibyte_offsets_do_not_panic() {
        let source = "var caf\u{e9}: int = 1;\n";
        for offset in [0, 8, 9, 1000] {
            let _ = complete_scope(&IntelRequest { source, offset, base_dir: std::path::Path::new(".") });
        }
    }

    #[test]
    fn removed_section_keyword_is_not_a_scope_type() {
        let token = crate::token::Token::TypeSection;
        let tokens = vec![&token];
        let mut index = 0usize;
        assert_eq!(super::super::chain::parse_type_tokens(&tokens, &mut index), None);
    }
}
