use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use serial_test::serial;

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
        let socket_name = format!("tmux-sidecar-startup-{}-{unique}", std::process::id());

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
fn startup_fatal_errors_are_reported_before_raw_mode() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let mut cmd = AssertCommand::cargo_bin("tmux-sidecar")?;
    cmd.args([
        "--socket-name",
        &server.socket_name,
        "--target-client",
        "missing-client",
    ]);

    cmd.assert().failure().stderr(
        predicate::str::contains("tmux client `missing-client` was not found").and(
            predicate::str::contains("failed to enable raw mode")
                .not()
                .and(predicate::str::contains("failed to configure terminal").not()),
        ),
    );

    Ok(())
}
