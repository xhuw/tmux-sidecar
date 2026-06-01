pub mod command;
pub mod hooks;
pub mod parse;
pub mod snapshot;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::model::{ClientName, SessionId, TmuxState, WindowId, WindowTarget};

use self::command::SocketOptions;

#[derive(Debug, Error)]
pub enum TmuxError {
    #[error(transparent)]
    Command(#[from] command::CommandError),
    #[error("failed to parse `{command}` output: {source}")]
    Parse {
        command: &'static str,
        #[source]
        source: parse::ParseError,
    },
    #[error("no tmux sessions found")]
    NoSessions,
    #[error("no tmux target client is available")]
    NoTargetClient,
    #[error("tmux client `{0}` was not found")]
    ClientNotFound(String),
    #[error("command `{command}` returned an empty value")]
    EmptyValue { command: &'static str },
    #[error("snapshot data mismatch: {0}")]
    SnapshotInvariant(String),
}

pub trait Tmux {
    fn snapshot(&self) -> Result<TmuxState, TmuxError>;
    fn resolve_target_client(&self, cli_override: Option<&str>) -> Result<ClientName, TmuxError>;
    fn switch_client_to_last_session(&self, client: &ClientName) -> Result<(), TmuxError>;
    fn switch_to(&self, client: &ClientName, target: WindowTarget) -> Result<(), TmuxError>;
    fn create_session(&self, name: Option<&str>) -> Result<SessionId, TmuxError>;
    fn create_window(
        &self,
        session: &SessionId,
        name: Option<&str>,
        current_path: Option<&Path>,
    ) -> Result<WindowId, TmuxError>;
    fn close_session(&self, session: &SessionId) -> Result<(), TmuxError>;
    fn close_window(&self, session: &SessionId, window: &WindowId) -> Result<(), TmuxError>;
    fn rename_session(&self, session: &SessionId, name: &str) -> Result<(), TmuxError>;
    fn rename_window(&self, window: &WindowId, name: &str) -> Result<(), TmuxError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowWorkdir {
    pub window_id: WindowId,
    pub window_index: u32,
    pub active: bool,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct TmuxCli {
    pub socket_name: Option<String>,
    pub socket_path: Option<PathBuf>,
}

impl TmuxCli {
    pub fn check_startup(&self, cli_override: Option<&str>) -> Result<ClientName, TmuxError> {
        self.ensure_tmux_exists()?;
        self.ensure_sessions_exist()?;
        self.resolve_target_client(cli_override)
    }

    fn ensure_tmux_exists(&self) -> Result<(), TmuxError> {
        let socket = self.socket_options();
        command::run_tmux(&socket, ["-V"])?;
        Ok(())
    }

    fn ensure_sessions_exist(&self) -> Result<(), TmuxError> {
        let socket = self.socket_options();
        let output = command::run_tmux(&socket, ["list-sessions", "-F", "#{session_id}"])?;
        if output.lines().any(|line| !line.trim().is_empty()) {
            Ok(())
        } else {
            Err(TmuxError::NoSessions)
        }
    }

    fn socket_options(&self) -> SocketOptions {
        SocketOptions::from_parts(self.socket_name.clone(), self.socket_path.clone())
    }

    fn client_from_tmux_pane(&self) -> Option<ClientName> {
        let pane = std::env::var("TMUX_PANE").ok()?;
        if pane.is_empty() {
            return None;
        }

        let socket = self.socket_options();
        self.display_pane_format(&socket, &pane, "#{client_name}")
            .map(ClientName)
    }

    fn display_pane_format(
        &self,
        socket: &SocketOptions,
        pane: &str,
        format: &'static str,
    ) -> Option<String> {
        let output =
            command::run_tmux(socket, ["display-message", "-p", "-t", pane, format]).ok()?;
        let value = output.trim();
        if value.is_empty() {
            return None;
        }

        Some(value.to_owned())
    }

    fn single_line_value(output: &str, command: &'static str) -> Result<String, TmuxError> {
        let value = output.lines().next().map(str::trim).unwrap_or_default();

        if value.is_empty() {
            return Err(TmuxError::EmptyValue { command });
        }

        Ok(value.to_owned())
    }

    pub fn session_window_workdirs(
        &self,
        session: &SessionId,
    ) -> Result<Vec<WindowWorkdir>, TmuxError> {
        let socket = self.socket_options();
        let output =
            command::run_tmux(&socket, ["list-panes", "-a", "-F", &pane_workdir_format()])?;
        parse_window_workdirs(&output, session).map_err(|source| TmuxError::Parse {
            command: "list-panes",
            source,
        })
    }

    #[allow(dead_code)]
    pub fn install_hooks(&self, program: &hooks::HookCommandProgram) -> Result<(), TmuxError> {
        hooks::install_hooks(&self.socket_options(), program)
    }

    #[allow(dead_code)]
    pub fn uninstall_hooks(&self) -> Result<(), TmuxError> {
        hooks::uninstall_hooks(&self.socket_options())
    }

    #[allow(dead_code)]
    pub fn init_plugin_snippet(program: &hooks::HookCommandProgram) -> String {
        hooks::init_plugin_snippet(program)
    }

    pub fn configure_window_monitoring(&self, window: &WindowId) -> Result<(), TmuxError> {
        hooks::configure_window_monitoring(&self.socket_options(), window)
    }

    #[allow(dead_code)]
    pub fn configure_all_window_monitoring(&self) -> Result<(), TmuxError> {
        hooks::configure_existing_window_monitoring(&self.socket_options())
    }
}

impl Tmux for TmuxCli {
    fn snapshot(&self) -> Result<TmuxState, TmuxError> {
        snapshot::collect_snapshot(&self.socket_options())
    }

    fn resolve_target_client(&self, cli_override: Option<&str>) -> Result<ClientName, TmuxError> {
        let socket = self.socket_options();
        let clients = snapshot::list_clients(&socket)?;

        if let Some(name) = cli_override {
            if clients.iter().any(|client| client.name.0 == name) {
                return Ok(ClientName(name.to_owned()));
            }

            return Err(TmuxError::ClientNotFound(name.to_owned()));
        }

        if let Some(current_client) = self.client_from_tmux_pane() {
            if clients.iter().any(|client| client.name == current_client) {
                return Ok(current_client);
            }
        }

        clients
            .into_iter()
            .max_by_key(|client| client.activity)
            .map(|client| client.name)
            .ok_or(TmuxError::NoTargetClient)
    }

    fn switch_to(&self, client: &ClientName, target: WindowTarget) -> Result<(), TmuxError> {
        let socket = self.socket_options();

        match target {
            WindowTarget::Session(session_id) => {
                command::run_tmux(
                    &socket,
                    [
                        "switch-client",
                        "-c",
                        client.0.as_str(),
                        "-t",
                        session_id.as_str(),
                    ],
                )?;
                Ok(())
            }
            WindowTarget::Window {
                session_id,
                window_id,
            } => {
                let target = format!("{session_id}:{window_id}");
                command::run_tmux(&socket, ["select-window", "-t", target.as_str()])?;
                command::run_tmux(
                    &socket,
                    [
                        "switch-client",
                        "-c",
                        client.0.as_str(),
                        "-t",
                        session_id.as_str(),
                    ],
                )?;
                Ok(())
            }
        }
    }

    fn switch_client_to_last_session(&self, client: &ClientName) -> Result<(), TmuxError> {
        let socket = self.socket_options();
        command::run_tmux(&socket, ["switch-client", "-c", client.0.as_str(), "-l"])?;
        Ok(())
    }

    fn create_session(&self, name: Option<&str>) -> Result<SessionId, TmuxError> {
        let socket = self.socket_options();
        let mut args = vec!["new-session", "-d", "-P", "-F", "#{session_id}"];
        if let Some(name) = name {
            args.extend(["-s", name]);
        }
        let output = command::run_tmux(&socket, args)?;

        Self::single_line_value(&output, "new-session")
    }

    fn create_window(
        &self,
        session: &SessionId,
        name: Option<&str>,
        current_path: Option<&Path>,
    ) -> Result<WindowId, TmuxError> {
        let socket = self.socket_options();
        let target = format!("{session}:");
        let mut args = vec![
            OsString::from("new-window"),
            OsString::from("-d"),
            OsString::from("-P"),
            OsString::from("-F"),
            OsString::from("#{window_id}"),
            OsString::from("-t"),
            OsString::from(target.as_str()),
        ];
        if let Some(current_path) = current_path {
            args.push(OsString::from("-c"));
            args.push(current_path.as_os_str().to_os_string());
        }
        if let Some(name) = name {
            args.push(OsString::from("-n"));
            args.push(OsString::from(name));
        }
        let output = command::run_tmux(&socket, args)?;

        Self::single_line_value(&output, "new-window")
    }

    fn close_session(&self, session: &SessionId) -> Result<(), TmuxError> {
        let socket = self.socket_options();
        command::run_tmux(&socket, ["kill-session", "-t", session.as_str()])?;
        Ok(())
    }

    fn close_window(&self, session: &SessionId, window: &WindowId) -> Result<(), TmuxError> {
        let socket = self.socket_options();
        let target = format!("{session}:{window}");
        command::run_tmux(&socket, ["kill-window", "-t", target.as_str()])?;
        Ok(())
    }

    fn rename_session(&self, session: &SessionId, name: &str) -> Result<(), TmuxError> {
        let socket = self.socket_options();
        command::run_tmux(&socket, ["rename-session", "-t", session.as_str(), name])?;
        Ok(())
    }

    fn rename_window(&self, window: &WindowId, name: &str) -> Result<(), TmuxError> {
        let socket = self.socket_options();
        command::run_tmux(&socket, ["rename-window", "-t", window.as_str(), name])?;
        Ok(())
    }
}

fn pane_workdir_format() -> String {
    let separator = parse::FIELD_SEPARATOR;
    format!(
        "#{{session_id}}{separator}#{{window_id}}{separator}#{{window_index}}{separator}#{{window_active}}{separator}#{{pane_active}}{separator}#{{pane_current_path}}"
    )
}

fn parse_window_workdirs(
    raw: &str,
    session_id: &str,
) -> Result<Vec<WindowWorkdir>, parse::ParseError> {
    let mut workdirs_by_window = BTreeMap::new();

    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }

        let fields = parse::split_fields(line);
        let [
            record_session_id,
            window_id,
            window_index,
            window_active,
            pane_active,
            current_path,
        ] = fields
            .try_into()
            .map_err(|values: Vec<&str>| parse::ParseError::WrongFieldCount {
                record: "pane",
                expected: 6,
                actual: values.len(),
                line: line.to_owned(),
            })?;

        if record_session_id != session_id || !parse_bool_field("pane_active", pane_active)? {
            continue;
        }
        if current_path.is_empty() {
            continue;
        }

        workdirs_by_window.insert(
            window_id.to_owned(),
            WindowWorkdir {
                window_id: window_id.to_owned(),
                window_index: parse_u32_field("window_index", window_index)?,
                active: parse_bool_field("window_active", window_active)?,
                path: PathBuf::from(current_path),
            },
        );
    }

    let mut workdirs: Vec<_> = workdirs_by_window.into_values().collect();
    workdirs.sort_by_key(|workdir| workdir.window_index);
    Ok(workdirs)
}

fn parse_u32_field(field: &'static str, value: &str) -> Result<u32, parse::ParseError> {
    value
        .parse()
        .map_err(|_| parse::ParseError::InvalidInteger {
            field,
            value: value.to_owned(),
        })
}

fn parse_bool_field(field: &'static str, value: &str) -> Result<bool, parse::ParseError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(parse::ParseError::InvalidBoolean {
            field,
            value: value.to_owned(),
        }),
    }
}
