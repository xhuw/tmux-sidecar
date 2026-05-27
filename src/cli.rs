use std::{path::PathBuf, time::Duration};

use clap::{ArgGroup, Args, Parser, Subcommand};

use crate::ipc::HookName;

#[derive(Debug, Clone, Parser)]
#[command(author, version, about, long_about = None)]
#[command(group(
    ArgGroup::new("socket")
        .args(["socket_name", "socket_path"])
        .multiple(false)
))]
pub struct Cli {
    /// tmux socket name passed with `tmux -L`.
    #[arg(long = "socket-name", value_name = "SOCKET")]
    pub socket_name: Option<String>,

    /// tmux socket path passed with `tmux -S`.
    #[arg(long = "socket-path", value_name = "PATH")]
    pub socket_path: Option<PathBuf>,

    /// Override the tmux client used for switch operations.
    #[arg(long = "target-client", value_name = "CLIENT")]
    pub target_client: Option<String>,

    /// UI render/input poll interval in milliseconds.
    #[arg(
        long = "poll-interval-ms",
        value_name = "MILLIS",
        default_value_t = 500
    )]
    pub poll_interval_ms: u64,

    /// Exit immediately after selecting a session or window.
    #[arg(long = "auto-quit")]
    pub auto_quit: bool,

    /// Test-only helper to print a snapshot and exit.
    #[arg(long = "print-snapshot", hide = true)]
    pub print_snapshot: bool,

    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

impl Cli {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms.max(1))
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    /// Install or refresh tmux-sidecar hooks for the current tmux server.
    InstallHooks(InstallHooksArgs),
    /// Remove tmux-sidecar-managed hooks from the current tmux server.
    UninstallHooks(UninstallHooksArgs),
    /// Print a tmux.conf snippet that installs tmux-sidecar hooks.
    InitPlugin,
    /// Run the per-tmux-socket sidecar server.
    Server(ServerArgs),
    /// Send a single tmux hook event to the sidecar server.
    Hook(HookArgs),
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("server_socket")
        .args(["socket_name", "socket_path"])
        .multiple(false)
))]
pub struct ServerArgs {
    /// Stop the running sidecar server for the selected tmux socket.
    #[arg(long = "kill")]
    pub kill: bool,

    /// tmux socket name passed with `tmux -L` when using `--kill`.
    #[arg(long = "socket-name", value_name = "SOCKET")]
    pub socket_name: Option<String>,

    /// tmux socket path passed through tmux hook interpolation.
    #[arg(long = "socket-path", value_name = "PATH")]
    pub socket_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("socket")
        .args(["socket_name", "socket_path"])
        .multiple(false)
))]
pub struct InstallHooksArgs {
    /// tmux socket name passed with `tmux -L`.
    #[arg(long = "socket-name", value_name = "SOCKET")]
    pub socket_name: Option<String>,

    /// tmux socket path passed with `tmux -S`.
    #[arg(long = "socket-path", value_name = "PATH")]
    pub socket_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("socket")
        .args(["socket_name", "socket_path"])
        .multiple(false)
))]
pub struct UninstallHooksArgs {
    /// tmux socket name passed with `tmux -L`.
    #[arg(long = "socket-name", value_name = "SOCKET")]
    pub socket_name: Option<String>,

    /// tmux socket path passed with `tmux -S`.
    #[arg(long = "socket-path", value_name = "PATH")]
    pub socket_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct HookArgs {
    /// tmux socket path passed through tmux hook interpolation.
    #[arg(long = "socket-path", value_name = "PATH")]
    pub socket_path: PathBuf,

    /// tmux hook event name.
    #[arg(long = "event", value_name = "EVENT")]
    pub event: HookName,

    #[arg(long = "session-id", value_name = "SESSION")]
    pub session_id: Option<String>,

    #[arg(long = "window-id", value_name = "WINDOW")]
    pub window_id: Option<String>,

    #[arg(long = "window-index", value_name = "INDEX")]
    pub window_index: Option<u32>,

    #[arg(long = "pane-id", value_name = "PANE")]
    pub pane_id: Option<String>,

    #[arg(long = "client-name", value_name = "CLIENT")]
    pub client_name: Option<String>,

    #[arg(long = "timestamp-ms", value_name = "MILLIS")]
    pub timestamp_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, CliCommand};

    #[test]
    fn parses_hook_management_subcommands() {
        let install = Cli::try_parse_from(["tmux-sidecar", "install-hooks"]).unwrap();
        assert!(matches!(install.command, Some(CliCommand::InstallHooks(_))));

        let uninstall = Cli::try_parse_from(["tmux-sidecar", "uninstall-hooks"]).unwrap();
        assert!(matches!(
            uninstall.command,
            Some(CliCommand::UninstallHooks(_))
        ));

        let init_plugin = Cli::try_parse_from(["tmux-sidecar", "init-plugin"]).unwrap();
        assert!(matches!(init_plugin.command, Some(CliCommand::InitPlugin)));
    }

    #[test]
    fn install_hooks_accepts_socket_selection_flags() {
        let cli = Cli::try_parse_from(["tmux-sidecar", "install-hooks", "--socket-name", "work"])
            .unwrap();

        let Some(CliCommand::InstallHooks(args)) = cli.command else {
            panic!("expected install-hooks command");
        };

        assert_eq!(args.socket_name.as_deref(), Some("work"));
    }

    #[test]
    fn server_kill_accepts_optional_socket_selection_flags() {
        let cli = Cli::try_parse_from(["tmux-sidecar", "server", "--kill"]).unwrap();

        let Some(CliCommand::Server(args)) = cli.command else {
            panic!("expected server command");
        };

        assert!(args.kill);
        assert!(args.socket_name.is_none());
        assert!(args.socket_path.is_none());

        let cli =
            Cli::try_parse_from(["tmux-sidecar", "server", "--kill", "--socket-name", "work"])
                .unwrap();

        let Some(CliCommand::Server(args)) = cli.command else {
            panic!("expected server command");
        };

        assert!(args.kill);
        assert_eq!(args.socket_name.as_deref(), Some("work"));
    }
}
