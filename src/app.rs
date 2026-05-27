use std::{collections::BTreeSet, env, io, panic, path::PathBuf, sync::Once, time::Duration};

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
    event::{self, AppEvent},
    input::InputBuffer,
    ipc::{Action, ActionResult, ActionResultKind, ProjectionState, ServerMessage, StateUpdated},
    model::{
        ActionError, AppState, ClientName, EditAction, Focus, FocusMove, FocusReconcile, Mode,
        TmuxState, WindowTarget,
    },
    tmux::{Tmux, TmuxCli, hooks::HookCommandProgram},
    ui,
};

static PANIC_HOOK: Once = Once::new();
const ACTION_SYNC_TIMEOUT: Duration = Duration::from_secs(2);
const SERVER_DRAIN_TIMEOUT: Duration = Duration::from_millis(1);
const SERVER_SETTLE_TIMEOUT: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub struct App {
    cli: Cli,
    state: AppState,
    tmux: TmuxCli,
    render_tick: Duration,
    subscription: Option<IpcClient>,
    should_quit: bool,
}

impl App {
    pub fn new(cli: Cli) -> Self {
        let render_tick = cli.poll_interval();
        let tmux = TmuxCli {
            socket_name: cli.socket_name.clone(),
            socket_path: cli.socket_path.clone(),
        };

        Self {
            cli,
            state: AppState::default(),
            tmux,
            render_tick,
            subscription: None,
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
        self.startup()?;
        install_panic_hook();

        let mut terminal = TerminalSession::enter()?;
        self.event_loop(&mut terminal.terminal)
    }

    pub fn startup(&mut self) -> Result<()> {
        let target_client = self.tmux.check_startup(self.cli.target_client.as_deref())?;
        let tmux_socket_path = client::resolve_tmux_socket_path(
            self.cli.socket_name.clone(),
            self.cli.socket_path.clone(),
        )?;
        client::ensure_server_running(&tmux_socket_path)?;
        self.install_hooks()?;

        let mut subscription = client::subscribe(&tmux_socket_path, Some(target_client.0.clone()))?;
        self.state.target_client = Some(target_client);
        self.apply_initial_state(&mut subscription)?;
        self.subscription = Some(subscription);
        Ok(())
    }

    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        while !self.should_quit {
            self.drain_server_messages()?;
            terminal.draw(|frame| ui::render(frame, &self.state))?;
            let event = event::poll_next(self.render_tick)?;
            self.handle_event(event)?;
        }

        Ok(())
    }

