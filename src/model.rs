use std::time::Instant;

use crate::input::InputBuffer;

pub type SessionId = String;
pub type WindowId = String;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TmuxState {
    pub sessions: Vec<Session>,
}

impl TmuxState {
    pub fn tree_rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        rows.push(TreeRow::create_session());

        for session in &self.sessions {
            rows.push(TreeRow::session(session));

            for window in &session.windows {
                rows.push(TreeRow::window(session.id.clone(), window));
            }

            rows.push(TreeRow::create_window(session.id.clone()));
        }

        rows
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

impl Session {
    pub fn is_active(&self) -> bool {
        self.active_window_id.is_some() || self.windows.iter().any(|window| window.active)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowAlert {
    #[default]
    None,
    Activity,
    Bell,
    Silence,
}

impl WindowAlert {
    pub fn from_flags(flags: &str) -> Self {
        if flags.contains('!') {
            Self::Bell
        } else if flags.contains('#') {
            Self::Activity
        } else if flags.contains('~') {
            Self::Silence
        } else {
            Self::None
        }
    }

    pub fn is_alerting(self) -> bool {
        !matches!(self, Self::None)
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum Focus {
    #[default]
    CreateSession,
    Session(SessionId),
    Window(WindowId),
    CreateWindow(SessionId),
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
        id: WindowId,
        original_name: String,
        input: InputBuffer,
    },
    CreateSessionName {
        id: SessionId,
        input: InputBuffer,
    },
    CreateWindowName {
        id: WindowId,
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
    Window(WindowId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionError {
    pub message: String,
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
        active: bool,
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
                active: session.is_active(),
            },
        }
    }

    fn window(session_id: SessionId, window: &Window) -> Self {
        Self {
            focus: Focus::Window(window.id.clone()),
            depth: 1,
            kind: TreeRowKind::Window {
                session_id,
                id: window.id.clone(),
                index: window.index,
                name: window.name.clone(),
                active: window.active,
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
            TreeRowKind::Session { active, .. } | TreeRowKind::Window { active, .. } => *active,
            TreeRowKind::CreateSession | TreeRowKind::CreateWindow { .. } => false,
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
        self.tmux.tree_rows()
    }

    pub fn focused_row_index(&self) -> Option<usize> {
        let rows = self.tree_rows();
        self.focused_row_index_in(&rows)
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
}
