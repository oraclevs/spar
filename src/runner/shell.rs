use std::path::Path;
use std::process::{Command, Stdio};

/// Bash-body task commands always run under `bash -c`; `run { }` (the Spar
/// shell) never reaches this module — it executes natively.
pub(super) fn command(script: &str) -> Command {
    let mut command = Command::new("bash");
    command.arg("-c").arg(script);
    inherit_stdio(&mut command);
    command
}

#[cfg(unix)]
pub(super) fn script_command(path: &Path, _script: &str) -> Result<Command, String> {
    let mut command = Command::new(path);
    inherit_stdio(&mut command);
    Ok(command)
}

#[cfg(windows)]
pub(super) fn script_command(path: &Path, script: &str) -> Result<Command, String> {
    let interpreter = parse_shebang(script)?;
    let mut command = Command::new(&interpreter[0]);
    command.args(&interpreter[1..]).arg(path);
    inherit_stdio(&mut command);
    Ok(command)
}

#[cfg(any(windows, test))]
fn parse_shebang(script: &str) -> Result<Vec<String>, String> {
    let line = script
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("#!"))
        .ok_or_else(|| "script is missing a #! interpreter line".to_owned())?;
    let interpreter: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
    if interpreter.is_empty() {
        Err("script has an empty #! interpreter line".to_owned())
    } else {
        Ok(interpreter)
    }
}

pub(super) fn inherit_stdio(command: &mut Command) {
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::command;

    #[test]
    fn bash_commands_run_under_bash_dash_c() {
        let command = command("printf hello");

        assert_eq!(command.get_program(), OsStr::new("bash"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("-c"), OsStr::new("printf hello")]
        );
    }

    #[test]
    fn parses_shebang_interpreter_tokens() {
        assert_eq!(
            super::parse_shebang("#!/usr/bin/env bash\nprintf hello\n").unwrap(),
            ["/usr/bin/env", "bash"]
        );
    }
}
