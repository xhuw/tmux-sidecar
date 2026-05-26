use std::{
    io, panic,
    sync::Once,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
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
    event::{self, AppEvent},
    input::InputBuffer,
    model::{
        ActionError, AppState, ClientName, EditAction, Focus, FocusMove, FocusReconcile, Mode,
        TmuxState, WindowTarget,
    },
    tmux::{Tmux, TmuxCli, TmuxError},
    ui,
};

static PANIC_HOOK: Once = Once::new();

#[derive(Debug)]
pub struct App {
    cli: Cli,
    state: AppState,
    tmux: TmuxCli,
    poll_interval: Duration,
    should_quit: bool,
}

impl App {
    pub fn new(cli: Cli) -> Self {
        let poll_interval = cli.poll_interval();
        let tmux = TmuxCli {
            socket_name: cli.socket_name.clone(),
            socket_path: cli.socket_path.clone(),
        };

        Self {
            cli,
            state: AppState::default(),
            tmux,
            poll_interval,
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
        self.startup_checks()?;
        install_panic_hook();

        let mut terminal = TerminalSession::enter()?;
        self.event_loop(&mut terminal.terminal)
    }

    fn startup_checks(&mut self) -> Result<()> {
        let target_client = self.tmux.check_startup(self.cli.target_client.as_deref())?;

        self.state.target_client = Some(target_client);
        self.state.seen_activity = self.tmux.load_seen_activity()?;
        if let Some(window_id) = self.tmux.sidecar_window_id_from_tmux_pane() {
            self.state.ignored_activity_window_ids.insert(window_id);
        }
        self.refresh_snapshot()?;
        self.state.focus_visible_target();
        Ok(())
    }

    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| ui::render(frame, &self.state))?;

            let event = event::poll_next(self.timeout_until_poll())?;
            self.handle_event(event)?;

            if self.poll_due() {
                self.refresh_snapshot()?;
            }
        }

        Ok(())
    }

    fn timeout_until_poll(&self) -> Duration {
        self.state
            .next_poll_at
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or(self.poll_interval)
    }

    fn poll_due(&self) -> bool {
        match self.state.next_poll_at {
            Some(deadline) => Instant::now() >= deadline,
            None => true,
        }
    }

    fn refresh_snapshot(&mut self) -> Result<()> {
        let snapshot = self.tmux.snapshot()?;
        self.apply_snapshot(snapshot);
        self.tmux.save_seen_activity(&self.state.seen_activity)?;
        self.state.next_poll_at = Some(Instant::now() + self.poll_interval);
        Ok(())
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
                self.close_focused_window()?;
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
        let Some(()) = self.try_tmux_action(|tmux| tmux.switch_to(&client, target))? else {
            return Ok(());
        };

        if self.cli.auto_quit {
            self.should_quit = true;
            return Ok(());
        }

        self.refresh_snapshot()
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

    fn close_focused_window(&mut self) -> Result<()> {
        let Focus::Window { window_id, .. } = self.state.focus.clone() else {
            return Ok(());
        };

        let Some(()) = self.try_tmux_action(|tmux| tmux.close_window(&window_id))? else {
            return Ok(());
        };

        self.refresh_snapshot()
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
                let Some(()) =
                    self.try_tmux_action(|tmux| tmux.rename_session(&id, input.as_str()))?
                else {
                    return Ok(());
                };

                self.refresh_snapshot()?;
                self.state.focus = Focus::Session(id);
            }
            Mode::RenameWindow {
                session_id,
                id,
                input,
                ..
            } => {
                let Some(()) =
                    self.try_tmux_action(|tmux| tmux.rename_window(&id, input.as_str()))?
                else {
                    return Ok(());
                };

                self.refresh_snapshot()?;
                self.state.focus = Focus::window(session_id, id);
            }
            Mode::CreateSessionName { input } => {
                let Some(client) = self.state.target_client.clone() else {
                    return Ok(());
                };
                let name = (!input.is_empty()).then_some(input.as_str());
                let Some(session_id) = self.try_tmux_action(|tmux| tmux.create_session(name))?
                else {
                    return Ok(());
                };

                self.switch_to_target(client, WindowTarget::Session(session_id.clone()))?;
                if !self.should_quit {
                    self.state.focus = Focus::Session(session_id);
                }
            }
            Mode::CreateWindowName { session_id, input } => {
                let Some(client) = self.state.target_client.clone() else {
                    return Ok(());
                };
                let name = (!input.is_empty()).then_some(input.as_str());
                let Some(window_id) =
                    self.try_tmux_action(|tmux| tmux.create_window(&session_id, name))?
                else {
                    return Ok(());
                };

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

    fn try_tmux_action<T>(
        &mut self,
        action: impl FnOnce(&TmuxCli) -> std::result::Result<T, TmuxError>,
    ) -> Result<Option<T>> {
        match action(&self.tmux) {
            Ok(value) => {
                self.state.last_error = None;
                Ok(Some(value))
            }
            Err(error) => {
                self.state.last_error = Some(ActionError {
                    message: error.to_string(),
                });
                self.state.mode = Mode::Normal;
                self.refresh_snapshot()?;
                Ok(None)
            }
        }
    }
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
