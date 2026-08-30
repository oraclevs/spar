use std::process::{Command, Stdio};

#[cfg(unix)]
pub(super) fn command(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-cu").arg(script);
    inherit_stdio(&mut command);
    command
}

#[cfg(windows)]
pub(super) fn command(script: &str) -> Command {
    let mut command = Command::new("cmd");
    command.args(["/S", "/C", script]);
    inherit_stdio(&mut command);
    command
}

fn inherit_stdio(command: &mut Command) {
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
        let command = command("printf hello");

        assert_eq!(command.get_program(), OsStr::new("sh"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("-cu"), OsStr::new("printf hello")]
        );
    }

    #[cfg(windows)]
    #[test]
    fn selects_the_windows_shell() {
        let command = command("echo hello");

        assert_eq!(command.get_program(), OsStr::new("cmd"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("/S"), OsStr::new("/C"), OsStr::new("echo hello")]
        );
    }
}
