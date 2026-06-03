use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
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

    fn socket_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let output = Command::new("tmux")
            .args([
                "-L",
                &self.socket_name,
                "display-message",
                "-p",
                "#{socket_path}",
            ])
            .output()?;

        if !output.status.success() {
            return Err("failed to resolve tmux socket path".into());
        }

        let socket_path = String::from_utf8(output.stdout)?.trim().to_owned();
        if socket_path.is_empty() {
            return Err("tmux returned an empty socket path".into());
        }

        Ok(PathBuf::from(socket_path))
    }

    fn kill(&self) -> Result<(), Box<dyn std::error::Error>> {
        let status = Command::new("tmux")
            .args(["-L", &self.socket_name, "kill-server"])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err("failed to kill isolated tmux server".into())
        }
    }
}

impl Drop for IsolatedServer {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket_name, "kill-server"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
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

#[test]
#[serial]
fn daemon_exits_when_tmux_server_stops() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let socket_path = server.socket_path()?;
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_tmux-sidecar"))
        .args(["daemon", "--socket-path"])
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    wait_for_socket_path(&socket_path, Duration::from_secs(2))?;
    server.kill()?;
    wait_for_process_exit(&mut daemon, Duration::from_secs(6))?;

    Ok(())
}

fn wait_for_socket_path(
    path: &PathBuf,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;

    while std::time::Instant::now() < deadline {
        if path.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }

    Err(format!("timed out waiting for socket path `{}`", path.display()).into())
}

fn wait_for_process_exit(
    child: &mut Child,
    timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = std::time::Instant::now() + timeout;

    while std::time::Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }

    let _ = child.kill();
    let _ = child.wait();
    Err("timed out waiting for daemon to exit".into())
}
