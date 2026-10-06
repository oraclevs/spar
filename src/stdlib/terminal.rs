use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{error, int_arg, string_arg};

pub(crate) fn register(registry: &mut NativeRegistry) {
    registry
        .register(NativeFunction::sync(
            "nativeTerminal",
            "isStdinTty",
            vec![],
            SparType::Bool,
            true,
            |context, _args| Ok(Value::Bool(context.stdin_is_terminal())),
        ))
        .expect("nativeTerminal::isStdinTty registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTerminal",
            "supportsColor",
            vec![],
            SparType::Bool,
            true,
            |context, _args| {
                let no_color = context
                    .env_get("NO_COLOR")
                    .is_some_and(|value| !value.is_empty());
                let term_dumb = context.env_get("TERM").as_deref() == Some("dumb");
                Ok(Value::Bool(
                    context.stdout_is_terminal() && !no_color && !term_dumb,
                ))
            },
        ))
        .expect("nativeTerminal::supportsColor registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTerminal",
            "isStdoutTty",
            vec![],
            SparType::Bool,
            true,
            |context, _args| Ok(Value::Bool(context.stdout_is_terminal())),
        ))
        .expect("nativeTerminal::isStdoutTty registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTerminal",
            "isStderrTty",
            vec![],
            SparType::Bool,
            true,
            |context, _args| Ok(Value::Bool(context.stderr_is_terminal())),
        ))
        .expect("nativeTerminal::isStderrTty registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTerminal",
            "width",
            vec![],
            SparType::Int,
            true,
            |context, _args| {
                let width = context
                    .env_get("COLUMNS")
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(80);
                Ok(Value::Int(width))
            },
        ))
        .expect("nativeTerminal::width registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTerminal",
            "height",
            vec![],
            SparType::Int,
            true,
            |context, _args| {
                let height = context
                    .env_get("LINES")
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(24);
                Ok(Value::Int(height))
            },
        ))
        .expect("nativeTerminal::height registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTerminal",
            "style",
            vec![("code", SparType::Int), ("text", SparType::Str)],
            SparType::Str,
            true,
            |_context, args| {
                Ok(Value::String(format!(
                    "\u{1b}[{}m{}\u{1b}[0m",
                    int_arg(args, 0, "code")?,
                    string_arg(args, 1, "text")?
                )))
            },
        ))
        .expect("nativeTerminal::style registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTerminal",
            "clearScreen",
            vec![],
            SparType::Str,
            true,
            |_context, _args| Ok(Value::String("\u{1b}[2J\u{1b}[H".into())),
        ))
        .expect("nativeTerminal::clearScreen registration must be unique");
    registry
        .register(NativeFunction::sync(
            "nativeTerminal",
            "moveCursor",
            vec![("row", SparType::Int), ("column", SparType::Int)],
            SparType::Str,
            true,
            |_context, args| {
                Ok(Value::String(format!(
                    "\u{1b}[{};{}H",
                    int_arg(args, 0, "row")?,
                    int_arg(args, 1, "column")?
                )))
            },
        ))
        .expect("nativeTerminal::moveCursor registration must be unique");
    for (name, direction) in [
        ("moveUp", 'A'),
        ("moveDown", 'B'),
        ("moveRight", 'C'),
        ("moveLeft", 'D'),
    ] {
        registry
            .register(NativeFunction::sync(
                "nativeTerminal",
                name,
                vec![("count", SparType::Int)],
                SparType::Str,
                true,
                move |_context, args| {
                    let count = int_arg(args, 0, "count")?;
                    if count < 1 {
                        return Err(error("cursor move count must be at least 1"));
                    }
                    Ok(Value::String(format!("\u{1b}[{count}{direction}")))
                },
            ))
            .expect("nativeTerminal cursor movement registration must be unique");
    }
    for (name, sequence) in [
        ("reset", "\u{1b}[0m"),
        ("clearLine", "\u{1b}[2K\r"),
        ("clearToEnd", "\u{1b}[0J"),
        ("clearToLineEnd", "\u{1b}[0K"),
        ("bell", "\u{7}"),
        ("hideCursor", "\u{1b}[?25l"),
        ("showCursor", "\u{1b}[?25h"),
        ("saveCursor", "\u{1b}7"),
        ("restoreCursor", "\u{1b}8"),
        ("enterAlternateScreen", "\u{1b}[?1049h"),
        ("leaveAlternateScreen", "\u{1b}[?1049l"),
    ] {
        registry
            .register(NativeFunction::sync(
                "nativeTerminal",
                name,
                vec![],
                SparType::Str,
                true,
                move |_context, _args| Ok(Value::String(sequence.into())),
            ))
            .expect("nativeTerminal escape registration must be unique");
    }
}
