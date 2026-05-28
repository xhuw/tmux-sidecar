use super::{
    ClientName, DomainState, SessionId, SessionNode, WindowAlert, WindowId, WindowState, WinlinkKey,
};

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

    pub fn winlink(key: WinlinkKey) -> Self {
        Self::Window {
            session_id: key.session_id,
            window_id: key.window_id,
        }
    }
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

    fn session(session: &SessionNode) -> Self {
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

    fn window(key: WinlinkKey, window: &WindowState, active: bool) -> Self {
        Self {
            focus: Focus::winlink(key.clone()),
            depth: 1,
            kind: TreeRowKind::Window {
                session_id: key.session_id,
                id: window.id.clone(),
                index: window.index,
                name: window.name.clone(),
                active,
                alert: window.alert(),
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

impl DomainState {
    pub fn tree_rows(&self) -> Vec<TreeRow> {
        self.tree_rows_for_client(None)
    }

    pub fn tree_rows_for_client(&self, target_client: Option<&ClientName>) -> Vec<TreeRow> {
        let visible_window = self.visible_window_key(target_client);
        let mut rows = Vec::new();

        for session in self.ordered_sessions() {
            rows.push(TreeRow::session(session));

            for (key, window) in self.session_windows(&session.id) {
                rows.push(TreeRow::window(
                    key.clone(),
                    window,
                    visible_window.as_ref() == Some(&key),
                ));
            }

            rows.push(TreeRow::create_window(session.id.clone()));
        }

        rows.push(TreeRow::create_session());
        rows
    }
}
