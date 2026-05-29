use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use serial_test::serial;
use tmux_sidecar::{
    app::App,
    cli::Cli,
    model::{Focus, Mode},
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
        self.app_with_auto_quit(false)
    }

    fn app_with_auto_quit(&self, auto_quit: bool) -> Result<App, Box<dyn std::error::Error>> {
        let cli = Cli {
            socket_name: Some(self.socket_name.clone()),
            socket_path: None,
            target_client: Some(self.client_name.clone()),
            poll_interval_ms: 500,
            auto_quit,
            print_snapshot: false,
            command: None,
        };
        unsafe {
            env::set_var(
                "CARGO_BIN_EXE_tmux-sidecar",
                env!("CARGO_BIN_EXE_tmux-sidecar"),
            );
        }
        let mut app = App::new(cli);
        app.startup()?;
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

    fn client_window_id(&self) -> Result<String, Box<dyn std::error::Error>> {
        let output = run_tmux(
            &self.socket_name,
            &["list-clients", "-F", "#{client_name}\t#{window_id}"],
        )?;
        output
            .lines()
            .find_map(|line| {
                let mut fields = line.split('\t');
                let name = fields.next()?;
                let window = fields.next()?;
                (name == self.client_name).then(|| window.to_owned())
            })
            .ok_or_else(|| "failed to resolve client window".into())
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

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = env::temp_dir().join(format!("tmux-sidecar-{name}-{unique}"));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
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

fn window_current_path(
    socket_name: &str,
    target: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(PathBuf::from(
        run_tmux(
            socket_name,
            &[
                "display-message",
                "-p",
                "-t",
                target,
                "#{pane_current_path}",
            ],
        )?
        .trim(),
    ))
}

fn set_window_path(
    socket_name: &str,
    target: &str,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let path_str = path.to_str().ok_or("window path is not valid utf-8")?;
    run_tmux(
        socket_name,
        &[
            "send-keys",
            "-t",
            target,
            &format!("cd {path_str}"),
            "Enter",
        ],
    )?;

    for _ in 0..20 {
        if window_current_path(socket_name, target)? == path {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }

    Err(format!(
        "window `{target}` did not change to expected path `{}`",
        path.display()
    )
    .into())
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

fn window_focus(session_id: &str, window_id: &str) -> Focus {
    Focus::window(session_id, window_id)
}

#[test]
#[serial]
fn snapshot_tracks_target_client_visible_window_for_active_rows()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let app = server.app()?;

    let snapshot = tmux.snapshot()?;
    let main_session_id = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .map(|session| session.id.clone())
        .ok_or("missing it-main session in snapshot")?;
    let main_window_id = snapshot
        .sessions
        .iter()
        .find(|session| session.id == main_session_id)
        .and_then(|session| session.active_window_id.clone())
        .ok_or("missing active it-main window in snapshot")?;
    let second_session_id = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-second")
        .map(|session| session.id.clone())
        .ok_or("missing it-second session in snapshot")?;
    let second_window_id = snapshot
        .sessions
        .iter()
        .find(|session| session.id == second_session_id)
        .and_then(|session| session.windows.first().map(|window| window.id.clone()))
        .ok_or("missing it-second window in snapshot")?;
    let client = snapshot
        .clients
        .iter()
        .find(|client| client.name.0 == server.client_name)
        .ok_or("missing control-mode client in snapshot")?;
    assert_eq!(client.session_id, main_session_id);
    assert_eq!(
        client.current_window_id.as_deref(),
        Some(main_window_id.as_str())
    );

    let rows = app.state().tree_rows();
    let main_session = rows
        .iter()
        .find(|row| row.focus == Focus::Session(main_session_id.clone()))
        .ok_or("missing it-main session row")?;
    let main_window = rows
        .iter()
        .find(|row| row.focus == window_focus(&main_session_id, &main_window_id))
        .ok_or("missing it-main window row")?;
    let second_session = rows
        .iter()
        .find(|row| row.focus == Focus::Session(second_session_id.clone()))
        .ok_or("missing it-second session row")?;
    let second_window = rows
        .iter()
        .find(|row| row.focus == window_focus(&second_session_id, &second_window_id))
        .ok_or("missing it-second window row")?;

    assert!(!main_session.active());
    assert!(main_window.active());
    assert!(!second_session.active());
    assert!(!second_window.active());

    Ok(())
}

#[test]
#[serial]
fn startup_focuses_target_clients_active_window() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let app = server.app()?;
    let focused_session_id = server.client_session_id()?;
    let focused_window_id = server.client_window_id()?;

    assert_eq!(
        app.state().focus,
        window_focus(&focused_session_id, &focused_window_id)
    );

    Ok(())
}

#[test]
#[serial]
fn startup_shows_toast_when_sidecar_server_auto_starts() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let app = server.app()?;

    assert_eq!(
        app.state()
            .toast
            .as_ref()
            .map(|toast| toast.message.as_str()),
        Some("Started tmux-sidecar server")
    );

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
    assert!(app.sync_with_server(Duration::from_secs(2))?);
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

    assert!(app.sync_with_server(Duration::from_secs(2))?);
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
fn syncs_hook_driven_active_window_and_client_session_changes()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let mut app = server.app()?;
    let snapshot = app.state().tmux.clone();
    let main_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .ok_or("missing it-main session")?;
    let main_session_id = main_session.id.clone();
    let main_window_id = main_session
        .active_window_id
        .clone()
        .ok_or("missing active it-main window")?;
    let extra_window_id = main_session
        .windows
        .iter()
        .find(|window| window.name == "it-extra")
        .map(|window| window.id.clone())
        .ok_or("missing it-extra window")?;
    let second_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-second")
        .ok_or("missing it-second session")?;
    let second_session_id = second_session.id.clone();
    let second_window_id = second_session
        .active_window_id
        .clone()
        .or_else(|| {
            second_session
                .windows
                .first()
                .map(|window| window.id.clone())
        })
        .ok_or("missing active it-second window")?;

    run_tmux(
        &server.socket_name,
        &["select-window", "-t", &extra_window_id],
    )?;
    assert!(app.sync_with_server(Duration::from_secs(2))?);
    assert_eq!(server.client_window_id()?, extra_window_id);

    let rows = app.state().tree_rows();
    let main_window = rows
        .iter()
        .find(|row| row.focus == window_focus(&main_session_id, &main_window_id))
        .ok_or("missing original it-main window row")?;
    let extra_window = rows
        .iter()
        .find(|row| row.focus == window_focus(&main_session_id, &extra_window_id))
        .ok_or("missing it-extra window row")?;
    assert!(!main_window.active());
    assert!(extra_window.active());

    run_tmux(
        &server.socket_name,
        &[
            "switch-client",
            "-c",
            &server.client_name,
            "-t",
            &second_session_id,
        ],
    )?;
    assert_eq!(server.client_session_id()?, second_session_id);

    let mut switched = false;
    for _ in 0..10 {
        let _ = app.sync_with_server(Duration::from_millis(250))?;
        let Some(client) = app
            .state()
            .tmux
            .clients
            .iter()
            .find(|client| client.name.0 == server.client_name)
        else {
            continue;
        };

        if client.session_id == second_session_id
            && client.current_window_id.as_deref() == Some(second_window_id.as_str())
        {
            switched = true;
            break;
        }
    }
    assert!(switched, "target client switch did not reach the UI state");

    let rows = app.state().tree_rows();
    let extra_window = rows
        .iter()
        .find(|row| row.focus == window_focus(&main_session_id, &extra_window_id))
        .ok_or("missing it-extra window row after switch-client")?;
    let second_window = rows
        .iter()
        .find(|row| row.focus == window_focus(&second_session_id, &second_window_id))
        .ok_or("missing it-second window row after switch-client")?;
    assert!(!extra_window.active());
    assert!(second_window.active());

    Ok(())
}

#[test]
#[serial]
fn syncs_external_session_close_without_manual_tmux_polling()
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

    assert!(app.sync_with_server(Duration::from_secs(2))?);
    assert!(
        !app.state()
            .tmux
            .sessions
            .iter()
            .any(|session| session.id == second_session_id)
    );
    assert_ne!(app.state().focus, Focus::Session(second_session_id));
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
fn auto_quit_exits_after_keyboard_and_mouse_window_selection()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let snapshot = server.tmux_cli().snapshot()?;
    let main_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .ok_or("missing it-main session")?;
    let main_window_id = main_session
        .windows
        .iter()
        .find(|window| window.name == "it-win")
        .map(|window| window.id.clone())
        .ok_or("missing it-win window")?;
    let extra_window_id = main_session
        .windows
        .iter()
        .find(|window| window.name == "it-extra")
        .map(|window| window.id.clone())
        .ok_or("missing it-extra window")?;

    let mut keyboard_app = server.app_with_auto_quit(true)?;
    keyboard_app.state_mut().focus = window_focus(&main_session.id, &extra_window_id);
    keyboard_app.on_key_event(key(KeyCode::Enter))?;
    assert_eq!(server.client_window_id()?, extra_window_id);
    assert!(keyboard_app.should_quit());

    let mut mouse_app = server.app_with_auto_quit(true)?;
    let rows = mouse_app.state().tree_rows();
    let row_index = rows
        .iter()
        .position(|row| row.focus == window_focus(&main_session.id, &main_window_id))
        .ok_or("missing row for it-win")?;
    let row = TREE_START_ROW + u16::try_from(row_index)?;
    mouse_app.on_mouse_event(mouse_left(row))?;

    assert_eq!(server.client_window_id()?, main_window_id);
    assert!(mouse_app.should_quit());
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
    let initial_client_session_id = server.client_session_id()?;
    let initial_session_ids: Vec<_> = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .map(|session| session.id)
        .collect();

    app.state_mut().focus = Focus::CreateSession;
    app.on_key_event(key(KeyCode::Enter))?;

    match app.state().mode.clone() {
        Mode::CreateSessionName { .. } => {}
        other => return Err(format!("unexpected mode after create session: {other:?}").into()),
    }
    assert_eq!(server.client_session_id()?, initial_client_session_id);
    assert_eq!(tmux.snapshot()?.sessions.len(), initial_session_ids.len());

    app.on_key_event(ctrl(KeyCode::Char('u')))?;
    type_text(&mut app, "created-session")?;
    app.on_key_event(key(KeyCode::Enter))?;
    assert_eq!(app.state().mode, Mode::Normal);
    let snapshot = tmux.snapshot()?;
    let created_session = snapshot
        .sessions
        .iter()
        .find(|session| !initial_session_ids.contains(&session.id))
        .ok_or("missing created session after confirm")?;
    let first_session_id = created_session.id.clone();
    assert_eq!(created_session.name, "created-session");
    assert_eq!(server.client_session_id()?, first_session_id);

    let session_count_before_cancel = tmux.snapshot()?.sessions.len();
    app.state_mut().focus = Focus::CreateSession;
    app.on_key_event(key(KeyCode::Enter))?;
    match app.state().mode.clone() {
        Mode::CreateSessionName { .. } => {}
        other => return Err(format!("unexpected mode after second create: {other:?}").into()),
    }

    app.on_key_event(key(KeyCode::Esc))?;
    assert_eq!(app.state().mode, Mode::Normal);
    assert_eq!(tmux.snapshot()?.sessions.len(), session_count_before_cancel);
    assert_eq!(server.client_session_id()?, first_session_id);

    Ok(())
}