    pub fn sync_with_server(&mut self, timeout: Duration) -> Result<bool> {
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

    fn install_hooks(&self) -> Result<()> {
        let program = HookCommandProgram::new(vec![hook_program_path()?.display().to_string()]);
        self.tmux.install_hooks(&program)?;
        Ok(())
    }

    fn drain_server_messages(&mut self) -> Result<()> {
        let Some(mut subscription) = self.subscription.take() else {
            return Ok(());
        };

        let result = (|| {
            subscription.set_read_timeout(Some(SERVER_DRAIN_TIMEOUT))?;
            loop {
                match subscription.read_status()? {
                    ReadStatus::Message(message) => self.handle_server_message(message)?,
                    ReadStatus::Pending => break Ok(()),
                    ReadStatus::Closed => break Err(anyhow!("sidecar server disconnected")),
                }
            }
        })();
        let reset = subscription.set_read_timeout(None);
        self.subscription = Some(subscription);
        result.and(reset)
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

    fn handle_server_message(&mut self, message: ServerMessage) -> Result<()> {
        match message {
            ServerMessage::StateUpdated(update) => {
                self.handle_state_update(update);
                Ok(())
            }
            ServerMessage::ActionResult(result) => self.handle_action_result(result),
            ServerMessage::Error(error) => bail!(error.message),
            ServerMessage::HelloAck(_) => Ok(()),
            ServerMessage::Ack(_) => Ok(()),
        }
    }

    fn handle_action_result(&mut self, result: ActionResult) -> Result<()> {
        match result.result {
            ActionResultKind::Ok => {
                self.state.last_error = None;
                Ok(())
            }
            ActionResultKind::Error { message } => {
                self.state.last_error = Some(ActionError { message });
                Ok(())
            }
        }
    }

    fn handle_state_update(&mut self, update: StateUpdated) {
        let should_focus_visible_target = self.state.is_tree_loading();
        self.apply_projection(update.state);
        if should_focus_visible_target {
            self.state.focus_visible_target();
        }
    }

    fn apply_projection(&mut self, projection: ProjectionState) -> FocusReconcile {
        self.state.reconcile_tmux(projection.into_tmux_state())
    }

    fn handle_event(&mut self, event: AppEvent) -> Result<()> {
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
            KeyCode::Char('s') => {
                self.state.clear_navigation();
                self.begin_create_session_naming()?;
            }
            KeyCode::Char('S') => {
                self.state.start_jump();
            }
            KeyCode::Char('c') => {
                self.state.clear_navigation();
                self.begin_create_window_from_focus()?;
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
        let Some(()) = self.try_server_action(Some(client.0), action)? else {
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
        let action = match self.state.focus.clone() {
            Focus::Session(session_id) => Action::CloseSession { session_id },
            Focus::Window { window_id, .. } => Action::CloseWindow { window_id },
            Focus::CreateSession | Focus::CreateWindow(_) => return Ok(()),
        };

        let _ = self.try_server_action(None, action)?;
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
                let Some(()) = self.try_server_action(
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
                let Some(()) = self.try_server_action(
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
                let session_ids_before = self.session_ids();
                let Some(()) =
                    self.try_server_action(None, Action::CreateSession { name: name.clone() })?
                else {
                    return Ok(());
                };
                let session_id = self.created_session_id(&session_ids_before, name.as_deref())?;

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
                let window_ids_before = self.window_ids(&session_id)?;
                let Some(()) = self.try_server_action(
                    None,
                    Action::CreateWindow {
                        session_id: session_id.clone(),
                        name: name.clone(),
                    },
                )?
                else {
                    return Ok(());
                };
                let window_id =
                    self.created_window_id(&session_id, &window_ids_before, name.as_deref())?;

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

    fn session_ids(&self) -> BTreeSet<String> {
        self.state
            .tmux
            .sessions
            .iter()
            .map(|session| session.id.clone())
            .collect()
    }

    fn window_ids(&self, session_id: &str) -> Result<BTreeSet<String>> {
        let session = self
            .state
            .tmux
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("missing session `{session_id}` in current state"))?;
        Ok(session
            .windows
            .iter()
            .map(|window| window.id.clone())
            .collect())
    }

    fn created_session_id(
        &self,
        previous_session_ids: &BTreeSet<String>,
        requested_name: Option<&str>,
    ) -> Result<String> {
        self.created_row_id(
            self.state
                .tmux
                .sessions
                .iter()
                .filter(|session| !previous_session_ids.contains(&session.id))
                .map(|session| (session.id.as_str(), session.name.as_str())),
            requested_name,
            "session",
        )
    }

    fn created_window_id(
        &self,
        session_id: &str,
        previous_window_ids: &BTreeSet<String>,
        requested_name: Option<&str>,
    ) -> Result<String> {
        let session = self
            .state
            .tmux
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .ok_or_else(|| anyhow!("missing session `{session_id}` after create-window"))?;
        self.created_row_id(
            session
                .windows
                .iter()
                .filter(|window| !previous_window_ids.contains(&window.id))
                .map(|window| (window.id.as_str(), window.name.as_str())),
            requested_name,
            "window",
        )
    }

    fn created_row_id<'a>(
        &self,
        rows: impl Iterator<Item = (&'a str, &'a str)>,
        requested_name: Option<&str>,
        kind: &str,
    ) -> Result<String> {
        let created: Vec<_> = rows.collect();

        if let Some(requested_name) = requested_name {
            let matching: Vec<_> = created
                .iter()
                .filter(|(_, name)| *name == requested_name)
                .collect();
            if matching.len() == 1 {
                return Ok(matching[0].0.to_owned());
            }
        }

        match created.as_slice() {
            [(id, _)] => Ok((*id).to_owned()),
            [] => bail!("action succeeded but created {kind} was missing from the refreshed state"),
            _ => bail!("action succeeded but created {kind} was ambiguous in the refreshed state"),
        }
    }

    fn try_server_action(
        &mut self,
        target_client: Option<String>,
        action: Action,
    ) -> Result<Option<()>> {
        if self.perform_server_action(target_client, action)? {
            Ok(Some(()))
        } else {
            self.state.mode = Mode::Normal;
            Ok(None)
        }
    }

    fn perform_server_action(
        &mut self,
        target_client: Option<String>,
        action: Action,
    ) -> Result<bool> {
        let Some(mut subscription) = self.subscription.take() else {
            return Ok(false);
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
                        let succeeded = matches!(result.result, ActionResultKind::Ok);
                        self.handle_action_result(result)?;
                        return Ok(succeeded);
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
            (Ok(succeeded), Ok(())) => Ok(succeeded),
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
    use super::App;
    use crate::{
        cli::Cli,
        ipc::{
            ActionResult, ActionResultKind, ProjectionClient, ProjectionSession, ProjectionState,
            ProjectionWindow, ServerMessage, StateUpdated,
        },
        model::{ClientName, Focus},
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

        app.handle_server_message(ServerMessage::StateUpdated(StateUpdated {
            generation: 1,
            state: projection_state(&[("$1", "main", &[("@1", "win", true)])]),
        }))
        .expect("apply initial state");

        assert_eq!(app.state().focus, Focus::window("$1", "@1"));
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
    fn action_result_errors_surface_in_ui_state() {
        let mut app = App::new(test_cli());

        app.handle_server_message(ServerMessage::ActionResult(ActionResult {
            request_id: String::from("req-1"),
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
            result: ActionResultKind::Ok,
        }))
        .expect("handle action result");

        assert!(app.state().last_error.is_none());
    }
}
