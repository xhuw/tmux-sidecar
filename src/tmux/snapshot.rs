use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
};

use crate::{
    domain::{
        ClientName, ClientNode, DomainState, SessionId, SessionNode, SessionState, WindowState,
        WinlinkKey,
    },
    model::TmuxState,
};

use super::{
    TmuxError,
    command::{self, SocketOptions},
    parse::{self, AlertFlags, ClientRecord, WindowFlags},
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotData {
    pub sessions: Vec<SnapshotSession>,
    pub clients: Vec<ClientRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotSession {
    pub id: String,
    pub name: String,
    pub attached_count: u32,
    pub active_window_id: Option<String>,
    pub attached_clients: Vec<ClientName>,
    pub alerts_by_index: HashMap<u32, AlertFlags>,
    pub windows: Vec<SnapshotWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotWindow {
    pub key: WinlinkKey,
    pub id: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub activity: u64,
    pub flags: WindowFlags,
    pub alert_flags: AlertFlags,
}

impl SnapshotData {
    pub fn into_domain_state(self, tmux_socket_path: impl Into<PathBuf>) -> DomainState {
        let mut state = DomainState::empty(tmux_socket_path);

        for (order, session) in self.sessions.into_iter().enumerate() {
            let session_id = session.id.clone();
            state.sessions.insert(
                session_id.clone(),
                SessionNode {
                    id: session.id,
                    name: session.name,
                    attached_count: session
                        .attached_count
                        .max(session.attached_clients.len() as u32),
                    active_window_id: session.active_window_id,
                    order,
                },
            );

            for window in session.windows {
                let key = window.key.clone();
                state.winlinks.insert(key, window.into_window_state());
            }
        }

        for (order, client) in self.clients.into_iter().enumerate() {
            state.clients.insert(
                client.name.clone(),
                ClientNode {
                    name: client.name,
                    session_id: client.session_id,
                    current_window_id: client.current_window_id,
                    activity: client.activity,
                    tty: client.tty,
                    order,
                },
            );
        }

        state
    }

    pub fn into_state(self) -> TmuxState {
        TmuxState::from_domain_state(self.into_domain_state(PathBuf::new()))
    }

    pub fn into_session_states(self) -> BTreeMap<SessionId, SessionState> {
        self.into_domain_state(PathBuf::new()).session_states()
    }
}

impl SnapshotSession {
    pub fn active_window_key(&self) -> Option<WinlinkKey> {
        self.active_window_id
            .as_ref()
            .map(|window_id| WinlinkKey::new(self.id.clone(), window_id.clone()))
    }
}

impl SnapshotWindow {
    pub fn into_window_state(self) -> WindowState {
        WindowState {
            id: self.id,
            index: self.index,
            name: self.name,
            active: self.active,
            activity: self.activity,
            activity_flag: self.alert_flags.has_activity,
            bell_flag: self.alert_flags.has_bell,
            silence_flag: self.alert_flags.has_silence,
        }
    }
}

pub fn collect_snapshot(socket: &SocketOptions) -> Result<TmuxState, TmuxError> {
    Ok(TmuxState::from_domain_state(collect_domain_state(socket)?))
}

pub fn collect_domain_state(socket: &SocketOptions) -> Result<DomainState, TmuxError> {
    Ok(collect_snapshot_data(socket)?.into_domain_state(socket.path.clone().unwrap_or_default()))
}

pub fn collect_snapshot_data(socket: &SocketOptions) -> Result<SnapshotData, TmuxError> {
    let sessions_output =
        command::run_tmux(socket, ["list-sessions", "-F", &parse::session_format()])?;
    let session_records =
        parse::parse_sessions(&sessions_output).map_err(|source| TmuxError::Parse {
            command: "list-sessions",
            source,
        })?;

    if session_records.is_empty() {
        return Err(TmuxError::NoSessions);
    }

    let windows_output = command::run_tmux(
        socket,
        ["list-windows", "-a", "-F", &parse::window_format()],
    )?;
    let window_records =
        parse::parse_windows(&windows_output).map_err(|source| TmuxError::Parse {
            command: "list-windows",
            source,
        })?;

    let clients = list_clients(socket)?;

    let mut sessions: Vec<SnapshotSession> = session_records
        .iter()
        .map(|session| SnapshotSession {
            id: session.id.clone(),
            name: session.name.clone(),
            attached_count: session.attached_count,
            active_window_id: session.active_window_id.clone(),
            attached_clients: Vec::new(),
            alerts_by_index: session
                .alerts
                .iter()
                .map(|alert| (alert.window_index, alert.flags))
                .collect(),
            windows: Vec::new(),
        })
        .collect();

    let session_indexes: HashMap<String, usize> = sessions
        .iter()
        .enumerate()
        .map(|(index, session)| (session.id.clone(), index))
        .collect();

    for window in window_records {
        let Some(session_index) = session_indexes.get(&window.session_id) else {
            return Err(TmuxError::SnapshotInvariant(format!(
                "window {} references unknown session {}",
                window.id, window.session_id
            )));
        };

        let session = &mut sessions[*session_index];
        if session.name.is_empty() && !window.session_name.is_empty() {
            session.name = window.session_name.clone();
        }

        if window.active {
            session.active_window_id = Some(window.id.clone());
        }

        let alert_flags =
            resolve_window_alerts(&window.flags, &session.alerts_by_index, window.index);
        session.windows.push(SnapshotWindow {
            key: WinlinkKey::new(session.id.clone(), window.id.clone()),
            id: window.id,
            index: window.index,
            name: window.name,
            active: window.active,
            activity: window.activity,
            flags: window.flags,
            alert_flags,
        });
    }

    for client in &clients {
        if let Some(session_index) = session_indexes.get(&client.session_id) {
            sessions[*session_index]
                .attached_clients
                .push(client.name.clone());
        }
    }

    for session in &mut sessions {
        session.windows.sort_by_key(|window| window.index);

        let mut seen = HashSet::new();
        session
            .attached_clients
            .retain(|client| seen.insert(client.0.clone()));
    }

    Ok(SnapshotData { sessions, clients })
}

fn resolve_window_alerts(
    window_flags: &WindowFlags,
    session_alerts: &HashMap<u32, AlertFlags>,
    window_index: u32,
) -> AlertFlags {
    window_flags.alerts.merge(
        session_alerts
            .get(&window_index)
            .copied()
            .unwrap_or_default(),
    )
}

pub fn list_clients(socket: &SocketOptions) -> Result<Vec<ClientRecord>, TmuxError> {
    let output = command::run_tmux(socket, ["list-clients", "-F", &parse::client_format()])?;
    parse::parse_clients(&output).map_err(|source| TmuxError::Parse {
        command: "list-clients",
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{SnapshotData, SnapshotSession, SnapshotWindow, resolve_window_alerts};
    use crate::{
        model::{SessionState, WinlinkKey},
        tmux::parse::{AlertFlags, WindowFlags},
    };

    #[test]
    fn resolve_window_alerts_keeps_explicit_window_alert_flags() {
        let alerts = resolve_window_alerts(
            &WindowFlags::from_parts(String::from("*"), true, false, false),
            &HashMap::new(),
            1,
        );

        assert!(alerts.has_activity);
    }

    #[test]
    fn resolve_window_alerts_uses_session_alerts_as_fallback() {
        let mut session_alerts = HashMap::new();
        session_alerts.insert(
            3,
            AlertFlags {
                has_activity: false,
                has_bell: true,
                has_silence: false,
            },
        );

        let alerts = resolve_window_alerts(
            &WindowFlags::from_parts(String::new(), false, false, false),
            &session_alerts,
            3,
        );

        assert!(alerts.has_bell);
    }

    #[test]
    fn snapshot_data_projects_linked_windows_by_session_local_key() {
        let session_states = SnapshotData {
            sessions: vec![
                SnapshotSession {
                    id: "$1".to_string(),
                    name: "current".to_string(),
                    attached_count: 1,
                    active_window_id: Some("@shared".to_string()),
                    attached_clients: vec![],
                    alerts_by_index: HashMap::new(),
                    windows: vec![snapshot_window(
                        "$1",
                        "@shared",
                        0,
                        true,
                        Default::default(),
                    )],
                },
                SnapshotSession {
                    id: "$2".to_string(),
                    name: "other".to_string(),
                    attached_count: 0,
                    active_window_id: Some("@20".to_string()),
                    attached_clients: vec![],
                    alerts_by_index: HashMap::new(),
                    windows: vec![
                        snapshot_window("$2", "@20", 0, true, Default::default()),
                        snapshot_window(
                            "$2",
                            "@shared",
                            5,
                            false,
                            AlertFlags {
                                has_activity: false,
                                has_bell: true,
                                has_silence: false,
                            },
                        ),
                    ],
                },
            ],
            clients: vec![],
        }
        .into_session_states();

        assert_eq!(
            session_states
                .get("$1")
                .and_then(SessionState::active_window_key),
            Some(WinlinkKey::new("$1", "@shared"))
        );
        assert_eq!(
            session_states
                .get("$2")
                .and_then(SessionState::active_window_key),
            Some(WinlinkKey::new("$2", "@20"))
        );

        let current_key = WinlinkKey::new("$1", "@shared");
        let alerted_key = WinlinkKey::new("$2", "@shared");
        let current_window = session_states
            .get("$1")
            .and_then(|session| session.windows.get(&current_key))
            .expect("expected current-session linked window");
        let alerted_window = session_states
            .get("$2")
            .and_then(|session| session.windows.get(&alerted_key))
            .expect("expected other-session linked window");

        assert!(current_window.active);
        assert!(!current_window.bell_flag);
        assert!(!alerted_window.active);
        assert!(alerted_window.bell_flag);
        assert_eq!(session_states["$2"].windows.len(), 2);
        assert_eq!(
            session_states["$2"]
                .windows
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                WinlinkKey::new("$2", "@20"),
                WinlinkKey::new("$2", "@shared")
            ]
        );
    }

    fn snapshot_window(
        session_id: &str,
        id: &str,
        index: u32,
        active: bool,
        alert_flags: AlertFlags,
    ) -> SnapshotWindow {
        SnapshotWindow {
            key: WinlinkKey::new(session_id, id),
            id: id.to_string(),
            index,
            name: id.to_string(),
            active,
            activity: 0,
            flags: WindowFlags::default(),
            alert_flags,
        }
    }
}
