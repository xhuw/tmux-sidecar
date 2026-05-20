use std::{
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use serial_test::serial;
use tmux_sidecar::{
    app::App,
    cli::Cli,
    model::{ClientName, Focus, Mode},
    tmux::{Tmux, TmuxCli},
    ui::TREE_START_ROW,
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
        let socket_name = format!("tmux-sidecar-wf-{}-{unique}", std::process::id());

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

        let control_client = Command::new("tmux")
            .args(["-L", &socket_name, "-C", "attach-session", "-t", "it-main"])
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

    fn app(&self) -> Result<App, Box<dyn std::error::Error>> {
        let cli = Cli {
            socket_name: Some(self.socket_name.clone()),
            socket_path: None,
            target_client: Some(self.client_name.clone()),
            poll_interval_ms: 500,
            print_snapshot: false,
        };
        let mut app = App::new(cli);
        let tmux = self.tmux_cli();
        app.apply_snapshot(tmux.snapshot()?);
        app.state_mut().target_client = Some(ClientName(self.client_name.clone()));
        Ok(app)
    }

    fn client_session_id(&self) -> Result<String, Box<dyn std::error::Error>> {
        let output = run_tmux(
            &self.socket_name,
            &["list-clients", "-F", "#{client_name}\t#{session_id}"],
        )?;
        output
            .lines()
            .find_map(|line| {
                let mut fields = line.split('\t');
                let name = fields.next()?;
                let session = fields.next()?;
                (name == self.client_name).then(|| session.to_owned())
            })
            .ok_or_else(|| "failed to resolve client session".into())
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::CONTROL)
}

fn mouse_left(row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 0,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn type_text(app: &mut App, text: &str) -> Result<(), Box<dyn std::error::Error>> {
    for ch in text.chars() {
        app.on_key_event(key(KeyCode::Char(ch)))?;
    }
    Ok(())
}

#[test]
#[serial]
fn refresh_syncs_external_create_rename_close_reindex_and_active_changes()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;

    let main_session = app
        .state()
        .tmux
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .ok_or("missing it-main session")?;
    let main_session_id = main_session.id.clone();
    let first_window_id = main_session
        .windows
        .first()
        .map(|window| window.id.clone())
        .ok_or("missing first window in it-main")?;
    let extra_window_id = main_session
        .windows
        .iter()
        .find(|window| window.name == "it-extra")
        .map(|window| window.id.clone())
        .ok_or("missing it-extra window")?;

    run_tmux(
        &server.socket_name,
        &[
            "new-window",
            "-d",
            "-t",
            &main_session_id,
            "-n",
            "ext-created",
        ],
    )?;
    app.apply_snapshot(tmux.snapshot()?);
    let created_window_id = app
        .state()
        .tmux
        .sessions
        .iter()
        .find(|session| session.id == main_session_id)
        .and_then(|session| {
            session
                .windows
                .iter()
                .find(|window| window.name == "ext-created")
        })
        .map(|window| window.id.clone())
        .ok_or("missing externally created window after refresh")?;

    run_tmux(
        &server.socket_name,
        &["rename-session", "-t", &main_session_id, "renamed-main"],
    )?;
    run_tmux(
        &server.socket_name,
        &["rename-window", "-t", &extra_window_id, "renamed-extra"],
    )?;
    run_tmux(
        &server.socket_name,
        &["select-window", "-t", &created_window_id],
    )?;
    run_tmux(
        &server.socket_name,
        &[
            "set-option",
            "-t",
            &main_session_id,
            "renumber-windows",
            "on",
        ],
    )?;
    run_tmux(
        &server.socket_name,
        &["kill-window", "-t", &first_window_id],
    )?;

    app.apply_snapshot(tmux.snapshot()?);
    let refreshed_main = app
        .state()
        .tmux
        .sessions
        .iter()
        .find(|session| session.id == main_session_id)
        .ok_or("missing refreshed it-main session")?;

    assert_eq!(refreshed_main.name, "renamed-main");
    assert_eq!(
        refreshed_main.active_window_id.as_deref(),
        Some(created_window_id.as_str())
    );
    assert!(
        refreshed_main
            .windows
            .iter()
            .any(|window| window.name == "renamed-extra")
    );
    assert!(
        refreshed_main
            .windows
            .iter()
            .any(|window| window.id == created_window_id && window.active)
    );
    assert!(
        !refreshed_main
            .windows
            .iter()
            .any(|window| window.id == first_window_id)
    );

    let indexes: Vec<u32> = refreshed_main
        .windows
        .iter()
        .map(|window| window.index)
        .collect();
    let expected: Vec<u32> = (0..u32::try_from(indexes.len())?).collect();
    assert_eq!(indexes, expected);

    Ok(())
}

#[test]
#[serial]
fn switches_session_from_keyboard_and_mouse_activation() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let mut app = server.app()?;
    let snapshot = app.state().tmux.clone();
    let main_id = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .map(|session| session.id.clone())
        .ok_or("missing it-main session")?;
    let second_id = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-second")
        .map(|session| session.id.clone())
        .ok_or("missing it-second session")?;

    app.state_mut().focus = Focus::Session(second_id.clone());
    app.on_key_event(key(KeyCode::Enter))?;
    assert_eq!(server.client_session_id()?, second_id);

    let rows = app.state().tree_rows();
    let row_index = rows
        .iter()
        .position(|row| row.focus == Focus::Session(main_id.clone()))
        .ok_or("missing row for it-main")?;
    let row = TREE_START_ROW + u16::try_from(row_index)?;
    app.on_mouse_event(mouse_left(row))?;

    assert_eq!(server.client_session_id()?, main_id);
    Ok(())
}

