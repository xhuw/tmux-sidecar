use std::path::PathBuf;

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

    /// Legacy compatibility knob retained for older scripts; ignored by the event-driven UI runtime.
    #[arg(
        long = "poll-interval-ms",
        value_name = "MILLIS",
        default_value_t = 500,
        hide = true
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

#[derive(Debug, Clone, Subcommand)]
pub enum CliCommand {
    /// Install or refresh tmux-sidecar hooks and monitoring for the current tmux server.
    #[command(alias = "install-hooks")]
    Setup(SetupArgs),
    /// Remove tmux-sidecar-managed hooks from the current tmux server.
    #[command(alias = "uninstall-hooks")]
    Teardown(TeardownArgs),
    /// Print a tmux.conf snippet that installs tmux-sidecar hooks.
    InitPlugin,
    /// Run the per-tmux-socket sidecar daemon.
    #[command(alias = "server")]
    Daemon(DaemonArgs),
    /// Send a single tmux hook event to the sidecar daemon.
    Hook(HookArgs),
    /// Query current sidecar state for scripts and status lines.
    Query(QueryArgs),
}

#[derive(Debug, Clone, Args, Default)]
#[command(group(
    ArgGroup::new("query_socket")
        .args(["socket_name", "socket_path"])
        .multiple(false)
))]
pub struct QueryArgs {
    /// tmux socket name passed with `tmux -L`.
    #[arg(long = "socket-name", value_name = "SOCKET")]
    pub socket_name: Option<String>,

    /// tmux socket path passed with `tmux -S`.
    #[arg(long = "socket-path", value_name = "PATH")]
    pub socket_path: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<QueryCommand>,
}

impl QueryArgs {
    pub fn command_or_default(&self) -> QueryCommand {
        self.command.unwrap_or(QueryCommand::Alerts)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum QueryCommand {
    /// Print the number of active bell alerts.
    Alerts,
    /// Print the current sidecar projection as JSON.
    All,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("daemon_socket")
        .args(["socket_name", "socket_path"])
        .multiple(false)
))]
pub struct DaemonArgs {
    /// Stop the running sidecar daemon for the selected tmux socket.
    #[arg(long = "stop", alias = "kill")]
    pub stop: bool,

    /// tmux socket name passed with `tmux -L` when using `--stop`.
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
pub struct SetupArgs {
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
pub struct TeardownArgs {
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

    #[arg(long = "pane-current-path", value_name = "PATH")]
    pub pane_current_path: Option<PathBuf>,

    #[arg(long = "client-name", value_name = "CLIENT")]
    pub client_name: Option<String>,

    #[arg(long = "timestamp-ms", value_name = "MILLIS")]
    pub timestamp_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, CliCommand, QueryCommand};

    #[test]
    fn parses_public_surface_subcommands() {
        let setup = Cli::try_parse_from(["tmux-sidecar", "setup"]).unwrap();
        assert!(matches!(setup.command, Some(CliCommand::Setup(_))));

        let teardown = Cli::try_parse_from(["tmux-sidecar", "teardown"]).unwrap();
        assert!(matches!(teardown.command, Some(CliCommand::Teardown(_))));

        let init_plugin = Cli::try_parse_from(["tmux-sidecar", "init-plugin"]).unwrap();
        assert!(matches!(init_plugin.command, Some(CliCommand::InitPlugin)));

        let daemon = Cli::try_parse_from([
            "tmux-sidecar",
            "daemon",
            "--socket-path",
            "/tmp/tmux/default",
        ])
        .unwrap();
        assert!(matches!(daemon.command, Some(CliCommand::Daemon(_))));
    }

    #[test]
    fn legacy_command_aliases_still_parse() {
        let setup = Cli::try_parse_from(["tmux-sidecar", "install-hooks"]).unwrap();
        assert!(matches!(setup.command, Some(CliCommand::Setup(_))));

        let teardown = Cli::try_parse_from(["tmux-sidecar", "uninstall-hooks"]).unwrap();
        assert!(matches!(teardown.command, Some(CliCommand::Teardown(_))));

        let daemon = Cli::try_parse_from(["tmux-sidecar", "server", "--kill"]).unwrap();
        assert!(matches!(daemon.command, Some(CliCommand::Daemon(_))));
    }

    #[test]
    fn setup_accepts_socket_selection_flags() {
        let cli = Cli::try_parse_from(["tmux-sidecar", "setup", "--socket-name", "work"]).unwrap();

        let Some(CliCommand::Setup(args)) = cli.command else {
            panic!("expected setup command");
        };

        assert_eq!(args.socket_name.as_deref(), Some("work"));
    }

    #[test]
    fn query_defaults_to_alerts_and_accepts_explicit_subcommands() {
        let cli = Cli::try_parse_from(["tmux-sidecar", "query"]).unwrap();

        let Some(CliCommand::Query(args)) = cli.command else {
            panic!("expected query command");
        };
        assert_eq!(args.command_or_default(), QueryCommand::Alerts);

        let cli = Cli::try_parse_from(["tmux-sidecar", "query", "alerts"]).unwrap();

        let Some(CliCommand::Query(args)) = cli.command else {
            panic!("expected query alerts command");
        };
        assert_eq!(args.command_or_default(), QueryCommand::Alerts);

        let cli = Cli::try_parse_from(["tmux-sidecar", "query", "all"]).unwrap();

        let Some(CliCommand::Query(args)) = cli.command else {
            panic!("expected query all command");
        };
        assert_eq!(args.command_or_default(), QueryCommand::All);
    }

    #[test]
    fn query_accepts_socket_selection_flags() {
        let cli = Cli::try_parse_from([
            "tmux-sidecar",
            "query",
            "--socket-path",
            "/tmp/tmux/default",
            "alerts",
        ])
        .unwrap();

        let Some(CliCommand::Query(args)) = cli.command else {
            panic!("expected query command");
        };
        assert_eq!(
            args.socket_path.as_deref(),
            Some(std::path::Path::new("/tmp/tmux/default"))
        );
    }

    #[test]
    fn daemon_stop_accepts_optional_socket_selection_flags() {
        let cli = Cli::try_parse_from(["tmux-sidecar", "daemon", "--stop"]).unwrap();

        let Some(CliCommand::Daemon(args)) = cli.command else {
            panic!("expected daemon command");
        };

        assert!(args.stop);
        assert!(args.socket_name.is_none());
        assert!(args.socket_path.is_none());

        let cli =
            Cli::try_parse_from(["tmux-sidecar", "daemon", "--stop", "--socket-name", "work"])
                .unwrap();

        let Some(CliCommand::Daemon(args)) = cli.command else {
            panic!("expected daemon command");
        };

        assert!(args.stop);
        assert_eq!(args.socket_name.as_deref(), Some("work"));

        let cli = Cli::try_parse_from(["tmux-sidecar", "server", "--kill"]).unwrap();

        let Some(CliCommand::Daemon(args)) = cli.command else {
            panic!("expected daemon alias command");
        };

        assert!(args.stop);
    }
}