#[test]
#[serial]
fn navigation_hotkeys_start_expected_flows_from_focused_context()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;
    let snapshot = tmux.snapshot()?;
    let second_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-second")
        .ok_or("missing it-second session")?;
    let second_session_id = second_session.id.clone();
    let second_window_id = second_session
        .windows
        .first()
        .map(|window| window.id.clone())
        .ok_or("missing it-second window")?;

    app.state_mut().focus = window_focus(&second_session_id, &second_window_id);
    app.on_key_event(key(KeyCode::Char('n')))?;
    assert_eq!(app.state().focus, Focus::CreateSession);
    assert!(matches!(app.state().mode, Mode::CreateSessionName { .. }));
    app.on_key_event(key(KeyCode::Esc))?;

    app.on_key_event(key(KeyCode::Char('s')))?;
    let jump_targets = app.state().jump_targets();
    assert!(!jump_targets.is_empty());
    app.on_key_event(key(KeyCode::Char('!')))?;
    assert!(app.state().jump_targets().is_empty());
    assert_eq!(app.state().focus, Focus::CreateSession);

    app.state_mut().focus = Focus::Session(second_session_id.clone());
    app.on_key_event(key(KeyCode::Char('c')))?;
    match app.state().mode.clone() {
        Mode::CreateWindowName { session_id, .. } => {
            assert_eq!(session_id, second_session_id);
        }
        other => {
            return Err(format!("unexpected mode after session create hotkey: {other:?}").into());
        }
    }
    assert_eq!(
        app.state().focus,
        Focus::CreateWindow(second_session_id.clone())
    );
    app.on_key_event(key(KeyCode::Esc))?;

    app.state_mut().focus = window_focus(
        &second_session_id,
        &second_session
            .windows
            .first()
            .map(|window| window.id.clone())
            .ok_or("missing it-second window for create hotkey")?,
    );
    app.on_key_event(key(KeyCode::Char('c')))?;
    match app.state().mode.clone() {
        Mode::CreateWindowName { session_id, .. } => {
            assert_eq!(session_id, second_session_id);
        }
        other => {
            return Err(format!("unexpected mode after window create hotkey: {other:?}").into());
        }
    }
    assert_eq!(
        app.state().focus,
        Focus::CreateWindow(second_session_id.clone())
    );

    app.on_key_event(key(KeyCode::Esc))?;
    app.state_mut().focus = window_focus(
        &second_session_id,
        &second_session
            .windows
            .first()
            .map(|window| window.id.clone())
            .ok_or("missing it-second window for jump hotkey")?,
    );
    app.on_key_event(key(KeyCode::Char('s')))?;
    let jump_targets = app.state().jump_targets();
    let target_label = jump_targets
        .iter()
        .find(|target| target.focus == Focus::Session(second_session_id.clone()))
        .map(|target| target.label)
        .ok_or("missing jump label for it-second session")?;
    app.on_key_event(key(KeyCode::Char(target_label)))?;
    assert_eq!(server.client_session_id()?, second_session_id);
    assert_eq!(app.state().focus, Focus::Session(second_session_id.clone()));

    Ok(())
}

