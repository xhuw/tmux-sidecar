use std::{
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serial_test::serial;
use tmux_sidecar::tmux::{TmuxCli, hooks};

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

    let tmux = server.tmux_cli();
    tmux.install_hooks(&hooks::HookCommandProgram::default())?;

    for target in ["it-main:0", "it-main:1", "it-second:0"] {
        assert_eq!(
            run_tmux(
                &server.socket_name,
                &["show-options", "-wqv", "-t", target, "monitor-activity"],
            )?
            .trim(),
            "on"
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
            "10"
        );
    }

    assert_eq!(
        run_tmux(
            &server.socket_name,
            &["show-options", "-gwqv", "monitor-activity"],
        )?
        .trim(),
        "on"
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
        "10"
    );

    run_tmux(
        &server.socket_name,
        &["new-window", "-d", "-t", "it-main:", "-n", "post-install"],
    )?;
    for option in ["monitor-activity", "monitor-bell", "monitor-silence"] {
        let expected = if option == "monitor-silence" {
            "10"
        } else {
            "on"
        };
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

    Ok(())
}
