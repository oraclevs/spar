//! JSON emission shared by the `spar` CLI and the WebAssembly playground.
//!
//! `build_emit_json` is the single source of truth for the shape of emitted
//! JSON (exported globals + public top-level sections, keys sorted). Both the
//! CLI (`spar emit`, with imports) and `emit_to_json` (single-file, no imports)
//! funnel through it so their output is identical.

use std::collections::HashMap;

use crate::evaluator::{ConfigValue, EvalResult, Evaluator};
use crate::resolver::{GlobalEntry, Resolver, SymbolTable};
use crate::typechecker::TypeChecker;
use crate::{Lexer, Parser, SparError};

/// Compile a single Spar source string to pretty-printed JSON, with no
/// cross-file imports (the browser/playground has no filesystem). On failure
/// returns one human-readable message per pipeline error.
pub fn emit_to_json(src: &str) -> Result<String, Vec<String>> {
    let tokens = Lexer::new(src).tokenize().map_err(|e| vec![e.to_string()])?;
    let program = Parser::new(tokens).parse().map_err(|e| vec![e.to_string()])?;

    if program.is_schema_file {
        return Err(vec![
            "this is a schema file and cannot be emitted — schema files declare shape only"
                .to_string(),
        ]);
    }

    let no_imports = HashMap::new();
    let symbols = Resolver::resolve_with_imports(&program, &no_imports).map_err(messages)?;

    if let Err(errs) = TypeChecker::check(&program, &symbols) {
        return Err(messages(errs));
    }

    let result = Evaluator::evaluate(&program, &symbols).map_err(messages)?;
    let value = build_emit_json(&result, &symbols);
    Ok(serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string()))
}

fn messages(errs: Vec<SparError>) -> Vec<String> {
    errs.iter().map(|e| e.to_string()).collect()
}

/// Build the emitted JSON value: exported globals, then public top-level
/// sections (private sections excluded), all keys sorted.
pub fn build_emit_json(result: &EvalResult, symbols: &SymbolTable) -> serde_json::Value {
    let mut root = serde_json::Map::new();

    // Exported globals only
    for (name, value) in &result.globals {
        let exported = symbols
            .globals
            .get(name)
            .map(|entry| match entry {
                GlobalEntry::Var { exported, .. } => *exported,
                GlobalEntry::Dynamic { .. } => false,
            })
            .unwrap_or(false);
        if exported {
            root.insert(name.clone(), config_value_to_json(value));
        }
    }

    // Public top-level sections only (path length == 1)
    let mut section_keys: Vec<&Vec<String>> =
        result.sections.keys().filter(|p| p.len() == 1).collect();
    section_keys.sort();

    for path in section_keys {
        let private = symbols
            .sections
            .get(path)
            .map(|e| e.private)
            .unwrap_or(false);
        if !private {
            let name = &path[0];
            root.insert(name.clone(), build_section_value(path, result));
        }
    }

    serde_json::Value::Object(root)
}

fn build_section_value(path: &[String], result: &EvalResult) -> serde_json::Value {
    let mut map = serde_json::Map::new();

    if let Some(fields) = result.sections.get(path) {
        let mut pairs: Vec<_> = fields.iter().collect();
        pairs.sort_by_key(|(k, _)| k.as_str());
        for (field_name, value) in pairs {
            map.insert(field_name.clone(), config_value_to_json(value));
        }
    }

    let mut nested: Vec<&Vec<String>> = result
        .sections
        .keys()
        .filter(|p| p.len() == path.len() + 1 && p.starts_with(path))
        .collect();
    nested.sort();
    for nested_path in nested {
        let nested_name = nested_path.last().unwrap().clone();
        map.insert(nested_name, build_section_value(nested_path, result));
    }

    serde_json::Value::Object(map)
}

fn config_value_to_json(val: &ConfigValue) -> serde_json::Value {
    match val {
        ConfigValue::Str(s) => serde_json::Value::String(s.clone()),
        ConfigValue::Int(i) => serde_json::json!(i),
        ConfigValue::Float(f) => serde_json::json!(f),
        ConfigValue::Bool(b) => serde_json::Value::Bool(*b),
        ConfigValue::List(vs) => {
            serde_json::Value::Array(vs.iter().map(config_value_to_json).collect())
        }
        ConfigValue::Section(map) => {
            let mut obj = serde_json::Map::new();
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            for (k, v) in pairs {
                obj.insert(k.clone(), config_value_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
    }
}
