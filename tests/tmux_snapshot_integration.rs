use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serial_test::serial;
use tmux_sidecar::tmux::{Tmux, TmuxCli};

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

struct IsolatedServer {
    socket_name: String,
}

impl IsolatedServer {
    fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let socket_name = format!("tmux-sidecar-it-{}-{unique}", std::process::id());

        let status = Command::new("tmux")
            .args([
                "-L",
                &socket_name,
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "it-main",
                "-n",
                "it-win",
            ])
            .status()?;

        if !status.success() {
            return Err("failed to create isolated tmux server".into());
        }

        let status = Command::new("tmux")
            .args([
                "-L",
                &socket_name,
                "new-window",
                "-d",
                "-t",
                "it-main:",
                "-n",
                "extra",
            ])
            .status()?;

        if !status.success() {
            return Err("failed to create window in isolated tmux server".into());
        }

        Ok(Self { socket_name })
    }
}

impl Drop for IsolatedServer {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket_name, "kill-server"])
            .status();
    }
}

#[test]
#[serial]
fn snapshot_reads_isolated_tmux_server() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = TmuxCli {
        socket_name: Some(server.socket_name.clone()),
        socket_path: None,
    };

    let snapshot = tmux.snapshot()?;

    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.sessions[0].name, "it-main");
    assert_eq!(snapshot.sessions[0].windows.len(), 2);
    assert_eq!(snapshot.sessions[0].windows[0].name, "it-win");
    assert_eq!(snapshot.sessions[0].windows[1].name, "extra");

    Ok(())
}
