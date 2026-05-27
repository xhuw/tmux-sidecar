use std::{
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serial_test::serial;
use tmux_sidecar::{
    model::{ClientName, TreeRowKind, WindowAlert, WinlinkKey},
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
    control_client: Child,
    client_name: String,
}

impl IsolatedServer {
    fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let socket_name = format!("tmux-sidecar-linked-{}-{unique}", std::process::id());

        run_tmux(
            &socket_name,
            &[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "s1",
                "-n",
                "shared",
            ],
        )?;
        run_tmux(
            &socket_name,
            &["new-session", "-d", "-s", "s2", "-n", "own"],
        )?;
        run_tmux(
            &socket_name,
            &["link-window", "-d", "-s", "s1:0", "-t", "s2:5"],
        )?;
        run_tmux(&socket_name, &["select-window", "-t", "s2:0"])?;

        let control_client = Command::new("tmux")
            .args(["-L", &socket_name, "-C", "attach-session", "-t", "s1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let client_name = wait_for_client_name(&socket_name)?;

        Ok(Self {
            socket_name,
            control_client,
            client_name,
        })
    }

    fn tmux_cli(&self) -> TmuxCli {
        TmuxCli {
            socket_name: Some(self.socket_name.clone()),
            socket_path: None,
        }
    }
}

impl Drop for IsolatedServer {
    fn drop(&mut self) {
        let _ = self.control_client.kill();
        let _ = self.control_client.wait();
        let _ = Command::new("tmux")
            .args(["-L", &self.socket_name, "kill-server"])
            .status();
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

fn wait_for_client_name(socket_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    for _ in 0..20 {
        let output = run_tmux(socket_name, &["list-clients", "-F", "#{client_name}"])?;
        if let Some(name) = output.lines().find(|line| !line.trim().is_empty()) {
            return Ok(name.to_owned());
        }
        thread::sleep(Duration::from_millis(50));
    }

    Err("control-mode client did not attach".into())
}

#[test]
#[serial]
fn linked_window_alerts_remain_session_local() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    run_tmux(
        &server.socket_name,
        &["set", "-w", "-t", "s2:5", "monitor-bell", "on"],
    )?;
    run_tmux(
        &server.socket_name,
        &["send-keys", "-t", "s2:5", "printf \"\\a\"", "Enter"],
    )?;

    let tmux = server.tmux_cli();
    let target_client = ClientName(server.client_name.clone());
    for _ in 0..20 {
        let snapshot = tmux.snapshot()?;
        let current_session_id = snapshot
            .sessions
            .iter()
            .find(|session| session.name == "s1")
            .map(|session| session.id.clone())
            .ok_or("missing s1 session")?;
        let alerted_session_id = snapshot
            .sessions
            .iter()
            .find(|session| session.name == "s2")
            .map(|session| session.id.clone())
            .ok_or("missing s2 session")?;
        let linked_window_id = snapshot
            .sessions
            .iter()
            .find(|session| session.id == current_session_id)
            .and_then(|session| session.windows.first())
            .map(|window| window.id.clone())
            .ok_or("missing linked window in s1")?;
        let session_states = snapshot.session_states();
        let current_key = WinlinkKey::new(current_session_id.clone(), linked_window_id.clone());
        let alerted_key = WinlinkKey::new(alerted_session_id.clone(), linked_window_id.clone());
        let rows = snapshot.tree_rows_for_client(Some(&target_client));
        let mut current_row = None;
        let mut alerted_row = None;

        for row in rows {
            match row.kind {
                TreeRowKind::Window {
                    ref session_id,
                    ref id,
                    active,
                    alert,
                    ..
                } if *id == linked_window_id && *session_id == current_session_id => {
                    current_row = Some((row.focus, active, alert));
                }
                TreeRowKind::Window {
                    ref session_id,
                    ref id,
                    active,
                    alert,
                    ..
                } if *id == linked_window_id && *session_id == alerted_session_id => {
                    alerted_row = Some((row.focus, active, alert));
                }
                _ => {}
            }
        }

        let Some((current_focus, current_active, current_alert)) = current_row else {
            return Err("missing current-session linked row".into());
        };
        let Some((alerted_focus, alerted_active, alerted_alert)) = alerted_row else {
            return Err("missing alerted-session linked row".into());
        };

        if current_active
            && current_alert == WindowAlert::None
            && !alerted_active
            && alerted_alert == WindowAlert::Bell
            && current_focus != alerted_focus
            && snapshot.visible_window_key(Some(&target_client)) == Some(current_key.clone())
            && session_states
                .get(&current_session_id)
                .and_then(|session| session.windows.get(&current_key))
                .map(|window| window.active && !window.bell_flag)
                .unwrap_or(false)
            && session_states
                .get(&alerted_session_id)
                .and_then(|session| session.windows.get(&alerted_key))
                .map(|window| !window.active && window.bell_flag)
                .unwrap_or(false)
        {
            return Ok(());
        }

        thread::sleep(Duration::from_millis(50));
    }

    Err("linked window rows did not keep session-local alert state".into())
}