#[test]
#[serial]
fn creates_session_with_inline_naming_accept_and_cancel() -> Result<(), Box<dyn std::error::Error>>
{
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;

    app.state_mut().focus = Focus::CreateSession;
    app.on_key_event(key(KeyCode::Enter))?;

    let first_session_id = match app.state().mode.clone() {
        Mode::CreateSessionName { id, .. } => id,
        other => return Err(format!("unexpected mode after create session: {other:?}").into()),
    };
    assert_eq!(server.client_session_id()?, first_session_id);

    app.on_key_event(ctrl(KeyCode::Char('u')))?;
    type_text(&mut app, "created-session")?;
    app.on_key_event(key(KeyCode::Enter))?;
    assert_eq!(app.state().mode, Mode::Normal);
    let snapshot = tmux.snapshot()?;
    let created_name = snapshot
        .sessions
        .iter()
        .find(|session| session.id == first_session_id)
        .map(|session| session.name.as_str());
    assert_eq!(created_name, Some("created-session"));

    app.state_mut().focus = Focus::CreateSession;
    app.on_key_event(key(KeyCode::Enter))?;
    let second_session_id = match app.state().mode.clone() {
        Mode::CreateSessionName { id, .. } => id,
        other => return Err(format!("unexpected mode after second create: {other:?}").into()),
    };
    let default_name = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == second_session_id)
        .map(|session| session.name)
        .ok_or("missing second created session")?;

    app.on_key_event(key(KeyCode::Esc))?;
    assert_eq!(app.state().mode, Mode::Normal);

    let retained_name = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == second_session_id)
        .map(|session| session.name);
    assert_eq!(retained_name.as_deref(), Some(default_name.as_str()));

    Ok(())
}

#[test]
#[serial]
fn creates_window_with_inline_naming_accept_and_cancel() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;

    let main_session_id = app
        .state()
        .tmux
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .map(|session| session.id.clone())
        .ok_or("missing it-main session")?;

    app.state_mut().focus = Focus::CreateWindow(main_session_id.clone());
    app.on_key_event(key(KeyCode::Enter))?;
    let first_window_id = match app.state().mode.clone() {
        Mode::CreateWindowName { id, .. } => id,
        other => return Err(format!("unexpected mode after create window: {other:?}").into()),
    };

    let created_window_active = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .and_then(|session| session.active_window_id)
        .map(|id| id == first_window_id)
        .unwrap_or(false);
    assert!(created_window_active);

    app.on_key_event(ctrl(KeyCode::Char('u')))?;
    type_text(&mut app, "created-window")?;
    app.on_key_event(key(KeyCode::Enter))?;
    assert_eq!(app.state().mode, Mode::Normal);

    let renamed_window = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .flat_map(|session| session.windows.into_iter())
        .find(|window| window.id == first_window_id)
        .map(|window| window.name);
    assert_eq!(renamed_window.as_deref(), Some("created-window"));

    app.state_mut().focus = Focus::CreateWindow(main_session_id.clone());
    app.on_key_event(key(KeyCode::Enter))?;
    let second_window_id = match app.state().mode.clone() {
        Mode::CreateWindowName { id, .. } => id,
        other => {
            return Err(format!("unexpected mode after second create window: {other:?}").into());
        }
    };
    let default_name = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .flat_map(|session| session.windows.into_iter())
        .find(|window| window.id == second_window_id)
        .map(|window| window.name)
        .ok_or("missing second created window")?;

    app.on_key_event(key(KeyCode::Esc))?;
    assert_eq!(app.state().mode, Mode::Normal);

    let retained_name = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .flat_map(|session| session.windows.into_iter())
        .find(|window| window.id == second_window_id)
        .map(|window| window.name);
    assert_eq!(retained_name.as_deref(), Some(default_name.as_str()));

    Ok(())
}

