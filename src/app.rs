use std::{
    collections::VecDeque,
    env, io, panic,
    path::PathBuf,
    sync::Once,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use crossterm::{
    cursor,
    event::{
        DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    cli::Cli,
    client::{self, IpcClient, ReadStatus},
    event::AppEvent,
    input::InputBuffer,
    ipc::{
        Action, ActionOutcome, ActionResult, ActionResultKind, ClientKind, ProjectionState,
        ServerMessage, StateUpdated,
    },
    model::{
        ActionError, AppState, ClientName, EditAction, Focus, FocusMove, FocusReconcile, Mode,
        TmuxState, Toast, WindowTarget,
    },
    tmux::{Tmux, TmuxCli, hooks::HookCommandProgram},
    ui,
    ui_app::runtime::{UiEvent, UiRuntime},
};

static PANIC_HOOK: Once = Once::new();
const ACTION_SYNC_TIMEOUT: Duration = Duration::from_secs(2);
const SERVER_SETTLE_TIMEOUT: Duration = Duration::from_millis(50);
const STARTUP_TOAST_DURATION: Duration = Duration::from_secs(3);
const STARTUP_TOAST_MESSAGE: &str = "Started tmux-sidecar server";

#[derive(Debug)]
pub struct App {
    cli: Cli,
    state: AppState,
    server_generation: u64,
    tmux: TmuxCli,
    subscription: Option<IpcClient>,
    runtime: Option<UiRuntime>,
    tmux_socket_path: Option<PathBuf>,
    toast_deadline: Option<Instant>,
    should_quit: bool,
}

struct StartupContext {
    target_client: ClientName,
    tmux_socket_path: PathBuf,
}

struct SuccessfulAction {
    outcome: Option<ActionOutcome>,
}

struct CompletedAction {
    generation: u64,
    completion: ActionCompletion,
}

enum ActionCompletion {
    Succeeded(SuccessfulAction),
    Failed,
}

impl SuccessfulAction {
    fn created_session_id(&self) -> Option<String> {
        match &self.outcome {
            Some(ActionOutcome::CreatedSession { session_id }) => Some(session_id.clone()),
            _ => None,
        }
    }

    fn created_window_id(&self, expected_session_id: &str) -> Option<String> {
        match &self.outcome {
            Some(ActionOutcome::CreatedWindow {
                session_id,
                window_id,
            }) if session_id == expected_session_id => Some(window_id.clone()),
            _ => None,
        }
    }
}

impl App {
    pub fn new(cli: Cli) -> Self {
        let tmux = TmuxCli {
            socket_name: cli.socket_name.clone(),
            socket_path: cli.socket_path.clone(),
        };

        Self {
            cli,
            state: AppState::default(),
            server_generation: 0,
            tmux,
            subscription: None,
            runtime: None,
            tmux_socket_path: None,
            toast_deadline: None,
            should_quit: false,
        }
    }

    pub fn run(&mut self) -> Result<()> {
        if self.cli.print_snapshot {
            self.print_snapshot()
        } else {
            self.run_interactive()
        }
    }

    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    #[allow(dead_code)]
    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn apply_snapshot(&mut self, tmux: TmuxState) -> FocusReconcile {
        self.state.reconcile_tmux(tmux)
    }

    pub fn move_focus(&mut self, movement: FocusMove) -> bool {
        self.state.move_focus(movement)
    }

    pub fn apply_edit_action(&mut self, action: EditAction) -> bool {
        self.state.apply_edit_action(action)
    }

    pub fn on_event(&mut self, event: AppEvent) -> Result<()> {
        self.handle_event(event)
    }

    pub fn on_key_event(&mut self, key: KeyEvent) -> Result<()> {
        self.handle_key(key)
    }

    pub fn on_mouse_event(&mut self, mouse: MouseEvent) -> Result<()> {
        self.handle_mouse(mouse)
    }

    fn print_snapshot(&self) -> Result<()> {
        let snapshot = self.tmux.snapshot()?;
        for session in snapshot.sessions {
            println!("{} {}", session.id, session.name);
            for window in session.windows {
                println!("  {} {}", window.id, window.name);
            }
        }

        Ok(())
    }

    fn run_interactive(&mut self) -> Result<()> {
        let startup = self.startup_preflight()?;
        install_panic_hook();
        self.prepare_first_paint_state(&startup);

        let mut terminal = TerminalSession::enter()?;
        terminal
            .terminal
            .draw(|frame| ui::render(frame, &self.state))?;
        self.finish_startup(startup)?;
        self.event_loop(&mut terminal.terminal)
    }

    pub fn startup(&mut self) -> Result<()> {
        let startup = self.startup_preflight()?;
        self.finish_startup(startup)
    }

    fn startup_preflight(&self) -> Result<StartupContext> {
        let target_client = self.tmux.check_startup(self.cli.target_client.as_deref())?;
        let tmux_socket_path = client::resolve_tmux_socket_path(
            self.cli.socket_name.clone(),
            self.cli.socket_path.clone(),
        )?;
        Ok(StartupContext {
            target_client,
            tmux_socket_path,
        })
    }

    fn finish_startup(&mut self, startup: StartupContext) -> Result<()> {
        let StartupContext {
            target_client,
            tmux_socket_path,
        } = startup;
        self.tmux_socket_path = Some(tmux_socket_path.clone());
        let started_server = client::ensure_server_running(&tmux_socket_path)?;
        self.install_hooks()?;

        let mut subscription = client::subscribe(&tmux_socket_path, Some(target_client.0.clone()))?;
        self.state.target_client = Some(target_client);
        self.apply_initial_state(&mut subscription)?;
        if started_server {
            self.show_toast(STARTUP_TOAST_MESSAGE);
        }
        self.subscription = Some(subscription);
        Ok(())
    }

    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        self.start_runtime()?;
        let result = (|| {
            let mut dirty = true;
            while !self.should_quit {
                if dirty {
                    terminal.draw(|frame| ui::render(frame, &self.state))?;
                }

                dirty = match self.recv_runtime_event()? {
                    Some(event) => {
                        self.handle_ui_event(event)?;
                        true
                    }
                    None => self.expire_toast_if_needed(),
                };
            }

            Ok(())
        })();
        self.runtime = None;
        result
    }

    pub fn sync_with_server(&mut self, timeout: Duration) -> Result<bool> {
        if self.runtime.is_some() {
            return self.sync_with_runtime(timeout);
        }

        let Some(mut subscription) = self.subscription.take() else {
            return Ok(false);
        };

        let result = self.sync_with_server_inner(&mut subscription, timeout);
        let reset = subscription.set_read_timeout(None);
        self.subscription = Some(subscription);
        match (result, reset) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(received), Ok(())) => Ok(received),
        }
    }

    fn apply_initial_state(&mut self, subscription: &mut IpcClient) -> Result<()> {
        subscription.set_read_timeout(Some(ACTION_SYNC_TIMEOUT))?;
        let result = loop {
            match subscription.read_status()? {
                ReadStatus::Message(message) => {
                    if let ServerMessage::StateUpdated(update) = message {
                        self.handle_state_update(update);
                        break Ok(());
                    }

                    self.handle_server_message(message)?;
                }
                ReadStatus::Pending => {
                    break Err(anyhow!("timed out waiting for initial sidecar state"));
                }
                ReadStatus::Closed => break Err(anyhow!("sidecar server closed the connection")),
            }
        };
        subscription.set_read_timeout(None)?;
        result
    }

    fn sync_with_server_inner(
        &mut self,
        subscription: &mut IpcClient,
        timeout: Duration,
    ) -> Result<bool> {
        subscription.set_read_timeout(Some(timeout))?;
        let mut received_message = false;

        loop {
            match subscription.read_status()? {
                ReadStatus::Message(message) => {
                    received_message = true;
                    self.handle_server_message(message)?;
                    subscription.set_read_timeout(Some(SERVER_SETTLE_TIMEOUT))?;
                }
                ReadStatus::Pending => break,
                ReadStatus::Closed => bail!("sidecar server disconnected"),
            }
        }
        Ok(received_message)
    }

    fn sync_with_runtime(&mut self, timeout: Duration) -> Result<bool> {
        let Some(_) = self.runtime else {
            return Ok(false);
        };

        let mut deadline = Instant::now() + timeout;
        let mut received_server_message = false;
        let mut deferred_events = VecDeque::new();

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = {
                let runtime = self.runtime.as_mut().expect("runtime checked above");
                runtime.recv_timeout(remaining)?
            };
            let Some(event) = event else {
                break;
            };

            match event {
                UiEvent::Server(message) => {
                    received_server_message = true;
                    self.handle_server_message(message)?;
                    deadline = Instant::now() + SERVER_SETTLE_TIMEOUT;
                }
                UiEvent::ServerDisconnected => bail!("sidecar server disconnected"),
                UiEvent::RuntimeError(message) => bail!(message),
                other => deferred_events.push_back(other),
            }
        }

        if let Some(runtime) = self.runtime.as_mut() {
            runtime.prepend_pending(deferred_events);
        }

        Ok(received_server_message)
    }

    fn install_hooks(&self) -> Result<()> {
        let program = HookCommandProgram::new(vec![hook_program_path()?.display().to_string()]);
        self.tmux.install_hooks(&program)?;
        Ok(())
    }

    fn handle_server_message(&mut self, message: ServerMessage) -> Result<()> {
        match message {
            ServerMessage::StateUpdated(update) => {
                self.handle_state_update(update);
                Ok(())
            }
            ServerMessage::ActionResult(result) => self.handle_action_result(result).map(|_| ()),
            ServerMessage::Error(error) => bail!(error.message),
            ServerMessage::HelloAck(_) => Ok(()),
            ServerMessage::Ack(_) => Ok(()),
        }
    }

    fn handle_action_result(&mut self, result: ActionResult) -> Result<CompletedAction> {
        let completion = match result.result {
            ActionResultKind::Ok { outcome } => {
                self.state.last_error = None;
                ActionCompletion::Succeeded(SuccessfulAction { outcome })
            }
            ActionResultKind::Error { message } => {
                self.state.last_error = Some(ActionError { message });
                ActionCompletion::Failed
            }
        };
        Ok(CompletedAction {
            generation: result.generation,
            completion,
        })
    }

    fn handle_state_update(&mut self, update: StateUpdated) {
        let should_focus_visible_target = self.state.is_tree_loading();
        self.state.tree_loading = false;
        self.server_generation = update.generation;
        self.apply_projection(update.state);
        if should_focus_visible_target {
            self.state.focus_visible_target();
        }
    }

    fn apply_projection(&mut self, projection: ProjectionState) -> FocusReconcile {
        self.state.reconcile_tmux(projection.into_tmux_state())
    }

    fn prepare_first_paint_state(&mut self, startup: &StartupContext) {
        self.state.tree_loading = true;
        self.state.target_client = Some(startup.target_client.clone());
    }

    fn show_toast(&mut self, message: impl Into<String>) {
        self.state.toast = Some(Toast {
            message: message.into(),
        });
        self.toast_deadline = Some(Instant::now() + STARTUP_TOAST_DURATION);
    }

    fn expire_toast_if_needed(&mut self) -> bool {
        let Some(deadline) = self.toast_deadline else {
            return false;
        };

        if Instant::now() < deadline {
            return false;
        }

        self.toast_deadline = None;
        self.state.toast = None;
        true
    }

    fn handle_event(&mut self, event: AppEvent) -> Result<()> {
        let _ = self.expire_toast_if_needed();
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Mouse(mouse) => self.handle_mouse(mouse),
            AppEvent::Resize(_, _) | AppEvent::Tick => Ok(()),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return Ok(());
        }

        if self.state.mode == Mode::Help {
            return self.handle_help_key(key);
        }

        if self.state.mode != Mode::Normal {
            return self.handle_edit_key(key);
        }

        if self.state.navigation.jumping {
            return self.handle_jump_key(key);
        }

        if self.state.navigation.pending_g {
            return self.handle_pending_g_key(key);
        }

        self.handle_normal_key(key)
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('?') => self.state.mode = Mode::Help,
            KeyCode::Char('g') => {
                self.state.start_g_prefix();
            }
            KeyCode::Char('G') => {
                self.state.clear_navigation();
                self.state.focus_last_row();
            }
            KeyCode::Char('n') => {
                self.state.clear_navigation();
                self.begin_create_session_naming()?;
            }
            KeyCode::Char('s') => {
                self.state.start_jump();
            }
            KeyCode::Char('c') => {
                self.state.clear_navigation();
                self.begin_create_window_from_focus()?;
            }
            KeyCode::Char(label) if label.is_ascii_digit() => {
                self.state.clear_navigation();
                self.activate_alert_jump_label(label)?;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.state.clear_navigation();
                self.move_focus(FocusMove::Up);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.state.clear_navigation();
                self.move_focus(FocusMove::Down);
            }
            KeyCode::Char('x') => {
                self.state.clear_navigation();
                self.close_focused_target()?;
            }
            KeyCode::Char('r') => {
                self.state.clear_navigation();
                self.start_rename();
            }
            KeyCode::Enter => {
                self.state.clear_navigation();
                self.activate_focused_target()?;
            }
            _ => {
                self.state.clear_navigation();
            }
        }

        Ok(())
    }

    fn handle_pending_g_key(&mut self, key: KeyEvent) -> Result<()> {
        self.state.clear_g_prefix();

        match key.code {
            KeyCode::Char('g') => {
                self.state.focus_first_row();
                Ok(())
            }
            _ => self.handle_normal_key(key),
        }
    }

    fn handle_jump_key(&mut self, key: KeyEvent) -> Result<()> {
        let selected = match key.code {
            KeyCode::Esc => {
                self.state.cancel_jump();
                return Ok(());
            }
            KeyCode::Char(label) => self.state.focus_jump_label(label),
            _ => false,
        };

        self.state.cancel_jump();
        if selected {
            self.activate_focused_target()?;
        }

        Ok(())
    }

    fn activate_alert_jump_label(&mut self, label: char) -> Result<()> {
        if self.state.focus_alert_jump_label(label) {
            self.activate_focused_target()?;
        }

        Ok(())
    }

    fn handle_help_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => self.state.mode = Mode::Normal,
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.state.mode = Mode::Normal,
            KeyCode::Enter => self.submit_edit_mode()?,
            KeyCode::Backspace => {
                self.apply_edit_action(EditAction::Backspace);
            }
            KeyCode::Delete => {
                self.apply_edit_action(EditAction::Delete);
            }
            KeyCode::Left => {
                self.apply_edit_action(EditAction::MoveLeft);
            }
            KeyCode::Right => {
                self.apply_edit_action(EditAction::MoveRight);
            }
            KeyCode::Home => {
                self.apply_edit_action(EditAction::MoveHome);
            }
            KeyCode::End => {
                self.apply_edit_action(EditAction::MoveEnd);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.apply_edit_action(EditAction::Clear);
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.apply_edit_action(EditAction::Insert(ch));
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        if self.state.mode != Mode::Normal {
            return Ok(());
        }

        self.state.clear_g_prefix();
        if self.state.navigation.jumping {
            return Ok(());
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.focus_row_at_terminal_row(mouse.row) {
                    self.activate_focused_target()?;
                }
            }
            MouseEventKind::ScrollUp => {
                self.move_focus(FocusMove::Up);
            }
            MouseEventKind::ScrollDown => {
                self.move_focus(FocusMove::Down);
            }
            _ => {}
        }

        Ok(())
    }

    fn focus_row_at_terminal_row(&mut self, row: u16) -> bool {
        let Some(index) = ui::tree_index_for_terminal_row(row) else {
            return false;
        };

        let rows = self.state.tree_rows();
        let Some(target) = rows.get(index) else {
            return false;
        };

        self.state.focus = target.focus.clone();
        true
    }

    fn activate_focused_target(&mut self) -> Result<()> {
        match self.state.focus.clone() {
            Focus::Session(session_id) => {
                let Some(client) = self.state.target_client.clone() else {
                    return Ok(());
                };
                self.switch_to_target(client, WindowTarget::Session(session_id))
            }
            Focus::Window {
                session_id,
                window_id,
            } => {
                let Some(client) = self.state.target_client.clone() else {
                    return Ok(());
                };
                self.switch_to_target(
                    client,
                    WindowTarget::Window {
                        session_id,
                        window_id,
                    },
                )
            }
            Focus::CreateSession => self.begin_create_session_naming(),
            Focus::CreateWindow(session_id) => self.begin_create_window_naming(session_id),
        }
    }

    fn switch_to_target(&mut self, client: ClientName, target: WindowTarget) -> Result<()> {
        let action = match target {
            WindowTarget::Session(session_id) => Action::SwitchSession { session_id },
            WindowTarget::Window {
                session_id,
                window_id,
            } => Action::SwitchWindow {
                session_id,
                window_id,
            },
        };
        let Some(_) = self.try_server_action(Some(client.0), action)? else {
            return Ok(());
        };

        if self.cli.auto_quit {
            self.should_quit = true;
            return Ok(());
        }

        Ok(())
    }

    fn begin_create_session_naming(&mut self) -> Result<()> {
        self.state.focus = Focus::CreateSession;
        self.state.mode = Mode::CreateSessionName {
            input: InputBuffer::new(),
        };
        Ok(())
    }

    fn begin_create_window_naming(&mut self, session_id: String) -> Result<()> {
        self.state.focus = Focus::CreateWindow(session_id.clone());
        self.state.mode = Mode::CreateWindowName {
            session_id,
            input: InputBuffer::new(),
        };
        Ok(())
    }

    fn begin_create_window_from_focus(&mut self) -> Result<()> {
        let Some(session_id) = self.focused_session_id_for_new_window() else {
            return Ok(());
        };

        self.begin_create_window_naming(session_id)
    }

    fn close_focused_target(&mut self) -> Result<()> {
        let (action, target_client) = match self.state.focus.clone() {
            Focus::Session(session_id) => (
                Action::CloseSession { session_id },
                self.state
                    .target_client
                    .as_ref()
                    .map(|client| client.0.clone()),
            ),
            Focus::Window {
                session_id,
                window_id,
            } => (
                Action::CloseWindow {
                    session_id,
                    window_id,
                },
                None,
            ),
            Focus::CreateSession | Focus::CreateWindow(_) => return Ok(()),
        };

        let _ = self.try_server_action(target_client, action)?;
        Ok(())
    }

    fn start_rename(&mut self) {
        match self.state.focus.clone() {
            Focus::Session(id) => {
                let Some(name) = self.session_name(&id) else {
                    return;
                };

                self.state.mode = Mode::RenameSession {
                    id,
                    original_name: name.clone(),
                    input: InputBuffer::from_text(name),
                };
            }
            Focus::Window {
                session_id,
                window_id,
            } => {
                let Some(name) = self.window_name(&session_id, &window_id) else {
                    return;
                };

                self.state.mode = Mode::RenameWindow {
                    session_id,
                    id: window_id,
                    original_name: name.clone(),
                    input: InputBuffer::from_text(name),
                };
            }
            Focus::CreateSession | Focus::CreateWindow(_) => {}
        }
    }

    fn focused_session_id_for_new_window(&self) -> Option<String> {
        match self.state.focus.clone() {
            Focus::Session(session_id) | Focus::CreateWindow(session_id) => Some(session_id),
            Focus::Window { session_id, .. } => Some(session_id),
            Focus::CreateSession => self
                .state
                .tmux
                .visible_session(self.state.target_client.as_ref())
                .map(|session| session.id.clone()),
        }
    }

    fn submit_edit_mode(&mut self) -> Result<()> {
        let mode = self.state.mode.clone();
        self.state.mode = Mode::Normal;

        match mode {
            Mode::RenameSession { id, input, .. } => {
                let Some(_) = self.try_server_action(
                    None,
                    Action::RenameSession {
                        session_id: id.clone(),
                        name: input.as_str().to_owned(),
                    },
                )?
                else {
                    return Ok(());
                };

                self.state.focus = Focus::Session(id);
            }
            Mode::RenameWindow {
                session_id,
                id,
                input,
                ..
            } => {
                let Some(_) = self.try_server_action(
                    None,
                    Action::RenameWindow {
                        window_id: id.clone(),
                        name: input.as_str().to_owned(),
                    },
                )?
                else {
                    return Ok(());
                };

                self.state.focus = Focus::window(session_id, id);
            }
            Mode::CreateSessionName { input } => {
                let Some(client) = self.state.target_client.clone() else {
                    return Ok(());
                };
                let name = (!input.is_empty()).then_some(input.as_str().to_owned());
                let Some(action_success) =
                    self.try_server_action(None, Action::CreateSession { name: name.clone() })?
                else {
                    return Ok(());
                };
                let session_id = action_success.created_session_id().ok_or_else(|| {
                    anyhow!("create-session action completed without a created session id")
                })?;

                self.switch_to_target(client, WindowTarget::Session(session_id.clone()))?;
                if !self.should_quit {
                    self.state.focus = Focus::Session(session_id);
                }
            }
            Mode::CreateWindowName { session_id, input } => {
                let Some(client) = self.state.target_client.clone() else {
                    return Ok(());
                };
                let name = (!input.is_empty()).then_some(input.as_str().to_owned());
                let Some(action_success) = self.try_server_action(
                    None,
                    Action::CreateWindow {
                        session_id: session_id.clone(),
                        name: name.clone(),
                    },
                )?
                else {
                    return Ok(());
                };
                let window_id = action_success
                    .created_window_id(&session_id)
                    .ok_or_else(|| {
                        anyhow!(
                            "create-window action completed without a created window id for session `{session_id}`"
                        )
                    })?;

                self.switch_to_target(
                    client,
                    WindowTarget::Window {
                        session_id: session_id.clone(),
                        window_id: window_id.clone(),
                    },
                )?;
                if !self.should_quit {
                    self.state.focus = Focus::window(session_id, window_id);
                }
            }
            Mode::Normal | Mode::Help => {}
        }

        Ok(())
    }

    fn session_name(&self, id: &str) -> Option<String> {
        self.state
            .tmux
            .sessions
            .iter()
            .find(|session| session.id == id)
            .map(|session| session.name.clone())
    }

    fn window_name(&self, session_id: &str, id: &str) -> Option<String> {
        self.state
            .tmux
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .and_then(|session| session.windows.iter().find(|window| window.id == id))
            .map(|window| window.name.clone())
    }

    fn try_server_action(
        &mut self,
        target_client: Option<String>,
        action: Action,
    ) -> Result<Option<SuccessfulAction>> {
        match self.perform_server_action(target_client, action)? {
            ActionCompletion::Succeeded(success) => Ok(Some(success)),
            ActionCompletion::Failed => {
                self.state.mode = Mode::Normal;
                Ok(None)
            }
        }
    }

    fn perform_server_action(
        &mut self,
        target_client: Option<String>,
        action: Action,
    ) -> Result<ActionCompletion> {
        if self.runtime.is_some() {
            return self.perform_runtime_action(target_client, action);
        }

        let Some(mut subscription) = self.subscription.take() else {
            return Ok(ActionCompletion::Failed);
        };

        self.state.last_error = None;
        let result = (|| {
            let request_id = subscription.send_action_request(target_client, action)?;
            subscription.set_read_timeout(Some(ACTION_SYNC_TIMEOUT))?;

            loop {
                match subscription.read_status()? {
                    ReadStatus::Message(ServerMessage::ActionResult(result)) => {
                        if result.request_id != request_id {
                            bail!(
                                "received unexpected action result `{}` while waiting for `{request_id}`",
                                result.request_id
                            );
                        }
                        let completed = self.handle_action_result(result)?;
                        self.wait_for_subscription_generation(
                            &mut subscription,
                            completed.generation,
                        )?;
                        return Ok(completed.completion);
                    }
                    ReadStatus::Message(message) => self.handle_server_message(message)?,
                    ReadStatus::Pending => {
                        bail!("timed out waiting for action result `{request_id}`")
                    }
                    ReadStatus::Closed => bail!("sidecar server disconnected"),
                }
            }
        })();
        let reset = subscription.set_read_timeout(None);
        self.subscription = Some(subscription);
        match (result, reset) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(completion), Ok(())) => Ok(completion),
        }
    }

    fn perform_runtime_action(
        &mut self,
        target_client: Option<String>,
        action: Action,
    ) -> Result<ActionCompletion> {
        let tmux_socket_path = self
            .runtime
            .as_ref()
            .map(|runtime| runtime.tmux_socket_path().to_path_buf())
            .or_else(|| self.tmux_socket_path.clone())
            .ok_or_else(|| anyhow!("missing tmux socket path for action request"))?;

        let mut action_client =
            IpcClient::connect_or_spawn(&tmux_socket_path, ClientKind::Control)?;
        self.state.last_error = None;
        let result = (|| {
            let request_id = action_client.send_action_request(target_client, action)?;
            action_client.set_read_timeout(Some(ACTION_SYNC_TIMEOUT))?;

            loop {
                match action_client.read_status()? {
                    ReadStatus::Message(ServerMessage::ActionResult(result)) => {
                        if result.request_id != request_id {
                            bail!(
                                "received unexpected action result `{}` while waiting for `{request_id}`",
                                result.request_id
                            );
                        }
                        let completed = self.handle_action_result(result)?;
                        self.wait_for_runtime_generation(
                            completed.generation,
                            ACTION_SYNC_TIMEOUT,
                        )?;
                        return Ok(completed.completion);
                    }
                    ReadStatus::Message(message) => self.handle_server_message(message)?,
                    ReadStatus::Pending => {
                        bail!("timed out waiting for action result `{request_id}`")
                    }
                    ReadStatus::Closed => bail!("sidecar server disconnected"),
                }
            }
        })();
        let reset = action_client.set_read_timeout(None);
        match (result, reset) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(completion), Ok(())) => Ok(completion),
        }
    }

    fn wait_for_subscription_generation(
        &mut self,
        subscription: &mut IpcClient,
        generation: u64,
    ) -> Result<()> {
        if self.server_generation >= generation {
            return Ok(());
        }

        loop {
            match subscription.read_status()? {
                ReadStatus::Message(message) => {
                    self.handle_server_message(message)?;
                    if self.server_generation >= generation {
                        return Ok(());
                    }
                }
                ReadStatus::Pending => {
                    bail!("timed out waiting for sidecar state generation `{generation}`")
                }
                ReadStatus::Closed => bail!("sidecar server disconnected"),
            }
        }
    }

    fn wait_for_runtime_generation(&mut self, generation: u64, timeout: Duration) -> Result<()> {
        if self.server_generation >= generation {
            return Ok(());
        }

        let deadline = Instant::now() + timeout;
        let mut deferred_events = VecDeque::new();

        while self.server_generation < generation {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = {
                let runtime = self.runtime.as_mut().expect("runtime checked above");
                runtime.recv_timeout(remaining)?
            };
            let Some(event) = event else {
                bail!("timed out waiting for sidecar state generation `{generation}`");
            };
            match event {
                UiEvent::Server(message) => {
                    self.handle_server_message(message)?;
                }
                UiEvent::ServerDisconnected => bail!("sidecar server disconnected"),
                UiEvent::RuntimeError(message) => bail!(message),
                other => deferred_events.push_back(other),
            }
        }

        if let Some(runtime) = self.runtime.as_mut() {
            runtime.prepend_pending(deferred_events);
        }

        Ok(())
    }

    fn start_runtime(&mut self) -> Result<()> {
        if self.runtime.is_some() {
            return Ok(());
        }

        let subscription = self
            .subscription
            .take()
            .ok_or_else(|| anyhow!("missing sidecar subscription for UI runtime"))?;
        let tmux_socket_path = self
            .tmux_socket_path
            .clone()
            .ok_or_else(|| anyhow!("missing tmux socket path for UI runtime"))?;
        self.runtime = Some(UiRuntime::spawn(subscription, tmux_socket_path));
        Ok(())
    }

    fn recv_runtime_event(&mut self) -> Result<Option<UiEvent>> {
        let timeout = self.next_timer_timeout();
        let Some(runtime) = self.runtime.as_mut() else {
            return Ok(None);
        };

        if let Some(timeout) = timeout {
            runtime.recv_timeout(timeout)
        } else {
            runtime.recv().map(Some)
        }
    }

    fn next_timer_timeout(&self) -> Option<Duration> {
        self.toast_deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    fn handle_ui_event(&mut self, event: UiEvent) -> Result<()> {
        match event {
            UiEvent::Terminal(event) => self.handle_event(event),
            UiEvent::Server(message) => self.handle_server_message(message),
            UiEvent::ServerDisconnected => bail!("sidecar server disconnected"),
            UiEvent::RuntimeError(message) => bail!(message),
        }
    }
}

