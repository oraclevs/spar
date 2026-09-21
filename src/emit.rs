//! JSON/YAML/TOML emission shared by the `spar` CLI and the WebAssembly
//! playground.
//!
//! `build_emit_json` is the single source of truth for the shape of emitted
//! config (top-level vars and sections marked `#[emit]`, keys sorted) — as a
//! `serde_json::Value`, which every supported output format serializes from.
//! Both the CLI (`spar emit`, with imports) and `emit_to_json`/`emit_to_yaml`/
//! `emit_to_toml` (single-file, no imports) funnel through it so their output
//! is identical modulo format.

use crate::evaluator::{ConfigValue, EvalResult};
use crate::resolver::{GlobalEntry, SymbolTable};
use crate::{CompileOptions, Compiler};

/// Output format for `spar emit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmitFormat {
    Json,
    Yaml,
    Toml,
}

/// Compile a single Spar source string (no cross-file imports — the
/// browser/playground has no filesystem) to a `serde_json::Value`. On failure
/// returns one human-readable message per pipeline error.
fn compile_for_emit(src: &str) -> Result<serde_json::Value, Vec<String>> {
    let options = CompileOptions {
        allow_schema_file: false,
        ..CompileOptions::default()
    };
    let compilation = Compiler::new(options).compile(src);
    if !compilation.errors.is_empty() {
        return Err(compilation.errors.iter().map(ToString::to_string).collect());
    }
    build_emit_json(
        compilation
            .result
            .as_ref()
            .expect("successful compilation evaluates"),
        compilation
            .symbols
            .as_ref()
            .expect("successful compilation resolves"),
    )
    .map_err(|error| vec![error])
}

/// Compile a single Spar source string to pretty-printed JSON, with no
/// cross-file imports (the browser/playground has no filesystem). On failure
/// returns one human-readable message per pipeline error.
pub fn emit_to_json(src: &str) -> Result<String, Vec<String>> {
    let value = compile_for_emit(src)?;
    Ok(serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string()))
}

/// Compile a single Spar source string to YAML. Same scope/error contract as
/// [`emit_to_json`].
pub fn emit_to_yaml(src: &str) -> Result<String, Vec<String>> {
    let value = compile_for_emit(src)?;
    serde_yaml::to_string(&value).map_err(|e| vec![e.to_string()])
}

/// Compile a single Spar source string to TOML. Same scope/error contract as
/// [`emit_to_json`].
pub fn emit_to_toml(src: &str) -> Result<String, Vec<String>> {
    let value = compile_for_emit(src)?;
    toml::to_string_pretty(&value).map_err(|e| vec![e.to_string()])
}

/// Build the emitted JSON value: top-level vars and sections marked
/// `#[emit]`, all keys sorted.
pub fn build_emit_json(
    result: &EvalResult,
    symbols: &SymbolTable,
) -> Result<serde_json::Value, String> {
    let mut root = serde_json::Map::new();

    const NOTHING_TO_EMIT: &str = "nothing to emit: mark top-level structs or vars with #[emit]";
    let mut marked = 0usize;

    // Top-level vars marked #[emit]
    for (name, value) in &result.globals {
        let emitted = matches!(
            symbols.globals.get(name),
            Some(GlobalEntry::Var { emit: true, .. })
        );
        if emitted {
            marked += 1;
            root.insert(name.clone(), config_value_to_json(value)?);
        }
    }

    // Top-level sections marked #[emit] (path length == 1)
    let mut section_keys: Vec<&Vec<String>> =
        result.sections.keys().filter(|p| p.len() == 1).collect();
    section_keys.sort();

    for path in section_keys {
        let emitted = symbols
            .sections
            .get(path)
            .map(|entry| entry.emit)
            .unwrap_or(false);
        if emitted {
            marked += 1;
            root.insert(path[0].clone(), build_section_value(path, result)?);
        }
    }

    if marked == 0 {
        return Err(NOTHING_TO_EMIT.to_string());
    }

    Ok(sort_keys(serde_json::Value::Object(root)))
}

/// Recursively sort object keys. `serde_json` preserves insertion order when
/// any crate in the build enables `preserve_order` (scoc does), so emitted
/// output sorts explicitly to stay deterministic.
fn sort_keys(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, sort_keys(value)))
                    .collect(),
            )
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sort_keys).collect())
        }
        other => other,
    }
}

