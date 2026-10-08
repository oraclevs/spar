//! Type-aware `receiver.` member completion.
//!
//! [`complete_members`] compiles the request source and answers from the
//! resulting symbols. The lower-level functions take symbols the caller already
//! has (a language server keeps last-good and repaired tables per document) and
//! an optional hook for methods only the caller knows about.

use super::chain::{local_names_at, type_of_chain, Chain, ChainStep};
use super::cursor::{inside_string_interpolation, lexical_state_at, CursorLexicalState};
use super::exports::{blank_line_around, blank_statement_around, package_locator, REPAIR_ATTEMPTS};
use super::{IntelItem, IntelKind, IntelRequest};
use crate::ast::{Program, SparType};
use crate::error::SparError;
use crate::resolver::{GlobalEntry, SymbolTable};
use crate::semantics::owner_name_for_type;
use crate::{CompileOptions, Compiler, Lexer, Parser};
use std::borrow::Cow;

/// What the lower-level member functions need besides the source text.
pub struct MemberEnv<'a> {
    pub symbols: &'a SymbolTable,
    /// The program, parsed lazily: only the type-checker fallback needs it.
    pub ast: &'a dyn Fn() -> Option<Cow<'a, Program>>,
    /// Extra methods for `(owner, static_receiver)` known to the caller only
    /// (a workspace index); merged ahead of the built-in ones.
    pub extra_methods: &'a dyn Fn(&str, bool) -> Vec<IntelItem>,
}

fn display(ty: &SparType) -> String {
    crate::typechecker::display_type(ty)
}

fn is_internal_dependency_name(name: &str) -> bool {
    name.starts_with("sparModule") || name.starts_with("SparModule")
}

/// The part of the source before the cursor that names the receiver: everything
/// after an optional partial member name and the `.` that precedes it.
pub(crate) struct ReceiverAt {
    pub(crate) chain: Chain,
    /// Byte offset just after the `.`.
    pub(crate) after_dot: usize,
}

