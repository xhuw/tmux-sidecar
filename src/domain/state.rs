use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

pub type SessionId = String;
pub type WindowId = String;

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientName(pub String);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WinlinkKey {
    pub session_id: SessionId,
    pub window_id: WindowId,
}

impl WinlinkKey {
    pub fn new(session_id: impl Into<SessionId>, window_id: impl Into<WindowId>) -> Self {
        Self {
            session_id: session_id.into(),
            window_id: window_id.into(),
        }
    }
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
pub struct SessionNode {
    pub id: SessionId,
    pub name: String,
    pub attached_count: u32,
    pub active_window_id: Option<WindowId>,
    pub order: usize,
}

impl SessionNode {
    pub fn active_window_key(&self) -> Option<WinlinkKey> {
        self.active_window_id
            .as_ref()
            .map(|window_id| WinlinkKey::new(self.id.clone(), window_id.clone()))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowState {
    pub id: WindowId,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub activity: u64,
    pub activity_flag: bool,
    pub bell_flag: bool,
    pub silence_flag: bool,
}

impl WindowState {
    pub fn alert(&self) -> WindowAlert {
        WindowAlert::from_indicators(self.activity_flag, self.bell_flag, self.silence_flag)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientNode {
    pub name: ClientName,
    pub session_id: SessionId,
    pub current_window_id: Option<WindowId>,
    pub activity: u64,
    pub tty: String,
    pub order: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionState {
    pub id: SessionId,
    pub name: String,
    pub attached_count: u32,
    pub active_window_id: Option<WindowId>,
    pub windows: BTreeMap<WinlinkKey, WindowState>,
}

impl SessionState {
    pub fn active_window_key(&self) -> Option<WinlinkKey> {
        self.active_window_id
            .as_ref()
            .map(|window_id| WinlinkKey::new(self.id.clone(), window_id.clone()))
    }
}

/// Canonical normalized tmux snapshot state.
///
/// Linked windows are represented as session-local winlinks in `winlinks`, keyed by the stable
/// `(session_id, window_id)` pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainState {
    pub tmux_socket_path: PathBuf,
    pub sessions: BTreeMap<SessionId, SessionNode>,
    pub winlinks: BTreeMap<WinlinkKey, WindowState>,
    pub clients: BTreeMap<ClientName, ClientNode>,
}

impl DomainState {
    pub fn empty(tmux_socket_path: impl Into<PathBuf>) -> Self {
        Self {
            tmux_socket_path: tmux_socket_path.into(),
            sessions: BTreeMap::new(),
            winlinks: BTreeMap::new(),
            clients: BTreeMap::new(),
        }
    }

    pub fn session(&self, session_id: &str) -> Option<&SessionNode> {
        self.sessions.get(session_id)
    }

    pub fn client_named(&self, client_name: &str) -> Option<&ClientNode> {
        self.clients.get(&ClientName(client_name.to_owned()))
    }

    pub fn visible_client(&self, target_client: Option<&ClientName>) -> Option<&ClientNode> {
        match target_client {
            Some(name) => self.clients.get(name),
            None => self
                .clients
                .values()
                .max_by_key(|client| (client.activity, client.order)),
        }
    }

    pub fn visible_window_key(&self, target_client: Option<&ClientName>) -> Option<WinlinkKey> {
        let client = self.visible_client(target_client)?;
        let session = self.sessions.get(&client.session_id)?;
        let window_id = client
            .current_window_id
            .as_deref()
            .or(session.active_window_id.as_deref())?;
        let key = WinlinkKey::new(session.id.clone(), window_id.to_owned());
        self.winlinks.contains_key(&key).then_some(key)
    }

    pub fn visible_session(&self, target_client: Option<&ClientName>) -> Option<&SessionNode> {
        if let Some(window_key) = self.visible_window_key(target_client) {
            return self.sessions.get(&window_key.session_id);
        }

        let client = self.visible_client(target_client)?;
        self.sessions.get(&client.session_id)
    }

    pub fn session_windows(&self, session_id: &str) -> Vec<(WinlinkKey, &WindowState)> {
        let mut windows: Vec<_> = self
            .winlinks
            .iter()
            .filter(|(key, _)| key.session_id == session_id)
            .map(|(key, window)| (key.clone(), window))
            .collect();
        windows.sort_by(|(left_key, left), (right_key, right)| {
            left.index
                .cmp(&right.index)
                .then_with(|| left_key.window_id.cmp(&right_key.window_id))
        });
        windows
    }

    pub fn session_window(&self, session_id: &str, window_id: &str) -> Option<&WindowState> {
        self.winlinks.get(&WinlinkKey::new(
            session_id.to_owned(),
            window_id.to_owned(),
        ))
    }

    pub fn session_window_mut(
        &mut self,
        session_id: &str,
        window_id: &str,
    ) -> Option<&mut WindowState> {
        self.winlinks.get_mut(&WinlinkKey::new(
            session_id.to_owned(),
            window_id.to_owned(),
        ))
    }

    pub fn session_window_by_index(
        &self,
        session_id: &str,
        window_index: u32,
    ) -> Option<(WinlinkKey, &WindowState)> {
        self.session_windows(session_id)
            .into_iter()
            .find(|(_, window)| window.index == window_index)
    }

    pub fn session_window_by_index_mut(
        &mut self,
        session_id: &str,
        window_index: u32,
    ) -> Option<&mut WindowState> {
        self.winlinks
            .iter_mut()
            .find(|(key, window)| key.session_id == session_id && window.index == window_index)
            .map(|(_, window)| window)
    }

    pub fn session_states(&self) -> BTreeMap<SessionId, SessionState> {
        self.ordered_sessions()
            .into_iter()
            .map(|session| {
                (
                    session.id.clone(),
                    SessionState {
                        id: session.id.clone(),
                        name: session.name.clone(),
                        attached_count: session.attached_count,
                        active_window_id: session.active_window_id.clone(),
                        windows: self
                            .session_windows(&session.id)
                            .into_iter()
                            .map(|(key, window)| (key, window.clone()))
                            .collect(),
                    },
                )
            })
            .collect()
    }

    pub fn ordered_sessions(&self) -> Vec<&SessionNode> {
        let mut sessions: Vec<_> = self.sessions.values().collect();
        sessions.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.id.cmp(&right.id))
        });
        sessions
    }

    pub fn ordered_clients(&self) -> Vec<&ClientNode> {
        let mut clients: Vec<_> = self.clients.values().collect();
        clients.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.name.0.cmp(&right.name.0))
        });
        clients
    }

    pub fn viewed_window_keys(&self) -> BTreeSet<WinlinkKey> {
        let active_window_by_session: BTreeMap<&str, &str> = self
            .sessions
            .values()
            .filter_map(|session| Some((session.id.as_str(), session.active_window_id.as_deref()?)))
            .collect();

        self.clients
            .values()
            .filter_map(|client| {
                let window_id = client.current_window_id.as_deref().or_else(|| {
                    active_window_by_session
                        .get(client.session_id.as_str())
                        .copied()
                })?;
                Some(WinlinkKey::new(
                    client.session_id.clone(),
                    window_id.to_owned(),
                ))
            })
            .collect()
    }

    pub fn window_count(&self) -> usize {
        self.winlinks.len()
    }

    pub fn active_alert_count(&self) -> usize {
        self.winlinks
            .values()
            .filter(|window| window.bell_flag)
            .count()
    }
}
