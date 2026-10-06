use crate::ast::SparType;
use crate::runtime::{NativeFunction, NativeRegistry, Value};

use super::support::{error, int_arg, string_arg};

#[cfg(not(target_arch = "wasm32"))]
fn os_terminal_size() -> Option<(i64, i64)> {
    crossterm::terminal::size()
        .ok()
        .map(|(width, height)| (i64::from(width), i64::from(height)))
}

#[cfg(target_arch = "wasm32")]
fn os_terminal_size() -> Option<(i64, i64)> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
struct RawModeGuard(bool);

#[cfg(not(target_arch = "wasm32"))]
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn key_name(key: crossterm::event::KeyEvent) -> String {
    use crossterm::event::{KeyCode, KeyModifiers};
    let name = match key.code {
        KeyCode::Char(character) => character.to_string(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Escape".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Delete => "Delete".into(),
        KeyCode::Insert => "Insert".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "BackTab".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::F(number) => format!("F{number}"),
        other => format!("{other:?}"),
    };
    let mut modifiers = String::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        modifiers.push_str("Ctrl+");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        modifiers.push_str("Alt+");
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        modifiers.push_str("Shift+");
    }
    format!("{modifiers}{name}")
}

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
            "readKey",
            vec![("timeoutMs", SparType::Int)],
            SparType::Applied {
                name: "Option".into(),
                arguments: vec![SparType::Str],
            },
            true,
            |context, args| {
                let timeout = int_arg(args, 0, "timeoutMs")?;
                if timeout < -1 {
                    return Err(error("timeoutMs must be -1 or a nonnegative number"));
                }
                if !context.stdin_is_terminal() {
                    return Ok(Value::Option(None));
                }
                #[cfg(target_arch = "wasm32")]
                {
                    Ok(Value::Option(None))
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    use crossterm::event::{self, Event, KeyEventKind};
                    use std::time::{Duration, Instant};
                    let already_raw =
                        crossterm::terminal::is_raw_mode_enabled().map_err(|reason| {
                            error(format!("could not inspect terminal mode: {reason}"))
                        })?;
                    if !already_raw {
                        crossterm::terminal::enable_raw_mode().map_err(|reason| {
                            error(format!("could not enable raw mode: {reason}"))
                        })?;
                    }
                    let _guard = RawModeGuard(!already_raw);
                    let deadline = if timeout >= 0 {
                        Instant::now().checked_add(Duration::from_millis(timeout as u64))
                    } else {
                        None
                    };
                    if timeout >= 0 && deadline.is_none() {
                        return Err(error("timeoutMs is too large"));
                    }
                    loop {
                        if let Some(deadline) = deadline {
                            let remaining = deadline.saturating_duration_since(Instant::now());
                            if !event::poll(remaining).map_err(|reason| {
                                error(format!("could not poll terminal: {reason}"))
                            })? {
                                return Ok(Value::Option(None));
                            }
                        }
                        let next = event::read().map_err(|reason| {
                            error(format!("could not read terminal: {reason}"))
                        })?;
                        if let Event::Key(key) = next {
                            if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                                return Ok(Value::Option(Some(Box::new(Value::String(key_name(
                                    key,
                                ))))));
                            }
                        }
                    }
                }
            },
        ))
        .expect("nativeTerminal::readKey registration must be unique");
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
                    .stdout_is_terminal()
                    .then(os_terminal_size)
                    .flatten()
                    .map(|(width, _)| width)
                    .or_else(|| {
                        context
                            .env_get("COLUMNS")
                            .and_then(|value| value.parse::<i64>().ok())
                            .filter(|value| *value > 0)
                    })
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
                    .stdout_is_terminal()
                    .then(os_terminal_size)
                    .flatten()
                    .map(|(_, height)| height)
                    .or_else(|| {
                        context
                            .env_get("LINES")
                            .and_then(|value| value.parse::<i64>().ok())
                            .filter(|value| *value > 0)
                    })
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::key_name;

    #[test]
    fn key_names_include_modifiers() {
        assert_eq!(
            key_name(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            "Ctrl+c"
        );
        assert_eq!(
            key_name(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT)),
            "Alt+Up"
        );
        assert_eq!(
            key_name(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            "Enter"
        );
    }
}
