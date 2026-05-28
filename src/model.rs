use std::{collections::BTreeMap, path::PathBuf};

pub use crate::domain::{
    ClientName, ClientNode, DomainState, Focus, SessionId, SessionNode, SessionState, TreeRow,
    TreeRowKind, WindowAlert, WindowId, WindowState, WinlinkKey,
};
use crate::input::InputBuffer;

const JUMP_LABELS: &[u8] = b"asdfghjklqwertyuiopzxcvbnmASDFGHJKLQWERTYUIOPZXCVBNM";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TmuxState {
    pub sessions: Vec<Session>,
    pub clients: Vec<Client>,
}

impl TmuxState {
    pub fn to_domain_state(&self) -> DomainState {
        let mut state = DomainState::empty(PathBuf::new());

        for (order, session) in self.sessions.iter().enumerate() {
            state.sessions.insert(
                session.id.clone(),
                SessionNode {
                    id: session.id.clone(),
                    name: session.name.clone(),
                    attached_count: session.attached_count,
                    active_window_id: session.active_window_id.clone(),
                    order,
                },
            );

            for window in &session.windows {
                state.winlinks.insert(
                    window.winlink_key(session.id.clone()),
                    window.window_state(),
                );
            }
        }

        for (order, client) in self.clients.iter().enumerate() {
            state.clients.insert(
                client.name.clone(),
                ClientNode {
                    name: client.name.clone(),
                    session_id: client.session_id.clone(),
                    current_window_id: client.current_window_id.clone(),
                    activity: client.activity,
                    tty: client.tty.clone(),
                    order,
                },
            );
        }

        state
    }

    pub fn from_domain_state(state: DomainState) -> Self {
        let sessions = state
            .ordered_sessions()
            .into_iter()
            .map(|session| Session {
                id: session.id.clone(),
                name: session.name.clone(),
                attached_count: session.attached_count,
                active_window_id: session.active_window_id.clone(),
                windows: state
                    .session_windows(&session.id)
                    .into_iter()
                    .map(|(_, window)| Window::from_window_state(window))
                    .collect(),
            })
            .collect();
        let clients = state
            .ordered_clients()
            .into_iter()
            .map(|client| Client {
                name: client.name.clone(),
                session_id: client.session_id.clone(),
                current_window_id: client.current_window_id.clone(),
                activity: client.activity,
                tty: client.tty.clone(),
            })
            .collect();

        Self { sessions, clients }
    }

    pub fn tree_rows(&self) -> Vec<TreeRow> {
        self.to_domain_state().tree_rows()
    }

    pub fn tree_rows_for_client(&self, target_client: Option<&ClientName>) -> Vec<TreeRow> {
        self.to_domain_state().tree_rows_for_client(target_client)
    }

    pub fn visible_window(
        &self,
        target_client: Option<&ClientName>,
    ) -> Option<(&Session, &Window)> {
        let key = self.visible_window_key(target_client)?;
        let session = self
            .sessions
            .iter()
            .find(|session| session.id == key.session_id)?;
        let window = session
            .windows
            .iter()
            .find(|window| window.id == key.window_id)?;
        Some((session, window))
    }

    pub fn visible_window_key(&self, target_client: Option<&ClientName>) -> Option<WinlinkKey> {
        self.to_domain_state().visible_window_key(target_client)
    }

    pub fn session_states(&self) -> BTreeMap<SessionId, SessionState> {
        self.to_domain_state().session_states()
    }

    pub fn visible_session(&self, target_client: Option<&ClientName>) -> Option<&Session> {
        let session_id = self
            .to_domain_state()
            .visible_session(target_client)
            .map(|session| session.id.clone())?;
        self.sessions
            .iter()
            .find(|session| session.id == session_id)
    }
}

impl From<DomainState> for TmuxState {
    fn from(state: DomainState) -> Self {
        Self::from_domain_state(state)
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
    pub fn active_window_key(&self) -> Option<WinlinkKey> {
        self.active_window_id
            .as_ref()
            .map(|window_id| WinlinkKey::new(self.id.clone(), window_id.clone()))
    }

    pub fn session_state(&self) -> SessionState {
        SessionState {
            id: self.id.clone(),
            name: self.name.clone(),
            attached_count: self.attached_count,
            active_window_id: self.active_window_id.clone(),
            windows: self
                .windows
                .iter()
                .map(|window| (window.winlink_key(self.id.clone()), window.window_state()))
                .collect(),
        }
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
    pub activity_flag: bool,
    pub silence_flag: bool,
}

impl Window {
    pub fn winlink_key(&self, session_id: impl Into<SessionId>) -> WinlinkKey {
        WinlinkKey::new(session_id, self.id.clone())
    }

    pub fn window_state(&self) -> WindowState {
        let activity_flag = self.activity_flag || self.flags.contains('#');
        let bell_flag = self.alert.is_alerting() || self.flags.contains('!');
        let silence_flag = self.silence_flag || self.flags.contains('~');
        WindowState {
            id: self.id.clone(),
            index: self.index,
            name: self.name.clone(),
            active: self.active,
            activity: self.activity,
            activity_flag,
            bell_flag,
            silence_flag,
        }
    }

    fn from_window_state(window: &WindowState) -> Self {
        let mut flags = String::new();
        if window.active {
            flags.push('*');
        }
        if window.activity_flag {
            flags.push('#');
        }
        if window.bell_flag {
            flags.push('!');
        }
        if window.silence_flag {
            flags.push('~');
        }

        Self {
            id: window.id.clone(),
            index: window.index,
            name: window.name.clone(),
            active: window.active,
            flags,
            alert: window.alert(),
            activity: window.activity,
            activity_flag: window.activity_flag,
            silence_flag: window.silence_flag,
        }
    }

    pub fn set_flags(&mut self, flags: impl Into<String>) {
        self.flags = flags.into();
        self.activity_flag = self.flags.contains('#');
        self.silence_flag = self.flags.contains('~');
        self.alert = WindowAlert::from_flags(&self.flags);
    }

    pub fn has_alert(&self) -> bool {
        self.alert.is_alerting()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub name: ClientName,
    pub session_id: SessionId,
    pub current_window_id: Option<WindowId>,
    pub activity: u64,
    pub tty: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
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

#[derive(Debug, Clone)]
pub struct AppState {
    pub tmux: TmuxState,
    pub focus: Focus,
    pub mode: Mode,
    pub navigation: NavigationState,
    pub target_client: Option<ClientName>,
    pub last_error: Option<ActionError>,
    pub toast: Option<Toast>,
    pub tree_loading: bool,
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
            toast: None,
            tree_loading: false,
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

    pub fn is_tree_loading(&self) -> bool {
        self.tree_loading || (self.target_client.is_some() && self.tmux.sessions.is_empty())
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
            .visible_window_key(self.target_client.as_ref())
            .map(Focus::winlink)
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
