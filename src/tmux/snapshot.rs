use std::collections::{HashMap, HashSet};

use crate::model::{Client, ClientName, Session, TmuxState, Window, WindowAlert};

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
    pub id: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub activity: u64,
    pub flags: WindowFlags,
    pub alert_flags: AlertFlags,
}

impl SnapshotData {
    pub fn into_state(self) -> TmuxState {
        TmuxState {
            sessions: self
                .sessions
                .into_iter()
                .map(|session| Session {
                    id: session.id,
                    name: session.name,
                    attached_count: session
                        .attached_count
                        .max(session.attached_clients.len() as u32),
                    active_window_id: session.active_window_id,
                    windows: session
                        .windows
                        .into_iter()
                        .map(|window| {
                            let flags = window.flags.raw;
                            Window {
                                id: window.id,
                                index: window.index,
                                name: window.name,
                                active: window.active,
                                alert: WindowAlert::from_indicators(
                                    window.alert_flags.has_activity,
                                    window.alert_flags.has_bell,
                                    window.alert_flags.has_silence,
                                ),
                                flags,
                                activity: window.activity,
                            }
                        })
                        .collect(),
                })
                .collect(),
            clients: self
                .clients
                .into_iter()
                .map(|client| Client {
                    name: client.name,
                    session_id: client.session_id,
                    current_window_id: client.current_window_id,
                    activity: client.activity,
                    tty: client.tty,
                })
                .collect(),
        }
    }
}

pub fn collect_snapshot(socket: &SocketOptions) -> Result<TmuxState, TmuxError> {
    Ok(collect_snapshot_data(socket)?.into_state())
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

    use super::resolve_window_alerts;
    use crate::tmux::parse::{AlertFlags, WindowFlags};

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
}