#[test]
#[serial]
fn numbered_alert_hotkeys_switch_to_alerted_windows() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;
    let snapshot = tmux.snapshot()?;
    let main_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .ok_or("missing it-main session")?;
    let main_session_id = main_session.id.clone();
    let extra_window_id = main_session
        .windows
        .iter()
        .find(|window| window.name == "it-extra")
        .map(|window| window.id.clone())
        .ok_or("missing it-extra window")?;
    let second_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-second")
        .ok_or("missing it-second session")?;
    let second_session_id = second_session.id.clone();
    let second_window_id = second_session
        .windows
        .first()
        .map(|window| window.id.clone())
        .ok_or("missing it-second window")?;

    run_tmux(
        &server.socket_name,
        &["set", "-w", "-t", "it-main:1", "monitor-bell", "on"],
    )?;
    run_tmux(
        &server.socket_name,
        &["send-keys", "-t", "it-main:1", "printf \"\\a\"", "Enter"],
    )?;
    run_tmux(
        &server.socket_name,
        &["set", "-w", "-t", "it-second:0", "monitor-bell", "on"],
    )?;
    run_tmux(
        &server.socket_name,
        &["send-keys", "-t", "it-second:0", "printf \"\\a\"", "Enter"],
    )?;

    let mut alert_targets = Vec::new();
    for _ in 0..20 {
        let _ = app.sync_with_server(Duration::from_millis(250))?;
        alert_targets = app.state().alert_jump_targets();
        if alert_targets.len() == 2 {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(alert_targets.len(), 2, "alert shortcuts did not refresh");
    assert_eq!(alert_targets[0].label, '1');
    assert_eq!(
        alert_targets[0].focus,
        window_focus(&main_session_id, &extra_window_id)
    );
    assert_eq!(alert_targets[1].label, '2');
    assert_eq!(
        alert_targets[1].focus,
        window_focus(&second_session_id, &second_window_id)
    );

    app.on_key_event(key(KeyCode::Char('2')))?;

    assert_eq!(server.client_session_id()?, second_session_id);
    assert_eq!(server.client_window_id()?, second_window_id);
    assert_eq!(
        app.state().focus,
        window_focus(&second_session_id, &second_window_id)
    );

    Ok(())
}

#[test]
#[serial]
fn gg_g_and_flash_jump_follow_visible_rows_and_auto_quit() -> Result<(), Box<dyn std::error::Error>>
{
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let snapshot = server.tmux_cli().snapshot()?;
    let main_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .ok_or("missing it-main session")?;
    let main_window_id = main_session
        .windows
        .iter()
        .find(|window| window.name == "it-win")
        .map(|window| window.id.clone())
        .ok_or("missing it-win window")?;
    let extra_window_id = main_session
        .windows
        .iter()
        .find(|window| window.name == "it-extra")
        .map(|window| window.id.clone())
        .ok_or("missing it-extra window")?;

    let mut app = server.app()?;
    app.state_mut().focus = window_focus(&main_session.id, &extra_window_id);
    app.on_key_event(key(KeyCode::Char('g')))?;
    assert_eq!(
        app.state().focus,
        window_focus(&main_session.id, &extra_window_id)
    );
    app.on_key_event(key(KeyCode::Char('g')))?;
    assert_eq!(app.state().focus, Focus::Session(main_session.id.clone()));

    app.on_key_event(key(KeyCode::Char('G')))?;
    assert_eq!(app.state().focus, Focus::CreateSession);

    let mut auto_quit_app = server.app_with_auto_quit(true)?;
    auto_quit_app.state_mut().focus = Focus::CreateSession;
    auto_quit_app.on_key_event(key(KeyCode::Char('s')))?;
    let jump_targets = auto_quit_app.state().jump_targets();
    let jump_label = jump_targets
        .iter()
        .find(|target| target.focus == window_focus(&main_session.id, &main_window_id))
        .map(|target| target.label)
        .ok_or("missing jump label for it-win")?;
    auto_quit_app.on_key_event(key(KeyCode::Char(jump_label)))?;

    assert_eq!(server.client_window_id()?, main_window_id);
    assert!(auto_quit_app.should_quit());

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
    let initial_client_window_id = server.client_window_id()?;

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
    match app.state().mode.clone() {
        Mode::CreateWindowName { session_id, .. } => {
            assert_eq!(session_id, main_session_id);
        }
        other => return Err(format!("unexpected mode after create window: {other:?}").into()),
    }
    assert_eq!(server.client_window_id()?, initial_client_window_id);
    let initial_window_ids: Vec<_> = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .ok_or("missing it-main session before create")?
        .windows
        .into_iter()
        .map(|window| window.id)
        .collect();

    app.on_key_event(ctrl(KeyCode::Char('u')))?;
    type_text(&mut app, "created-window")?;
    app.on_key_event(key(KeyCode::Enter))?;
    assert_eq!(app.state().mode, Mode::Normal);

    let renamed_window = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .ok_or("missing it-main after create")?
        .windows
        .into_iter()
        .find(|window| !initial_window_ids.contains(&window.id))
        .ok_or("missing created window after confirm")?;
    let first_window_id = renamed_window.id.clone();
    assert_eq!(renamed_window.name, "created-window");
    assert_eq!(server.client_window_id()?, first_window_id);

    let window_count_before_cancel = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .map(|session| session.windows.len())
        .ok_or("missing it-main before cancel")?;
    app.state_mut().focus = Focus::CreateWindow(main_session_id.clone());
    app.on_key_event(key(KeyCode::Enter))?;
    match app.state().mode.clone() {
        Mode::CreateWindowName { session_id, .. } => {
            assert_eq!(session_id, main_session_id);
        }
        other => {
            return Err(format!("unexpected mode after second create window: {other:?}").into());
        }
    }

    app.on_key_event(key(KeyCode::Esc))?;
    assert_eq!(app.state().mode, Mode::Normal);
    let retained_window_count = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .map(|session| session.windows.len())
        .ok_or("missing it-main after cancel")?;
    assert_eq!(retained_window_count, window_count_before_cancel);
    assert_eq!(server.client_window_id()?, first_window_id);

    Ok(())
}

#[test]
#[serial]
fn creates_window_in_shared_session_workdir() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let workdir = TempDir::new("shared-session-workdir")?;
    set_window_path(&server.socket_name, "it-main:0", &workdir.path)?;
    set_window_path(&server.socket_name, "it-main:1", &workdir.path)?;

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
    type_text(&mut app, "cwd-shared")?;
    app.on_key_event(key(KeyCode::Enter))?;

    let created_window_id = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .ok_or("missing it-main after cwd create")?
        .windows
        .into_iter()
        .find(|window| window.name == "cwd-shared")
        .map(|window| window.id)
        .ok_or("missing created cwd-shared window")?;

    assert_eq!(
        window_current_path(&server.socket_name, &created_window_id)?,
        workdir.path
    );

    Ok(())
}