pub(crate) fn receiver_before_cursor(source: &str, offset: usize) -> Option<ReceiverAt> {
    let mut end = offset.min(source.len());
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    let bytes = source.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    // Skip a partially typed member name.
    let mut cursor = end;
    while cursor > 0 && is_ident(bytes[cursor - 1]) {
        cursor -= 1;
    }
    if cursor == 0 || bytes[cursor - 1] != b'.' {
        return None;
    }
    let dot = cursor - 1;
    // `...spread` and `1.5` are not member accesses.
    if dot > 0 && bytes[dot - 1] == b'.' {
        return None;
    }
    // Walk the receiver chain backwards: idents, `.`, and balanced `[...]`.
    let mut steps_rev: Vec<ChainStep> = Vec::new();
    let mut position = dot; // exclusive end of the receiver text
    loop {
        if position == 0 {
            return None;
        }
        if bytes[position - 1] == b']' {
            let mut depth = 0i32;
            let mut index = position;
            loop {
                if index == 0 {
                    return None;
                }
                index -= 1;
                match bytes[index] {
                    b']' => depth += 1,
                    b'[' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            steps_rev.push(ChainStep::Index);
            position = index;
            continue;
        }
        let end_ident = position;
        while position > 0 && is_ident(bytes[position - 1]) {
            position -= 1;
        }
        if position == end_ident {
            return None;
        }
        let word = &source[position..end_ident];
        if word.as_bytes()[0].is_ascii_digit() {
            return None;
        }
        if position > 0 && bytes[position - 1] == b'.' && !(position > 1 && bytes[position - 2] == b'.') {
            steps_rev.push(ChainStep::Field(word.to_string()));
            position -= 1;
            continue;
        }
        steps_rev.reverse();
        return Some(ReceiverAt {
            chain: Chain { root: word.to_string(), steps: steps_rev },
            after_dot: dot + 1,
        });
    }
}


fn builtin_method_items(symbols: &SymbolTable, ty: &SparType) -> Vec<IntelItem> {
    crate::SemanticSnapshot::new(symbols.clone())
        .methods_for_type(ty)
        .into_iter()
        .filter(|method| method.is_native && !method.is_static)
        .map(|method| {
            let name = method.callable.name;
            let params = method
                .callable
                .parameters
                .iter()
                .map(|param| format!("{}: {}", param.name, display(&param.ty)))
                .collect::<Vec<_>>();
            IntelItem {
                label: name.clone(),
                kind: IntelKind::Method,
                detail: Some(format!("{name}({}) -> {}", params.join(", "), display(&method.callable.return_type))),
                insert_text: name.clone(),
                doc: None,
                sort_text: Some(format!("1_{:03}_{name}", params.len())),
            }
        })
        .collect()
}

fn merge_member_items(mut fields: Vec<IntelItem>, mut methods: Vec<IntelItem>) -> Vec<IntelItem> {
    for field in &mut fields {
        field.sort_text.get_or_insert_with(|| format!("0_{}", field.label));
    }
    fields.append(&mut methods);
    fields.sort_by(|a, b| a.sort_text.cmp(&b.sort_text).then_with(|| a.label.cmp(&b.label)));
    fields
}

fn type_field_items(symbols: &SymbolTable, ty: &SparType) -> Vec<IntelItem> {
    crate::SemanticSnapshot::new(symbols.clone())
        .fields_for_type(ty)
        .iter()
        .enumerate()
        .map(|(position, field)| IntelItem {
            label: field.name.clone(),
            kind: IntelKind::Field,
            detail: Some(display(&field.ty)),
            insert_text: String::new(),
            doc: None,
            sort_text: Some(format!("{position:03}")),
        })
        .collect()
}

/// Fields and methods of `ty`.
pub fn member_items_for_type(env: &MemberEnv, ty: &SparType) -> Vec<IntelItem> {
    let fields = type_field_items(env.symbols, ty);
    let mut methods = owner_name_for_type(ty)
        .map(|owner| (env.extra_methods)(owner, false))
        .unwrap_or_default();
    for item in builtin_method_items(env.symbols, ty) {
        if !methods.iter().any(|existing| existing.label == item.label) {
            methods.push(item);
        }
    }
    let mut items = merge_member_items(fields, methods);
    if matches!(ty, SparType::InlineRecord | SparType::Any) {
        for (name, doc) in [
            ("asStr", "Read as str"),
            ("asInt", "Read as int"),
            ("asFloat", "Read as float"),
            ("asBool", "Read as bool"),
            ("asList", "Read as a list of dynamic values"),
            ("typeName", "Runtime type name"),
        ] {
            if !items.iter().any(|i| i.label == name) {
                items.push(IntelItem {
                    label: name.into(),
                    kind: IntelKind::Method,
                    detail: Some(doc.into()),
                    insert_text: String::new(),
                    doc: None,
                    sort_text: Some(format!("1_000_{name}")),
                });
            }
        }
    }
    items
}

/// Completion after `receiver.` (or `receiver.par|`). `Some(items)` means the
/// cursor is in a member-access position and these are the members, possibly
/// none: callers must not fall back to general expression completion. `None`
/// means this is not a member access. Walks the text of the receiver chain.
pub fn typed_members_indexed(source: &str, offset: usize, env: &MemberEnv) -> Option<Vec<IntelItem>> {
    let symbols = env.symbols;
    let receiver = receiver_before_cursor(source, offset)?;
    let scope = local_names_at(source, offset);
    let is_variable = scope.iter().any(|binding| binding.name == receiver.chain.root)
        || matches!(symbols.globals.get(&receiver.chain.root), Some(GlobalEntry::Var { .. }));
    if is_variable {
        return Some(match type_of_chain(&receiver.chain, &scope, symbols, 0) {
            Some(ty) => {
                let fields = type_field_items(symbols, &ty);
                let mut methods = owner_name_for_type(&ty)
                    .map(|owner| (env.extra_methods)(owner, false))
                    .unwrap_or_default();
                if owner_name_for_type(&ty).is_some() {
                    for item in builtin_method_items(symbols, &ty) {
                        if !methods.iter().any(|existing| existing.label == item.label) {
                            methods.push(item);
                        }
                    }
                }
                merge_member_items(fields, methods)
            }
            None => Vec::new(),
        });
    }

    // A declaration exposes only receiver-less methods. Fields require an instance.
    if receiver.chain.steps.is_empty() {
        let owner = receiver.chain.root.as_str();
        let path = vec![owner.to_string()];
        if symbols.structs.contains_key(&path) {
            return Some((env.extra_methods)(owner, true));
        }
        let upto = &source[..receiver.after_dot];
        return declaration_members(upto, receiver.after_dot, symbols).or(Some(Vec::new()));
    }
    Some(Vec::new())
}

/// [`typed_members_indexed`], falling back to the type checker's inferred
/// receiver type when the text-based chain walk cannot resolve it (closure
/// parameters, call results, loop variables...).
pub fn typed_members(source: &str, offset: usize, env: &MemberEnv) -> Option<Vec<IntelItem>> {
    let primary = typed_members_indexed(source, offset, env);
    if matches!(&primary, Some(items) if !items.is_empty()) {
        return primary;
    }
    // The `.` before the (possibly partial) member name; the receiver may be any expression,
    // including a call result, which the text-based chain walk cannot describe.
    let bytes = source.as_bytes();
    let mut cursor = offset.min(bytes.len());
    while cursor > 0 && (bytes[cursor - 1].is_ascii_alphanumeric() || bytes[cursor - 1] == b'_') {
        cursor -= 1;
    }
    if cursor == 0 || bytes[cursor - 1] != b'.' || (cursor > 1 && bytes[cursor - 2] == b'.') {
        return primary;
    }
    let dot = cursor - 1;
    let ty = (env.ast)()
        .map(|ast| typed_map_of(&ast, env.symbols, source.len()))
        .and_then(|map| receiver_type_at_dot(source, &map, dot))
        .or_else(|| receiver_type_for_incomplete(source, env.symbols, dot))?;
    let items = member_items_for_type(env, &ty);
    if items.is_empty() {
        primary
    } else {
        Some(items)
    }
}

pub fn typed_map_of(ast: &Program, symbols: &SymbolTable, len: usize) -> crate::typechecker::TypeMap {
    let (_, mut map) = crate::typechecker::TypeChecker::check_with_type_map(ast, symbols);
    map.expressions.retain(|s| s.end <= len && s.start < s.end);
    map.receivers.retain(|s| s.end <= len);
    map
}

/// Receiver type of the member access whose `.` is at byte `dot`.
pub fn receiver_type_at_dot(source: &str, map: &crate::typechecker::TypeMap, dot: usize) -> Option<SparType> {
    if source.as_bytes().get(dot) != Some(&b'.') {
        return None;
    }
    map.receivers.iter().rev().find(|r| r.start == dot).map(|r| r.ty.clone())
}

/// While the user is typing `recv.` the file usually does not parse. Insert a placeholder member
/// (and the closers the surrounding text needs) so the checker can type the receiver anyway.
pub fn receiver_type_for_incomplete(source: &str, symbols: &SymbolTable, dot: usize) -> Option<SparType> {
    if source.as_bytes().get(dot) != Some(&b'.') {
        return None;
    }
    let after = dot + 1;
    // Drop a partially typed member name.
    let mut member_end = after;
    let bytes = source.as_bytes();
    while member_end < bytes.len() && (bytes[member_end].is_ascii_alphanumeric() || bytes[member_end] == b'_') {
        member_end += 1;
    }
    for suffix in ["", ";", ")", ");", "]", "];", "}", "};"] {
        let candidate = format!("{}.__member{}{}", &source[..dot], suffix, &source[member_end..]);
        let Ok(tokens) = Lexer::new(&candidate).tokenize() else { continue };
        let Ok(program) = Parser::new(tokens).parse() else { continue };
        let map = typed_map_of(&program, symbols, candidate.len());
        if let Some(ty) = receiver_type_at_dot(&candidate, &map, dot) {
            return Some(ty);
        }
    }
    None
}

fn format_type_field_shape(shape: &crate::ast::TypeFieldShape) -> String {
    use crate::ast::TypeFieldShape;
    match shape {
        TypeFieldShape::Primitive(ty) => display(ty),
        TypeFieldShape::InlineRecord(_) => "Record".to_string(),
        TypeFieldShape::Named(name) => crate::naming::demangle(name),
        TypeFieldShape::TypeParameter(name) => name.clone(),
        TypeFieldShape::Applied { name, arguments } => format!(
            "{}<{}>",
            name,
            arguments.iter().map(display).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Function-group members, as listed after `Group.` or `Group::`.
pub fn function_group_items(functions: &std::collections::HashMap<String, crate::resolver::FunctionEntry>) -> Vec<IntelItem> {
    functions
        .iter()
        .filter(|(name, _)| !is_internal_dependency_name(name))
        .map(|(name, entry)| {
            let param_list = entry
                .params
                .iter()
                .map(|(pname, pty)| format!("{}: {}", pname, display(pty)))
                .collect::<Vec<_>>()
                .join(", ");
            IntelItem {
                label: name.clone(),
                kind: IntelKind::Function,
                detail: Some(format!("({}) -> {}", param_list, display(&entry.ret))),
                insert_text: name.clone(),
                doc: None,
                sort_text: None,
            }
        })
        .collect()
}

fn enum_member_items(variants: &[String]) -> Vec<IntelItem> {
    variants
        .iter()
        .map(|variant| IntelItem {
            label: variant.clone(),
            kind: IntelKind::EnumMember,
            detail: None,
            insert_text: String::new(),
            doc: None,
            sort_text: None,
        })
        .collect()
}

/// Members reached through a declaration name directly before the dot: native
/// module functions, function-group members, type fields and enum variants.
/// `None` when the text before `offset` does not end in `.`.
pub fn declaration_members(source: &str, offset: usize, symbols: &SymbolTable) -> Option<Vec<IntelItem>> {
    let before_cursor = source.get(..offset)?;
    let before_dot = before_cursor.strip_suffix('.')?;
    let base_start = before_dot
        .char_indices()
        .rev()
        .find(|(_, ch)| !ch.is_alphanumeric() && *ch != '_')
        .map_or(0, |(index, ch)| index + ch.len_utf8());
    let base = before_dot.get(base_start..)?;
    if base.is_empty() {
        return Some(Vec::new());
    }

    let mut native_functions = symbols
        .natives
        .iter()
        .filter(|((module, _), signature)| module == base && !signature.private)
        .map(|((_, name), signature)| IntelItem {
            label: name.clone(),
            kind: IntelKind::Function,
            detail: Some(format!(
                "({}) -> {}",
                signature
                    .params
                    .iter()
                    .map(|(param, ty)| format!("{param}: {}", display(ty)))
                    .collect::<Vec<_>>()
                    .join(", "),
                display(&signature.ret)
            )),
            insert_text: name.clone(),
            doc: None,
            sort_text: None,
        })
        .collect::<Vec<_>>();
    if !native_functions.is_empty() {
        native_functions.sort_by(|a, b| a.label.cmp(&b.label));
        return Some(native_functions);
    }

    if let Some(group) = symbols.function_groups.get(base) {
        return Some(function_group_items(&group.functions));
    }

    let struct_path = vec![base.to_string()];
    if symbols.structs.contains_key(&struct_path) {
        return Some(Vec::new());
    }

    let named_kind = match symbols.globals.get(base) {
        Some(GlobalEntry::Var { ty: SparType::Named(name), .. }) => Some(name.as_str()),
        _ if symbols.types.contains_key(base) || symbols.enums.contains_key(base) => Some(base),
        _ => None,
    };
    let Some(name) = named_kind else {
        return Some(Vec::new());
    };

    if let Some(entry) = symbols.types.get(name) {
        return Some(
            entry
                .fields
                .iter()
                .map(|field| IntelItem {
                    label: field.name.clone(),
                    kind: IntelKind::Field,
                    detail: Some(format_type_field_shape(&field.shape)),
                    insert_text: String::new(),
                    doc: None,
                    sort_text: None,
                })
                .collect(),
        );
    }

    Some(
        symbols
            .enums
            .get(name)
            .into_iter()
            .flat_map(|entry| enum_member_items(&entry.variants))
            .collect(),
    )
}

/// `Enum::` and `Group::` members; `None` when the cursor does not follow `name::`.
fn path_members(source: &str, offset: usize, symbols: &SymbolTable) -> Option<Vec<IntelItem>> {
    let before = source.get(..offset)?;
    let is_ident = |ch: char| ch.is_alphanumeric() || ch == '_';
    let without_partial = before.trim_end_matches(is_ident);
    let before_colons = without_partial.strip_suffix("::")?;
    let name = &before_colons[before_colons.trim_end_matches(is_ident).len()..];
    if name.is_empty() {
        return None;
    }
    if let Some(entry) = symbols.enums.get(name) {
        return Some(enum_member_items(&entry.variants));
    }
    symbols.function_groups.get(name).map(|group| function_group_items(&group.functions))
}

// ── Source analysis for the stateless entry point ────────────────────────────

/// The text of `source` after blanking, in place, each statement the lexer or
/// parser rejects, until the rest lexes and parses. Offsets never move.
fn repair_source(source: &str) -> Option<String> {
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

struct Analysis {
    symbols: SymbolTable,
    program: Option<Program>,
}

/// Symbols of `req.source`; for a buffer that does not compile (the line being
/// typed ends in `.`), of a copy with the offending statements blanked.
fn analyze(req: &IntelRequest) -> Option<Analysis> {
    let mut options = CompileOptions { base_dir: req.base_dir.to_path_buf(), evaluate: false, ..CompileOptions::default() };
    options.locator = package_locator(req.base_dir);
    let compilation = Compiler::new(options.clone()).compile(req.source);
    if let Some(symbols) = compilation.symbols {
        return Some(Analysis { symbols, program: compilation.program });
    }
    let mut text = repair_source(req.source)?;
    for _ in 0..6 {
        let compilation = Compiler::new(options.clone()).compile(&text);
        if let Some(symbols) = compilation.symbols {
            let program = compilation.program.or_else(|| {
                let tokens = Lexer::new(&text).tokenize().ok()?;
                Parser::new(tokens).parse().ok()
            });
            return Some(Analysis { symbols, program });
        }
        let start = compilation.errors.iter().find_map(|e| match e {
            SparError::ResolveError { span, .. } if span.end > span.start => Some(span.start),
            _ => None,
        })?;
        let blanked = blank_statement_around(&text, start);
        if blanked == text {
            return None;
        }
        text = blanked;
    }
    None
}

/// Members of the receiver in front of the cursor: struct fields, methods and
/// built-in methods of its type, `Enum.`/`Enum::` variants, module and
/// function-group members. The partial member name before the cursor filters
/// the list by prefix. Empty when the cursor is not after `receiver.`, is in
/// a string or comment, or the receiver's type is unknown.
pub fn complete_members(req: &IntelRequest) -> Vec<IntelItem> {
    let mut offset = req.offset.min(req.source.len());
    while !req.source.is_char_boundary(offset) {
        offset -= 1;
    }
    match lexical_state_at(req.source, offset) {
        CursorLexicalState::Code => {}
        CursorLexicalState::String if inside_string_interpolation(req.source, offset) => {}
        _ => return Vec::new(),
    }
    let Some(analysis) = analyze(req) else { return Vec::new() };
    let source = req.source;
    let ast = || analysis.program.as_ref().map(Cow::Borrowed);
    let no_extra = |_: &str, _: bool| Vec::new();
    let env = MemberEnv { symbols: &analysis.symbols, ast: &ast, extra_methods: &no_extra };
    let items = typed_members(source, offset, &env)
        .or_else(|| declaration_members(source, offset, &analysis.symbols))
        .or_else(|| path_members(source, offset, &analysis.symbols))
        .unwrap_or_default();
    let typed = source[..offset]
        .rsplit(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .next()
        .unwrap_or("");
    items.into_iter().filter(|item| item.label.starts_with(typed)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// `marked` holds one `|` for the cursor.
    fn members(marked: &str) -> Vec<IntelItem> {
        let offset = marked.find('|').expect("cursor marker");
        let source = marked.replacen('|', "", 1);
        complete_members(&IntelRequest { source: &source, offset, base_dir: Path::new(".") })
    }

    fn labels(marked: &str) -> Vec<String> {
        members(marked).into_iter().map(|item| item.label).collect()
    }

    const P: &str = "struct P { name: str = \"\"; port: int = 0; };\nvar p: P = P();\n";

    #[test]
    fn struct_fields_after_dot() {
        let items = members(&format!("{P}p.|"));
        let names: Vec<_> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(names.contains(&"name") && names.contains(&"port"), "{names:?}");
        let name = items.iter().find(|i| i.label == "name").unwrap();
        assert_eq!(name.kind, IntelKind::Field);
        assert_eq!(name.detail.as_deref(), Some("str"));
    }

    #[test]
    fn partial_name_filters_by_prefix() {
        assert_eq!(labels(&format!("{P}p.na|")), vec!["name"]);
        assert_eq!(labels(&format!("{P}p.po|")), vec!["port"]);
        assert!(labels(&format!("{P}p.zz|")).is_empty());
    }

    #[test]
    fn methods_of_str_and_list() {
        let str_items = members("var s: str = \"x\";\ns.|");
        assert!(str_items.iter().any(|i| i.label == "length" && i.kind == IntelKind::Method), "{str_items:?}");
        let list_items = members("var xs: List<int> = [1];\nxs.|");
        assert!(list_items.iter().any(|i| i.label == "append" && i.kind == IntelKind::Method));
        assert!(list_items.iter().all(|i| i.kind != IntelKind::Field));
    }

    #[test]
    fn enum_members_after_double_colon_and_dot() {
        let source = "enum Color { Red, Green };\n";
        let colons = members(&format!("{source}Color::|"));
        let names: Vec<_> = colons.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(names, vec!["Red", "Green"]);
        assert!(colons.iter().all(|i| i.kind == IntelKind::EnumMember));
        assert_eq!(labels(&format!("{source}Color.|")), vec!["Red", "Green"]);
        assert_eq!(labels(&format!("{source}Color::Gr|")), vec!["Green"]);
    }

    #[test]
    fn unknown_receiver_is_empty() {
        assert!(labels(&format!("{P}nothing.|")).is_empty());
        assert!(labels(&format!("{P}p.name.nothing.|")).is_empty());
    }

    #[test]
    fn not_a_member_position_is_empty() {
        assert!(labels(&format!("{P}p|")).is_empty());
        assert!(labels(&format!("{P}var n = 1.|")).is_empty());
    }

    #[test]
    fn cursor_inside_a_string_or_comment_is_empty() {
        assert!(labels(&format!("{P}var t = \"p.|\";")).is_empty());
        assert!(labels(&format!("{P}// p.|")).is_empty());
    }

    #[test]
    fn interpolation_inside_a_string_still_completes() {
        let names = labels(&format!("{P}var t = \"${{p.|}}\";"));
        assert!(names.contains(&"name".to_string()), "{names:?}");
    }

    #[test]
    fn chained_receivers_follow_field_types() {
        let source = "struct Inner { id: int = 0; };\nstruct Outer { inner: Inner = Inner(); };\nvar o: Outer = Outer();\no.inner.|";
        assert!(labels(source).contains(&"id".to_string()));
    }

    #[test]
    fn other_statements_with_errors_do_not_hide_members() {
        let source = format!("{P}var broken = ;\np.|");
        assert!(labels(&source).contains(&"name".to_string()));
    }

    #[test]
    fn multibyte_text_and_out_of_range_offsets_do_not_panic() {
        let source = format!("{P}var é = 1;\np.");
        let base = Path::new(".");
        for offset in 0..=source.len() + 3 {
            let _ = complete_members(&IntelRequest { source: &source, offset, base_dir: base });
        }
    }

    #[test]
    fn multi_line_sources_with_functions_and_comments() {
        let source = format!("// header \u{e9}\nfn helper() -> int {{\n    return 1;\n}};\n\n{P}var q: int = 2;\np.po|");
        assert_eq!(labels(&source), vec!["port"]);
    }
}
