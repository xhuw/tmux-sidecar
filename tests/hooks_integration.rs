use std::{
    env,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use serial_test::serial;
use tmux_sidecar::{
    client::{self, IpcClient, ReadStatus},
    ipc::{ProjectionState, ServerMessage},
    tmux::{TmuxCli, hooks},
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
        let socket_name = format!("tmux-sidecar-hooks-it-{}-{unique}", std::process::id());

        run_tmux(
            &socket_name,
            &[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "it-main",
                "-n",
                "it-win",
            ],
        )?;
        run_tmux(
            &socket_name,
            &["new-window", "-d", "-t", "it-main:", "-n", "it-extra"],
        )?;
        run_tmux(
            &socket_name,
            &[
                "new-session",
                "-d",
                "-s",
                "it-second",
                "-n",
                "it-second-win",
            ],
        )?;

        Ok(Self { socket_name })
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
        let _ = Command::new("tmux")
            .args(["-L", &self.socket_name, "kill-server"])
            .status();
    }
}

struct AlertServer {
    socket_name: String,
    control_client: Child,
}

impl AlertServer {
    fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let socket_name = format!(
            "tmux-sidecar-alert-hooks-it-{}-{unique}",
            std::process::id()
        );

        run_tmux(
            &socket_name,
            &[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "it-main",
                "-n",
                "it-main-win",
            ],
        )?;
        run_tmux(
            &socket_name,
            &[
                "new-session",
                "-d",
                "-s",
                "it-detached",
                "-n",
                "it-detached-win",
            ],
        )?;

        let control_client = Command::new("tmux")
            .args(["-L", &socket_name, "-C", "attach-session", "-t", "it-main"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        wait_for_attached_client(&socket_name)?;

        Ok(Self {
            socket_name,
            control_client,
        })
    }

    fn tmux_cli(&self) -> TmuxCli {
        TmuxCli {
            socket_name: Some(self.socket_name.clone()),
            socket_path: None,
        }
    }

    fn socket_path(&self) -> Result<PathBuf, Box<dyn std::error::Error>> {
        Ok(PathBuf::from(
            run_tmux(
                &self.socket_name,
                &["display-message", "-p", "#{socket_path}"],
            )?
            .trim(),
        ))
    }
}

impl Drop for AlertServer {
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

fn wait_for_attached_client(socket_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..20 {
        let output = run_tmux(socket_name, &["list-clients", "-F", "#{client_name}"])?;
        if output.lines().any(|line| !line.trim().is_empty()) {
            return Ok(());
        }
        thread::sleep(std::time::Duration::from_millis(50));
    }

    Err("control-mode client did not attach".into())
}

fn next_state(
    subscription: &mut IpcClient,
) -> Result<Option<ProjectionState>, Box<dyn std::error::Error>> {
    match subscription.read_status()? {
        ReadStatus::Message(ServerMessage::StateUpdated(update)) => Ok(Some(update.state)),
        ReadStatus::Message(_) | ReadStatus::Pending => Ok(None),
        ReadStatus::Closed => Err("sidecar server closed subscription".into()),
    }
}

fn state_has_detached_bell(state: &ProjectionState) -> bool {
    state.sessions.iter().any(|session| {
        session.name == "it-detached"
            && session
                .windows
                .iter()
                .any(|window| window.name == "it-detached-win" && window.bell_flag)
    })
}

#[test]
#[serial]
fn install_hooks_reserves_indexed_slots_without_duplication()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    run_tmux(
        &server.socket_name,
        &[
            "set-hook",
            "-g",
            "alert-bell[42]",
            "display-message user-bell",
        ],
    )?;

    let tmux = server.tmux_cli();
    let program = hooks::HookCommandProgram::default();
    let expected_hooks = hooks::installed_hooks(&program);

    tmux.install_hooks(&program)?;
    tmux.install_hooks(&program)?;

