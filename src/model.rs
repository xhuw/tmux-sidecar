use std::time::Instant;

use crate::input::InputBuffer;

pub type SessionId = String;
pub type WindowId = String;

const JUMP_LABELS: &[u8] = b"asdfghjklqwertyuiopzxcvbnmASDFGHJKLQWERTYUIOPZXCVBNM";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TmuxState {
    pub sessions: Vec<Session>,
    pub clients: Vec<Client>,
}

impl TmuxState {
    pub fn tree_rows(&self) -> Vec<TreeRow> {
        self.tree_rows_for_client(None)
    }

    pub fn tree_rows_for_client(&self, target_client: Option<&ClientName>) -> Vec<TreeRow> {
        let visible_window = self
            .visible_window(target_client)
            .map(|(session, window)| (session.id.as_str(), window.id.as_str()));
        let mut rows = Vec::new();

        for session in &self.sessions {
            rows.push(TreeRow::session(session));

            for window in &session.windows {
                rows.push(TreeRow::window(
                    session.id.clone(),
                    window,
                    visible_window == Some((session.id.as_str(), window.id.as_str())),
                ));
            }

            rows.push(TreeRow::create_window(session.id.clone()));
        }

        rows.push(TreeRow::create_session());
        rows
    }

    pub fn visible_window(
        &self,
        target_client: Option<&ClientName>,
    ) -> Option<(&Session, &Window)> {
        let client = self.visible_client(target_client)?;
        let session = self
            .sessions
            .iter()
            .find(|session| session.id == client.session_id)?;
        let window_id = client
            .current_window_id
            .as_deref()
            .or(session.active_window_id.as_deref())?;
        let window = session
            .windows
            .iter()
            .find(|window| window.id == window_id)?;

        Some((session, window))
    }

    pub fn visible_session(&self, target_client: Option<&ClientName>) -> Option<&Session> {
        if let Some((session, _)) = self.visible_window(target_client) {
            return Some(session);
        }

        let client = self.visible_client(target_client)?;
        self.sessions
            .iter()
            .find(|session| session.id == client.session_id)
    }

