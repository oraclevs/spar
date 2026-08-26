use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use crate::ast::Program;
use crate::error::SparError;

#[derive(Debug)]
pub struct LoadedImport {
    pub path:    String,
    pub exports: HashSet<String>,
}

pub struct ImportLoader {
    base_dir: PathBuf,
}

impl ImportLoader {
    pub fn new(base: &Path) -> Self {
        Self { base_dir: base.to_path_buf() }
    }
}

/// Splice selective/asPartOf import targets into `program`'s own top-level
/// items, in place, before resolve/typecheck ever run. `import "path" as
/// alias;` and `import schema "path";` pass through untouched — they're
/// still handled by `collect_imports` / `validate_schema_imports`.
pub fn expand_imports(
    program: &mut Program,
    loader: &mut ImportLoader,
) -> Result<(), Vec<SparError>> {
    let mut visiting: Vec<PathBuf> = Vec::new();
    expand_imports_inner(program, loader, &mut visiting)
}

fn expand_imports_inner(
    program: &mut Program,
    loader: &mut ImportLoader,
    visiting: &mut Vec<PathBuf>,
) -> Result<(), Vec<SparError>> {
    use crate::ast::{ImportKind, TopLevelItem};

    let mut errors: Vec<SparError> = Vec::new();
    let mut declared: HashSet<String> = program.items.iter()
        .filter_map(top_level_name)
        .map(|s| s.to_string())
        .collect();

    let old_items = std::mem::take(&mut program.items);
    let mut new_items: Vec<TopLevelItem> = Vec::with_capacity(old_items.len());

    for item in old_items {
        let TopLevelItem::Import(decl) = &item else {
            new_items.push(item);
            continue;
        };
        let decl = decl.clone();

        let spliced = match &decl.kind {
            ImportKind::Aliased(_) | ImportKind::Schema => {
                new_items.push(item);
                continue;
            }
            ImportKind::Selective(requested) => splice_selective(&decl, requested, false, loader),
            ImportKind::TypeSelective(requested) => splice_selective(&decl, requested, true, loader),
            ImportKind::AsPartOf => splice_as_part_of(&decl, loader, visiting),
        };

        match spliced {
            Ok(items) => {
                for it in items {
                    if let Some(name) = top_level_name(&it) {
                        if !declared.insert(name.to_string()) {
                            let span = top_level_span(&it).unwrap_or_else(|| decl.span.clone());
                            errors.push(SparError::ResolveError {
                                message: format!(
                                    "'{}' brought in from '{}' collides with a declaration already in scope",
                                    name, decl.path
                                ),
                                hint: None,
                                span,
                            });
                            continue;
                        }
                    }
                    new_items.push(it);
                }
            }
            Err(es) => errors.extend(es),
        }
    }

    program.items = new_items;
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

fn top_level_name(item: &crate::ast::TopLevelItem) -> Option<&str> {
    use crate::ast::TopLevelItem;
    match item {
        TopLevelItem::Var(v) => Some(&v.name),
        TopLevelItem::Section(s) => s.path.first().map(|s| s.as_str()),
        TopLevelItem::Function(f) => Some(&f.name),
        TopLevelItem::Type(t) => Some(&t.name),
        _ => None,
    }
}

fn top_level_span(item: &crate::ast::TopLevelItem) -> Option<crate::error::Span> {
    use crate::ast::TopLevelItem;
    match item {
        TopLevelItem::Var(v) => Some(v.span.clone()),
        TopLevelItem::Section(s) => Some(s.span.clone()),
        TopLevelItem::Function(f) => Some(f.span.clone()),
        TopLevelItem::Type(t) => Some(t.span.clone()),
        _ => None,
    }
}

/// Reset a selectively-imported item's visibility so it behaves as an
/// internal local declaration in the importing file — not schema-validated,
/// not emitted, not part of the importing file's own export surface.
/// Root-cause fix for: splicing preserved `exported`/`private` verbatim
/// from the source file, so an `export [Colors]{...}` pulled in via
/// `import { Colors } from "...";` was indistinguishable from a section the
/// importing file declared and exported itself, and got flagged by schema
/// Rule 2 as "not declared in any imported schema." Must NOT be applied to
/// asPartOf's splice path — that mechanism intentionally preserves
/// visibility flags verbatim ("as if pasted in directly").
fn localize_visibility(item: crate::ast::TopLevelItem) -> crate::ast::TopLevelItem {
    use crate::ast::TopLevelItem;
    match item {
        TopLevelItem::Var(mut v) => { v.exported = false; TopLevelItem::Var(v) }
        TopLevelItem::Section(mut s) => { s.exported = false; s.private = true; TopLevelItem::Section(s) }
        TopLevelItem::Function(mut f) => { f.is_private = true; TopLevelItem::Function(f) }
        TopLevelItem::Type(mut t) => { t.exported = false; TopLevelItem::Type(t) }
        TopLevelItem::Enum(mut e) => { e.exported = false; TopLevelItem::Enum(e) }
        other => other,
    }
}

/// Point a selectively-imported item's own top-level span at the `import`
/// statement that brought it in, instead of leaving it pointing into the
/// source file's text. `Span` carries no file identity — it's just byte
/// offsets + line/col relative to whichever single source string the CLI
/// or LSP is currently rendering against — so a foreign span (e.g. from
/// `lib.spar`) rendered against the importing file's text lands on an
/// unrelated, misleading line. Retagging the top-level span to the `import`
/// line at least keeps every error inside the importing file's own text.
/// This is a partial mitigation, not a full fix: it does not touch spans
/// nested inside the item (individual fields, expressions) — an error
/// anchored on one of those will still carry a foreign span. A complete
/// fix needs file-provenance on `Span`/`SparError` plus a multi-file-aware
/// renderer; tracked as follow-up work, not attempted here.
fn retag_top_level_span(item: crate::ast::TopLevelItem, span: &crate::error::Span) -> crate::ast::TopLevelItem {
    use crate::ast::TopLevelItem;
    match item {
        TopLevelItem::Var(mut v) => { v.span = span.clone(); TopLevelItem::Var(v) }
        TopLevelItem::Section(mut s) => { s.span = span.clone(); TopLevelItem::Section(s) }
        TopLevelItem::Function(mut f) => { f.span = span.clone(); f.name_span = span.clone(); TopLevelItem::Function(f) }
        TopLevelItem::Type(mut t) => { t.span = span.clone(); t.name_span = span.clone(); TopLevelItem::Type(t) }
        TopLevelItem::Enum(mut e) => { e.span = span.clone(); e.name_span = span.clone(); TopLevelItem::Enum(e) }
        other => other,
    }
}

fn rename_top_level_item(item: crate::ast::TopLevelItem, new_name: &str) -> crate::ast::TopLevelItem {
    use crate::ast::TopLevelItem;
    match item {
        TopLevelItem::Var(mut v) => { v.name = new_name.to_string(); TopLevelItem::Var(v) }
        TopLevelItem::Section(mut s) => {
            if let Some(first) = s.path.first_mut() { *first = new_name.to_string(); }
            TopLevelItem::Section(s)
        }
        TopLevelItem::Function(mut f) => { f.name = new_name.to_string(); TopLevelItem::Function(f) }
        TopLevelItem::Type(mut t) => { t.name = new_name.to_string(); TopLevelItem::Type(t) }
        TopLevelItem::Enum(mut e) => { e.name = new_name.to_string(); TopLevelItem::Enum(e) }
        other => other,
    }
}

fn splice_selective(
    decl: &crate::ast::ImportDecl,
    requested: &[crate::ast::ImportItem],
    types_only: bool,
    loader: &ImportLoader,
) -> Result<Vec<crate::ast::TopLevelItem>, Vec<SparError>> {
    use crate::ast::TopLevelItem;

    let full_path = loader.base_dir.join(&decl.path);
    if !full_path.exists() {
        return Err(vec![SparError::ResolveError {
            message: format!("cannot find import file '{}' — file does not exist", decl.path),
            hint: Some("check the file path and ensure it is relative to the current file".into()),
            span: decl.span.clone(),
        }]);
    }

    let src = std::fs::read_to_string(&full_path).map_err(|e| vec![SparError::ResolveError {
        message: format!("cannot read import file '{}': {}", decl.path, e),
        hint: None,
        span: decl.span.clone(),
    }])?;

    let tokens = crate::lexer::Lexer::new(&src).tokenize().map_err(|e| vec![SparError::ResolveError {
        message: format!("import file '{}' has a lex error: {}", decl.path, e),
        hint: None,
        span: decl.span.clone(),
    }])?;

    let imported_program = crate::parser::Parser::new(tokens).parse().map_err(|e| vec![SparError::ResolveError {
        message: format!("import file '{}' has a parse error: {}", decl.path, e),
        hint: None,
        span: decl.span.clone(),
    }])?;

    let available: Vec<(&str, &TopLevelItem)> = imported_program.items.iter().filter_map(|it| {
        match it {
            TopLevelItem::Var(v) if v.exported => Some((v.name.as_str(), it)),
            TopLevelItem::Section(s) if s.exported => s.path.first().map(|n| (n.as_str(), it)),
            TopLevelItem::Function(f) if !f.is_private => Some((f.name.as_str(), it)),
            TopLevelItem::Type(t) if t.exported => Some((t.name.as_str(), it)),
            TopLevelItem::Enum(e) if e.exported => Some((e.name.as_str(), it)),
            _ => None,
        }
    }).collect();

    let mut errors = Vec::new();
    let mut spliced = Vec::new();

    for req in requested {
        match available.iter().find(|(n, _)| *n == req.name) {
            None => {
                let candidates = available.iter().map(|(n, _)| *n);
                let hint = crate::resolver::suggest(&req.name, candidates);
                errors.push(SparError::ResolveError {
                    message: format!("'{}' is not exported by '{}'", req.name, decl.path),
                    hint,
                    span: req.name_span.clone(),
                });
            }
            Some((_, item)) => {
                if types_only && !matches!(item, TopLevelItem::Type(_) | TopLevelItem::Enum(_)) {
                    errors.push(SparError::ResolveError {
                        message: format!(
                            "'{}' is not a type or enum — `import type {{...}}` can only bring in \
                             `type` or `enum` declarations",
                            req.name
                        ),
                        hint: None,
                        span: req.name_span.clone(),
                    });
                    continue;
                }
                let final_name = req.alias.clone().unwrap_or_else(|| req.name.clone());
                let renamed = rename_top_level_item((*item).clone(), &final_name);
                let localized = localize_visibility(renamed);
                // Each item gets its OWN span (not the whole import line) —
                // so `import { A, B }` positions A's and B's tokens/errors
                // at their own names in the brace list, not both at one spot.
                spliced.push(retag_top_level_span(localized, &req.span));
            }
        }
    }

    // Transitively pull in any type referenced by a spliced type's own
    // fields (e.g. `packages?: Libs;` inside `FlutterType`) that the caller
    // didn't name explicitly. Without this, splicing only the exactly
    // requested items leaves `Libs` undeclared in the importing file even
    // though it was never meant to be a user-facing import target — it's
    // load-bearing structure of `FlutterType`, not something the caller
    // should have to know about. Pulled-in types keep their original name
    // (never aliased) and are localized like any other spliced item; the
    // loop is index-based over a growing `spliced`, so a dependency that
    // itself references further types is picked up on a later pass.
    let mut pulled: HashSet<String> = requested.iter().map(|r| r.name.clone()).collect();
    let mut i = 0;
    while i < spliced.len() {
        if let TopLevelItem::Type(t) = &spliced[i] {
            let mut refs = Vec::new();
            collect_named_type_refs(&t.fields, &mut refs);
            let parent_name = t.name.clone();
            for name in refs {
                if pulled.insert(name.clone()) {
                    match available.iter().find(|(n, _)| *n == name) {
                        Some((_, dep_item @ (TopLevelItem::Type(_) | TopLevelItem::Enum(_)))) => {
                            let localized = localize_visibility((*dep_item).clone());
                            spliced.push(retag_top_level_span(localized, &decl.span));
                        }
                        Some(_) => {} // name resolves to a non-type item; resolver reports the shape mismatch
                        None => {
                            errors.push(SparError::ResolveError {
                                message: format!(
                                    "type `{}`, used by `{}`'s field, is not exported by '{}' — \
                                     export it so the transitive import can resolve",
                                    name, parent_name, decl.path
                                ),
                                hint: None,
                                span: decl.span.clone(),
                            });
                        }
                    }
                }
            }
        }
        i += 1;
    }

    if errors.is_empty() { Ok(spliced) } else { Err(errors) }
}

fn collect_named_type_refs(fields: &[crate::ast::TypeField], out: &mut Vec<String>) {
    use crate::ast::TypeFieldShape;
    for f in fields {
        match &f.shape {
            TypeFieldShape::Primitive(_) => {}
            TypeFieldShape::Named(name) => out.push(name.clone()),
            TypeFieldShape::Section(nested) => collect_named_type_refs(nested, out),
        }
    }
}

fn splice_as_part_of(
    decl: &crate::ast::ImportDecl,
    loader: &mut ImportLoader,
    visiting: &mut Vec<PathBuf>,
) -> Result<Vec<crate::ast::TopLevelItem>, Vec<SparError>> {
    let full_path = loader.base_dir.join(&decl.path);
    let canonical = full_path.canonicalize().unwrap_or_else(|_| full_path.clone());

    if let Some(cycle) = crate::depgraph::find_cycle_in_stack(visiting, &canonical) {
        let chain: Vec<String> = cycle.iter()
            .map(|p| p.display().to_string())
            .collect();
        return Err(vec![SparError::ResolveError {
            message: format!("import cycle detected via `asPartOf`: {}", chain.join(" -> ")),
            hint: None,
            span: decl.span.clone(),
        }]);
    }

    if !full_path.exists() {
        return Err(vec![SparError::ResolveError {
            message: format!("cannot find import file '{}' — file does not exist", decl.path),
            hint: Some("check the file path and ensure it is relative to the current file".into()),
            span: decl.span.clone(),
        }]);
    }

    let src = std::fs::read_to_string(&full_path).map_err(|e| vec![SparError::ResolveError {
        message: format!("cannot read import file '{}': {}", decl.path, e),
        hint: None,
        span: decl.span.clone(),
    }])?;

    let tokens = crate::lexer::Lexer::new(&src).tokenize().map_err(|e| vec![SparError::ResolveError {
        message: format!("import file '{}' has a lex error: {}", decl.path, e),
        hint: None,
        span: decl.span.clone(),
    }])?;

    let mut sub_program = crate::parser::Parser::new(tokens).parse().map_err(|e| vec![SparError::ResolveError {
        message: format!("import file '{}' has a parse error: {}", decl.path, e),
        hint: None,
        span: decl.span.clone(),
    }])?;

    let sub_base = full_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut sub_loader = ImportLoader::new(&sub_base);

    visiting.push(canonical);
    let result = expand_imports_inner(&mut sub_program, &mut sub_loader, visiting);
    visiting.pop();
    result?;

    Ok(sub_program.items)
}

pub fn collect_imports(
    program: &Program,
    loader: &mut ImportLoader,
) -> Result<HashMap<String, LoadedImport>, Vec<SparError>> {
    use crate::ast::TopLevelItem;

    let mut errors: Vec<SparError> = Vec::new();
    let mut result: HashMap<String, LoadedImport> = HashMap::new();

    for item in &program.items {
        let TopLevelItem::Import(decl) = item else { continue };
        let crate::ast::ImportKind::Aliased(decl_alias) = &decl.kind else { continue };

        let alias = decl_alias.clone().unwrap_or_else(|| {
            decl.path
                .rsplit('/')
                .next()
                .unwrap_or(&decl.path)
                .trim_end_matches(".spar")
                .to_string()
        });

        let full_path = loader.base_dir.join(&decl.path);
        if !full_path.exists() {
            errors.push(SparError::ResolveError {
                message: format!(
                    "cannot find import file '{}' — file does not exist",
                    decl.path
                ),
                hint: Some(
                    "check the file path and ensure it is relative to the current file".into()
                ),
                span: decl.span.clone(),
            });
            continue;
        }

        // Parse the imported file to extract exported names
        let src = match std::fs::read_to_string(&full_path) {
            Ok(s) => s,
            Err(e) => {
                errors.push(SparError::ResolveError {
                    message: format!("cannot read import file '{}': {}", decl.path, e),
                    hint: None,
                    span: decl.span.clone(),
                });
                continue;
            }
        };

        let tokens = match crate::lexer::Lexer::new(&src).tokenize() {
            Ok(t) => t,
            Err(e) => {
                errors.push(SparError::ResolveError {
                    message: format!("import file '{}' has a lex error: {}", decl.path, e),
                    hint: None,
                    span: decl.span.clone(),
                });
                continue;
            }
        };

        let imported_program = match crate::parser::Parser::new(tokens).parse() {
            Ok(p) => p,
            Err(e) => {
                errors.push(SparError::ResolveError {
                    message: format!("import file '{}' has a parse error: {}", decl.path, e),
                    hint: None,
                    span: decl.span.clone(),
                });
                continue;
            }
        };

        // Collect exported symbol names
        let mut exports = HashSet::new();
        for item in &imported_program.items {
            match item {
                TopLevelItem::Var(v) if v.exported => { exports.insert(v.name.clone()); }
                TopLevelItem::Section(s) if s.exported => {
                    if let Some(name) = s.path.first() {
                        exports.insert(name.clone());
                    }
                }
                TopLevelItem::Function(f) if !f.is_private => { exports.insert(f.name.clone()); }
                TopLevelItem::Type(t) if t.exported => { exports.insert(t.name.clone()); }
                TopLevelItem::FunctionGroup(g) if !g.is_private => { exports.insert(g.name.clone()); }
                _ => {}
            }
        }

        result.insert(alias, LoadedImport {
            path: decl.path.clone(),
            exports,
        });
    }

    if errors.is_empty() { Ok(result) } else { Err(errors) }
}

fn type_fields_to_schema_fields(
    type_fields: &[crate::ast::TypeField],
    schema_prog: &Program,
) -> Vec<crate::ast::SchemaField> {
    use crate::ast::{TypeFieldShape, SchemaFieldShape, SchemaField, TopLevelItem};

    type_fields.iter().map(|tf| {
        let shape = match &tf.shape {
            TypeFieldShape::Primitive(ty) => SchemaFieldShape::Primitive(ty.clone()),
            TypeFieldShape::Section(nested) => {
                SchemaFieldShape::Section(type_fields_to_schema_fields(nested, schema_prog))
            }
            TypeFieldShape::Named(other_name) => {
                let is_enum = schema_prog.items.iter().any(|it| {
                    matches!(it, TopLevelItem::Enum(e) if &e.name == other_name)
                });
                if is_enum {
                    // An enum-typed field has a single named value, not
                    // nested fields — reuse `Primitive(SparType::Named)`
                    // rather than expanding into a `Section`, so schema
                    // value-checking compares it the same way a `var x:
                    // EnumName = EnumName::Variant;` declaration already does.
                    SchemaFieldShape::Primitive(crate::ast::SparType::Named(other_name.clone()))
                } else {
                    let other_fields = schema_prog.items.iter().find_map(|it| {
                        if let TopLevelItem::Type(t) = it {
                            if &t.name == other_name { return Some(&t.fields); }
                        }
                        None
                    });
                    let expanded = other_fields
                        .map(|fields| type_fields_to_schema_fields(fields, schema_prog))
                        .unwrap_or_default();
                    SchemaFieldShape::Section(expanded)
                }
            }
        };
        SchemaField {
            name: tf.name.clone(),
            optional: tf.optional,
            shape,
            span: tf.span.clone(),
        }
    }).collect()
}

/// Convert every `SchemaFrom`/`SchemaFrom?` in `schema_prog` into an
/// equivalent generated `TopLevelItem::SchemaSection`, removing the
/// `SchemaFrom` items. Must run after `expand_imports` has spliced in any
/// `import type {...}` targets, so `source_type` lookups see real
/// `TypeDecl`s.
fn expand_schema_from(schema_prog: &mut Program) -> Result<(), Vec<SparError>> {
    use crate::ast::{TopLevelItem, SchemaSectionDecl};

    let mut errors: Vec<SparError> = Vec::new();
    let mut generated: Vec<TopLevelItem> = Vec::new();

    for item in &schema_prog.items {
        let TopLevelItem::SchemaFrom(sf) = item else { continue };

        let source = schema_prog.items.iter().find_map(|it| {
            if let TopLevelItem::Type(t) = it {
                if t.name == sf.source_type { return Some(t); }
            }
            None
        });

        match source {
            None => {
                errors.push(SparError::SchemaError {
                    message: format!(
                        "SchemaFrom references undeclared type `{}` — bring it in with \
                         `import type {{ {} }} from \"...\";`",
                        sf.source_type, sf.source_type
                    ),
                    span: sf.source_type_span.clone(),
                });
            }
            Some(t) => {
                let fields = type_fields_to_schema_fields(&t.fields, schema_prog);
                generated.push(TopLevelItem::SchemaSection(SchemaSectionDecl {
                    name: sf.name.clone(),
                    marker: sf.marker.clone(),
                    fields,
                    span: sf.span.clone(),
                }));
            }
        }
    }

    schema_prog.items.retain(|it| !matches!(it, TopLevelItem::SchemaFrom(_)));
    schema_prog.items.extend(generated);

    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

/// Validate config `program` against any `import schema "..."` declarations it contains.
/// Loads each schema file, verifies it has @SchemaFile, then checks all sections.
///
/// On success, also returns every config section's schema-derived field
/// list, keyed by section name. The typechecker uses this to exempt
/// schema-bound sections from the "every field needs an explicit type"
/// rule that applies to sections with neither a `-> Type` binding nor a
/// schema — the schema already tells it each field's expected shape.
pub fn validate_schema_imports(
    program: &crate::ast::Program,
    base_dir: &std::path::Path,
) -> Result<std::collections::HashMap<String, Vec<crate::ast::SchemaField>>, Vec<crate::error::SparError>> {
    use crate::ast::TopLevelItem;
    use crate::error::SparError;

    let mut errors: Vec<SparError> = Vec::new();
    let mut bindings: std::collections::HashMap<String, Vec<crate::ast::SchemaField>> =
        std::collections::HashMap::new();

    // Build config section map once — it is the same for every schema import.
    let mut config_sections: std::collections::HashMap<String, &crate::ast::SectionDecl> =
        std::collections::HashMap::new();
    for cfg_item in &program.items {
        if let TopLevelItem::Section(s) = cfg_item {
            if s.private { continue; }  // private sections are never emitted; skip schema validation
            if let Some(name) = s.path.first() {
                config_sections.insert(name.clone(), s);
            }
        }
    }

    // Fix 1: collect ALL schema section names across ALL imports before running Rule 2.
    // With multiple `import schema` lines, each schema only knows about its own sections;
    // checking Rule 2 inside the per-import loop would flag sections from schema B as
    // "undeclared" while processing schema A.
    let mut combined_schema_section_names: HashSet<String> = HashSet::new();
    let mut has_schema_imports = false;

    for item in &program.items {
        let TopLevelItem::Import(decl) = item else { continue };
        if !matches!(decl.kind, crate::ast::ImportKind::Schema) { continue; }
        has_schema_imports = true;

        // Resolve and load the schema file
        let full_path = base_dir.join(&decl.path);
        let schema_src = match std::fs::read_to_string(&full_path) {
            Ok(s) => s,
            Err(e) => {
                errors.push(SparError::SchemaError {
                    message: format!("cannot read schema file '{}': {}", decl.path, e),
                    span: decl.span.clone(),
                });
                continue;
            }
        };

        let schema_tokens = match crate::lexer::Lexer::new(&schema_src).tokenize() {
            Ok(t) => t,
            Err(e) => {
                errors.push(SparError::SchemaError {
                    message: format!("schema file '{}' has a lex error: {}", decl.path, e),
                    span: decl.span.clone(),
                });
                continue;
            }
        };

        let mut schema_prog = match crate::parser::Parser::new(schema_tokens).parse() {
            Ok(p) => p,
            Err(e) => {
                errors.push(SparError::SchemaError {
                    message: format!("schema file '{}' has a parse error: {}", decl.path, e),
                    span: decl.span.clone(),
                });
                continue;
            }
        };

        if !schema_prog.is_schema_file {
            errors.push(SparError::SchemaError {
                message: format!(
                    "'{}' is not a schema file — add `@SchemaFile` at the top of that file",
                    decl.path
                ),
                span: decl.span.clone(),
            });
            continue;
        }

        // Resolve `import type {...}` inside the schema file so any
        // `SchemaFrom` below has real `TypeDecl`s to convert.
        let schema_file_base = full_path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
        let mut schema_expand_loader = ImportLoader::new(&schema_file_base);
        if let Err(es) = expand_imports(&mut schema_prog, &mut schema_expand_loader) {
            errors.extend(es.into_iter().map(|e| match e {
                SparError::ResolveError { message, span, .. } => SparError::SchemaError { message, span },
                other => other,
            }));
            continue;
        }

        if let Err(es) = expand_schema_from(&mut schema_prog) {
            errors.extend(es);
            continue;
        }

        // Build schema section map for this import: name → (optional, fields)
        let mut schema_sections: std::collections::HashMap<
            String,
            (bool, &Vec<crate::ast::SchemaField>)
        > = std::collections::HashMap::new();

        for schema_item in &schema_prog.items {
            if let TopLevelItem::SchemaSection(s) = schema_item {
                schema_sections.insert(s.name.clone(), (s.marker.optional, &s.fields));
            }
        }

        // Accumulate names into the combined set for Rule 2 (checked after the loop).
        for name in schema_sections.keys() {
            combined_schema_section_names.insert(name.clone());
        }

        // Rule 1: every required schema section must have a matching config section
        for (name, (optional, schema_fields)) in &schema_sections {
            match config_sections.get(name) {
                None if !optional => {
                    errors.push(SparError::SchemaError {
                        message: format!(
                            "schema '{}' requires section `[{}]` but it is missing from the config",
                            decl.path, name
                        ),
                        span: decl.span.clone(),
                    });
                }
                None => {} // optional section, fine to omit
                Some(cfg_section) => {
                    bindings.insert(name.clone(), (*schema_fields).clone());
                    // Fix 2: skip field-level validation for sections that contain spread items.
                    // Spreads are resolved at runtime; we cannot statically know which fields
                    // they contribute, so a "missing required field" error would be a false positive.
                    let has_spreads = cfg_section.items.iter()
                        .any(|i| matches!(i, crate::ast::SectionItem::Spread(_)));
                    if !has_spreads {
                        let config_fields: Vec<&crate::ast::FieldDecl> = cfg_section.items.iter()
                            .filter_map(|i| {
                                if let crate::ast::SectionItem::Field(f) = i { Some(f) } else { None }
                            })
                            .collect();
                        validate_fields(
                            schema_fields,
                            &config_fields,
                            name,
                            &mut errors,
                            &cfg_section.span,
                        );
                    }
                }
            }
        }
    }

    // Rule 2 (Fix 1): check config sections against the COMBINED set of all schema section
    // names, so that sections declared in schema B are not falsely rejected while processing
    // schema A.  Only runs when at least one `import schema` is present.
    if has_schema_imports {
        for (name, cfg_section) in &config_sections {
            if !combined_schema_section_names.contains(name) {
                errors.push(SparError::SchemaError {
                    message: format!(
                        "section `[{}]` is not declared in any imported schema",
                        name
                    ),
                    span: cfg_section.span.clone(),
                });
            }
        }
    }

    if errors.is_empty() { Ok(bindings) } else { Err(errors) }
}

fn validate_fields(
    schema_fields: &[crate::ast::SchemaField],
    config_fields: &[&crate::ast::FieldDecl],
    section_path: &str,
    errors: &mut Vec<crate::error::SparError>,
    section_span: &crate::error::Span,
) {
    use crate::ast::{SchemaFieldShape, FieldValue, SparType};
    use crate::error::SparError;

    // Check: every required schema field is present with correct type
    for sf in schema_fields {
        let cf = config_fields.iter().find(|f| f.name == sf.name);
        match cf {
            None if !sf.optional => {
                errors.push(SparError::SchemaError {
                    message: format!(
                        "section `{}` is missing required field `{}`",
                        section_path, sf.name
                    ),
                    span: section_span.clone(),
                });
            }
            None => {} // optional, fine to omit
            Some(cf) => {
                match &sf.shape {
                    SchemaFieldShape::Primitive(expected_ty) => {
                        match &cf.ty {
                            // Type omitted (inferred from a `-> TypeName`
                            // binding) — can't statically verify it here
                            // without re-deriving inference; the
                            // type-binding's own typechecker pass already
                            // covers this field. Same "can't statically
                            // know" precedent as the has_spreads skip
                            // below.
                            None => {}
                            Some(actual_ty) => {
                                if actual_ty != expected_ty {
                                    errors.push(SparError::SchemaError {
                                        message: format!(
                                            "field `{}::{}` declared as `{}` but schema expects `{}`",
                                            section_path,
                                            sf.name,
                                            kl_type_name(actual_ty),
                                            kl_type_name(expected_ty),
                                        ),
                                        span: cf.span.clone(),
                                    });
                                }
                            }
                        }
                    }
                    SchemaFieldShape::Section(nested_schema) => {
                        // A field is a nested section if its value is
                        // FieldValue::Nested, regardless of whether its
                        // type is explicit (Some(Section)) or inferred
                        // (None, from a `-> TypeName` binding).
                        let explicit_non_section =
                            matches!(&cf.ty, Some(ty) if *ty != SparType::Section);
                        if explicit_non_section {
                            errors.push(SparError::SchemaError {
                                message: format!(
                                    "field `{}::{}` must be type `section` (schema requires a nested section)",
                                    section_path, sf.name
                                ),
                                span: cf.span.clone(),
                            });
                        } else {
                            let nested_items: &[crate::ast::SectionItem] = match &cf.value {
                                Some(FieldValue::Nested(items)) => items,
                                _ => {
                                    errors.push(SparError::SchemaError {
                                        message: format!(
                                            "field `{}::{}` must have an inline section value `= {{ ... }}`",
                                            section_path, sf.name
                                        ),
                                        span: cf.span.clone(),
                                    });
                                    continue;
                                }
                            };
                            // A spread's contributed fields can't be statically
                            // known here — same "can't verify" precedent as the
                            // has_spreads skip above, one level deeper.
                            let has_spreads = nested_items.iter()
                                .any(|i| matches!(i, crate::ast::SectionItem::Spread(_)));
                            if !has_spreads {
                                let nested_config: Vec<&crate::ast::FieldDecl> = nested_items.iter()
                                    .filter_map(|i| if let crate::ast::SectionItem::Field(f) = i { Some(f) } else { None })
                                    .collect();
                                let nested_path = format!("{}::{}", section_path, sf.name);
                                validate_fields(nested_schema, &nested_config, &nested_path, errors, &cf.span);
                            }
                        }
                    }
                }
            }
        }
    }

    // Check: no extra config fields beyond what the schema declares
    for cf in config_fields {
        if !schema_fields.iter().any(|sf| sf.name == cf.name) {
            errors.push(SparError::SchemaError {
                message: format!(
                    "field `{}::{}` is not declared in the schema",
                    section_path, cf.name
                ),
                span: cf.span.clone(),
            });
        }
    }
}

fn kl_type_name(ty: &crate::ast::SparType) -> String {
    match ty {
        crate::ast::SparType::Str     => "str".to_string(),
        crate::ast::SparType::Int     => "int".to_string(),
        crate::ast::SparType::Float   => "float".to_string(),
        crate::ast::SparType::Bool    => "bool".to_string(),
        crate::ast::SparType::Section => "section".to_string(),
        crate::ast::SparType::List(_) => "list".to_string(),
        crate::ast::SparType::Named(name) => name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use crate::ast::TopLevelItem;

    fn parse_src(src: &str) -> Program {
        let tokens = crate::lexer::Lexer::new(src).tokenize().unwrap();
        crate::parser::Parser::new(tokens).parse().unwrap()
    }

    #[test]
    fn import_nonexistent_file_produces_error() {
        let dir = tempdir().unwrap();
        let src = r#"import "absolutely_does_not_exist.spar" as cfg;"#;
        let program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let result = collect_imports(&program, &mut loader);
        assert!(result.is_err(), "missing import file must produce an error");
        let errs = result.unwrap_err();
        assert!(
            errs.iter().any(|e| matches!(e,
                SparError::ResolveError { message, .. } if message.contains("does not exist")
            )),
            "error must explain that the file was not found, got: {:?}", errs
        );
    }

    #[test]
    fn import_existing_file_succeeds() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("db.spar"),
            r#"export var host: str = "localhost";"#,
        ).unwrap();
        let src = r#"import "db.spar" as db;"#;
        let program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let result = collect_imports(&program, &mut loader);
        assert!(result.is_ok(), "existing import must succeed, got: {:?}", result.err());
    }

    #[test]
    fn import_existing_file_exposes_exports() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            r#"export var version: str = "1.0"; var internal: int = 42;"#,
        ).unwrap();
        let src = r#"import "shared.spar" as shared;"#;
        let program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let loaded = collect_imports(&program, &mut loader).unwrap();
        let imp = &loaded["shared"];
        assert!(imp.exports.contains("version"), "exported var must appear in exports");
        assert!(!imp.exports.contains("internal"), "non-exported var must not appear");
    }

    #[test]
    fn import_existing_file_exposes_function_group_exports() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            r#"
                functionGroup EdgeInsect { function only() -> int { return 1; } }
                private functionGroup Hidden { function f() -> int { return 1; } }
            "#,
        ).unwrap();
        let src = r#"import "shared.spar" as shared;"#;
        let program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let loaded = collect_imports(&program, &mut loader).unwrap();
        let imp = &loaded["shared"];
        assert!(imp.exports.contains("EdgeInsect"), "got: {:?}", imp.exports);
        assert!(!imp.exports.contains("Hidden"), "got: {:?}", imp.exports);
    }

    #[test]
    fn import_existing_file_exposes_exported_type() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            "export type [PostgresType]{ image: str; }\ntype [Internal]{ a: int; }\n",
        ).unwrap();
        let src = r#"import "shared.spar" as shared;"#;
        let program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let loaded = collect_imports(&program, &mut loader).unwrap();
        let imp = &loaded["shared"];
        assert!(imp.exports.contains("PostgresType"), "exported type must appear in exports");
        assert!(!imp.exports.contains("Internal"), "non-exported type must not appear");
    }

    #[test]
    fn private_section_not_validated_against_schema() {
        use std::fs;
        let dir = tempdir().unwrap();

        // Schema declares only [Server]
        fs::write(dir.path().join("schema.spar"), concat!(
            "@SchemaFile\n",
            "Schema [Server]{ port: int; }\n",
        )).unwrap();

        // Config has [Server] (public) and private [Defaults]
        let src = concat!(
            "import schema \"schema.spar\";\n",
            "[Server] { port: int = 8080; };\n",
            "private [Defaults] { timeout: int = 30; };\n",
        );
        let program = parse_src(src);
        let result = validate_schema_imports(&program, dir.path());
        assert!(result.is_ok(), "private section must not be validated against schema, got: {:?}", result.err());
    }

    // ── Phase 3: expand_imports (selective import) ──────────────────────

    #[test]
    fn expand_imports_splices_selective_section_and_var() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            concat!(
                "export var version: str = \"1.0\";\n",
                "export [Server]{ port: int = 8080; };\n",
                "var internal: int = 42;\n",
            ),
        ).unwrap();
        let src = r#"import { version, Server } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        assert!(program.items.iter().all(|it| !matches!(it, TopLevelItem::Import(_))),
            "the consumed import decl must be removed");
        assert!(program.items.iter().any(|it| matches!(it, TopLevelItem::Var(v) if v.name == "version")));
        assert!(program.items.iter().any(|it| matches!(it, TopLevelItem::Section(s) if s.path == vec!["Server".to_string()])));
    }

    #[test]
    fn expand_imports_selective_items_lose_exported_flag() {
        // Regression: a section/var/function/type pulled in via selective
        // import must NOT keep the `exported`/`is_private` flags it had in
        // its own source file — otherwise it's indistinguishable from a
        // declaration the importing file made and exported itself, and
        // silently becomes part of the importing file's own emit output.
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            concat!(
                "export var version: str = \"1.0\";\n",
                "export [Server]{ port: int = 8080; };\n",
                "function greet() -> str { return \"hi\"; }\n",
                "export type [PostgresType]{ image: str; }\n",
            ),
        ).unwrap();
        let src = r#"import { version, Server, greet, PostgresType } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        for item in &program.items {
            match item {
                TopLevelItem::Var(v) => assert!(!v.exported, "spliced var must not stay exported"),
                TopLevelItem::Section(s) => {
                    assert!(!s.exported, "spliced section must not stay exported");
                    assert!(s.private, "spliced section must become private (exempt from schema/emit)");
                }
                TopLevelItem::Function(f) => assert!(f.is_private, "spliced function must become private"),
                TopLevelItem::Type(t) => assert!(!t.exported, "spliced type must not stay exported"),
                _ => {}
            }
        }
    }

    #[test]
    fn expand_imports_selective_item_span_points_at_import_statement() {
        // Regression: Span has no file identity, so a spliced item's
        // ORIGINAL span (from the source file) renders against the
        // importing file's text and lands on an unrelated, misleading
        // line. The spliced item's top-level span must be retagged to its
        // OWN name in the `import { ... }` brace list — not the whole
        // import line — so multiple names each get their own position.
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("shared.spar"), "export var greeting: str = \"hi\";\n").unwrap();
        let src = r#"import { greeting } from "shared.spar";"#;
        let mut program = parse_src(src);
        let item_span = match &program.items[0] {
            TopLevelItem::Import(d) => match &d.kind {
                crate::ast::ImportKind::Selective(items) => items[0].span.clone(),
                other => panic!("expected Selective, got {:?}", other),
            },
            other => panic!("expected Import, got {:?}", other),
        };
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        let spliced_span = program.items.iter().find_map(|it| {
            if let TopLevelItem::Var(v) = it { Some(v.span.clone()) } else { None }
        }).expect("expected a spliced var");
        assert_eq!(spliced_span, item_span,
            "spliced item's span must point at its own name in the import braces, not the source file's coordinates");
    }

    #[test]
    fn expand_imports_multiple_selective_items_get_distinct_spans() {
        // Two names in one `import { A, B }` must not collapse to the
        // same position — each gets its own item's span.
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            "export var a: str = \"a\";\nexport var b: str = \"b\";\n",
        ).unwrap();
        let src = r#"import { a, b } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        let a_span = program.items.iter().find_map(|it| {
            if let TopLevelItem::Var(v) = it { if v.name == "a" { return Some(v.span.clone()); } }
            None
        }).expect("expected spliced var a");
        let b_span = program.items.iter().find_map(|it| {
            if let TopLevelItem::Var(v) = it { if v.name == "b" { return Some(v.span.clone()); } }
            None
        }).expect("expected spliced var b");
        assert_ne!(a_span, b_span, "each spliced item must get its own distinct span");
    }

    #[test]
    fn expand_imports_applies_as_rename() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            "export type [PostgresType]{ image: str; }\n",
        ).unwrap();
        let src = r#"import { PostgresType as PgType } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        assert!(program.items.iter().any(|it| matches!(it, TopLevelItem::Type(t) if t.name == "PgType")));
        assert!(!program.items.iter().any(|it| matches!(it, TopLevelItem::Type(t) if t.name == "PostgresType")));
    }

    #[test]
    fn expand_imports_rejects_non_exported_name() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("shared.spar"), "var internal: int = 42;\n").unwrap();
        let src = r#"import { internal } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let err = expand_imports(&mut program, &mut loader).unwrap_err();
        assert!(err.iter().any(|e| matches!(e, SparError::ResolveError { message, .. } if message.contains("not exported"))),
            "got: {:?}", err);
    }

    #[test]
    fn expand_imports_rejects_collision_with_existing_declaration() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("shared.spar"), "export var host: str = \"remote\";\n").unwrap();
        let src = concat!(
            "var host: str = \"local\";\n",
            "import { host } from \"shared.spar\";\n",
        );
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let err = expand_imports(&mut program, &mut loader).unwrap_err();
        assert!(err.iter().any(|e| matches!(e, SparError::ResolveError { message, .. } if message.contains("already"))),
            "got: {:?}", err);
    }

    // ── Phase 3: expand_imports (asPartOf) ───────────────────────────────

    #[test]
    fn expand_imports_as_part_of_inlines_target_file() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("common.spar"),
            concat!(
                "export var host: str = \"localhost\";\n",
                "private [Defaults]{ timeout: int = 30; };\n",
            ),
        ).unwrap();
        let src = r#"import asPartOf "common.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        assert!(program.items.iter().any(|it| matches!(it, TopLevelItem::Var(v) if v.name == "host")));
        assert!(program.items.iter().any(|it| matches!(it, TopLevelItem::Section(s) if s.private)),
            "private sections must be pulled in too — true textual inclusion, not a namespaced import");
    }

    #[test]
    fn expand_imports_as_part_of_is_transitive() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("base.spar"), "export var version: str = \"1.0\";\n").unwrap();
        fs::write(
            dir.path().join("middle.spar"),
            "import asPartOf \"base.spar\";\nexport var name: str = \"mid\";\n",
        ).unwrap();
        let src = r#"import asPartOf "middle.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        assert!(program.items.iter().any(|it| matches!(it, TopLevelItem::Var(v) if v.name == "version")),
            "transitively-included file's declarations must flatten in too");
        assert!(program.items.iter().any(|it| matches!(it, TopLevelItem::Var(v) if v.name == "name")));
    }

    #[test]
    fn expand_imports_as_part_of_detects_direct_cycle() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("a.spar"), "import asPartOf \"b.spar\";\n").unwrap();
        fs::write(dir.path().join("b.spar"), "import asPartOf \"a.spar\";\n").unwrap();
        let src = r#"import asPartOf "a.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let err = expand_imports(&mut program, &mut loader).unwrap_err();
        assert!(err.iter().any(|e| matches!(e, SparError::ResolveError { message, .. } if message.contains("cycle"))),
            "got: {:?}", err);
    }

    // ── Phase 3: import type (Task 4) ────────────────────────────────────

    #[test]
    fn expand_imports_type_selective_rejects_non_type_name() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("shared.spar"), "export var host: str = \"x\";\n").unwrap();
        let src = r#"import type { host } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let err = expand_imports(&mut program, &mut loader).unwrap_err();
        assert!(err.iter().any(|e| matches!(e, SparError::ResolveError { message, .. } if message.contains("is not a type"))),
            "got: {:?}", err);
    }

    #[test]
    fn expand_imports_type_selective_transitively_pulls_dependent_type() {
        // Regression: `import type { FlutterType }` must silently bring in
        // `Libs` too — FlutterType's own field (`packages?: Libs;`) needs
        // it, and the caller never asked to import Libs directly.
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("types.spar"),
            concat!(
                "export type [Libs]{ dependencies?: [str]; }\n",
                "export type [FlutterType]{ projectName: str; packages?: Libs; }\n",
            ),
        ).unwrap();
        let src = r#"import type { FlutterType } from "types.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        let has_libs = program.items.iter().any(|it| matches!(
            it, TopLevelItem::Type(t) if t.name == "Libs"
        ));
        assert!(has_libs, "Libs must be transitively spliced in, got items: {:?}", program.items);
    }

    #[test]
    fn expand_imports_type_selective_can_import_an_enum_directly() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("types.spar"),
            "export enum Protocol { Http, Https };\n",
        ).unwrap();
        let src = r#"import type { Protocol } from "types.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        let has_protocol = program.items.iter().any(|it| matches!(
            it, TopLevelItem::Enum(e) if e.name == "Protocol"
        ));
        assert!(has_protocol, "Protocol enum must be spliced in, got items: {:?}", program.items);
    }

    #[test]
    fn expand_imports_type_selective_transitively_pulls_dependent_enum() {
        // A type's field can reference an enum, not just another type —
        // the transitive pull must handle both.
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("types.spar"),
            concat!(
                "export enum RestartPolicy { Always, Never };\n",
                "export type [Container]{ name: str; restart: RestartPolicy; }\n",
            ),
        ).unwrap();
        let src = r#"import type { Container } from "types.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        let has_restart_policy = program.items.iter().any(|it| matches!(
            it, TopLevelItem::Enum(e) if e.name == "RestartPolicy"
        ));
        assert!(has_restart_policy, "RestartPolicy enum must be transitively spliced in, got items: {:?}", program.items);
    }

    // ── Phase 3: import type inside @SchemaFile (Task 5) ─────────────────

    #[test]
    fn validate_schema_imports_resolves_import_type_inside_schema_file() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("types.spar"),
            "export type [PostgresType]{ image: str; }\n",
        ).unwrap();
        fs::write(
            dir.path().join("schema.spar"),
            concat!(
                "@SchemaFile\n",
                "import type { PostgresType } from \"types.spar\";\n",
                "Schema [Postgres]{ image: str; }\n",
            ),
        ).unwrap();
        let src = concat!(
            "import schema \"schema.spar\";\n",
            "[Postgres]{ image: str = \"postgres:16\"; };\n",
        );
        let program = parse_src(src);
        let result = validate_schema_imports(&program, dir.path());
        assert!(result.is_ok(), "got: {:?}", result.err());
    }

    #[test]
    fn selectively_imported_section_is_not_checked_against_schema() {
        // Direct regression for the reported bug: `import { Colors } from
        // "lib.spar";` (a section exported by lib.spar for cross-file
        // reuse, e.g. `Colors::red`) must NOT be treated as one of the
        // importing file's own top-level config sections — it should be
        // exempt from schema Rule 2 ("every section must be declared in
        // the schema"), the same way a `private [Section]` already is.
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("lib.spar"),
            "export [Colors]{ red: str = \"#ff0000\"; };\n",
        ).unwrap();
        fs::write(
            dir.path().join("schema.spar"),
            "@SchemaFile\nSchema [Container]{ x?: str; }\n",
        ).unwrap();
        let src = concat!(
            "import schema \"schema.spar\";\n",
            "import { Colors } from \"lib.spar\";\n",
            "[Container]{\n",
            "    x: str = Colors::red;\n",
            "};\n",
        );
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        let result = validate_schema_imports(&program, dir.path());
        assert!(result.is_ok(), "got: {:?}", result.err());
    }

    // ── Phase 3: SchemaFrom (Task 6) ──────────────────────────────────────

    #[test]
    fn expand_schema_from_generates_equivalent_schema_section() {
        // Schema files can't declare `type` per the parser's own rule —
        // this test exercises `expand_schema_from` directly against a
        // hand-built Program rather than going through the parser, since
        // the type here stands in for one that arrived via `import type`.
        let mut program = Program {
            is_schema_file: true,
            items: vec![
                TopLevelItem::Type(crate::ast::TypeDecl {
                    name: "PostgresType".into(),
                    name_span: crate::error::Span::dummy(),
                    exported: true,
                    fields: vec![
                        crate::ast::TypeField {
                            name: "image".into(),
                            optional: false,
                            shape: crate::ast::TypeFieldShape::Primitive(crate::ast::SparType::Str),
                            span: crate::error::Span::dummy(),
                        },
                        crate::ast::TypeField {
                            name: "port".into(),
                            optional: true,
                            shape: crate::ast::TypeFieldShape::Primitive(crate::ast::SparType::Int),
                            span: crate::error::Span::dummy(),
                        },
                    ],
                    span: crate::error::Span::dummy(),
                }),
                TopLevelItem::SchemaFrom(crate::ast::SchemaFromDecl {
                    name: "Postgres".into(),
                    source_type: "PostgresType".into(),
                    source_type_span: crate::error::Span::dummy(),
                    marker: crate::ast::SchemaMarker { optional: false },
                    span: crate::error::Span::dummy(),
                }),
            ],
        };
        expand_schema_from(&mut program).expect("expand must succeed");

        assert!(!program.items.iter().any(|it| matches!(it, TopLevelItem::SchemaFrom(_))));
        let generated = program.items.iter().find_map(|it| {
            if let TopLevelItem::SchemaSection(s) = it { Some(s) } else { None }
        }).expect("SchemaFrom must generate a SchemaSection");
        assert_eq!(generated.name, "Postgres");
        assert!(!generated.marker.optional);
        assert_eq!(generated.fields.len(), 2);
        assert!(generated.fields.iter().any(|f| f.name == "image" && !f.optional));
        assert!(generated.fields.iter().any(|f| f.name == "port" && f.optional));
    }

    #[test]
    fn expand_schema_from_expands_named_type_reference_recursively() {
        let mut program = Program {
            is_schema_file: true,
            items: vec![
                TopLevelItem::Type(crate::ast::TypeDecl {
                    name: "Border".into(),
                    name_span: crate::error::Span::dummy(),
                    exported: true,
                    fields: vec![crate::ast::TypeField {
                        name: "width".into(),
                        optional: false,
                        shape: crate::ast::TypeFieldShape::Primitive(crate::ast::SparType::Int),
                        span: crate::error::Span::dummy(),
                    }],
                    span: crate::error::Span::dummy(),
                }),
                TopLevelItem::Type(crate::ast::TypeDecl {
                    name: "Decoration".into(),
                    name_span: crate::error::Span::dummy(),
                    exported: true,
                    fields: vec![crate::ast::TypeField {
                        name: "border".into(),
                        optional: false,
                        shape: crate::ast::TypeFieldShape::Named("Border".into()),
                        span: crate::error::Span::dummy(),
                    }],
                    span: crate::error::Span::dummy(),
                }),
                TopLevelItem::SchemaFrom(crate::ast::SchemaFromDecl {
                    name: "Deco".into(),
                    source_type: "Decoration".into(),
                    source_type_span: crate::error::Span::dummy(),
                    marker: crate::ast::SchemaMarker { optional: true },
                    span: crate::error::Span::dummy(),
                }),
            ],
        };
        expand_schema_from(&mut program).expect("expand must succeed");

        let generated = program.items.iter().find_map(|it| {
            if let TopLevelItem::SchemaSection(s) = it { Some(s) } else { None }
        }).expect("SchemaFrom must generate a SchemaSection");
        assert!(generated.marker.optional);
        let border_field = generated.fields.iter().find(|f| f.name == "border").expect("border field");
        match &border_field.shape {
            crate::ast::SchemaFieldShape::Section(nested) => {
                assert!(nested.iter().any(|f| f.name == "width"));
            }
            other => panic!("expected nested Section shape, got {:?}", other),
        }
    }

    #[test]
    fn expand_schema_from_errors_on_undeclared_type() {
        let mut program = Program {
            is_schema_file: true,
            items: vec![
                TopLevelItem::SchemaFrom(crate::ast::SchemaFromDecl {
                    name: "Postgres".into(),
                    source_type: "NoSuchType".into(),
                    source_type_span: crate::error::Span::dummy(),
                    marker: crate::ast::SchemaMarker { optional: false },
                    span: crate::error::Span::dummy(),
                }),
            ],
        };
        let err = expand_schema_from(&mut program).unwrap_err();
        assert!(err.iter().any(|e| matches!(e, SparError::SchemaError { message, .. } if message.contains("NoSuchType"))),
            "got: {:?}", err);
    }

    #[test]
    fn validate_schema_imports_accepts_config_matching_schema_from() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("types.spar"),
            "export type [PostgresType]{ image: str; }\n",
        ).unwrap();
        fs::write(
            dir.path().join("schema.spar"),
            concat!(
                "@SchemaFile\n",
                "import type { PostgresType } from \"types.spar\";\n",
                "SchemaFrom [Postgres, PostgresType];\n",
            ),
        ).unwrap();
        let src = concat!(
            "import schema \"schema.spar\";\n",
            "[Postgres]{ image: str = \"postgres:16\"; };\n",
        );
        let program = parse_src(src);
        let result = validate_schema_imports(&program, dir.path());
        assert!(result.is_ok(), "got: {:?}", result.err());
    }

    #[test]
    fn validate_schema_imports_accepts_enum_typed_field_via_schema_from() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("types.spar"),
            concat!(
                "export enum RestartPolicy { Always, Never };\n",
                "export type [ServiceType]{ image: str; restart: RestartPolicy; }\n",
            ),
        ).unwrap();
        fs::write(
            dir.path().join("schema.spar"),
            concat!(
                "@SchemaFile\n",
                "import type { ServiceType } from \"types.spar\";\n",
                "SchemaFrom [Service, ServiceType];\n",
            ),
        ).unwrap();
        let src = concat!(
            "import schema \"schema.spar\";\n",
            "import type { RestartPolicy } from \"types.spar\";\n",
            "[Service]{ image: str = \"nginx\"; restart: RestartPolicy = RestartPolicy::Always; };\n",
        );
        let program = parse_src(src);
        let result = validate_schema_imports(&program, dir.path());
        assert!(result.is_ok(), "got: {:?}", result.err());
    }

    #[test]
    fn validate_schema_imports_rejects_config_missing_schema_from_field() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("types.spar"),
            "export type [PostgresType]{ image: str; port: int; }\n",
        ).unwrap();
        fs::write(
            dir.path().join("schema.spar"),
            concat!(
                "@SchemaFile\n",
                "import type { PostgresType } from \"types.spar\";\n",
                "SchemaFrom [Postgres, PostgresType];\n",
            ),
        ).unwrap();
        let src = concat!(
            "import schema \"schema.spar\";\n",
            "[Postgres]{ image: str = \"postgres:16\"; };\n", // missing required `port`
        );
        let program = parse_src(src);
        let result = validate_schema_imports(&program, dir.path());
        assert!(result.is_err());
    }
}
