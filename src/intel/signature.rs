//! Call signatures: what the call around the cursor takes, and which
//! parameter the cursor is on.
//!
//! [`signature_at`] is the stateless entry (compiles the request source). The
//! lower-level [`resolve_callee`] takes symbols the caller already has and a
//! [`SignatureHooks`] for callables only the caller knows about (a workspace
//! index), so a language server can keep its own state.

use super::chain::{local_names_at, type_of_chain, Chain, ChainStep};
use super::cursor::{call_context, inside_string_interpolation, lexical_state_at, CursorLexicalState};
use super::member::{analyze_session, MemberAnalysis};
use super::IntelRequest;
use crate::ast::SparType;
use crate::resolver::{FunctionEntry, SymbolTable};
use crate::semantics::{owner_name_for_type, SemanticCallable, SemanticSnapshot};
use std::collections::HashMap;

/// One parameter of a callable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignatureParam {
    pub name: String,
    pub ty: String,
    /// The default value's source text; `…` when the parameter has a default
    /// whose text is not known. `None` for a required parameter.
    pub default: Option<String>,
}

/// A callable and the parameter the cursor is on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignatureInfo {
    pub name: String,
    pub params: Vec<SignatureParam>,
    pub ret: Option<String>,
    /// Index into `params` of the parameter being typed.
    pub active_param: usize,
    /// Where it is defined, e.g. `Defined in shared`.
    pub doc: Option<String>,
    pub is_async: bool,
}

impl SignatureParam {
    /// `name: ty` or `name: ty = default`.
    pub fn label(&self) -> String {
        let mut rendered = format!("{}: {}", self.name, self.ty);
        if let Some(default) = &self.default {
            rendered.push_str(" = ");
            rendered.push_str(default);
        }
        rendered
    }
}

impl SignatureInfo {
    /// `[async ]name(a: int, b: str = "x") -> ret`.
    pub fn label(&self) -> String {
        let params = self.params.iter().map(SignatureParam::label).collect::<Vec<_>>().join(", ");
        let prefix = if self.is_async { "async " } else { "" };
        match &self.ret {
            Some(ret) => format!("{prefix}{}({params}) -> {ret}", self.name),
            None => format!("{prefix}{}({params})", self.name),
        }
    }
}

fn display(ty: &SparType) -> String {
    crate::typechecker::display_type(ty)
}

const UNKNOWN_DEFAULT: &str = "…";

/// The one builder for a declared function. `raw_decl` supplies the default
/// values' source text.
pub fn from_entry(
    name: &str,
    entry: &FunctionEntry,
    raw_decl: Option<&crate::ast::FunctionDecl>,
    doc: Option<String>,
) -> SignatureInfo {
    SignatureInfo {
        name: name.to_string(),
        params: entry
            .params
            .iter()
            .map(|(param_name, ty)| SignatureParam {
                name: param_name.clone(),
                ty: display(ty),
                default: entry.default_params.contains(param_name).then(|| {
                    raw_decl
                        .and_then(|decl| decl.params.iter().find(|p| &p.name == param_name))
                        .and_then(|p| p.default.as_ref())
                        .map(crate::formatter::format_expression)
                        .unwrap_or_else(|| UNKNOWN_DEFAULT.to_string())
                }),
            })
            .collect(),
        ret: Some(display(&entry.ret)),
        active_param: 0,
        doc,
        is_async: entry.is_async,
    }
}

/// A callable from the semantic snapshot (methods, constructors).
pub fn from_semantic(callable: SemanticCallable, doc: Option<String>) -> SignatureInfo {
    SignatureInfo {
        name: callable.name,
        params: callable
            .parameters
            .into_iter()
            .map(|param| SignatureParam {
                name: param.name,
                ty: display(&param.ty),
                default: param.has_default.then(|| UNKNOWN_DEFAULT.to_string()),
            })
            .collect(),
        ret: Some(display(&callable.return_type)),
        active_param: 0,
        doc,
        is_async: callable.is_async,
    }
}

fn defined_in(origin: &str) -> Option<String> {
    Some(format!("Defined in {origin}"))
}

/// Callables only the caller knows about; every hook defaults to "unknown".
pub trait SignatureHooks {
    /// The constructor of struct `owner`.
    fn constructor(&self, _owner: &str) -> Option<SignatureInfo> {
        None
    }
    /// A function visible by `name`.
    fn callable(&self, _name: &str) -> Option<SignatureInfo> {
        None
    }
    /// Method `name` of `owner` (`static_receiver`: `Type.method` form).
    fn method(&self, _owner: &str, _name: &str, _static_receiver: bool) -> Option<SignatureInfo> {
        None
    }
    /// A function declared in the current document, by `name`.
    fn declared(&self, _name: &str) -> Option<SignatureInfo> {
        None
    }
}