    let all_hooks = run_tmux(&server.socket_name, &["show-hooks", "-g"])?;
    assert_eq!(
        all_hooks
            .lines()
            .filter(|line| line.contains("tmux-sidecar hook"))
            .count(),
        expected_hooks.len()
    );

    for hook in &expected_hooks {
        let output = run_tmux(&server.socket_name, &["show-hooks", "-g", hook.name])?;
        let expected = format!("{} {}", hook.qualified_name(), hook.command);
        assert!(
            output.lines().any(|line| line.trim() == expected),
            "missing reserved hook entry `{expected}` in `{output}`"
        );
    }

    let alert_bell_hooks = run_tmux(&server.socket_name, &["show-hooks", "-g", "alert-bell"])?;
    assert!(
        alert_bell_hooks
            .lines()
            .any(|line| line.trim() == "alert-bell[42] display-message user-bell")
    );

    Ok(())
}

#[test]
#[serial]
fn alert_bell_hook_marks_unattached_session_alerts_for_subscribers()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    unsafe {
        env::set_var(
            "CARGO_BIN_EXE_tmux-sidecar",
            env!("CARGO_BIN_EXE_tmux-sidecar"),
        );
    }

    let server = AlertServer::start()?;
    let socket_path = server.socket_path()?;
    let program =
        hooks::HookCommandProgram::new(vec![env!("CARGO_BIN_EXE_tmux-sidecar").to_owned()]);
    server.tmux_cli().install_hooks(&program)?;

    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut subscription = client::subscribe(&socket_path, None)?;
        subscription.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
        let _ = next_state(&mut subscription)?;
        subscription.set_read_timeout(Some(std::time::Duration::from_millis(500)))?;

        run_tmux(
            &server.socket_name,
            &[
                "send-keys",
                "-t",
                "it-detached:0",
                "printf \"\\a\"",
                "Enter",
            ],
        )?;

        for _ in 0..10 {
            if let Some(state) = next_state(&mut subscription)? {
                if state_has_detached_bell(&state) {
                    return Ok(());
                }
            }
        }

        Err("detached session bell alert was not pushed to the sidecar subscriber".into())
    })();

    let _ = client::shutdown_server(&socket_path);
    result
}

#[test]
#[serial]
fn alert_bell_hook_is_visible_to_late_subscribers_after_auto_spawn()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    unsafe {
        env::set_var(
            "CARGO_BIN_EXE_tmux-sidecar",
            env!("CARGO_BIN_EXE_tmux-sidecar"),
        );
    }

    let server = AlertServer::start()?;
    let socket_path = server.socket_path()?;
    let program =
        hooks::HookCommandProgram::new(vec![env!("CARGO_BIN_EXE_tmux-sidecar").to_owned()]);
    server.tmux_cli().install_hooks(&program)?;

    run_tmux(
        &server.socket_name,
        &[
            "send-keys",
            "-t",
            "it-detached:0",
            "printf \"\\a\"",
            "Enter",
        ],
    )?;
    thread::sleep(std::time::Duration::from_millis(300));
    run_tmux(
        &server.socket_name,
        &["rename-session", "-t", "it-main", "it-main-renamed"],
    )?;
    thread::sleep(std::time::Duration::from_millis(500));

    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut subscription = client::subscribe(&socket_path, None)?;
        subscription.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;

        for _ in 0..5 {
            if let Some(state) = next_state(&mut subscription)? {
                if state_has_detached_bell(&state) {
                    return Ok(());
                }
            }
        }

        Err("detached session bell alert was not retained for a late sidecar subscriber".into())
    })();

    let _ = client::shutdown_existing_server(&socket_path);
    result
}

