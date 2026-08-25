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
                            errors.push(SparError::ResolveError {
                                message: format!(
                                    "'{}' brought in from '{}' collides with a declaration already in scope",
                                    name, decl.path
                                ),
                                hint: None,
                                span: decl.span.clone(),
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
                if types_only && !matches!(item, TopLevelItem::Type(_)) {
                    errors.push(SparError::ResolveError {
                        message: format!(
                            "'{}' is not a type — `import type {{...}}` can only bring in `type` declarations",
                            req.name
                        ),
                        hint: None,
                        span: req.name_span.clone(),
                    });
                    continue;
                }
                let final_name = req.alias.clone().unwrap_or_else(|| req.name.clone());
                spliced.push(rename_top_level_item((*item).clone(), &final_name));
            }
        }
    }

    if errors.is_empty() { Ok(spliced) } else { Err(errors) }
}

fn splice_as_part_of(
    decl: &crate::ast::ImportDecl,
    _loader: &mut ImportLoader,
    _visiting: &mut Vec<PathBuf>,
) -> Result<Vec<crate::ast::TopLevelItem>, Vec<SparError>> {
    Err(vec![SparError::ResolveError {
        message: format!("`import asPartOf \"{}\";` is not yet implemented", decl.path),
        hint: None,
        span: decl.span.clone(),
    }])
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

/// Validate config `program` against any `import schema "..."` declarations it contains.
/// Loads each schema file, verifies it has @SchemaFile, then checks all sections.
pub fn validate_schema_imports(
    program: &crate::ast::Program,
    base_dir: &std::path::Path,
) -> Result<(), Vec<crate::error::SparError>> {
    use crate::ast::TopLevelItem;
    use crate::error::SparError;

    let mut errors: Vec<SparError> = Vec::new();

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

        let schema_prog = match crate::parser::Parser::new(schema_tokens).parse() {
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

    if errors.is_empty() { Ok(()) } else { Err(errors) }
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
                            let nested_config: Vec<&crate::ast::FieldDecl> = match &cf.value {
                                Some(FieldValue::Nested(fields)) => fields.iter().collect(),
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
                            let nested_path = format!("{}::{}", section_path, sf.name);
                            validate_fields(nested_schema, &nested_config, &nested_path, errors, &cf.span);
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

fn kl_type_name(ty: &crate::ast::SparType) -> &'static str {
    match ty {
        crate::ast::SparType::Str     => "str",
        crate::ast::SparType::Int     => "int",
        crate::ast::SparType::Float   => "float",
        crate::ast::SparType::Bool    => "bool",
        crate::ast::SparType::Section => "section",
        crate::ast::SparType::List(_) => "list",
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
}