#[test]
#[serial]
fn creates_window_in_active_window_workdir_when_session_paths_diverge()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let first_dir = TempDir::new("diverged-workdir-first")?;
    let active_dir = TempDir::new("diverged-workdir-active")?;
    set_window_path(&server.socket_name, "it-main:0", &first_dir.path)?;
    set_window_path(&server.socket_name, "it-main:1", &active_dir.path)?;
    run_tmux(&server.socket_name, &["select-window", "-t", "it-main:1"])?;

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
    type_text(&mut app, "cwd-active")?;
    app.on_key_event(key(KeyCode::Enter))?;

    let created_window_id = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .ok_or("missing it-main after divergent cwd create")?
        .windows
        .into_iter()
        .find(|window| window.name == "cwd-active")
        .map(|window| window.id)
        .ok_or("missing created cwd-active window")?;

    assert_eq!(
        window_current_path(&server.socket_name, &created_window_id)?,
        active_dir.path
    );

    Ok(())
}

#[test]
#[serial]
fn auto_quit_waits_for_second_enter_before_create_switch() -> Result<(), Box<dyn std::error::Error>>
{
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let main_session_id = tmux
        .snapshot()?
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .map(|session| session.id.clone())
        .ok_or("missing it-main session")?;

    let mut session_app = server.app_with_auto_quit(true)?;
    let initial_client_session_id = server.client_session_id()?;
    session_app.state_mut().focus = Focus::CreateSession;
    session_app.on_key_event(key(KeyCode::Enter))?;
    assert!(matches!(
        session_app.state().mode,
        Mode::CreateSessionName { .. }
    ));
    assert!(!session_app.should_quit());
    assert_eq!(server.client_session_id()?, initial_client_session_id);

    type_text(&mut session_app, "auto-session")?;
    session_app.on_key_event(key(KeyCode::Enter))?;
    assert!(session_app.should_quit());
    let created_session_id = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.name == "auto-session")
        .map(|session| session.id)
        .ok_or("missing auto-created session")?;
    assert_eq!(server.client_session_id()?, created_session_id);

    let mut window_app = server.app_with_auto_quit(true)?;
    let initial_client_window_id = server.client_window_id()?;
    window_app.state_mut().focus = Focus::CreateWindow(main_session_id.clone());
    window_app.on_key_event(key(KeyCode::Enter))?;
    assert!(matches!(
        window_app.state().mode,
        Mode::CreateWindowName { .. }
    ));
    assert!(!window_app.should_quit());
    assert_eq!(server.client_window_id()?, initial_client_window_id);

    type_text(&mut window_app, "auto-window")?;
    window_app.on_key_event(key(KeyCode::Enter))?;
    assert!(window_app.should_quit());
    let created_window_id = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .ok_or("missing it-main after auto window create")?
        .windows
        .into_iter()
        .find(|window| window.name == "auto-window")
        .map(|window| window.id)
        .ok_or("missing auto-created window")?;
    assert_eq!(server.client_window_id()?, created_window_id);

    Ok(())
}