fn hook_program_path() -> Result<PathBuf> {
    Ok(env::var_os("CARGO_BIN_EXE_tmux-sidecar")
        .map(PathBuf::from)
        .unwrap_or(env::current_exe().context("failed to resolve current executable")?))
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;

        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
            let _ = disable_raw_mode();
            return Err(error).context("failed to configure terminal");
        }

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                restore_terminal();
                return Err(error).context("failed to create terminal");
            }
        };

        if let Err(error) = terminal.clear() {
            restore_terminal();
            return Err(error).context("failed to clear terminal");
        }

        Ok(Self { terminal })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            restore_terminal();
            previous(panic_info);
        }));
    });
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(
        stdout,
        LeaveAlternateScreen,
        DisableMouseCapture,
        cursor::Show
    );
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::mpsc,
        time::{Duration, Instant},
    };

    use super::{App, StartupContext};
    use crate::{
        cli::Cli,
        event::AppEvent,
        ipc::{
            ActionResult, ActionResultKind, ProjectionClient, ProjectionSession, ProjectionState,
            ProjectionWindow, ServerMessage, StateUpdated,
        },
        model::{ClientName, Focus, WinlinkKey},
        ui_app::runtime::{UiEvent, UiRuntime},
    };

    fn test_cli() -> Cli {
        Cli {
            socket_name: None,
            socket_path: None,
            target_client: None,
            poll_interval_ms: 50,
            auto_quit: false,
            print_snapshot: false,
            command: None,
        }
    }

    fn projection_state(session_names: &[(&str, &str, &[(&str, &str, bool)])]) -> ProjectionState {
        ProjectionState {
            tmux_socket_path: "/tmux.sock".into(),
            sessions: session_names
                .iter()
                .map(|(session_id, session_name, windows)| ProjectionSession {
                    id: (*session_id).to_string(),
                    name: (*session_name).to_string(),
                    attached_count: 1,
                    active_window_id: windows
                        .iter()
                        .find(|(_, _, active)| *active)
                        .map(|(window_id, _, _)| (*window_id).to_string()),
                    windows: windows
                        .iter()
                        .enumerate()
                        .map(
                            |(index, (window_id, window_name, active))| ProjectionWindow {
                                id: (*window_id).to_string(),
                                index: index as u32,
                                name: (*window_name).to_string(),
                                active: *active,
                                activity: 0,
                                activity_flag: false,
                                bell_flag: false,
                                silence_flag: false,
                            },
                        )
                        .collect(),
                })
                .collect(),
            clients: vec![ProjectionClient {
                name: String::from("client-1"),
                session_id: String::from("$1"),
                current_window_id: Some(String::from("@1")),
                activity: 1,
                tty: String::from("/dev/pts/1"),
            }],
        }
    }

    #[test]
    fn initial_server_state_focuses_visible_target() {
        let mut app = App::new(test_cli());
        app.state_mut().target_client = Some(ClientName(String::from("client-1")));
        app.state_mut().tree_loading = true;

        app.handle_server_message(ServerMessage::StateUpdated(StateUpdated {
            generation: 1,
            state: projection_state(&[("$1", "main", &[("@1", "win", true)])]),
        }))
        .expect("apply initial state");

        assert_eq!(app.state().focus, Focus::window("$1", "@1"));
        assert!(!app.state().is_tree_loading());
    }

    #[test]
    fn prepare_first_paint_state_uses_loading_state_until_subscription() {
        let mut app = App::new(test_cli());
        let target_client = ClientName(String::from("client-1"));

        app.prepare_first_paint_state(&StartupContext {
            target_client: target_client.clone(),
            tmux_socket_path: PathBuf::from("/tmux.sock"),
        });

        assert_eq!(app.state().target_client, Some(target_client));
        assert!(app.state().is_tree_loading());
        assert!(app.state().tmux.sessions.is_empty());
    }

    #[test]
    fn pushed_state_updates_reconcile_removed_focus() {
        let mut app = App::new(test_cli());
        app.state_mut().target_client = Some(ClientName(String::from("client-1")));
        app.handle_server_message(ServerMessage::StateUpdated(StateUpdated {
            generation: 1,
            state: projection_state(&[
                ("$1", "main", &[("@1", "win-1", true)]),
                ("$2", "other", &[("@2", "win-2", false)]),
            ]),
        }))
        .expect("apply initial state");
        app.state_mut().focus = Focus::Session(String::from("$2"));

        app.handle_server_message(ServerMessage::StateUpdated(StateUpdated {
            generation: 2,
            state: projection_state(&[("$1", "main", &[("@1", "win-1", true)])]),
        }))
        .expect("apply pushed state");

        assert_ne!(app.state().focus, Focus::Session(String::from("$2")));
        assert!(
            app.state()
                .tree_rows()
                .iter()
                .any(|row| row.focus == app.state().focus)
        );
    }

    #[test]
    fn pushed_state_updates_visible_target_without_manual_sync() {
        let mut app = App::new(test_cli());
        app.state_mut().target_client = Some(ClientName(String::from("client-1")));

        app.handle_server_message(ServerMessage::StateUpdated(StateUpdated {
            generation: 1,
            state: projection_state(&[
                ("$1", "main", &[("@1", "win-1", true)]),
                ("$2", "other", &[("@2", "win-2", true)]),
            ]),
        }))
        .expect("apply initial state");

        let mut switched_target = projection_state(&[
            ("$1", "main", &[("@1", "win-1", true)]),
            ("$2", "other", &[("@2", "win-2", true)]),
        ]);
        switched_target.clients[0].session_id = String::from("$2");
        switched_target.clients[0].current_window_id = Some(String::from("@2"));

        app.handle_server_message(ServerMessage::StateUpdated(StateUpdated {
            generation: 2,
            state: switched_target,
        }))
        .expect("apply pushed target switch");

        let rows = app.state().tree_rows();
        let initial_window = rows
            .iter()
            .find(|row| row.focus == Focus::window("$1", "@1"))
            .expect("expected initial target row");
        let switched_window = rows
            .iter()
            .find(|row| row.focus == Focus::window("$2", "@2"))
            .expect("expected switched target row");

        assert!(!initial_window.active());
        assert!(switched_window.active());
        assert_eq!(
            app.state()
                .tmux
                .visible_window_key(app.state().target_client.as_ref()),
            Some(WinlinkKey::new("$2", "@2"))
        );
    }

    #[test]
    fn action_result_errors_surface_in_ui_state() {
        let mut app = App::new(test_cli());

        app.handle_server_message(ServerMessage::ActionResult(ActionResult {
            request_id: String::from("req-1"),
            generation: 1,
            result: ActionResultKind::Error {
                message: String::from("boom"),
            },
        }))
        .expect("handle action result");

        assert_eq!(
            app.state()
                .last_error
                .as_ref()
                .map(|error| error.message.as_str()),
            Some("boom")
        );
    }

    #[test]
    fn successful_action_results_clear_previous_errors() {
        let mut app = App::new(test_cli());
        app.state_mut().last_error = Some(crate::model::ActionError {
            message: String::from("stale"),
        });

        app.handle_server_message(ServerMessage::ActionResult(ActionResult {
            request_id: String::from("req-2"),
            generation: 1,
            result: ActionResultKind::Ok { outcome: None },
        }))
        .expect("handle action result");

        assert!(app.state().last_error.is_none());
    }

    #[test]
    fn runtime_generation_wait_applies_target_update_and_preserves_other_events() {
        let mut app = App::new(test_cli());
        app.state_mut().target_client = Some(ClientName(String::from("client-1")));
        let (sender, receiver) = mpsc::channel();
        app.runtime = Some(UiRuntime::for_test(PathBuf::from("/tmux.sock"), receiver));

        let mut switched_target = projection_state(&[
            ("$1", "main", &[("@1", "win-1", true)]),
            ("$2", "other", &[("@2", "win-2", true)]),
        ]);
        switched_target.clients[0].session_id = String::from("$2");
        switched_target.clients[0].current_window_id = Some(String::from("@2"));

        sender
            .send(UiEvent::Terminal(AppEvent::Tick))
            .expect("send tick");
        sender
            .send(UiEvent::Server(ServerMessage::StateUpdated(StateUpdated {
                generation: 1,
                state: projection_state(&[
                    ("$1", "main", &[("@1", "win-1", true)]),
                    ("$2", "other", &[("@2", "win-2", true)]),
                ]),
            })))
            .expect("send first runtime update");
        sender
            .send(UiEvent::Server(ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: switched_target,
            })))
            .expect("send second runtime update");

        app.wait_for_runtime_generation(2, Duration::from_millis(25))
            .expect("wait for runtime generation");

        assert_eq!(app.server_generation, 2);
        assert_eq!(
            app.state()
                .tmux
                .visible_window_key(app.state().target_client.as_ref()),
            Some(WinlinkKey::new("$2", "@2"))
        );
        assert!(matches!(
            app.runtime
                .as_mut()
                .expect("runtime is present")
                .recv()
                .expect("read deferred runtime event"),
            UiEvent::Terminal(AppEvent::Tick)
        ));
    }

    #[test]
    fn runtime_channel_sync_applies_pushed_state_without_socket_polling() {
        let mut app = App::new(test_cli());
        app.state_mut().target_client = Some(ClientName(String::from("client-1")));
        let (sender, receiver) = mpsc::channel();
        app.runtime = Some(UiRuntime::for_test(PathBuf::from("/tmux.sock"), receiver));

        sender
            .send(UiEvent::Server(ServerMessage::StateUpdated(StateUpdated {
                generation: 1,
                state: projection_state(&[
                    ("$1", "main", &[("@1", "win-1", true)]),
                    ("$2", "other", &[("@2", "win-2", false)]),
                ]),
            })))
            .expect("send runtime update");

        assert!(
            app.sync_with_server(Duration::from_millis(1))
                .expect("sync through runtime channel")
        );
        assert_eq!(app.state().focus, Focus::window("$1", "@1"));
    }

    #[test]
    fn expired_toast_is_cleared_on_next_event() {
        let mut app = App::new(test_cli());
        app.state_mut().toast = Some(crate::model::Toast {
            message: String::from("Started tmux-sidecar server"),
        });
        app.toast_deadline = Some(Instant::now() - Duration::from_millis(1));

        app.handle_event(AppEvent::Tick).expect("handle tick");

        assert!(app.state().toast.is_none());
    }
}
