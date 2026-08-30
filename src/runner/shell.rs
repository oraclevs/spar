use std::path::Path;
use std::process::{Command, Stdio};

pub(super) fn command(script: &str, shell: Option<&[String]>) -> Command {
    if let Some((program, arguments)) = shell.and_then(|shell| shell.split_first()) {
        let mut command = Command::new(program);
        command.args(arguments).arg(script);
        inherit_stdio(&mut command);
        return command;
    }
    platform_command(script)
}

#[cfg(unix)]
fn platform_command(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-cu").arg(script);
    inherit_stdio(&mut command);
    command
}

#[cfg(windows)]
fn platform_command(script: &str) -> Command {
    let mut command = Command::new("cmd");
    command.args(["/S", "/C", script]);
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

    #[cfg(unix)]
    #[test]
    fn selects_the_unix_shell() {
        let command = command("printf hello", None);

        assert_eq!(command.get_program(), OsStr::new("sh"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("-cu"), OsStr::new("printf hello")]
        );
    }

    #[cfg(windows)]
    #[test]
    fn selects_the_windows_shell() {
        let command = command("echo hello", None);

        assert_eq!(command.get_program(), OsStr::new("cmd"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("/S"), OsStr::new("/C"), OsStr::new("echo hello")]
        );
    }

    #[test]
    fn custom_shell_appends_script_as_final_argument() {
        let shell = vec![
            "bash".to_owned(),
            "-euo".to_owned(),
            "pipefail".to_owned(),
            "-c".to_owned(),
        ];

        let command = command("printf hello", Some(&shell));

        assert_eq!(command.get_program(), OsStr::new("bash"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                OsStr::new("-euo"),
                OsStr::new("pipefail"),
                OsStr::new("-c"),
                OsStr::new("printf hello"),
            ]
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