#[test]
#[serial]
fn closes_focused_window_with_x_without_confirmation() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;
    let snapshot = tmux.snapshot()?;
    let main_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .ok_or("missing it-main session")?;
    let doomed_window_id = main_session
        .windows
        .iter()
        .find(|window| window.name == "it-extra")
        .map(|window| window.id.clone())
        .ok_or("missing it-extra window")?;
    let initial_window_count = main_session.windows.len();

    app.state_mut().focus = window_focus(&main_session.id, &doomed_window_id);
    app.on_key_event(key(KeyCode::Char('x')))?;

    assert_eq!(app.state().mode, Mode::Normal);
    assert!(app.state().last_error.is_none());
    assert_ne!(
        app.state().focus,
        window_focus(&main_session.id, &doomed_window_id)
    );
    assert!(
        app.state()
            .tree_rows()
            .iter()
            .any(|row| row.focus == app.state().focus)
    );

    let refreshed_main = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session.id)
        .ok_or("missing it-main session after close")?;
    assert_eq!(refreshed_main.windows.len(), initial_window_count - 1);
    assert!(
        !refreshed_main
            .windows
            .iter()
            .any(|window| window.id == doomed_window_id)
    );

    Ok(())
}

#[test]
#[serial]
fn sidecar_close_then_create_window_keeps_tree_in_sync() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;
    let snapshot = tmux.snapshot()?;
    let main_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .ok_or("missing it-main session")?;
    let main_session_id = main_session.id.clone();
    let closed_window_id = main_session
        .windows
        .iter()
        .find(|window| window.name == "it-extra")
        .map(|window| window.id.clone())
        .ok_or("missing it-extra window")?;

    app.state_mut().focus = window_focus(&main_session_id, &closed_window_id);
    app.on_key_event(key(KeyCode::Char('x')))?;
    assert!(app.state().last_error.is_none());
    assert!(
        !app.state()
            .tmux
            .sessions
            .iter()
            .flat_map(|session| session.windows.iter())
            .any(|window| window.id == closed_window_id)
    );

    app.state_mut().focus = Focus::CreateWindow(main_session_id.clone());
    app.on_key_event(key(KeyCode::Enter))?;
    type_text(&mut app, "after-close")?;
    app.on_key_event(key(KeyCode::Enter))?;

    assert_eq!(app.state().mode, Mode::Normal);
    assert!(app.state().last_error.is_none());
    let created_window_id = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .ok_or("missing it-main after close/create")?
        .windows
        .into_iter()
        .find(|window| window.name == "after-close")
        .map(|window| window.id)
        .ok_or("missing replacement window after close/create")?;
    assert_eq!(server.client_window_id()?, created_window_id);
    assert_eq!(
        app.state().focus,
        window_focus(&main_session_id, &created_window_id)
    );

    Ok(())
}

