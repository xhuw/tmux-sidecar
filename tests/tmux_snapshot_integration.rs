use std::{
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use serial_test::serial;
use tmux_sidecar::{
    model::WindowAlert,
    tmux::{Tmux, TmuxCli},
};

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

struct ControlClient {
    child: Child,
}

impl ControlClient {
    fn attach(socket_name: &str, target: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let child = Command::new("tmux")
            .args(["-L", socket_name, "-C", "attach-session", "-t", target])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        thread::sleep(Duration::from_millis(200));
        Ok(Self { child })
    }
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run_tmux(socket_name: &str, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("tmux")
        .arg("-L")
        .arg(socket_name)
        .args(args)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("tmux command failed: {} ({stderr})", args.join(" ")).into());
    }
    Ok(String::from_utf8(output.stdout)?)
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

#[test]
#[serial]
fn snapshot_reads_background_window_alerts() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let _control = ControlClient::attach(&server.socket_name, "it-main")?;
    run_tmux(
        &server.socket_name,
        &["set", "-w", "-t", "it-main:1", "monitor-bell", "on"],
    )?;
    run_tmux(
        &server.socket_name,
        &["send-keys", "-t", "it-main:1", "printf \"\\a\"", "Enter"],
    )?;

    let tmux = TmuxCli {
        socket_name: Some(server.socket_name.clone()),
        socket_path: None,
    };

    for _ in 0..20 {
        let snapshot = tmux.snapshot()?;
        let extra_window = snapshot.sessions[0]
            .windows
            .iter()
            .find(|window| window.name == "extra")
            .ok_or("missing extra window in snapshot")?;
        if extra_window.alert == WindowAlert::Bell {
            return Ok(());
        }

        thread::sleep(Duration::from_millis(50));
    }

    Err("background window bell alert was not observed in snapshot".into())
}
