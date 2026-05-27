pub mod command;
pub mod parse;
pub mod snapshot;

use std::path::PathBuf;

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
    fn switch_to(&self, client: &ClientName, target: WindowTarget) -> Result<(), TmuxError>;
    fn create_session(&self, name: Option<&str>) -> Result<SessionId, TmuxError>;
    fn create_window(&self, session: &SessionId, name: Option<&str>)
    -> Result<WindowId, TmuxError>;
    fn close_window(&self, window: &WindowId) -> Result<(), TmuxError>;
    fn rename_session(&self, session: &SessionId, name: &str) -> Result<(), TmuxError>;
    fn rename_window(&self, window: &WindowId, name: &str) -> Result<(), TmuxError>;
}

#[derive(Debug, Clone, Default)]
pub struct TmuxCli {
    pub socket_name: Option<String>,
    pub socket_path: Option<PathBuf>,
}

impl TmuxCli {
    pub fn check_startup(&self, cli_override: Option<&str>) -> Result<ClientName, TmuxError> {
        self.ensure_tmux_exists()?;
        let snapshot = self.snapshot()?;
        if snapshot.sessions.is_empty() {
            return Err(TmuxError::NoSessions);
        }

        self.resolve_target_client(cli_override)
    }

    fn ensure_tmux_exists(&self) -> Result<(), TmuxError> {
        let socket = self.socket_options();
        command::run_tmux(&socket, ["-V"])?;
        Ok(())
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
    ) -> Result<WindowId, TmuxError> {
        let socket = self.socket_options();
        let target = format!("{session}:");
        let mut args = vec![
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{window_id}",
            "-t",
            target.as_str(),
        ];
        if let Some(name) = name {
            args.extend(["-n", name]);
        }
        let output = command::run_tmux(&socket, args)?;

        Self::single_line_value(&output, "new-window")
    }

    fn close_window(&self, window: &WindowId) -> Result<(), TmuxError> {
        let socket = self.socket_options();
        command::run_tmux(&socket, ["kill-window", "-t", window.as_str()])?;
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