#[test]
#[serial]
fn external_close_then_sidecar_create_window_keeps_tree_in_sync()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;
    let snapshot = tmux.snapshot()?;
    let main_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-main")
        .ok_or("missing it-main session")?;
    let main_session_id = main_session.id.clone();
    let closed_window_id = main_session
        .windows
        .iter()
        .find(|window| window.name == "it-extra")
        .map(|window| window.id.clone())
        .ok_or("missing it-extra window")?;

    run_tmux(
        &server.socket_name,
        &["kill-window", "-t", &closed_window_id],
    )?;

    app.state_mut().focus = Focus::CreateWindow(main_session_id.clone());
    app.on_key_event(key(KeyCode::Enter))?;
    type_text(&mut app, "after-external-close")?;
    app.on_key_event(key(KeyCode::Enter))?;

    assert_eq!(app.state().mode, Mode::Normal);
    assert!(app.state().last_error.is_none());
    assert!(
        !app.state()
            .tmux
            .sessions
            .iter()
            .flat_map(|session| session.windows.iter())
            .any(|window| window.id == closed_window_id)
    );
    let created_window_id = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .ok_or("missing it-main after external close/create")?
        .windows
        .into_iter()
        .find(|window| window.name == "after-external-close")
        .map(|window| window.id)
        .ok_or("missing replacement window after external close/create")?;
    assert_eq!(server.client_window_id()?, created_window_id);
    assert_eq!(
        app.state().focus,
        window_focus(&main_session_id, &created_window_id)
    );

    Ok(())
}

