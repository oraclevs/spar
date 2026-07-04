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

pub fn collect_imports(
    program: &Program,
    loader: &mut ImportLoader,
) -> Result<HashMap<String, LoadedImport>, Vec<SparError>> {
    use crate::ast::TopLevelItem;

    let mut errors: Vec<SparError> = Vec::new();
    let mut result: HashMap<String, LoadedImport> = HashMap::new();

    for item in &program.items {
        let TopLevelItem::Import(decl) = item else { continue };
        if decl.is_schema { continue; }

        let alias = decl.alias.clone().unwrap_or_else(|| {
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
        if !decl.is_schema { continue; }
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
                        if &cf.ty != expected_ty {
                            errors.push(SparError::SchemaError {
                                message: format!(
                                    "field `{}::{}` declared as `{}` but schema expects `{}`",
                                    section_path,
                                    sf.name,
                                    kl_type_name(&cf.ty),
                                    kl_type_name(expected_ty),
                                ),
                                span: cf.span.clone(),
                            });
                        }
                    }
                    SchemaFieldShape::Section(nested_schema) => {
                        if cf.ty != SparType::Section {
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
    fn private_section_not_validated_against_schema() {
        use std::fs;
        let dir = tempdir().unwrap();

        // Schema declares only [Server]
        fs::write(dir.path().join("schema.spar"), concat!(
            "@SchemaFile\n",
            "[Server]<Schema>{ port: int; }\n",
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
}
