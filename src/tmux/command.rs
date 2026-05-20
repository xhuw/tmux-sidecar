use std::{
    ffi::OsString,
    io,
    path::PathBuf,
    process::{Command, ExitStatus},
};

use thiserror::Error;

#[derive(Debug, Clone, Default)]
pub struct SocketOptions {
    pub name: Option<String>,
    pub path: Option<PathBuf>,
}

impl SocketOptions {
    pub fn from_parts(name: Option<String>, path: Option<PathBuf>) -> Self {
        Self { name, path }
    }

    pub fn apply_to(&self, command: &mut Command) {
        if let Some(name) = &self.name {
            command.arg("-L").arg(name);
        }

        if let Some(path) = &self.path {
            command.arg("-S").arg(path);
        }
    }

    fn as_args(&self) -> Vec<OsString> {
        let mut args = Vec::new();

        if let Some(name) = &self.name {
            args.push(OsString::from("-L"));
            args.push(OsString::from(name));
        }

        if let Some(path) = &self.path {
            args.push(OsString::from("-S"));
            args.push(path.as_os_str().to_os_string());
        }

        args
    }
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("failed to spawn `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: io::Error,
    },
    #[error("`{command}` exited with status {status}: {stderr}")]
    Failed {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("`{command}` returned non-utf8 {stream}")]
    NonUtf8 {
        command: String,
        stream: &'static str,
    },
}

pub fn tmux_command(socket: &SocketOptions) -> Command {
    let mut command = Command::new("tmux");
    socket.apply_to(&mut command);
    command
}

pub fn run_tmux<I, S>(socket: &SocketOptions, args: I) -> Result<String, CommandError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.as_ref().to_owned())
        .collect();

    let mut command = tmux_command(socket);
    command.args(&args);

    let command_line = display_tmux_command(socket, &args);
    let output = command.output().map_err(|source| CommandError::Spawn {
        command: command_line.clone(),
        source,
    })?;

    if !output.status.success() {
        let stderr = match String::from_utf8(output.stderr) {
            Ok(stderr) => stderr.trim().to_owned(),
            Err(_) => String::from("<non-utf8 stderr>"),
        };

        return Err(CommandError::Failed {
            command: command_line,
            status: display_status(output.status),
            stderr,
        });
    }

    String::from_utf8(output.stdout).map_err(|_| CommandError::NonUtf8 {
        command: command_line,
        stream: "stdout",
    })
}

fn display_tmux_command(socket: &SocketOptions, args: &[String]) -> String {
    let mut parts = vec![String::from("tmux")];

    for arg in socket.as_args() {
        parts.push(arg.to_string_lossy().into_owned());
    }

    parts.extend(args.iter().cloned());
    parts.join(" ")
}

fn display_status(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => code.to_string(),
        None => String::from("terminated by signal"),
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::SocketOptions;

    #[test]
    fn socket_options_apply_name_and_path() {
        let socket = SocketOptions::from_parts(
            Some(String::from("isolated")),
            Some("/var/run/tmux-test.sock".into()),
        );
        let mut command = Command::new("tmux");

        socket.apply_to(&mut command);

        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            args,
            vec!["-L", "isolated", "-S", "/var/run/tmux-test.sock"]
        );
    }
}