fn build_section_value(path: &[String], result: &EvalResult) -> Result<serde_json::Value, String> {
    let mut map = serde_json::Map::new();

    if let Some(fields) = result.sections.get(path) {
        let mut pairs: Vec<_> = fields.iter().collect();
        pairs.sort_by_key(|(k, _)| k.as_str());
        for (field_name, value) in pairs {
            map.insert(field_name.clone(), config_value_to_json(value)?);
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
        map.insert(nested_name, build_section_value(nested_path, result)?);
    }

    Ok(serde_json::Value::Object(map))
}

fn config_value_to_json(val: &ConfigValue) -> Result<serde_json::Value, String> {
    Ok(match val {
        ConfigValue::Str(s) => serde_json::Value::String(s.clone()),
        ConfigValue::Int(i) => serde_json::json!(i),
        ConfigValue::Float(f) => serde_json::json!(f),
        ConfigValue::Bool(b) => serde_json::Value::Bool(*b),
        ConfigValue::List(vs) => serde_json::Value::Array(
            vs.iter()
                .map(config_value_to_json)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        ConfigValue::Section(map) => {
            let mut obj = serde_json::Map::new();
            let mut pairs: Vec<_> = map.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            for (k, v) in pairs {
                obj.insert(k.clone(), config_value_to_json(v)?);
            }
            serde_json::Value::Object(obj)
        }
        ConfigValue::Shell(plan) => shell_plan_to_json(plan),
        ConfigValue::ShellProgram(_) => {
            return Err("deferred shell programs cannot be emitted as configuration data".into())
        }
        ConfigValue::Promise(_) => {
            return Err("promise values cannot be emitted as configuration data".into())
        }
        ConfigValue::Error {
            message,
            kind,
            code,
            cause,
        } => serde_json::json!({
            "message": message,
            "kind": kind,
            "code": code,
            "cause": cause.as_deref().map(config_value_to_json).transpose()?,
        }),
    })
}

fn shell_plan_to_json(plan: &spar_command::ShellPlan) -> serde_json::Value {
    serde_json::json!({
        "steps": plan.steps.iter().map(|(join, step)| {
            let join = match join {
                spar_command::Join::Always => "always",
                spar_command::Join::OnSuccess => "onSuccess",
                spar_command::Join::OnFailure => "onFailure",
            };
            let (kind, commands): (&str, Vec<&spar_command::CommandPlan>) = match step {
                spar_command::Step::Command(command) => ("command", vec![command]),
                spar_command::Step::Pipeline(pipeline) => {
                    ("pipeline", pipeline.commands.iter().collect())
                }
            };
            serde_json::json!({
                "join": join,
                "kind": kind,
                "commands": commands.into_iter().map(command_plan_to_json).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>()
    })
}

fn command_plan_to_json(command: &spar_command::CommandPlan) -> serde_json::Value {
    serde_json::json!({
        "program": command.program,
        "args": command.args,
        "environment": command.env.iter().map(|entry| {
            serde_json::json!({ "key": entry.key, "value": entry.value })
        }).collect::<Vec<_>>(),
        "cwd": command.cwd.as_ref().map(|cwd| match cwd {
            spar_command::WorkingDirectory::Path(path) => path,
        }),
        "stdin": command.stdin.as_ref().map(redirection_to_json),
        "stdout": command.stdout.as_ref().map(redirection_to_json),
        "stderr": command.stderr.as_ref().map(redirection_to_json),
    })
}

fn redirection_to_json(redirection: &spar_command::Redirection) -> serde_json::Value {
    match redirection {
        spar_command::Redirection::File { path, mode } => serde_json::json!({
            "kind": "file",
            "path": path,
            "mode": match mode {
                spar_command::RedirectMode::Truncate => "truncate",
                spar_command::RedirectMode::Append => "append",
            },
        }),
        spar_command::Redirection::DuplicateFd(fd) => {
            serde_json::json!({ "kind": "duplicateFd", "fd": fd })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promise_values_cannot_be_emitted() {
        let errors = emit_to_json(
            "async function value() -> int { return 1; };\n#[emit]\nvar pending: Promise<int> = value();",
        )
        .unwrap_err();
        assert!(errors
            .iter()
            .any(|error| error.contains("promise values cannot be emitted")));
    }

    const SRC: &str = r#"
#[emit]
var name: str = "spar";

#[emit]
struct Server {
    port: int = 8080;
};
"#;

    #[test]
    fn emit_to_yaml_renders_nested_sections() {
        let yaml = emit_to_yaml(SRC).expect("compiles");
        assert_eq!(yaml, "Server:\n  port: 8080\nname: spar\n");
    }

    #[test]
    fn emit_to_toml_orders_scalars_before_tables() {
        let toml = emit_to_toml(SRC).expect("compiles");
        assert_eq!(toml, "name = \"spar\"\n\n[Server]\nport = 8080\n");
    }

    #[test]
    fn emit_to_yaml_reports_pipeline_errors() {
        let errors = emit_to_yaml("var x: int = \"not an int\";").unwrap_err();
        assert!(!errors.is_empty());
    }

    #[test]
    fn emit_to_toml_reports_pipeline_errors() {
        let errors = emit_to_toml("var x: int = \"not an int\";").unwrap_err();
        assert!(!errors.is_empty());
    }

    #[test]
    fn only_emit_marked_items_are_emitted() {
        let json = emit_to_json(
            "#[emit]\nstruct A { x: int = 1; };\nstruct B { y: int = 2; };\n#[emit]\nvar c: int = 3;\nvar d: int = 4;\nexport var e: int = 5;\n",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value, serde_json::json!({"A": {"x": 1}, "c": 3}));
    }

    #[test]
    fn export_var_without_emit_does_not_leak_env_values() {
        let json = emit_to_json(
            "#[emit]\nstruct Ok { a: int = 1; };\nexport var secret: str = env(\"HOME\") ?? \"x\";\n",
        )
        .unwrap();
        assert!(!json.contains("secret"), "{json}");
    }

    #[test]
    fn private_does_not_block_emit_when_marked() {
        let json = emit_to_json("#[emit]\nprivate struct P { x: int = 1; };\n").unwrap();
        assert!(json.contains("\"P\""), "{json}");
    }

    #[test]
    fn nothing_marked_is_an_error() {
        let errors =
            emit_to_json("struct A { x: int = 1; };\nexport var b: int = 2;\n").unwrap_err();
        assert_eq!(
            errors,
            vec!["nothing to emit: mark top-level structs or vars with #[emit]".to_string()]
        );
    }
}
