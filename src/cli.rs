use std::{path::PathBuf, time::Duration};

use clap::{ArgGroup, Parser};

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

    /// Poll interval in milliseconds for tmux snapshot refresh.
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
}

impl Cli {
    pub fn poll_interval(&self) -> Duration {
        Duration::from_millis(self.poll_interval_ms.max(1))
    }
}