#[test]
#[serial]
fn close_active_window_then_create_window_keeps_tree_in_sync()
-> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;
    let main_session_id = server.client_session_id()?;
    let closed_window_id = server.client_window_id()?;

    app.state_mut().focus = window_focus(&main_session_id, &closed_window_id);
    app.on_key_event(key(KeyCode::Char('x')))?;
    assert!(app.state().last_error.is_none());
    assert!(
        !app.state()
            .tmux
            .sessions
            .iter()
            .flat_map(|session| session.windows.iter())
            .any(|window| window.id == closed_window_id)
    );

    app.on_key_event(key(KeyCode::Char('c')))?;
    type_text(&mut app, "after-active-close")?;
    app.on_key_event(key(KeyCode::Enter))?;

    assert_eq!(app.state().mode, Mode::Normal);
    assert!(app.state().last_error.is_none());
    let created_window_id = tmux
        .snapshot()?
        .sessions
        .into_iter()
        .find(|session| session.id == main_session_id)
        .ok_or("missing it-main after active close/create")?
        .windows
        .into_iter()
        .find(|window| window.name == "after-active-close")
        .map(|window| window.id)
        .ok_or("missing replacement window after active close/create")?;
    assert_eq!(server.client_window_id()?, created_window_id);
    assert_eq!(
        app.state().focus,
        window_focus(&main_session_id, &created_window_id)
    );

    Ok(())
}