/// Hooks for a stateless request: constructors from the semantic snapshot,
/// default values from the parsed program.
struct AnalysisHooks<'a> {
    symbols: &'a SymbolTable,
    program: Option<&'a crate::ast::Program>,
}

impl SignatureHooks for AnalysisHooks<'_> {
    fn constructor(&self, owner: &str) -> Option<SignatureInfo> {
        let callable =
            SemanticSnapshot::new(self.symbols.clone()).constructor(&SparType::Named(owner.to_string()))?;
        Some(from_semantic(callable, None))
    }
    fn callable(&self, name: &str) -> Option<SignatureInfo> {
        let entry = self.symbols.functions.get(name)?;
        let decl = self.program?.items.iter().find_map(|item| match item {
            crate::ast::TopLevelItem::Function(decl) if decl.name == name => Some(decl),
            _ => None,
        })?;
        Some(from_entry(name, entry, Some(decl), None))
    }
}

/// The symbols a callee is resolved against.
pub struct SignatureEnv<'a> {
    pub symbols: Option<&'a SymbolTable>,
    /// Symbols of `import "x" as alias;` files, by alias.
    pub imports: &'a HashMap<String, SymbolTable>,
    pub hooks: &'a dyn SignatureHooks,
}

#[doc(hidden)] // unstable: spar-ls go-to-definition
pub fn simple_receiver_chain(text: &str) -> Option<Chain> {
    let mut parts = text.split('.');
    let root = parts.next()?.trim();
    if root.is_empty() || !root.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return None;
    }
    let mut steps = Vec::new();
    for part in parts {
        let part = part.trim();
        if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
            return None;
        }
        steps.push(ChainStep::Field(part.to_string()));
    }
    Some(Chain { root: root.to_string(), steps })
}

fn resolve_method(env: &SignatureEnv, source: &str, offset: usize, callee: &str) -> Option<SignatureInfo> {
    let (receiver_text, method_name) = callee.rsplit_once('.')?;
    let symbols = env.symbols?;
    let static_receiver =
        !receiver_text.contains('.') && symbols.structs.contains_key(&vec![receiver_text.to_string()]);
    let receiver_ty = if static_receiver {
        SparType::Named(receiver_text.to_string())
    } else {
        let chain = simple_receiver_chain(receiver_text)?;
        let scope = local_names_at(source, offset);
        type_of_chain(&chain, &scope, symbols, 0)?
    };
    if let Some(owner) = owner_name_for_type(&receiver_ty) {
        if let Some(found) = env.hooks.method(owner, method_name, static_receiver) {
            return Some(found);
        }
    }
    SemanticSnapshot::new(symbols.clone())
        .methods_for_type(&receiver_ty)
        .into_iter()
        .find(|method| method.callable.name == method_name && method.is_static == static_receiver)
        .map(|method| from_semantic(method.callable, None))
}

/// The signature of the callable named `callee` (`f`, `a.m`, `Group::f`,
/// `alias::f`, `alias::Group::f`, `Type<T>`) as seen from `offset`.
/// `active_param` is 0; see [`active_index`].
pub fn resolve_callee(env: &SignatureEnv, source: &str, offset: usize, callee: &str) -> Option<SignatureInfo> {
    if callee.contains('<') && !callee.contains("::") {
        let probe = format!("var sparSignature: {callee};");
        let tokens = crate::Lexer::new(&probe).tokenize().ok()?;
        let program = crate::Parser::new(tokens).parse().ok()?;
        let crate::ast::TopLevelItem::Var(decl) = program.items.first()? else { return None };
        let callable = SemanticSnapshot::new(env.symbols?.clone()).constructor(&decl.ty)?;
        return Some(from_semantic(callable, None));
    }
    if callee.contains('.') {
        return resolve_method(env, source, offset, callee);
    }

    if !callee.contains("::") {
        if let Some(symbols) = env.symbols {
            if symbols.structs.contains_key(&vec![callee.to_string()]) {
                if let Some(found) = env.hooks.constructor(callee) {
                    return Some(found);
                }
            }
        }
        if let Some(found) = env.hooks.callable(callee) {
            return Some(found);
        }
        if let Some(symbols) = env.symbols {
            if let Some(callable) = SemanticSnapshot::new(symbols.clone()).callable(callee) {
                return Some(from_semantic(callable, None));
            }
        }
    }

    let segments = callee.split("::").collect::<Vec<_>>();
    if segments.len() == 2 {
        let (qualifier, member) = (segments[0], segments[1]);
        if let Some(entry) = env.imports.get(qualifier).and_then(|i| i.functions.get(member)) {
            return Some(from_entry(member, entry, None, defined_in(qualifier)));
        }
        if let Some(entry) = env
            .symbols
            .and_then(|s| s.function_groups.get(qualifier))
            .and_then(|g| g.functions.get(member))
        {
            return Some(from_entry(member, entry, None, defined_in(qualifier)));
        }
    }
    if segments.len() == 3 {
        let (alias, group, member) = (segments[0], segments[1], segments[2]);
        if let Some(entry) = env
            .imports
            .get(alias)
            .and_then(|i| i.function_groups.get(group))
            .and_then(|g| g.functions.get(member))
        {
            return Some(from_entry(member, entry, None, defined_in(&format!("{alias}::{group}"))));
        }
    }

    if let Some(symbols) = env.symbols {
        if let Some(entry) = symbols.functions.get(callee).or_else(|| symbols.imported_functions.get(callee)) {
            if let Some(found) = env.hooks.declared(callee) {
                return Some(found);
            }
            return Some(from_entry(callee, entry, None, None));
        }
        if let Some(task) = symbols.tasks.get(callee) {
            return Some(SignatureInfo {
                name: callee.to_string(),
                params: task
                    .params
                    .iter()
                    .map(|(name, ty)| SignatureParam { name: name.clone(), ty: display(ty), default: None })
                    .collect(),
                ret: Some("task".to_string()),
                ..SignatureInfo::default()
            });
        }
    }
    None
}

