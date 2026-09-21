use crate::ast::Program;
use crate::error::SparError;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct LoadedImport {
    pub path: String,
    pub exports: HashSet<String>,
    pub functions: HashMap<String, crate::ast::FunctionDecl>,
    /// Package-resolution scope that applies while evaluating this imported
    /// module. Local modules inherit their caller's scope; package modules
    /// receive a locator scoped to the resolved dependency so their own
    /// `import pkg ...` statements follow that dependency's lockfile edges.
    pub locator: Option<crate::package::ModuleLocator>,
    /// Where `path` actually resolved to on disk — a plain
    /// `base_dir.join(path)` for a filesystem import, or a store
    /// snapshot path for an explicit package import resolved through a
    /// `ModuleLocator`. Evaluation reads this directly instead of
    /// re-deriving it from `path`, so it only ever needs to know how to
    /// resolve an import once.
    pub resolved_path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ResolvedImportSource {
    pub path: PathBuf,
    pub locator: Option<crate::package::ModuleLocator>,
}

pub struct ImportLoader {
    base_dir: PathBuf,
    locator: Option<crate::package::ModuleLocator>,
    bundled_packages: crate::compiler::BundledPackageRoots,
    /// Import the `std/data` functions the program neither declares nor
    /// imports itself (see `CompileOptions::data_prelude`).
    data_prelude: bool,
}

impl ImportLoader {
    pub fn new(base: &Path) -> Self {
        Self {
            base_dir: base.to_path_buf(),
            locator: None,
            bundled_packages: crate::compiler::BundledPackageRoots::default(),
            data_prelude: false,
        }
    }

    pub fn with_data_prelude(mut self, enabled: bool) -> Self {
        self.data_prelude = enabled;
        self
    }

    /// Makes a bare (non-filesystem-looking) import string resolve
    /// through `locator`'s package lock/store instead of failing as a
    /// missing file — chainable so existing `ImportLoader::new(...)`
    /// call sites are unaffected.
    pub fn with_locator(mut self, locator: crate::package::ModuleLocator) -> Self {
        self.locator = Some(locator);
        self
    }

    pub fn with_bundled_packages(
        mut self,
        bundled_packages: crate::compiler::BundledPackageRoots,
    ) -> Self {
        self.bundled_packages = bundled_packages;
        self
    }

    /// Resolves one import declaration to a concrete Spar source file and
    /// the package-resolution scope that applies inside that file.
    ///
    /// New code should make package intent explicit with `import pkg ...`.
    /// Local/module imports are resolved relative to `base_dir`, gain a
    /// `.spar` suffix automatically when omitted, and inherit the caller's
    /// package-resolution scope. Package dependencies are never guessed from
    /// a normal import; source must opt in with `import pkg ...`.
    pub fn resolve_import(
        &self,
        decl: &crate::ast::ImportDecl,
    ) -> Result<ResolvedImportSource, SparError> {
        if decl.package {
            if let Some(path) = crate::stdlib::resolve_bundled_import(&decl.path) {
                return Ok(ResolvedImportSource {
                    path,
                    locator: self.locator.clone(),
                });
            }
            if decl.path == "std" || decl.path.starts_with("std/") {
                return Err(SparError::ResolveError {
                    message: format!(
                        "cannot resolve bundled standard-library module '{}'",
                        decl.path
                    ),
                    hint: Some("check the std module path; bundled std imports cannot escape the std package".into()),
                    span: decl.span.clone(),
                });
            }
            if let Some(path) = self.bundled_packages.resolve(&decl.path) {
                if !path.is_file() {
                    return Err(SparError::ResolveError {
                        message: format!("cannot resolve bundled package module '{}'", decl.path),
                        hint: Some("check the bundled package module path".into()),
                        span: decl.span.clone(),
                    });
                }
                return Ok(ResolvedImportSource {
                    path,
                    locator: self.locator.clone(),
                });
            }
            if self.bundled_packages.contains_package(&decl.path) {
                return Err(SparError::ResolveError {
                    message: format!("cannot resolve bundled package module '{}'", decl.path),
                    hint: Some(
                        "bundled package imports cannot escape their registered source root".into(),
                    ),
                    span: decl.span.clone(),
                });
            }
            let locator = self
                .locator
                .as_ref()
                .ok_or_else(|| SparError::ResolveError {
                    message: format!(
                        "cannot resolve package import '{}' — no package lock/store is configured",
                        decl.path
                    ),
                    hint: Some(
                        "run `spar install` in a project with spar.package.spar, then try again"
                            .into(),
                    ),
                    span: decl.span.clone(),
                })?;
            return locator
                .resolve_import_scoped(&self.base_dir, &decl.path)
                .map(|(path, scoped)| ResolvedImportSource {
                    path,
                    locator: Some(scoped),
                })
                .ok_or_else(|| SparError::ResolveError {
                    message: format!(
                        "cannot resolve package import '{}' — dependency or module was not found",
                        decl.path
                    ),
                    hint: Some(
                        "check [Dependencies], run `spar install`, and verify the package module path"
                            .into(),
                    ),
                    span: decl.span.clone(),
                });
        }

        Ok(ResolvedImportSource {
            path: local_module_path(&self.base_dir, &decl.path),
            locator: self.locator.clone(),
        })
    }
}

fn local_module_path(base_dir: &Path, raw: &str) -> PathBuf {
    let mut path = base_dir.join(raw);
    // Extensionless imports normally use the `.spar` convenience suffix, but
    // an explicitly existing extensionless path is still a valid file. This
    // matters for temporary files and generated configs whose exact path was
    // supplied by the caller.
    if path.extension().is_none() && !path.exists() {
        path.set_extension("spar");
    }
    path
}

/// Splice selective import targets into `program`'s own top-level items,
/// in place, before resolve/typecheck ever run. `import "path" as alias;`
/// and `import schema "path";` pass through untouched — they're still
/// handled by `collect_imports` / `validate_schema_imports`.
pub fn expand_imports(
    program: &mut Program,
    loader: &mut ImportLoader,
) -> Result<(), Vec<SparError>> {
    expand_imports_inner(program, loader)
}

fn expand_imports_inner(
    program: &mut Program,
    loader: &mut ImportLoader,
) -> Result<(), Vec<SparError>> {
    use crate::ast::{ImportKind, TopLevelItem};

    let mut errors: Vec<SparError> = Vec::new();
    let mut declared: HashSet<String> = program
        .items
        .iter()
        .filter_map(top_level_name)
        .map(|s| s.to_string())
        .collect();

    if loader.data_prelude {
        add_data_prelude(program, &declared);
    }

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
            ImportKind::TypeSelective(requested) => {
                splice_selective(&decl, requested, true, loader)
            }
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
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Prepends `import pkg { ... } from "std/data";` for every `std/data`
/// function the program does not already declare or import. The program's own
/// names always win, wherever they appear, so a later `var count` at an
/// interactive prompt never collides with the prelude.
fn add_data_prelude(program: &mut Program, declared: &HashSet<String>) {
    use crate::ast::{ImportDecl, ImportItem, ImportKind, TopLevelItem};

    let mut taken = declared.clone();
    for item in &program.items {
        match item {
            TopLevelItem::Enum(decl) => {
                taken.insert(decl.name.clone());
            }
            TopLevelItem::FunctionGroup(decl) => {
                taken.insert(decl.name.clone());
            }
            TopLevelItem::Import(ImportDecl {
                kind: ImportKind::Selective(requested) | ImportKind::TypeSelective(requested),
                ..
            }) => {
                for item in requested {
                    taken.insert(item.alias.clone().unwrap_or_else(|| item.name.clone()));
                }
            }
            _ => {}
        }
    }

    let requested = crate::stdlib::DATA_FUNCTIONS
        .iter()
        .filter(|name| !taken.contains(**name))
        .map(|name| ImportItem {
            name: (*name).to_string(),
            name_span: crate::error::Span::dummy(),
            alias: None,
            span: crate::error::Span::dummy(),
        })
        .collect::<Vec<_>>();
    if requested.is_empty() {
        return;
    }
    program.items.insert(
        0,
        TopLevelItem::Import(ImportDecl {
            path: "std/data".to_string(),
            package: true,
            kind: ImportKind::Selective(requested),
            span: crate::error::Span::dummy(),
        }),
    );
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
/// Rule 2 as "not declared in any imported schema."
fn localize_visibility(item: crate::ast::TopLevelItem) -> crate::ast::TopLevelItem {
    use crate::ast::TopLevelItem;
    match item {
        TopLevelItem::Var(mut v) => {
            v.exported = false;
            v.attributes.retain(|attribute| attribute.name != "emit");
            TopLevelItem::Var(v)
        }
        TopLevelItem::Section(mut s) => {
            s.exported = false;
            s.private = true;
            s.attributes.retain(|attribute| attribute.name != "emit");
            TopLevelItem::Section(s)
        }
        TopLevelItem::Function(mut f) => {
            f.is_private = true;
            TopLevelItem::Function(f)
        }
        TopLevelItem::Type(mut t) => {
            t.exported = false;
            TopLevelItem::Type(t)
        }
        TopLevelItem::Enum(mut e) => {
            e.exported = false;
            TopLevelItem::Enum(e)
        }
        TopLevelItem::FunctionGroup(mut g) => {
            g.is_private = true;
            TopLevelItem::FunctionGroup(g)
        }
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
fn retag_top_level_span(
    item: crate::ast::TopLevelItem,
    span: &crate::error::Span,
) -> crate::ast::TopLevelItem {
    use crate::ast::TopLevelItem;
    match item {
        TopLevelItem::Var(mut v) => {
            v.span = span.clone();
            TopLevelItem::Var(v)
        }
        TopLevelItem::Section(mut s) => {
            s.span = span.clone();
            TopLevelItem::Section(s)
        }
        TopLevelItem::Function(mut f) => {
            f.span = span.clone();
            f.name_span = span.clone();
            TopLevelItem::Function(f)
        }
        TopLevelItem::Type(mut t) => {
            t.span = span.clone();
            t.name_span = span.clone();
            TopLevelItem::Type(t)
        }
        TopLevelItem::Enum(mut e) => {
            e.span = span.clone();
            e.name_span = span.clone();
            TopLevelItem::Enum(e)
        }
        TopLevelItem::FunctionGroup(mut g) => {
            g.span = span.clone();
            g.name_span = span.clone();
            TopLevelItem::FunctionGroup(g)
        }
        other => other,
    }
}

fn rename_top_level_item(
    item: crate::ast::TopLevelItem,
    new_name: &str,
) -> crate::ast::TopLevelItem {
    use crate::ast::TopLevelItem;
    match item {
        TopLevelItem::Var(mut v) => {
            v.name = new_name.to_string();
            TopLevelItem::Var(v)
        }
        TopLevelItem::Section(mut s) => {
            if let Some(first) = s.path.first_mut() {
                *first = new_name.to_string();
            }
            TopLevelItem::Section(s)
        }
        TopLevelItem::Function(mut f) => {
            f.name = new_name.to_string();
            TopLevelItem::Function(f)
        }
        TopLevelItem::Type(mut t) => {
            t.name = new_name.to_string();
            TopLevelItem::Type(t)
        }
        TopLevelItem::Enum(mut e) => {
            e.name = new_name.to_string();
            TopLevelItem::Enum(e)
        }
        TopLevelItem::FunctionGroup(mut g) => {
            g.name = new_name.to_string();
            TopLevelItem::FunctionGroup(g)
        }
        other => other,
    }
}

fn impl_target_name(implementation: &crate::ast::ImplDecl) -> Option<&str> {
    match &implementation.target {
        crate::ast::SparType::Named(name) => Some(name.as_str()),
        crate::ast::SparType::Applied { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

fn rename_spar_type(ty: &mut crate::ast::SparType, from: &str, to: &str) {
    use crate::ast::SparType;
    match ty {
        SparType::Named(name) if name == from => *name = to.to_string(),
        SparType::Applied { name, arguments } => {
            if name == from {
                *name = to.to_string();
            }
            for argument in arguments {
                rename_spar_type(argument, from, to);
            }
        }
        SparType::List(inner) => rename_spar_type(inner, from, to),
        SparType::Function {
            params,
            return_type,
        } => {
            for param in params {
                rename_spar_type(param, from, to);
            }
            rename_spar_type(return_type, from, to);
        }
        _ => {}
    }
}

fn retarget_impl(
    mut implementation: crate::ast::ImplDecl,
    from: &str,
    to: &str,
) -> crate::ast::ImplDecl {
    rename_spar_type(&mut implementation.target, from, to);
    for method in &mut implementation.methods {
        for parameter in &mut method.function.params {
            rename_spar_type(&mut parameter.ty, from, to);
        }
        rename_spar_type(&mut method.function.ret, from, to);
    }
    implementation
}

fn splice_selective(
    decl: &crate::ast::ImportDecl,
    requested: &[crate::ast::ImportItem],
    types_only: bool,
    loader: &ImportLoader,
) -> Result<Vec<crate::ast::TopLevelItem>, Vec<SparError>> {
    use crate::ast::TopLevelItem;

    let resolved = loader.resolve_import(decl).map_err(|error| vec![error])?;
    let full_path = resolved.path.clone();
    if !full_path.exists() {
        return Err(vec![SparError::ResolveError {
            message: format!(
                "cannot find import file '{}' — file does not exist",
                decl.path
            ),
            hint: Some("check the file path and ensure it is relative to the current file".into()),
            span: decl.span.clone(),
        }]);
    }

    let src = std::fs::read_to_string(&full_path).map_err(|e| {
        vec![SparError::ResolveError {
            message: format!("cannot read import file '{}': {}", decl.path, e),
            hint: None,
            span: decl.span.clone(),
        }]
    })?;

    let tokens = crate::lexer::Lexer::new(&src).tokenize().map_err(|e| {
        vec![SparError::ResolveError {
            message: format!("import file '{}' has a lex error: {}", decl.path, e),
            hint: None,
            span: decl.span.clone(),
        }]
    })?;

    let mut imported_program = crate::parser::Parser::new(tokens).parse().map_err(|e| {
        vec![SparError::ResolveError {
            message: format!("import file '{}' has a parse error: {}", decl.path, e),
            hint: None,
            span: decl.span.clone(),
        }]
    })?;

    if crate::stdlib::is_bundled_std_path(&full_path) {
        mark_program_trusted_native(&mut imported_program);
    }

    // The imported module's own imports (resolved from *its* directory and
    // package scope) must be visible to the functions we splice, so expand
    // them inside the module first. They land as private items that the
    // dependency closure below can pull in on demand. A failure here is left
    // for the ordinary resolver to report against the unresolved call.
    let module_dir = full_path.parent().unwrap_or_else(|| Path::new("."));
    let mut module_loader =
        ImportLoader::new(module_dir).with_bundled_packages(loader.bundled_packages.clone());
    if let Some(locator) = &resolved.locator {
        module_loader = module_loader.with_locator(locator.clone());
    }
    let canonical = full_path
        .canonicalize()
        .unwrap_or_else(|_| full_path.clone());
    let already_expanding = SPLICE_STACK.with(|stack| stack.borrow().contains(&canonical));
    if !already_expanding {
        SPLICE_STACK.with(|stack| stack.borrow_mut().push(canonical.clone()));
        let _ = expand_imports_inner(&mut imported_program, &mut module_loader);
        SPLICE_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });
    }

    let available: Vec<(&str, &TopLevelItem)> = imported_program
        .items
        .iter()
        .filter_map(|it| match it {
            TopLevelItem::Var(v) if v.exported => Some((v.name.as_str(), it)),
            TopLevelItem::Section(s) if s.exported => s.path.first().map(|n| (n.as_str(), it)),
            TopLevelItem::Function(f) if !f.is_private => Some((f.name.as_str(), it)),
            TopLevelItem::Type(t) if t.exported => Some((t.name.as_str(), it)),
            TopLevelItem::Enum(e) if e.exported => Some((e.name.as_str(), it)),
            TopLevelItem::FunctionGroup(g) if !g.is_private => Some((g.name.as_str(), it)),
            _ => None,
        })
        .collect();

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
                if matches!(item, TopLevelItem::Section(section) if section.canonical) {
                    for implementation in imported_program.items.iter().filter_map(|candidate| {
                        let TopLevelItem::Impl(implementation) = candidate else {
                            return None;
                        };
                        (impl_target_name(implementation) == Some(req.name.as_str()))
                            .then_some(implementation)
                    }) {
                        let implementation =
                            retarget_impl(implementation.clone(), &req.name, &final_name);
                        spliced.push(TopLevelItem::Impl(implementation));
                    }
                }
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
        match &spliced[i] {
            TopLevelItem::Type(t) => {
                let mut refs = Vec::new();
                collect_named_type_refs(&t.fields, &mut refs);
                let parent_name = t.name.clone();
                for name in refs {
                    pull_dependency(
                        &name,
                        &parent_name,
                        "field",
                        "type",
                        &available,
                        &mut pulled,
                        &mut spliced,
                        &mut errors,
                        decl,
                        |item| matches!(item, TopLevelItem::Type(_) | TopLevelItem::Enum(_)),
                    );
                }
            }
            TopLevelItem::Function(function) => {
                let parent_name = function.name.clone();
                let mut type_refs = Vec::new();
                for parameter in &function.params {
                    collect_spar_type_refs(&parameter.ty, &mut type_refs);
                }
                collect_spar_type_refs(&function.ret, &mut type_refs);
                let mut called = HashSet::new();
                collect_calls_in_statements(&function.body.stmts, &mut called);
                for name in type_refs {
                    if !available.iter().any(|(candidate, item)| {
                        *candidate == name
                            && matches!(item, TopLevelItem::Type(_) | TopLevelItem::Enum(_))
                    }) {
                        continue;
                    }
                    pull_dependency(
                        &name,
                        &parent_name,
                        "signature",
                        "type",
                        &available,
                        &mut pulled,
                        &mut spliced,
                        &mut errors,
                        decl,
                        |item| matches!(item, TopLevelItem::Type(_) | TopLevelItem::Enum(_)),
                    );
                }
                // Functions the spliced body calls travel with it, whether
                // they are private helpers of the module or came in through
                // the module's own imports.
                let mut called = called.into_iter().collect::<Vec<_>>();
                called.sort();
                for name in called {
                    if name.contains("::") || pulled.contains(&name) {
                        continue;
                    }
                    let helper = imported_program
                        .items
                        .iter()
                        .find(|item| matches!(item, TopLevelItem::Function(f) if f.name == name));
                    if let Some(helper) = helper {
                        pulled.insert(name);
                        let localized = localize_visibility(helper.clone());
                        spliced.push(retag_top_level_span(localized, &decl.span));
                    }
                }
            }
            TopLevelItem::FunctionGroup(group) => {
                let parent_name = group.name.clone();
                let mut type_refs = Vec::new();
                for function in &group.functions {
                    for parameter in &function.params {
                        collect_spar_type_refs(&parameter.ty, &mut type_refs);
                    }
                    collect_spar_type_refs(&function.ret, &mut type_refs);
                }
                for name in type_refs {
                    if !available.iter().any(|(candidate, item)| {
                        *candidate == name
                            && matches!(item, TopLevelItem::Type(_) | TopLevelItem::Enum(_))
                    }) {
                        continue;
                    }
                    pull_dependency(
                        &name,
                        &parent_name,
                        "signature",
                        "type",
                        &available,
                        &mut pulled,
                        &mut spliced,
                        &mut errors,
                        decl,
                        |item| matches!(item, TopLevelItem::Type(_) | TopLevelItem::Enum(_)),
                    );
                }
            }
            TopLevelItem::Section(s) => {
                let parent_name = s.path.first().cloned().unwrap_or_default();
                let mut type_refs = Vec::new();
                if let Some(tb) = &s.type_binding {
                    collect_spar_type_refs(&tb.ty, &mut type_refs);
                }
                let mut section_refs = Vec::new();
                collect_section_item_refs(&s.items, &mut section_refs);
                for name in type_refs {
                    pull_dependency(
                        &name,
                        &parent_name,
                        "type binding",
                        "type",
                        &available,
                        &mut pulled,
                        &mut spliced,
                        &mut errors,
                        decl,
                        |item| matches!(item, TopLevelItem::Type(_) | TopLevelItem::Enum(_)),
                    );
                }
                for name in section_refs {
                    pull_dependency(
                        &name,
                        &parent_name,
                        "spread",
                        "section",
                        &available,
                        &mut pulled,
                        &mut spliced,
                        &mut errors,
                        decl,
                        |item| matches!(item, TopLevelItem::Section(_)),
                    );
                }
            }
            _ => {}
        }
        i += 1;
    }

    if errors.is_empty() {
        Ok(spliced)
    } else {
        Err(errors)
    }
}

thread_local! {
    /// Modules whose own imports are currently being expanded for a
    /// selective splice; guards against import cycles between modules.
    static SPLICE_STACK: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Every function name called (directly, or inside interpolations and shell
/// blocks) anywhere in `statements`.
fn collect_calls_in_statements(statements: &[crate::ast::Statement], out: &mut HashSet<String>) {
    use crate::ast::{ReturnValue, Statement};
    for statement in statements {
        match statement {
            Statement::LocalVar(local) => collect_calls_in_expr(&local.value, out),
            Statement::Assignment { value, .. }
            | Statement::FieldAssignment { value, .. }
            | Statement::Expression(value, _) => collect_calls_in_expr(value, out),
            Statement::If(branch) => {
                collect_calls_in_expr(&branch.condition, out);
                collect_calls_in_statements(&branch.then_stmts, out);
                collect_calls_in_statements(&branch.else_stmts, out);
            }
            Statement::Return(value, _) => match value {
                ReturnValue::Void => {}
                ReturnValue::Expr(expr) => collect_calls_in_expr(expr, out),
                ReturnValue::SectionBlock(fields) => {
                    for field in fields {
                        collect_calls_in_expr(&field.value, out);
                    }
                }
            },
            Statement::For(looped) => {
                collect_calls_in_expr(&looped.iterable, out);
                collect_calls_in_statements(&looped.body, out);
            }
            Statement::Try(attempt) => {
                collect_calls_in_statements(&attempt.body, out);
                collect_calls_in_statements(&attempt.handler, out);
            }
            Statement::Break(_) | Statement::Continue(_) => {}
        }
    }
}

fn collect_calls_in_items(items: &[crate::ast::SectionItem], out: &mut HashSet<String>) {
    use crate::ast::{FieldValue, SectionItem};
    for item in items {
        match item {
            SectionItem::Field(field) => match &field.value {
                Some(FieldValue::Expr(expr)) => collect_calls_in_expr(expr, out),
                Some(FieldValue::Nested(nested)) => collect_calls_in_items(nested, out),
                None => {}
            },
            SectionItem::Spread(spread) => collect_calls_in_expr(&spread.expr, out),
        }
    }
}

fn collect_calls_in_expr(expr: &crate::ast::Expr, out: &mut HashSet<String>) {
    use crate::ast::{Expr, StringPart};
    match expr {
        Expr::Call { name, args, .. } => {
            out.insert(name.clone());
            for argument in args {
                collect_calls_in_expr(&argument.value, out);
            }
        }
        Expr::Closure { body, .. } => match body {
            crate::ast::ClosureBody::Expr(value) => collect_calls_in_expr(value, out),
            crate::ast::ClosureBody::Block(body) => collect_calls_in_statements(&body.stmts, out),
        },
        Expr::FnCall(call) => {
            out.insert(call.name.clone());
            for argument in &call.args {
                collect_calls_in_expr(argument, out);
            }
        }
        Expr::BinaryOp(operation) => {
            collect_calls_in_expr(&operation.lhs, out);
            collect_calls_in_expr(&operation.rhs, out);
        }
        Expr::Unary { operand: inner, .. }
        | Expr::Grouped(inner, _)
        | Expr::Await { value: inner, .. }
        | Expr::FieldAccess { base: inner, .. } => collect_calls_in_expr(inner, out),
        Expr::MethodCall { receiver, args, .. } => {
            collect_calls_in_expr(receiver, out);
            for argument in args {
                collect_calls_in_expr(argument, out);
            }
        }
        Expr::StructuredPipe { input, stage, .. } => {
            collect_calls_in_expr(input, out);
            collect_calls_in_expr(stage, out);
        }
        Expr::List(items, _) => {
            for item in items {
                collect_calls_in_expr(item, out);
            }
        }
        Expr::Index { source, index, .. } => {
            collect_calls_in_expr(source, out);
            collect_calls_in_expr(index, out);
        }
        Expr::Comprehension { source, body, .. } => {
            collect_calls_in_expr(source, out);
            collect_calls_in_expr(body, out);
        }
        Expr::Object(items, _) => collect_calls_in_items(items, out),
        Expr::String(string) => {
            for part in &string.parts {
                if let StringPart::Expr(inner) = part {
                    collect_calls_in_expr(inner, out);
                }
            }
        }
        Expr::Shell(shell) | Expr::ExecShell(shell) | Expr::CommandSubstitution(shell) => {
            collect_calls_in_shell(shell, out)
        }
        Expr::Literal(_) | Expr::NamespaceRef(_) => {}
    }
}

fn collect_calls_in_shell(shell: &crate::ast::ShellExpr, out: &mut HashSet<String>) {
    use crate::ast::ShellStep;
    collect_calls_in_statements(&shell.statements, out);
    for (_, step) in &shell.steps {
        match step {
            ShellStep::Command(command) => collect_calls_in_shell_command(command, out),
            ShellStep::Pipeline(commands) => {
                for command in commands {
                    collect_calls_in_shell_command(command, out);
                }
            }
            ShellStep::MixedPipeline(pipeline) => {
                for command in pipeline.input.iter().chain(pipeline.output.iter()) {
                    collect_calls_in_shell_command(command, out);
                }
                for stage in &pipeline.stages {
                    collect_calls_in_expr(stage, out);
                }
            }
        }
    }
}

fn collect_calls_in_shell_command(
    command: &crate::ast::ShellCommandExpr,
    out: &mut HashSet<String>,
) {
    use crate::ast::ShellFdRedirectTarget;
    let mut words = vec![&command.program];
    words.extend(command.environment.iter().map(|entry| &entry.value));
    words.extend(command.args.iter());
    for redirect in [&command.stdin, &command.stdout, &command.stderr]
        .into_iter()
        .flatten()
    {
        words.push(&redirect.target);
    }
    for redirect in &command.redirections {
        if let ShellFdRedirectTarget::File(file) = &redirect.target {
            words.push(&file.target);
        }
    }
    for word in words {
        for part in &word.parts {
            match part {
                crate::ast::ShellWordPart::Expr(expr) => collect_calls_in_expr(expr, out),
                crate::ast::ShellWordPart::CommandSubstitution(shell) => {
                    collect_calls_in_shell(shell, out)
                }
                crate::ast::ShellWordPart::Literal(_)
                | crate::ast::ShellWordPart::Environment(_) => {}
            }
        }
    }
}

fn collect_named_type_refs(fields: &[crate::ast::TypeField], out: &mut Vec<String>) {
    use crate::ast::TypeFieldShape;
    for f in fields {
        match &f.shape {
            TypeFieldShape::Primitive(_) => {}
            TypeFieldShape::Named(name) => out.push(name.clone()),
            TypeFieldShape::TypeParameter(_) => {}
            TypeFieldShape::Applied { name, arguments } => {
                out.push(name.clone());
                for argument in arguments {
                    collect_spar_type_refs(argument, out);
                }
            }
            TypeFieldShape::Section(nested) => collect_named_type_refs(nested, out),
        }
    }
}

fn collect_spar_type_refs(ty: &crate::ast::SparType, out: &mut Vec<String>) {
    use crate::ast::SparType;
    match ty {
        SparType::Named(name) => out.push(name.clone()),
        SparType::Applied { name, arguments } => {
            out.push(name.clone());
            for argument in arguments {
                collect_spar_type_refs(argument, out);
            }
        }
        SparType::List(inner) => collect_spar_type_refs(inner, out),
        SparType::Function {
            params,
            return_type,
        } => {
            for param in params {
                collect_spar_type_refs(param, out);
            }
            collect_spar_type_refs(return_type, out);
        }
        SparType::Str
        | SparType::Int
        | SparType::Float
        | SparType::Bool
        | SparType::Section
        | SparType::Void
        | SparType::Shell
        | SparType::Error
        | SparType::TypeParameter(_) => {}
    }
}

/// Collects `...Target;` spread names out of a section body, including
/// spreads nested inside inline section-typed field values (`field: {
/// ...Target; };`) — the same shape a selectively-imported section like
/// `Postgres -> PostgresType { environment: { ...ProductionEnvironment; }; }`
/// carries. Only single-segment refs are treated as candidate top-level
/// section names; a multi-segment path targets a field within an
/// already-resolved value, not another top-level item.
fn collect_section_item_refs(items: &[crate::ast::SectionItem], out: &mut Vec<String>) {
    use crate::ast::{Expr, FieldValue, SectionItem};
    for item in items {
        match item {
            SectionItem::Spread(spread) => {
                if let Expr::NamespaceRef(nref) = &spread.expr {
                    if let [name] = nref.segments.as_slice() {
                        out.push(name.clone());
                    }
                }
            }
            SectionItem::Field(f) => {
                if let Some(FieldValue::Nested(nested)) = &f.value {
                    collect_section_item_refs(nested, out);
                }
            }
        }
    }
}

/// Shared transitive-dependency resolver used while splicing a selectively
/// imported item: `name` was referenced structurally (a type binding, a type
/// field, or a spread target) by `parent_name` but wasn't itself requested.
/// It must still be exported by the source file — the same rule that governs
/// explicitly requested items — since a transitive pull is not a back door
/// around visibility.
#[allow(clippy::too_many_arguments)]
fn pull_dependency<F: Fn(&crate::ast::TopLevelItem) -> bool>(
    name: &str,
    parent_name: &str,
    dep_site: &str,
    label: &str,
    available: &[(&str, &crate::ast::TopLevelItem)],
    pulled: &mut HashSet<String>,
    spliced: &mut Vec<crate::ast::TopLevelItem>,
    errors: &mut Vec<SparError>,
    decl: &crate::ast::ImportDecl,
    matches_kind: F,
) {
    if !pulled.insert(name.to_string()) {
        return;
    }
    match available.iter().find(|(n, _)| *n == name) {
        Some((_, dep_item)) if matches_kind(dep_item) => {
            let localized = localize_visibility((*dep_item).clone());
            spliced.push(retag_top_level_span(localized, &decl.span));
        }
        Some(_) => {} // name resolves to a different-shaped item; resolver reports the mismatch
        None => {
            errors.push(SparError::ResolveError {
                message: format!(
                    "{} `{}`, used by `{}`'s {}, is not exported by '{}' — \
                     export it so the transitive import can resolve",
                    label, name, parent_name, dep_site, decl.path
                ),
                hint: None,
                span: decl.span.clone(),
            });
        }
    }
}

pub(crate) fn mark_program_trusted_native(program: &mut Program) {
    for item in &mut program.items {
        match item {
            crate::ast::TopLevelItem::Function(function) => {
                function.trusted_native = true;
            }
            crate::ast::TopLevelItem::FunctionGroup(group) => {
                for function in &mut group.functions {
                    function.trusted_native = true;
                }
            }
            _ => {}
        }
    }
}

pub fn collect_imports(
    program: &Program,
    loader: &mut ImportLoader,
) -> Result<HashMap<String, LoadedImport>, Vec<SparError>> {
    use crate::ast::TopLevelItem;

    let mut errors: Vec<SparError> = Vec::new();
    let mut result: HashMap<String, LoadedImport> = HashMap::new();

    for item in &program.items {
        let TopLevelItem::Import(decl) = item else {
            continue;
        };
        let crate::ast::ImportKind::Aliased(decl_alias) = &decl.kind else {
            continue;
        };

        let alias = decl_alias.clone().unwrap_or_else(|| {
            decl.path
                .rsplit('/')
                .next()
                .unwrap_or(&decl.path)
                .trim_end_matches(".spar")
                .to_string()
        });

        let resolved = match loader.resolve_import(decl) {
            Ok(source) => source,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        let full_path = resolved.path;
        let import_locator = resolved.locator;
        if !full_path.exists() {
            errors.push(SparError::ResolveError {
                message: format!(
                    "cannot find import file '{}' — file does not exist",
                    decl.path
                ),
                hint: Some(if decl.package {
                    "check the package alias/module path and run `spar install` if dependencies changed".into()
                } else {
                    "check the module path; `.spar` is optional and paths are relative to the current file".into()
                }),
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

        let mut imported_program = match crate::parser::Parser::new(tokens).parse() {
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

        if crate::stdlib::is_bundled_std_path(&full_path) {
            mark_program_trusted_native(&mut imported_program);
        }

        // Collect exported symbol names
        let mut exports = HashSet::new();
        let mut functions = HashMap::new();
        for item in &imported_program.items {
            match item {
                TopLevelItem::Var(v) if v.exported => {
                    exports.insert(v.name.clone());
                }
                TopLevelItem::Section(s) if s.exported => {
                    if let Some(name) = s.path.first() {
                        exports.insert(name.clone());
                    }
                }
                TopLevelItem::Function(f) if !f.is_private => {
                    exports.insert(f.name.clone());
                    functions.insert(f.name.clone(), f.clone());
                }
                TopLevelItem::Type(t) if t.exported => {
                    exports.insert(t.name.clone());
                }
                TopLevelItem::FunctionGroup(g) if !g.is_private => {
                    exports.insert(g.name.clone());
                }
                _ => {}
            }
        }

        result.insert(
            alias,
            LoadedImport {
                path: decl.path.clone(),
                exports,
                functions,
                resolved_path: full_path.clone(),
                locator: import_locator,
            },
        );
    }

    if errors.is_empty() {
        Ok(result)
    } else {
        Err(errors)
    }
}

fn type_fields_to_schema_fields(
    type_fields: &[crate::ast::TypeField],
    schema_prog: &Program,
) -> Vec<crate::ast::SchemaField> {
    use crate::ast::{SchemaField, SchemaFieldShape, TopLevelItem, TypeFieldShape};

    type_fields
        .iter()
        .map(|tf| {
            let shape = match &tf.shape {
                TypeFieldShape::Primitive(ty) => SchemaFieldShape::Primitive(ty.clone()),
                TypeFieldShape::Section(nested) => {
                    SchemaFieldShape::Section(type_fields_to_schema_fields(nested, schema_prog))
                }
                TypeFieldShape::Named(other_name) => {
                    let is_enum = schema_prog
                        .items
                        .iter()
                        .any(|it| matches!(it, TopLevelItem::Enum(e) if &e.name == other_name));
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
                                if &t.name == other_name {
                                    return Some(&t.fields);
                                }
                            }
                            None
                        });
                        let expanded = other_fields
                            .map(|fields| type_fields_to_schema_fields(fields, schema_prog))
                            .unwrap_or_default();
                        SchemaFieldShape::Section(expanded)
                    }
                }
                TypeFieldShape::TypeParameter(name) => {
                    SchemaFieldShape::Primitive(crate::ast::SparType::TypeParameter(name.clone()))
                }
                TypeFieldShape::Applied { name, arguments } => {
                    SchemaFieldShape::Primitive(crate::ast::SparType::Applied {
                        name: name.clone(),
                        arguments: arguments.clone(),
                    })
                }
            };
            SchemaField {
                name: tf.name.clone(),
                optional: tf.optional,
                shape,
                span: tf.span.clone(),
            }
        })
        .collect()
}

/// Convert every `SchemaFrom`/`SchemaFrom?` in `schema_prog` into an
/// equivalent generated `TopLevelItem::SchemaSection`, removing the
/// `SchemaFrom` items. Must run after `expand_imports` has spliced in any
/// `import type {...}` targets, so `source_type` lookups see real
/// `TypeDecl`s.
fn expand_schema_from(schema_prog: &mut Program) -> Result<(), Vec<SparError>> {
    use crate::ast::{SchemaSectionDecl, TopLevelItem};

    let mut errors: Vec<SparError> = Vec::new();
    let mut generated: Vec<TopLevelItem> = Vec::new();

    for item in &schema_prog.items {
        let TopLevelItem::SchemaFrom(sf) = item else {
            continue;
        };

        let source = schema_prog.items.iter().find_map(|it| {
            if let TopLevelItem::Type(t) = it {
                if t.name == sf.source_type {
                    return Some(t);
                }
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

    schema_prog
        .items
        .retain(|it| !matches!(it, TopLevelItem::SchemaFrom(_)));
    schema_prog.items.extend(generated);

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Validate config `program` against any `import schema "..."` declarations it contains.
/// Loads each schema file, verifies it declares schemas, then checks every
/// struct whose name matches a schema; structs with no schema are ignored.
///
/// On success, also returns every config section's schema-derived field
/// list, keyed by section name. The typechecker uses this to exempt
/// schema-bound sections from the "every field needs an explicit type"
/// rule that applies to sections with neither a `-> Type` binding nor a
/// schema — the schema already tells it each field's expected shape.
pub fn validate_schema_imports(
    program: &crate::ast::Program,
    base_dir: &std::path::Path,
) -> Result<
    std::collections::HashMap<String, Vec<crate::ast::SchemaField>>,
    Vec<crate::error::SparError>,
> {
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
            if let Some(name) = s.path.first() {
                config_sections.insert(name.clone(), s);
            }
        }
    }

    // Which import first declared each schema name; schema names share one
    // namespace across every `import schema` in the file.
    let mut schema_origin: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for item in &program.items {
        let TopLevelItem::Import(decl) = item else {
            continue;
        };
        if !matches!(decl.kind, crate::ast::ImportKind::Schema) {
            continue;
        }

        // Resolve and load the schema file
        let full_path = local_module_path(base_dir, &decl.path);
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
                    "'{}' is not a schema file — declare `schema Name {{ ... }};` in it",
                    decl.path
                ),
                span: decl.span.clone(),
            });
            continue;
        }

        // Resolve `import type {...}` inside the schema file so any
        // `SchemaFrom` below has real `TypeDecl`s to convert.
        let schema_file_base = full_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();
        let mut schema_expand_loader = ImportLoader::new(&schema_file_base);
        if let Err(es) = expand_imports(&mut schema_prog, &mut schema_expand_loader) {
            errors.extend(es.into_iter().map(|e| match e {
                SparError::ResolveError { message, span, .. } => {
                    SparError::SchemaError { message, span }
                }
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
            (bool, &Vec<crate::ast::SchemaField>),
        > = std::collections::HashMap::new();

        for schema_item in &schema_prog.items {
            if let TopLevelItem::SchemaSection(s) = schema_item {
                if let Some(first) = schema_origin.get(&s.name) {
                    errors.push(SparError::SchemaError {
                        message: format!(
                            "schema `{}` is declared in both '{}' and '{}'",
                            s.name, first, decl.path
                        ),
                        span: decl.span.clone(),
                    });
                    continue;
                }
                schema_origin.insert(s.name.clone(), decl.path.clone());
                schema_sections.insert(s.name.clone(), (s.marker.optional, &s.fields));
            }
        }

        // Every required schema must have a struct of the same name in this file;
        // structs with no schema are ordinary structs and are ignored.
        for (name, (optional, schema_fields)) in &schema_sections {
            match config_sections.get(name) {
                None if !optional => {
                    let mut message = format!(
                        "schema `{}` ({}) has no matching struct in this file",
                        name, decl.path
                    );
                    if let Some(close) =
                        closest_name(name, config_sections.keys().map(String::as_str))
                    {
                        message.push_str(&format!("; did you mean `{close}`?"));
                    }
                    errors.push(SparError::SchemaError {
                        message,
                        span: decl.span.clone(),
                    });
                }
                None => {} // optional section, fine to omit
                Some(cfg_section) => {
                    bindings.insert(name.clone(), (*schema_fields).clone());
                    // Fix 2: skip field-level validation for sections that contain spread items.
                    // Spreads are resolved at runtime; we cannot statically know which fields
                    // they contribute, so a "missing required field" error would be a false positive.
                    let has_spreads = cfg_section
                        .items
                        .iter()
                        .any(|i| matches!(i, crate::ast::SectionItem::Spread(_)));
                    if !has_spreads {
                        let config_fields: Vec<&crate::ast::FieldDecl> = cfg_section
                            .items
                            .iter()
                            .filter_map(|i| {
                                if let crate::ast::SectionItem::Field(f) = i {
                                    Some(f)
                                } else {
                                    None
                                }
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

    if errors.is_empty() {
        Ok(bindings)
    } else {
        Err(errors)
    }
}

/// The candidate within edit distance 2 of `target` (closest first, ties
/// broken alphabetically), used for "did you mean" hints.
fn closest_name<'a>(target: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    for candidate in candidates {
        let distance = edit_distance(target, candidate);
        if distance > 2 {
            continue;
        }
        match best {
            Some((d, name)) if d < distance || (d == distance && name <= candidate) => {}
            _ => best = Some((distance, candidate)),
        }
    }
    best.map(|(_, name)| name)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

fn validate_fields(
    schema_fields: &[crate::ast::SchemaField],
    config_fields: &[&crate::ast::FieldDecl],
    section_path: &str,
    errors: &mut Vec<crate::error::SparError>,
    section_span: &crate::error::Span,
) {
    use crate::ast::{FieldValue, SchemaFieldShape, SparType};
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
                            Some(actual_ty) if actual_ty != expected_ty => {
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
                            Some(_) => {}
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
                            let has_spreads = nested_items
                                .iter()
                                .any(|i| matches!(i, crate::ast::SectionItem::Spread(_)));
                            if !has_spreads {
                                let nested_config: Vec<&crate::ast::FieldDecl> = nested_items
                                    .iter()
                                    .filter_map(|i| {
                                        if let crate::ast::SectionItem::Field(f) = i {
                                            Some(f)
                                        } else {
                                            None
                                        }
                                    })
                                    .collect();
                                let nested_path = format!("{}::{}", section_path, sf.name);
                                validate_fields(
                                    nested_schema,
                                    &nested_config,
                                    &nested_path,
                                    errors,
                                    &cf.span,
                                );
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
        crate::ast::SparType::Str => "str".to_string(),
        crate::ast::SparType::Int => "int".to_string(),
        crate::ast::SparType::Float => "float".to_string(),
        crate::ast::SparType::Bool => "bool".to_string(),
        crate::ast::SparType::Section => "section".to_string(),
        crate::ast::SparType::Void => "void".to_string(),
        crate::ast::SparType::Shell => "shell".to_string(),
        crate::ast::SparType::Error => "error".to_string(),
        crate::ast::SparType::List(_) => "list".to_string(),
        crate::ast::SparType::Named(name) => name.clone(),
        crate::ast::SparType::TypeParameter(name) => name.clone(),
        crate::ast::SparType::Applied { name, arguments } => format!(
            "{}<{}>",
            name,
            arguments
                .iter()
                .map(kl_type_name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        crate::ast::SparType::Function {
            params,
            return_type,
        } => format!(
            "fn({}) -> {}",
            params
                .iter()
                .map(kl_type_name)
                .collect::<Vec<_>>()
                .join(", "),
            kl_type_name(return_type)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::TopLevelItem;
    use tempfile::tempdir;

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
            "error must explain that the file was not found, got: {:?}",
            errs
        );
    }

    #[test]
    fn import_existing_file_succeeds() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("db.spar"),
            r#"export var host: str = "localhost";"#,
        )
        .unwrap();
        let src = r#"import "db.spar" as db;"#;
        let program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let result = collect_imports(&program, &mut loader);
        assert!(
            result.is_ok(),
            "existing import must succeed, got: {:?}",
            result.err()
        );
    }

    #[test]
    fn import_existing_file_exposes_exports() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            r#"export var version: str = "1.0"; var internal: int = 42;"#,
        )
        .unwrap();
        let src = r#"import "shared.spar" as shared;"#;
        let program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let loaded = collect_imports(&program, &mut loader).unwrap();
        let imp = &loaded["shared"];
        assert!(
            imp.exports.contains("version"),
            "exported var must appear in exports"
        );
        assert!(
            !imp.exports.contains("internal"),
            "non-exported var must not appear"
        );
    }

    #[test]
    fn import_existing_file_exposes_function_group_exports() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            r#"
                functionGroup EdgeInsect { function only() -> int { return 1; } };
                private functionGroup Hidden { function f() -> int { return 1; } };
            "#,
        )
        .unwrap();
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
            "export type [PostgresType]{ image: str; };\ntype [Internal]{ a: int; };\n",
        )
        .unwrap();
        let src = r#"import "shared.spar" as shared;"#;
        let program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        let loaded = collect_imports(&program, &mut loader).unwrap();
        let imp = &loaded["shared"];
        assert!(
            imp.exports.contains("PostgresType"),
            "exported type must appear in exports"
        );
        assert!(
            !imp.exports.contains("Internal"),
            "non-exported type must not appear"
        );
    }

    #[test]
    fn private_struct_without_a_schema_is_ignored() {
        use std::fs;
        let dir = tempdir().unwrap();

        // Schema declares only [Server]
        fs::write(
            dir.path().join("schema.spar"),
            concat!("", "schema Server { port: int; };\n",),
        )
        .unwrap();

        // Config has [Server] (public) and private [Defaults]
        let src = concat!(
            "import schema \"schema.spar\";\n",
            "[Server] { port: int = 8080; };\n",
            "private [Defaults] { timeout: int = 30; };\n",
        );
        let program = parse_src(src);
        let result = validate_schema_imports(&program, dir.path());
        assert!(
            result.is_ok(),
            "a private struct with no schema must be ignored, got: {:?}",
            result.err()
        );
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
        )
        .unwrap();
        let src = r#"import { version, Server } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        assert!(
            program
                .items
                .iter()
                .all(|it| !matches!(it, TopLevelItem::Import(_))),
            "the consumed import decl must be removed"
        );
        assert!(program
            .items
            .iter()
            .any(|it| matches!(it, TopLevelItem::Var(v) if v.name == "version")));
        assert!(program.items.iter().any(
            |it| matches!(it, TopLevelItem::Section(s) if s.path == vec!["Server".to_string()])
        ));
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
                "function greet() -> str { return \"hi\"; };\n",
                "export type [PostgresType]{ image: str; };\n",
            ),
        )
        .unwrap();
        let src = r#"import { version, Server, greet, PostgresType } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        for item in &program.items {
            match item {
                TopLevelItem::Var(v) => assert!(!v.exported, "spliced var must not stay exported"),
                TopLevelItem::Section(s) => {
                    assert!(!s.exported, "spliced section must not stay exported");
                    assert!(
                        s.private,
                        "spliced section must become private (exempt from schema/emit)"
                    );
                }
                TopLevelItem::Function(f) => {
                    assert!(f.is_private, "spliced function must become private")
                }
                TopLevelItem::Type(t) => {
                    assert!(!t.exported, "spliced type must not stay exported")
                }
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
        fs::write(
            dir.path().join("shared.spar"),
            "export var greeting: str = \"hi\";\n",
        )
        .unwrap();
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

        let spliced_span = program
            .items
            .iter()
            .find_map(|it| {
                if let TopLevelItem::Var(v) = it {
                    Some(v.span.clone())
                } else {
                    None
                }
            })
            .expect("expected a spliced var");
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
        )
        .unwrap();
        let src = r#"import { a, b } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        let a_span = program
            .items
            .iter()
            .find_map(|it| {
                if let TopLevelItem::Var(v) = it {
                    if v.name == "a" {
                        return Some(v.span.clone());
                    }
                }
                None
            })
            .expect("expected spliced var a");
        let b_span = program
            .items
            .iter()
            .find_map(|it| {
                if let TopLevelItem::Var(v) = it {
                    if v.name == "b" {
                        return Some(v.span.clone());
                    }
                }
                None
            })
            .expect("expected spliced var b");
        assert_ne!(
            a_span, b_span,
            "each spliced item must get its own distinct span"
        );
    }

    #[test]
    fn expand_imports_applies_as_rename() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            "export type [PostgresType]{ image: str; };\n",
        )
        .unwrap();
        let src = r#"import { PostgresType as PgType } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");

        assert!(program
            .items
            .iter()
            .any(|it| matches!(it, TopLevelItem::Type(t) if t.name == "PgType")));
        assert!(!program
            .items
            .iter()
            .any(|it| matches!(it, TopLevelItem::Type(t) if t.name == "PostgresType")));
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
        fs::write(
            dir.path().join("shared.spar"),
            "export var host: str = \"remote\";\n",
        )
        .unwrap();
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

    // ── Phase 3: import type (Task 4) ────────────────────────────────────

    #[test]
    fn expand_imports_type_selective_rejects_non_type_name() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            "export var host: str = \"x\";\n",
        )
        .unwrap();
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
                "export type [Libs]{ dependencies?: [str]; };\n",
                "export type [FlutterType]{ projectName: str; packages?: Libs; };\n",
            ),
        )
        .unwrap();
        let src = r#"import type { FlutterType } from "types.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        let has_libs = program.items.iter().any(|it| {
            matches!(
                it, TopLevelItem::Type(t) if t.name == "Libs"
            )
        });
        assert!(
            has_libs,
            "Libs must be transitively spliced in, got items: {:?}",
            program.items
        );
    }

    #[test]
    fn expand_imports_selective_transitively_pulls_section_type_binding() {
        // Regression: `import { Postgres }` where `Postgres -> PostgresType`
        // must silently bring in `PostgresType` too — the caller never
        // asked for it directly, it's load-bearing structure of the
        // section they did ask for.
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("compose.spar"),
            concat!(
                "export type [PostgresType]{ image: str; };\n",
                "export [Postgres] -> PostgresType { image: \"postgres:16\"; };\n",
            ),
        )
        .unwrap();
        let src = r#"import { Postgres } from "compose.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        let has_type = program
            .items
            .iter()
            .any(|it| matches!(it, TopLevelItem::Type(t) if t.name == "PostgresType"));
        assert!(
            has_type,
            "PostgresType must be transitively spliced in, got items: {:?}",
            program.items
        );
    }

    #[test]
    fn expand_imports_selective_transitively_pulls_spread_target() {
        // Regression: `import { Postgres }` where Postgres's body spreads
        // `...ProductionEnvironment` must bring that section in too, kept
        // private (excluded from emit) the same as it was in the source file.
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("compose.spar"),
            concat!(
                "private [ProductionEnvironment] { nodeEnv: \"production\"; };\n",
                "export [Postgres] { environment: { ...ProductionEnvironment; }; };\n",
            ),
        )
        .unwrap();
        let src = r#"import { Postgres } from "compose.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());

        // Not exported yet — must fail with a clear, import-line-attributed error.
        let err = expand_imports(&mut program, &mut loader).expect_err("must fail");
        assert!(
            err.iter()
                .any(|e| e.to_string().contains("ProductionEnvironment")
                    && e.to_string().contains("not exported")),
            "expected a not-exported error naming ProductionEnvironment, got: {:?}",
            err
        );
    }

    #[test]
    fn expand_imports_type_selective_can_import_an_enum_directly() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("types.spar"),
            "export enum Protocol { Http, Https };\n",
        )
        .unwrap();
        let src = r#"import type { Protocol } from "types.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        let has_protocol = program.items.iter().any(|it| {
            matches!(
                it, TopLevelItem::Enum(e) if e.name == "Protocol"
            )
        });
        assert!(
            has_protocol,
            "Protocol enum must be spliced in, got items: {:?}",
            program.items
        );
    }

    #[test]
    fn expand_imports_named_selective_can_import_a_function_group() {
        // Regression: `import { X } from "...";` (plain named import, not
        // `import type`) didn't recognize functionGroup exports at all —
        // `available`/rename/localize/retag all skipped `FunctionGroup`.
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("shared.spar"),
            "functionGroup EdgeInsect { function only() -> int { return 1; } };\n",
        )
        .unwrap();
        let src = r#"import { EdgeInsect } from "shared.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        let has_group = program.items.iter().any(|it| {
            matches!(
                it, TopLevelItem::FunctionGroup(g) if g.name == "EdgeInsect"
            )
        });
        assert!(
            has_group,
            "EdgeInsect functionGroup must be spliced in, got items: {:?}",
            program.items
        );
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
                "export type [Container]{ name: str; restart: RestartPolicy; };\n",
            ),
        )
        .unwrap();
        let src = r#"import type { Container } from "types.spar";"#;
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        let has_restart_policy = program.items.iter().any(|it| {
            matches!(
                it, TopLevelItem::Enum(e) if e.name == "RestartPolicy"
            )
        });
        assert!(
            has_restart_policy,
            "RestartPolicy enum must be transitively spliced in, got items: {:?}",
            program.items
        );
    }

    // ── Phase 3: import type inside @SchemaFile (Task 5) ─────────────────

    #[test]
    fn validate_schema_imports_resolves_import_type_inside_schema_file() {
        use std::fs;
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("types.spar"),
            "export type [PostgresType]{ image: str; };\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("schema.spar"),
            concat!(
                "",
                "import type { PostgresType } from \"types.spar\";\n",
                "schema Postgres { image: str; };\n",
            ),
        )
        .unwrap();
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
        )
        .unwrap();
        fs::write(
            dir.path().join("schema.spar"),
            "schema Container { x?: str; };\n",
        )
        .unwrap();
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
            load_env: None,
            shebang: None,
            items: vec![
                TopLevelItem::Type(crate::ast::TypeDecl {
                    type_parameters: Vec::new(),
                    name: "PostgresType".into(),
                    name_span: crate::error::Span::dummy(),
                    exported: true,
                    fields: vec![
                        crate::ast::TypeField {
                            name: "image".into(),
                            optional: false,
                            shape: crate::ast::TypeFieldShape::Primitive(crate::ast::SparType::Str),
                            default: None,
                            span: crate::error::Span::dummy(),
                        },
                        crate::ast::TypeField {
                            name: "port".into(),
                            optional: true,
                            shape: crate::ast::TypeFieldShape::Primitive(crate::ast::SparType::Int),
                            default: None,
                            span: crate::error::Span::dummy(),
                        },
                    ],
                    span: crate::error::Span::dummy(),
                    end_line: 0,
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

        assert!(!program
            .items
            .iter()
            .any(|it| matches!(it, TopLevelItem::SchemaFrom(_))));
        let generated = program
            .items
            .iter()
            .find_map(|it| {
                if let TopLevelItem::SchemaSection(s) = it {
                    Some(s)
                } else {
                    None
                }
            })
            .expect("SchemaFrom must generate a SchemaSection");
        assert_eq!(generated.name, "Postgres");
        assert!(!generated.marker.optional);
        assert_eq!(generated.fields.len(), 2);
        assert!(generated
            .fields
            .iter()
            .any(|f| f.name == "image" && !f.optional));
        assert!(generated
            .fields
            .iter()
            .any(|f| f.name == "port" && f.optional));
    }

    #[test]
    fn expand_schema_from_expands_named_type_reference_recursively() {
        let mut program = Program {
            is_schema_file: true,
            load_env: None,
            shebang: None,
            items: vec![
                TopLevelItem::Type(crate::ast::TypeDecl {
                    type_parameters: Vec::new(),
                    name: "Border".into(),
                    name_span: crate::error::Span::dummy(),
                    exported: true,
                    fields: vec![crate::ast::TypeField {
                        name: "width".into(),
                        optional: false,
                        shape: crate::ast::TypeFieldShape::Primitive(crate::ast::SparType::Int),
                        default: None,
                        span: crate::error::Span::dummy(),
                    }],
                    span: crate::error::Span::dummy(),
                    end_line: 0,
                }),
                TopLevelItem::Type(crate::ast::TypeDecl {
                    type_parameters: Vec::new(),
                    name: "Decoration".into(),
                    name_span: crate::error::Span::dummy(),
                    exported: true,
                    fields: vec![crate::ast::TypeField {
                        name: "border".into(),
                        optional: false,
                        shape: crate::ast::TypeFieldShape::Named("Border".into()),
                        default: None,
                        span: crate::error::Span::dummy(),
                    }],
                    span: crate::error::Span::dummy(),
                    end_line: 0,
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

        let generated = program
            .items
            .iter()
            .find_map(|it| {
                if let TopLevelItem::SchemaSection(s) = it {
                    Some(s)
                } else {
                    None
                }
            })
            .expect("SchemaFrom must generate a SchemaSection");
        assert!(generated.marker.optional);
        let border_field = generated
            .fields
            .iter()
            .find(|f| f.name == "border")
            .expect("border field");
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
            load_env: None,
            shebang: None,
            items: vec![TopLevelItem::SchemaFrom(crate::ast::SchemaFromDecl {
                name: "Postgres".into(),
                source_type: "NoSuchType".into(),
                source_type_span: crate::error::Span::dummy(),
                marker: crate::ast::SchemaMarker { optional: false },
                span: crate::error::Span::dummy(),
            })],
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
            "export type [PostgresType]{ image: str; };\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("schema.spar"),
            concat!(
                "",
                "import type { PostgresType } from \"types.spar\";\n",
                "schema Postgres from PostgresType;\n",
            ),
        )
        .unwrap();
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
                "export type [ServiceType]{ image: str; restart: RestartPolicy; };\n",
            ),
        )
        .unwrap();
        fs::write(
            dir.path().join("schema.spar"),
            concat!(
                "",
                "import type { ServiceType } from \"types.spar\";\n",
                "schema Service from ServiceType;\n",
            ),
        )
        .unwrap();
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
            "export type [PostgresType]{ image: str; port: int; };\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("schema.spar"),
            concat!(
                "",
                "import type { PostgresType } from \"types.spar\";\n",
                "schema Postgres from PostgresType;\n",
            ),
        )
        .unwrap();
        let src = concat!(
            "import schema \"schema.spar\";\n",
            "[Postgres]{ image: str = \"postgres:16\"; };\n", // missing required `port`
        );
        let program = parse_src(src);
        let result = validate_schema_imports(&program, dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn registered_bundled_package_resolves_before_lockfile_packages() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sparsh");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("config.spar"),
            "type [SparshConfig]{ enabled?: bool; };\n",
        )
        .unwrap();

        let mut roots = crate::compiler::BundledPackageRoots::default();
        roots.register("sparsh", root.clone()).unwrap();
        let loader = ImportLoader::new(temp.path()).with_bundled_packages(roots);
        let tokens = crate::Lexer::new(r#"import pkg { SparshConfig } from "sparsh/config";"#)
            .tokenize()
            .unwrap();
        let program = crate::Parser::new(tokens).parse().unwrap();
        let crate::ast::TopLevelItem::Import(decl) = &program.items[0] else {
            panic!("expected import declaration");
        };

        let resolved = loader.resolve_import(decl).unwrap();
        assert_eq!(resolved.path, root.join("config.spar"));
    }

    #[test]
    fn registered_bundled_package_rejects_parent_escape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sparsh");
        std::fs::create_dir_all(&root).unwrap();
        let mut roots = crate::compiler::BundledPackageRoots::default();
        roots.register("sparsh", root).unwrap();
        assert!(roots.resolve("sparsh/../secret").is_none());
    }

    #[test]
    fn imported_emit_marked_struct_is_not_emitted_by_the_importer() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.spar"),
            "#[emit]\nexport struct Shared { x: int = 1; };\n",
        )
        .unwrap();
        let src = "import { Shared } from \"lib.spar\";\n#[emit]\nstruct Mine { y: int = 2; };\n";
        let mut program = parse_src(src);
        let mut loader = ImportLoader::new(dir.path());
        expand_imports(&mut program, &mut loader).expect("expand must succeed");
        let mut saw_shared = false;
        for item in &program.items {
            if let TopLevelItem::Section(s) = item {
                if s.path == vec!["Shared".to_string()] {
                    saw_shared = true;
                    assert!(!s.is_emit(), "imported struct must lose #[emit]");
                }
            }
        }
        assert!(saw_shared, "Shared should have been spliced in");
    }

    fn schema_result(
        schema_src: &str,
        config_src: &str,
    ) -> Result<
        std::collections::HashMap<String, Vec<crate::ast::SchemaField>>,
        Vec<crate::error::SparError>,
    > {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("s.spar"), schema_src).unwrap();
        let program = parse_src(&format!("import schema \"s.spar\";\n{config_src}"));
        validate_schema_imports(&program, dir.path())
    }

    fn messages(errors: Vec<crate::error::SparError>) -> Vec<String> {
        errors.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn matched_struct_is_type_checked() {
        let errors = schema_result(
            "schema Server { host: str; port: int; };",
            "struct Server { host: str = \"h\"; };",
        )
        .unwrap_err();
        assert!(messages(errors).iter().any(|m| m.contains("port")));
    }

    #[test]
    fn struct_without_schema_is_ignored() {
        assert!(schema_result(
            "schema Server { host: str; };",
            "struct Server { host: str = \"h\"; };\nstruct Other { z: int = 1; };",
        )
        .is_ok());
    }

    #[test]
    fn missing_required_schema_struct_is_an_error() {
        let errors = schema_result(
            "schema Server { host: str; };",
            "struct Other { z: int = 1; };",
        )
        .unwrap_err();
        let joined = messages(errors).join("\n");
        assert!(joined.contains("schema `Server`"), "{joined}");
        assert!(joined.contains("has no matching struct in this file"), "{joined}");
        assert!(!joined.contains("did you mean"), "{joined}");
    }

    #[test]
    fn missing_struct_suggests_a_close_name() {
        let errors = schema_result(
            "schema Server { host: str; };",
            "struct Servr { host: str = \"h\"; };",
        )
        .unwrap_err();
        assert!(messages(errors).join("\n").contains("did you mean `Servr`?"));
    }

    #[test]
    fn distant_names_get_no_suggestion() {
        let errors = schema_result(
            "schema Server { host: str; };",
            "struct Database { host: str = \"h\"; };",
        )
        .unwrap_err();
        assert!(!messages(errors).join("\n").contains("did you mean"));
    }

    #[test]
    fn optional_schema_struct_may_be_omitted() {
        assert!(schema_result(
            "schema? Cache { ttl: int; };",
            "struct Other { z: int = 1; };"
        )
        .is_ok());
    }

    #[test]
    fn private_struct_is_validated_against_its_schema() {
        let errors = schema_result(
            "schema Server { host: str; port: int; };",
            "private struct Server { host: str = \"h\"; };",
        )
        .unwrap_err();
        assert!(messages(errors).iter().any(|m| m.contains("port")));
    }

    #[test]
    fn duplicate_schema_names_across_imports_are_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.spar"), "schema Server { host: str; };").unwrap();
        std::fs::write(dir.path().join("b.spar"), "schema Server { port: int; };").unwrap();
        let program = parse_src(
            "import schema \"a.spar\";\nimport schema \"b.spar\";\nstruct Server { host: str = \"h\"; port: int = 1; };\n",
        );
        let errors = validate_schema_imports(&program, dir.path()).unwrap_err();
        let joined = messages(errors).join("\n");
        assert!(joined.contains("schema `Server` is declared in both"), "{joined}");
        assert!(joined.contains("a.spar") && joined.contains("b.spar"), "{joined}");
    }

    #[test]
    fn all_schema_errors_are_reported_together() {
        let errors = schema_result(
            "schema A { x: int; };\nschema B { y: int; };",
            "struct Unrelated { z: int = 1; };",
        )
        .unwrap_err();
        assert_eq!(errors.len(), 2);
    }
}