#[test]
#[serial]
fn install_hooks_configures_monitoring_and_uninstall_keeps_user_hooks()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    run_tmux(
        &server.socket_name,
        &[
            "set-hook",
            "-g",
            "session-created[7]",
            "display-message keep-me",
        ],
    )?;
    run_tmux(
        &server.socket_name,
        &[
            "set-hook",
            "-g",
            "alert-activity[909]",
            "display-message stale-activity",
        ],
    )?;
    run_tmux(
        &server.socket_name,
        &[
            "set-hook",
            "-g",
            "alert-silence[911]",
            "display-message stale-silence",
        ],
    )?;
    run_tmux(
        &server.socket_name,
        &["set-window-option", "-g", "monitor-activity", "off"],
    )?;
    run_tmux(
        &server.socket_name,
        &["set-window-option", "-g", "monitor-silence", "123"],
    )?;
    for target in ["it-main:0", "it-main:1", "it-second:0"] {
        run_tmux(
            &server.socket_name,
            &["set-window-option", "-t", target, "monitor-activity", "off"],
        )?;
        run_tmux(
            &server.socket_name,
            &["set-window-option", "-t", target, "monitor-silence", "77"],
        )?;
    }

    let tmux = server.tmux_cli();
    tmux.install_hooks(&hooks::HookCommandProgram::default())?;

    let activity_hooks = run_tmux(&server.socket_name, &["show-hooks", "-g", "alert-activity"])?;
    assert!(!activity_hooks.contains("stale-activity"));
    let silence_hooks = run_tmux(&server.socket_name, &["show-hooks", "-g", "alert-silence"])?;
    assert!(!silence_hooks.contains("stale-silence"));

    for target in ["it-main:0", "it-main:1", "it-second:0"] {
        assert_eq!(
            run_tmux(
                &server.socket_name,
                &["show-options", "-wqv", "-t", target, "monitor-activity"],
            )?
            .trim(),
            "off"
        );
        assert_eq!(
            run_tmux(
                &server.socket_name,
                &["show-options", "-wqv", "-t", target, "monitor-bell"],
            )?
            .trim(),
            "on"
        );
        assert_eq!(
            run_tmux(
                &server.socket_name,
                &["show-options", "-wqv", "-t", target, "monitor-silence"],
            )?
            .trim(),
            "77"
        );
    }

    assert_eq!(
        run_tmux(
            &server.socket_name,
            &["show-options", "-gwqv", "monitor-activity"],
        )?
        .trim(),
        "off"
    );
    assert_eq!(
        run_tmux(
            &server.socket_name,
            &["show-options", "-gwqv", "monitor-bell"],
        )?
        .trim(),
        "on"
    );
    assert_eq!(
        run_tmux(
            &server.socket_name,
            &["show-options", "-gwqv", "monitor-silence"],
        )?
        .trim(),
        "123"
    );

    run_tmux(
        &server.socket_name,
        &["new-window", "-d", "-t", "it-main:", "-n", "post-install"],
    )?;
    for (option, expected) in [
        ("monitor-activity", "off"),
        ("monitor-bell", "on"),
        ("monitor-silence", "123"),
    ] {
        assert_eq!(
            run_tmux(
                &server.socket_name,
                &[
                    "show-options",
                    "-Awqv",
                    "-t",
                    "it-main:post-install",
                    option
                ],
            )?
            .trim(),
            expected
        );
    }

    tmux.uninstall_hooks()?;

    let all_hooks = run_tmux(&server.socket_name, &["show-hooks", "-g"])?;
    assert_eq!(
        all_hooks
            .lines()
            .filter(|line| line.contains("tmux-sidecar hook"))
            .count(),
        0
    );

    let created_hooks = run_tmux(
        &server.socket_name,
        &["show-hooks", "-g", "session-created"],
    )?;
    assert!(
        created_hooks
            .lines()
            .any(|line| line.trim() == "session-created[7] display-message keep-me")
    );
    let activity_hooks = run_tmux(&server.socket_name, &["show-hooks", "-g", "alert-activity"])?;
    assert!(!activity_hooks.contains("stale-activity"));
    let silence_hooks = run_tmux(&server.socket_name, &["show-hooks", "-g", "alert-silence"])?;
    assert!(!silence_hooks.contains("stale-silence"));

    Ok(())
}