/// Which parameter the cursor is on. `supplied` are the names already given,
/// `positional` the argument index, `value_of` the name in `name: |`.
pub fn active_index(
    params: &[SignatureParam],
    supplied: &[String],
    positional: u32,
    value_of: Option<&str>,
) -> usize {
    let mut active = positional as usize;
    if active >= params.len() {
        active = params.len().saturating_sub(1);
    }
    // Named arguments can arrive out of positional order: after a comma,
    // prefer the first parameter still missing.
    if !supplied.is_empty() && positional as usize >= supplied.len() {
        if let Some(index) = params.iter().position(|p| !supplied.contains(&p.name)) {
            active = index;
        }
    }
    if let Some(name) = value_of {
        if let Some(position) = params.iter().position(|p| p.name == name) {
            active = position;
        }
    }
    active
}

/// True when the cursor can be inside a call's argument list.
fn in_call_position(source: &str, offset: usize) -> bool {
    match lexical_state_at(source, offset) {
        CursorLexicalState::Code => true,
        CursorLexicalState::String => inside_string_interpolation(source, offset),
        _ => false,
    }
}

fn clamp(source: &str, offset: usize) -> usize {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// The signature of the call around the cursor, with the active parameter.
/// `None` outside a call or when the callee is unknown.
///
/// Stateless: compiles `req.source` with default options (see
/// [`analyze_session`]). Callers asking many times about one unchanged source
/// should use [`signature_at_with`].
pub fn signature_at(req: &IntelRequest) -> Option<SignatureInfo> {
    let offset = clamp(req.source, req.offset);
    if !in_call_position(req.source, offset) {
        return None;
    }
    // Cheap exit before compiling.
    call_context(req.source, offset)?;
    let analysis = analyze_session(req.source, req.base_dir)?;
    signature_at_with(&analysis, req.source, offset)
}

/// [`signature_at`] against symbols computed earlier from a prefix of
/// `source` (the committed session); `source` is that prefix plus the line
/// being typed.
pub fn signature_at_with(analysis: &MemberAnalysis, source: &str, offset: usize) -> Option<SignatureInfo> {
    let offset = clamp(source, offset);
    if !in_call_position(source, offset) {
        return None;
    }
    let call = call_context(source, offset)?;
    let hooks = AnalysisHooks { symbols: analysis.symbols(), program: analysis.program() };
    let env = SignatureEnv { symbols: Some(analysis.symbols()), imports: analysis.imports(), hooks: &hooks };
    let mut info = resolve_callee(&env, source, offset, &call.callee)?;
    info.active_param = active_index(&info.params, &call.supplied, call.active_parameter, call.value_of.as_deref());
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn at_end(source: &str) -> Option<SignatureInfo> {
        signature_at(&IntelRequest { source, offset: source.len(), base_dir: Path::new(".") })
    }

    const DECL: &str = "function vidShrink(input: str, output: str, targetMb: float = 9.3) -> int { return 0; };\n";

    #[test]
    fn tracks_required_and_default_parameters() {
        let source = format!("{DECL}vidShrink(input: \"in\", ");
        let info = at_end(&source).expect("signature");
        assert!(info.label().contains("targetMb: float = 9.3"), "{}", info.label());
        assert_eq!(info.active_param, 1);
        assert_eq!(info.params[2].default.as_deref(), Some("9.3"));
        assert_eq!(info.params[0].default, None);
    }

    #[test]
    fn resolves_cross_file_function_group_member() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("shared.spar"),
            "functionGroup Tools { function run(input: str, mode: str = \"fast\") -> int { return 0; } };\n",
        )
        .unwrap();
        let source = "import \"shared.spar\" as shared;\nvar result: int = shared::Tools::run(input: \"x\", ";
        let info = signature_at(&IntelRequest { source, offset: source.len(), base_dir: temp.path() })
            .expect("signature");
        assert!(info.label().contains("run(input: str, mode: str ="), "{}", info.label());
        assert_eq!(info.active_param, 1);
    }

    #[test]
    fn named_argument_position_picks_that_parameter() {
        let source = format!("{DECL}vidShrink(targetMb: ");
        assert_eq!(at_end(&source).unwrap().active_param, 2);
        let source = format!("{DECL}vidShrink(output: \"o\", input: ");
        assert_eq!(at_end(&source).unwrap().active_param, 0);
    }

    #[test]
    fn positional_argument_counts_commas() {
        let source = format!("{DECL}vidShrink(\"a\", ");
        assert_eq!(at_end(&source).unwrap().active_param, 1);
    }

    #[test]
    fn call_inside_a_pipeline_stage() {
        let source = format!("{DECL}var x = 5 |> vidShrink(input: ");
        let info = at_end(&source).expect("signature");
        assert_eq!(info.name, "vidShrink");
        assert_eq!(info.active_param, 0);
    }

    #[test]
    fn method_call() {
        let source = "struct Shelf { id: int; };\nimpl Shelf { fn add(self, count: int, label: str = \"x\") -> int { return count; }; };\nfn main() -> int {\n    var s: Shelf = Shelf(id: 1);\n    var r: int = s.add(count: 1, );\n    return r;\n};\n";
        let offset = source.find(", )").unwrap() + 2;
        let info = signature_at(&IntelRequest { source, offset, base_dir: Path::new(".") }).expect("signature");
        assert_eq!(info.name, "add");
        assert!(info.label().contains("count: int"), "{}", info.label());
        assert_eq!(info.active_param, 1);
    }

    #[test]
    fn nested_calls_pick_the_innermost() {
        let source = format!("{DECL}fn two(a: int, b: int) -> int {{ return a; }};\ntwo(a: vidShrink(input: \"x\", ");
        let info = at_end(&source).expect("signature");
        assert_eq!(info.name, "vidShrink");
        assert_eq!(info.active_param, 1);
        let source = format!("{DECL}fn two(a: int, b: int) -> int {{ return a; }};\ntwo(a: vidShrink(input: \"x\", output: \"y\"), ");
        let info = at_end(&source).expect("signature");
        assert_eq!(info.name, "two");
        assert_eq!(info.active_param, 1);
    }

    #[test]
    fn outside_a_call_is_none() {
        assert!(at_end(&format!("{DECL}var x = 1;\n")).is_none());
        assert!(at_end(&format!("{DECL}vidShrink(input: \"a\", output: \"b\")")).is_none());
        assert!(at_end(&format!("{DECL}// vidShrink(")).is_none());
        assert!(at_end(&format!("{DECL}var s = \"vidShrink(")).is_none());
    }

    #[test]
    fn unknown_callee_is_none() {
        assert!(at_end("nothingHere(").is_none());
    }

    #[test]
    fn constructor_and_label_shape() {
        let source = "struct Point { x: int; y: int = 0; };\nvar p = Point(x: 1, ";
        let info = at_end(source).expect("signature");
        assert_eq!(info.name, "Point");
        assert_eq!(info.active_param, 1);
        assert!(info.label().starts_with("Point(x: int, y: int"), "{}", info.label());
    }

    #[test]
    fn async_prefix_and_no_return() {
        let info = SignatureInfo {
            name: "f".into(),
            params: vec![SignatureParam { name: "a".into(), ty: "int".into(), default: Some("1".into()) }],
            ret: None,
            is_async: true,
            ..SignatureInfo::default()
        };
        assert_eq!(info.label(), "async f(a: int = 1)");
    }

    #[test]
    fn offsets_past_the_end_and_inside_characters_do_not_panic() {
        let source = format!("{DECL}vidShrink(input: \"é");
        let _ = signature_at(&IntelRequest { source: &source, offset: source.len() + 10, base_dir: Path::new(".") });
        let _ = signature_at(&IntelRequest { source: &source, offset: source.len() - 1, base_dir: Path::new(".") });
    }

    #[test]
    fn session_analysis_serves_many_lines() {
        let session = "fn build(profile: str, release: bool = false) -> int { return 0; };\n";
        let analysis = analyze_session(session, Path::new(".")).unwrap();
        for line in ["build(", "build(profile: ", "build(profile: \"a\", "] {
            let source = format!("{session}\n{line}");
            let info = signature_at_with(&analysis, &source, source.len()).expect(line);
            assert_eq!(info.label(), "build(profile: str, release: bool = false) -> int");
        }
    }
}
