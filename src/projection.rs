use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    domain::{ClientName, ClientNode, DomainState, SessionNode, WindowState, WinlinkKey},
    model::TmuxState,
};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionState {
    pub tmux_socket_path: PathBuf,
    pub sessions: Vec<ProjectionSession>,
    pub clients: Vec<ProjectionClient>,
}

impl ProjectionState {
    pub fn empty(tmux_socket_path: impl Into<PathBuf>) -> Self {
        Self {
            tmux_socket_path: tmux_socket_path.into(),
            sessions: Vec::new(),
            clients: Vec::new(),
        }
    }

    pub fn from_tmux(tmux_socket_path: impl Into<PathBuf>, tmux_state: TmuxState) -> Self {
        let mut state = tmux_state.to_domain_state();
        state.tmux_socket_path = tmux_socket_path.into();
        Self::from_domain(&state)
    }

    pub fn from_domain(state: &DomainState) -> Self {
        Self {
            tmux_socket_path: state.tmux_socket_path.clone(),
            sessions: state
                .ordered_sessions()
                .into_iter()
                .map(|session| ProjectionSession {
                    id: session.id.clone(),
                    name: session.name.clone(),
                    attached_count: session.attached_count,
                    active_window_id: session.active_window_id.clone(),
                    windows: state
                        .session_windows(&session.id)
                        .into_iter()
                        .map(|(_, window)| ProjectionWindow {
                            id: window.id.clone(),
                            index: window.index,
                            name: window.name.clone(),
                            active: window.active,
                            activity: window.activity,
                            activity_flag: window.activity_flag,
                            bell_flag: window.bell_flag,
                            silence_flag: window.silence_flag,
                        })
                        .collect(),
                })
                .collect(),
            clients: state
                .ordered_clients()
                .into_iter()
                .map(|client| ProjectionClient {
                    name: client.name.0.clone(),
                    session_id: client.session_id.clone(),
                    current_window_id: client.current_window_id.clone(),
                    activity: client.activity,
                    tty: client.tty.clone(),
                })
                .collect(),
        }
    }

    pub fn to_domain_state(&self) -> DomainState {
        self.clone().into_domain_state()
    }

    pub fn into_domain_state(self) -> DomainState {
        let mut state = DomainState::empty(self.tmux_socket_path);

        for (order, session) in self.sessions.into_iter().enumerate() {
            let session_id = session.id.clone();
            state.sessions.insert(
                session_id.clone(),
                SessionNode {
                    id: session.id,
                    name: session.name,
                    attached_count: session.attached_count,
                    active_window_id: session.active_window_id,
                    order,
                },
            );

            for window in session.windows {
                state.winlinks.insert(
                    WinlinkKey::new(session_id.clone(), window.id.clone()),
                    WindowState {
                        id: window.id,
                        index: window.index,
                        name: window.name,
                        active: window.active,
                        activity: window.activity,
                        activity_flag: window.activity_flag,
                        bell_flag: window.bell_flag,
                        silence_flag: window.silence_flag,
                    },
                );
            }
        }

        for (order, client) in self.clients.into_iter().enumerate() {
            let name = ClientName(client.name);
            state.clients.insert(
                name.clone(),
                ClientNode {
                    name,
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

    pub fn into_tmux_state(self) -> TmuxState {
        TmuxState::from_domain_state(self.into_domain_state())
    }

    pub fn active_alert_count(&self) -> usize {
        self.sessions
            .iter()
            .flat_map(|session| &session.windows)
            .filter(|window| window.bell_flag)
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionSession {
    pub id: String,
    pub name: String,
    pub attached_count: u32,
    pub active_window_id: Option<String>,
    pub windows: Vec<ProjectionWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionWindow {
    pub id: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub activity: u64,
    pub activity_flag: bool,
    pub bell_flag: bool,
    pub silence_flag: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionClient {
    pub name: String,
    pub session_id: String,
    pub current_window_id: Option<String>,
    pub activity: u64,
    pub tty: String,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{ProjectionClient, ProjectionSession, ProjectionState, ProjectionWindow};
    use crate::domain::WinlinkKey;

    #[test]
    fn linked_windows_round_trip_as_session_local_winlinks() {
        let projection = ProjectionState {
            tmux_socket_path: Path::new("/socket").to_path_buf(),
            sessions: vec![
                ProjectionSession {
                    id: String::from("$1"),
                    name: String::from("current"),
                    attached_count: 1,
                    active_window_id: Some(String::from("@shared")),
                    windows: vec![ProjectionWindow {
                        id: String::from("@shared"),
                        index: 0,
                        name: String::from("shared"),
                        active: true,
                        activity: 10,
                        activity_flag: false,
                        bell_flag: false,
                        silence_flag: false,
                    }],
                },
                ProjectionSession {
                    id: String::from("$2"),
                    name: String::from("other"),
                    attached_count: 0,
                    active_window_id: Some(String::from("@20")),
                    windows: vec![
                        ProjectionWindow {
                            id: String::from("@20"),
                            index: 0,
                            name: String::from("own"),
                            active: true,
                            activity: 11,
                            activity_flag: false,
                            bell_flag: false,
                            silence_flag: false,
                        },
                        ProjectionWindow {
                            id: String::from("@shared"),
                            index: 5,
                            name: String::from("shared"),
                            active: false,
                            activity: 12,
                            activity_flag: false,
                            bell_flag: true,
                            silence_flag: false,
                        },
                    ],
                },
            ],
            clients: vec![ProjectionClient {
                name: String::from("client-1"),
                session_id: String::from("$1"),
                current_window_id: Some(String::from("@shared")),
                activity: 10,
                tty: String::from("/dev/pts/1"),
            }],
        };

        let domain = projection.clone().into_domain_state();

        assert!(
            domain
                .winlinks
                .contains_key(&WinlinkKey::new("$1", "@shared"))
        );
        assert!(
            domain
                .winlinks
                .contains_key(&WinlinkKey::new("$2", "@shared"))
        );
        assert_eq!(ProjectionState::from_domain(&domain), projection);
    }
}