    fn visible_client(&self, target_client: Option<&ClientName>) -> Option<&Client> {
        match target_client {
            Some(name) => self.clients.iter().find(|client| &client.name == name),
            None => self.clients.iter().max_by_key(|client| client.activity),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub attached_count: u32,
    pub active_window_id: Option<WindowId>,
    pub windows: Vec<Window>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowAlert {
    #[default]
    None,
    Bell,
}

impl WindowAlert {
    pub fn from_indicators(_has_activity: bool, has_bell: bool, _has_silence: bool) -> Self {
        if has_bell { Self::Bell } else { Self::None }
    }

    pub fn from_flags(flags: &str) -> Self {
        Self::from_indicators(
            flags.contains('#'),
            flags.contains('!'),
            flags.contains('~'),
        )
    }

    pub fn is_alerting(self) -> bool {
        matches!(self, Self::Bell)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    pub id: WindowId,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub flags: String,
    pub alert: WindowAlert,
    pub activity: u64,
}

impl Window {
    pub fn set_flags(&mut self, flags: impl Into<String>) {
        self.flags = flags.into();
        self.alert = WindowAlert::from_flags(&self.flags);
    }

    pub fn has_alert(&self) -> bool {
        self.alert.is_alerting()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientName(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub name: ClientName,
    pub session_id: SessionId,
    pub current_window_id: Option<WindowId>,
    pub activity: u64,
    pub tty: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum Focus {
    #[default]
    CreateSession,
    Session(SessionId),
    Window {
        session_id: SessionId,
        window_id: WindowId,
    },
    CreateWindow(SessionId),
}

impl Focus {
    pub fn window(session_id: impl Into<SessionId>, window_id: impl Into<WindowId>) -> Self {
        Self::Window {
            session_id: session_id.into(),
            window_id: window_id.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Help,
    RenameSession {
        id: SessionId,
        original_name: String,
        input: InputBuffer,
    },
    RenameWindow {
        session_id: SessionId,
        id: WindowId,
        original_name: String,
        input: InputBuffer,
    },
    CreateSessionName {
        input: InputBuffer,
    },
    CreateWindowName {
        session_id: SessionId,
        input: InputBuffer,
    },
}

impl Mode {
    pub fn input_buffer(&self) -> Option<&InputBuffer> {
        match self {
            Mode::RenameSession { input, .. }
            | Mode::RenameWindow { input, .. }
            | Mode::CreateSessionName { input, .. }
            | Mode::CreateWindowName { input, .. } => Some(input),
            Mode::Normal | Mode::Help => None,
        }
    }

    pub fn input_buffer_mut(&mut self) -> Option<&mut InputBuffer> {
        match self {
            Mode::RenameSession { input, .. }
            | Mode::RenameWindow { input, .. }
            | Mode::CreateSessionName { input, .. }
            | Mode::CreateWindowName { input, .. } => Some(input),
            Mode::Normal | Mode::Help => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowTarget {
    Session(SessionId),
    Window {
        session_id: SessionId,
        window_id: WindowId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionError {
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NavigationState {
    pub pending_g: bool,
    pub jumping: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpTarget {
    pub focus: Focus,
    pub label: char,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusMove {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    Insert(char),
    Backspace,
    Delete,
    MoveLeft,
    MoveRight,
    MoveHome,
    MoveEnd,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusRecovery {
    Preserved,
    NearestRow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusReconcile {
    pub recovery: FocusRecovery,
    pub row_index: usize,
    pub focus: Focus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRowKind {
    CreateSession,
    Session {
        id: SessionId,
        name: String,
        attached_count: u32,
    },
    Window {
        session_id: SessionId,
        id: WindowId,
        index: u32,
        name: String,
        active: bool,
        alert: WindowAlert,
    },
    CreateWindow {
        session_id: SessionId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub focus: Focus,
    pub depth: u8,
    pub kind: TreeRowKind,
}

impl TreeRow {
    fn create_session() -> Self {
        Self {
            focus: Focus::CreateSession,
            depth: 0,
            kind: TreeRowKind::CreateSession,
        }
    }

    fn session(session: &Session) -> Self {
        Self {
            focus: Focus::Session(session.id.clone()),
            depth: 0,
            kind: TreeRowKind::Session {
                id: session.id.clone(),
                name: session.name.clone(),
                attached_count: session.attached_count,
            },
        }
    }

    fn window(session_id: SessionId, window: &Window, active: bool) -> Self {
        Self {
            focus: Focus::window(session_id.clone(), window.id.clone()),
            depth: 1,
            kind: TreeRowKind::Window {
                session_id,
                id: window.id.clone(),
                index: window.index,
                name: window.name.clone(),
                active,
                alert: window.alert,
            },
        }
    }

    fn create_window(session_id: SessionId) -> Self {
        Self {
            focus: Focus::CreateWindow(session_id.clone()),
            depth: 1,
            kind: TreeRowKind::CreateWindow { session_id },
        }
    }

    pub fn active(&self) -> bool {
        match &self.kind {
            TreeRowKind::Window { active, .. } => *active,
            TreeRowKind::CreateSession
            | TreeRowKind::Session { .. }
            | TreeRowKind::CreateWindow { .. } => false,
        }
    }

    pub fn alert(&self) -> Option<WindowAlert> {
        match &self.kind {
            TreeRowKind::Window { alert, .. } if alert.is_alerting() => Some(*alert),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub tmux: TmuxState,
    pub focus: Focus,
    pub mode: Mode,
    pub navigation: NavigationState,
    pub target_client: Option<ClientName>,
    pub last_error: Option<ActionError>,
    pub next_poll_at: Option<Instant>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tmux: TmuxState::default(),
            focus: Focus::default(),
            mode: Mode::default(),
            navigation: NavigationState::default(),
            target_client: None,
            last_error: None,
            next_poll_at: None,
        }
    }
}

impl AppState {
    pub fn from_tmux(tmux: TmuxState) -> Self {
        Self {
            tmux,
            ..Self::default()
        }
    }

    pub fn tree_rows(&self) -> Vec<TreeRow> {
        self.tmux.tree_rows_for_client(self.target_client.as_ref())
    }

    pub fn focused_row_index(&self) -> Option<usize> {
        let rows = self.tree_rows();
        self.focused_row_index_in(&rows)
    }

    pub fn focus_first_row(&mut self) -> bool {
        self.focus_row_index(0)
    }

    pub fn focus_last_row(&mut self) -> bool {
        let rows = self.tree_rows();
        let Some(index) = rows.len().checked_sub(1) else {
            return false;
        };

        self.focus_row_index(index)
    }

    pub fn move_focus(&mut self, movement: FocusMove) -> bool {
        let rows = self.tree_rows();
        if rows.is_empty() {
            return false;
        }

        let current = self.focused_row_index_in(&rows).unwrap_or(0);
        let target = match movement {
            FocusMove::Up => current.saturating_sub(1),
            FocusMove::Down => (current + 1).min(rows.len().saturating_sub(1)),
        };

        let next_focus = rows[target].focus.clone();
        if next_focus == self.focus {
            return false;
        }

        self.focus = next_focus;
        true
    }

    pub fn start_g_prefix(&mut self) -> bool {
        if self.navigation.pending_g {
            return false;
        }

        self.navigation.pending_g = true;
        self.navigation.jumping = false;
        true
    }

    pub fn clear_g_prefix(&mut self) -> bool {
        if !self.navigation.pending_g {
            return false;
        }

        self.navigation.pending_g = false;
        true
    }

    pub fn start_jump(&mut self) -> bool {
        if self.tree_rows().is_empty() {
            return false;
        }

        self.navigation.pending_g = false;
        self.navigation.jumping = true;
        true
    }

    pub fn cancel_jump(&mut self) -> bool {
        if !self.navigation.jumping {
            return false;
        }

        self.navigation.jumping = false;
        true
    }

    pub fn clear_navigation(&mut self) {
        self.navigation = NavigationState::default();
    }

    pub fn jump_targets(&self) -> Vec<JumpTarget> {
        if !self.navigation.jumping {
            return Vec::new();
        }

        jump_targets_for_rows(&self.tree_rows())
    }

    pub fn focus_jump_label(&mut self, label: char) -> bool {
        let Some(target) = self
            .jump_targets()
            .into_iter()
            .find(|target| target.label == label)
        else {
            return false;
        };

        self.focus = target.focus;
        true
    }

    pub fn reconcile_tmux(&mut self, tmux: TmuxState) -> FocusReconcile {
        let previous_rows = self.tree_rows();
        let previous_focus = self.focus.clone();
        let previous_index = self
            .focused_row_index_in(&previous_rows)
            .unwrap_or_else(|| previous_rows.len().saturating_sub(1));

        self.tmux = tmux;
        let rows = self.tree_rows();

        let (recovery, row_index) = match rows.iter().position(|row| row.focus == previous_focus) {
            Some(index) => (FocusRecovery::Preserved, index),
            None => {
                let nearest = previous_index.min(rows.len().saturating_sub(1));
                (FocusRecovery::NearestRow, nearest)
            }
        };

        self.focus = rows
            .get(row_index)
            .map(|row| row.focus.clone())
            .unwrap_or_default();

        FocusReconcile {
            recovery,
            row_index,
            focus: self.focus.clone(),
        }
    }

    pub fn focus_visible_target(&mut self) -> bool {
        let next_focus = self
            .tmux
            .visible_window(self.target_client.as_ref())
            .map(|(session, window)| Focus::window(session.id.clone(), window.id.clone()))
            .or_else(|| {
                self.tmux
                    .visible_session(self.target_client.as_ref())
                    .map(|session| Focus::Session(session.id.clone()))
            });

        let Some(next_focus) = next_focus else {
            return false;
        };

        if self.focus == next_focus {
            return false;
        }

        self.focus = next_focus;
        true
    }

    pub fn edit_buffer(&self) -> Option<&InputBuffer> {
        self.mode.input_buffer()
    }

    pub fn edit_buffer_mut(&mut self) -> Option<&mut InputBuffer> {
        self.mode.input_buffer_mut()
    }

    pub fn apply_edit_action(&mut self, action: EditAction) -> bool {
        let Some(buffer) = self.edit_buffer_mut() else {
            return false;
        };

        match action {
            EditAction::Insert(ch) => buffer.insert_char(ch),
            EditAction::Backspace => buffer.backspace(),
            EditAction::Delete => buffer.delete(),
            EditAction::MoveLeft => buffer.move_left(),
            EditAction::MoveRight => buffer.move_right(),
            EditAction::MoveHome => buffer.move_home(),
            EditAction::MoveEnd => buffer.move_end(),
            EditAction::Clear => {
                if buffer.is_empty() {
                    false
                } else {
                    buffer.clear();
                    true
                }
            }
        }
    }

    fn focused_row_index_in(&self, rows: &[TreeRow]) -> Option<usize> {
        rows.iter().position(|row| row.focus == self.focus)
    }

    fn focus_row_index(&mut self, index: usize) -> bool {
        let rows = self.tree_rows();
        let Some(next_focus) = rows.get(index).map(|row| row.focus.clone()) else {
            return false;
        };

        if next_focus == self.focus {
            return false;
        }

        self.focus = next_focus;
        true
    }
}

fn jump_targets_for_rows(rows: &[TreeRow]) -> Vec<JumpTarget> {
    rows.iter()
        .zip(JUMP_LABELS.iter().copied())
        .map(|(row, label)| JumpTarget {
            focus: row.focus.clone(),
            label: char::from(label),
        })
        .collect()
}