#[test]
#[serial]
fn renames_with_r_and_refreshes_on_failed_rename() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;
    let snapshot = tmux.snapshot()?;
    let main_session_id = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .map(|session| session.id.clone())
        .ok_or("missing it-main session")?;
    let doomed_window_id = snapshot
        .sessions
        .iter()
        .find(|session| session.id == main_session_id)
        .and_then(|session| session.windows.first())
        .map(|window| window.id.clone())
        .ok_or("missing window in it-main")?;

    app.state_mut().focus = Focus::Session(main_session_id.clone());
    app.on_key_event(key(KeyCode::Char('r')))?;
    app.on_key_event(ctrl(KeyCode::Char('u')))?;
    type_text(&mut app, "renamed-main")?;
    app.on_key_event(key(KeyCode::Enter))?;

    let renamed_session = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .map(|session| session.name);
    assert_eq!(renamed_session.as_deref(), Some("renamed-main"));

    app.state_mut().focus = Focus::Window(doomed_window_id.clone());
    app.on_key_event(key(KeyCode::Char('r')))?;
    run_tmux(
        &server.socket_name,
        &["kill-window", "-t", &doomed_window_id],
    )?;
    app.on_key_event(key(KeyCode::Enter))?;

    assert_eq!(app.state().mode, Mode::Normal);
    assert!(app.state().last_error.is_some());
    assert!(
        !app.state()
            .tmux
            .sessions
            .iter()
            .flat_map(|session| session.windows.iter())
            .any(|window| window.id == doomed_window_id)
    );
    assert!(
        app.state()
            .tree_rows()
            .iter()
            .any(|row| row.focus == app.state().focus)
    );

    Ok(())
}

#[test]
#[serial]
fn failed_switch_action_refreshes_state_without_speculative_focus()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let mut app = server.app()?;
    let second_session_id = app
        .state()
        .tmux
        .sessions
        .iter()
        .find(|session| session.name == "it-second")
        .map(|session| session.id.clone())
        .ok_or("missing it-second session")?;

    app.state_mut().focus = Focus::Session(second_session_id.clone());
    run_tmux(
        &server.socket_name,
        &["kill-session", "-t", &second_session_id],
    )?;

    app.on_key_event(key(KeyCode::Enter))?;

    assert_eq!(app.state().mode, Mode::Normal);
    assert!(app.state().last_error.is_some());
    assert!(
        !app.state()
            .tmux
            .sessions
            .iter()
            .any(|session| session.id == second_session_id)
    );
    assert!(
        app.state()
            .tree_rows()
            .iter()
            .any(|row| row.focus == app.state().focus)
    );

    Ok(())
}

#[test]
#[serial]
fn rename_treats_shell_metacharacters_as_literal_text() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;
    let session_id = app
        .state()
        .tmux
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .map(|session| session.id.clone())
        .ok_or("missing it-main session")?;
    let literal_name = "semi; spaced name";

    app.state_mut().focus = Focus::Session(session_id.clone());
    app.on_key_event(key(KeyCode::Char('r')))?;
    app.on_key_event(ctrl(KeyCode::Char('u')))?;
    type_text(&mut app, literal_name)?;
    app.on_key_event(key(KeyCode::Enter))?;

    let renamed_session = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .map(|session| session.name);
    assert_eq!(renamed_session.as_deref(), Some(literal_name));

    Ok(())
}