#[test]
#[serial]
fn closes_focused_session_with_x_without_confirmation() -> Result<(), Box<dyn std::error::Error>> {
    if !tmux_available() {
        eprintln!("skipping integration test: tmux is unavailable");
        return Ok(());
    }

    let server = IsolatedServer::start()?;
    let tmux = server.tmux_cli();
    let mut app = server.app()?;
    let snapshot = tmux.snapshot()?;
    let doomed_session = snapshot
        .sessions
        .iter()
        .find(|session| session.name == "it-second")
        .ok_or("missing it-second session")?;
    let initial_session_count = snapshot.sessions.len();
    let initial_client_session_id = server.client_session_id()?;

    app.state_mut().focus = Focus::Session(doomed_session.id.clone());
    app.on_key_event(key(KeyCode::Char('x')))?;

    assert_eq!(app.state().mode, Mode::Normal);
    assert!(app.state().last_error.is_none());
    assert_ne!(app.state().focus, Focus::Session(doomed_session.id.clone()));
    assert!(
        app.state()
            .tree_rows()
            .iter()
            .any(|row| row.focus == app.state().focus)
    );

    let refreshed_sessions = tmux.snapshot()?.sessions;
    assert_eq!(refreshed_sessions.len(), initial_session_count - 1);
    assert!(
        !refreshed_sessions
            .iter()
            .any(|session| session.id == doomed_session.id)
    );
    assert_eq!(server.client_session_id()?, initial_client_session_id);

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

    app.state_mut().focus = window_focus(&main_session_id, &doomed_window_id);
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
