use std::{
    env,
    io::{self, BufRead, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use clap::ValueEnum;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub use crate::projection::{
    ProjectionClient, ProjectionSession, ProjectionState, ProjectionWindow,
};

/// Runtime protocol stays on v2 while the rewrite stages the public CLI first.
///
/// The intended clean-break follow-up keeps newline-delimited JSON and local Unix
/// sockets, but renames the envelopes around explicit client/server request,
/// query, snapshot, and problem message families. See `ARCHITECTURE.md` for the
/// target protocol direction.
pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum ClientKind {
    Ui,
    Hook,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
#[value(rename_all = "kebab-case")]
pub enum HookName {
    SessionCreated,
    SessionClosed,
    SessionRenamed,
    SessionWindowChanged,
    WindowLinked,
    WindowUnlinked,
    WindowRenamed,
    WindowPaneChanged,
    WindowLayoutChanged,
    AlertActivity,
    AlertBell,
    AlertSilence,
    ClientAttached,
    ClientDetached,
    ClientSessionChanged,
    AfterNewSession,
    AfterNewWindow,
    AfterRenameSession,
    AfterRenameWindow,
    AfterKillPane,
    AfterSelectWindow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub client_kind: ClientKind,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloAck {
    pub protocol_version: u32,
    pub server_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEvent {
    /// Current shell-safe hook payload.
    ///
    /// The rewrite direction is to narrow this into `HookNotify` plus a typed
    /// hook scope that carries only stable IDs, indexes, paths, client names,
    /// and timestamps—never shell-expanded session or window names.
    pub tmux_socket_path: PathBuf,
    pub event: HookName,
    pub session_id: Option<String>,
    pub window_id: Option<String>,
    pub window_index: Option<u32>,
    pub pane_id: Option<String>,
    pub pane_current_path: Option<PathBuf>,
    pub client_name: Option<String>,
    pub timestamp_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscribe {
    pub target_client: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRequest {
    pub request_id: String,
    pub target_client: Option<String>,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "action")]
pub enum Action {
    SwitchSession {
        session_id: String,
    },
    SwitchWindow {
        session_id: String,
        window_id: String,
    },
    CreateSession {
        name: Option<String>,
    },
    CreateWindow {
        session_id: String,
        name: Option<String>,
    },
    RenameSession {
        session_id: String,
        name: String,
    },
    RenameWindow {
        window_id: String,
        name: String,
    },
    CloseSession {
        session_id: String,
    },
    CloseWindow {
        session_id: String,
        window_id: String,
    },
}

static NEXT_ACTION_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

impl ActionRequest {
    pub fn new(target_client: Option<String>, action: Action) -> Self {
        let request_id = NEXT_ACTION_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            request_id: format!("req-{request_id}"),
            target_client,
            action,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    pub request_id: String,
    /// Server projection generation after the action's reconcile attempt.
    pub generation: u64,
    pub result: ActionResultKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "status")]
pub enum ActionResultKind {
    Ok { outcome: Option<ActionOutcome> },
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "outcome")]
pub enum ActionOutcome {
    CreatedSession {
        session_id: String,
    },
    CreatedWindow {
        session_id: String,
        window_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateUpdated {
    pub generation: u64,
    pub state: ProjectionState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub kind: AckKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AckKind {
    HookEvent,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type", content = "payload")]
pub enum ClientMessage {
    /// Current v2 client-to-server message envelope.
    ///
    /// A later clean-break protocol can rename these variants without changing
    /// the current staged runtime behavior.
    Hello(Hello),
    HookEvent(HookEvent),
    Subscribe(Subscribe),
    ActionRequest(ActionRequest),
    SnapshotRequest,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type", content = "payload")]
pub enum ServerMessage {
    /// Current v2 server-to-client message envelope.
    ///
    /// The rewrite direction is to split subscription snapshots, action/query
    /// replies, and problems into more explicit result types.
    HelloAck(HelloAck),
    Ack(Ack),
    StateUpdated(StateUpdated),
    ActionResult(ActionResult),
    Error(ErrorMessage),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarPaths {
    pub runtime_dir: PathBuf,
    pub socket_path: PathBuf,
    pub lock_path: PathBuf,
    pub pid_path: PathBuf,
}

impl SidecarPaths {
    pub fn from_tmux_socket_path(tmux_socket_path: &Path) -> Self {
        let runtime_root = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        Self::from_runtime_dir(tmux_socket_path, runtime_root.as_deref())
    }

    pub fn from_runtime_dir(tmux_socket_path: &Path, runtime_root: Option<&Path>) -> Self {
        let runtime_dir = runtime_root
            .map(|path| path.join("tmux-sidecar"))
            .unwrap_or_else(|| {
                tmux_socket_path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join("tmux-sidecar")
            });
        let digest = stable_path_digest(tmux_socket_path);
        let stem = format!("{digest:016x}");

        Self {
            runtime_dir: runtime_dir.clone(),
            socket_path: runtime_dir.join(format!("{stem}.sock")),
            lock_path: runtime_dir.join(format!("{stem}.lock")),
            pid_path: runtime_dir.join(format!("{stem}.pid")),
        }
    }
}

pub fn write_message<T: Serialize>(writer: &mut impl Write, message: &T) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, message)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub fn read_message<T: DeserializeOwned>(reader: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            return Ok(None);
        }

        if line.trim().is_empty() {
            continue;
        }

        let message = serde_json::from_str(line.trim_end())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        return Ok(Some(message));
    }
}

fn stable_path_digest(tmux_socket_path: &Path) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    tmux_socket_path
        .as_os_str()
        .as_bytes()
        .iter()
        .fold(FNV_OFFSET, |digest, byte| {
            (digest ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
        })
}

#[cfg(test)]
mod tests {
    use std::{io::Cursor, path::Path};

    use super::{
        Action, ActionRequest, ClientKind, ClientMessage, HookEvent, HookName, SidecarPaths,
        read_message, write_message,
    };

    #[test]
    fn client_messages_round_trip_as_newline_delimited_json() {
        let message = ClientMessage::ActionRequest(ActionRequest {
            request_id: String::from("req-1"),
            target_client: Some(String::from("client-1")),
            action: Action::SwitchWindow {
                session_id: String::from("$1"),
                window_id: String::from("@2"),
            },
        });
        let mut encoded = Vec::new();

        write_message(&mut encoded, &message).expect("encode protocol message");
        let decoded: ClientMessage = read_message(&mut Cursor::new(encoded))
            .expect("decode protocol message")
            .expect("message present");

        assert_eq!(decoded, message);
    }

    #[test]
    fn hook_events_round_trip_with_path_payloads() {
        let message = ClientMessage::HookEvent(HookEvent {
            tmux_socket_path: Path::new("/tmux/default.sock").to_path_buf(),
            event: HookName::AlertBell,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@2")),
            window_index: Some(3),
            pane_id: Some(String::from("%4")),
            pane_current_path: Some(Path::new("/tmp/worktree").to_path_buf()),
            client_name: Some(String::from("client-1")),
            timestamp_ms: Some(1234),
        });
        let mut encoded = Vec::new();

        write_message(&mut encoded, &message).expect("encode hook event");
        let decoded: ClientMessage = read_message(&mut Cursor::new(encoded))
            .expect("decode hook event")
            .expect("message present");

        assert_eq!(decoded, message);
    }

    #[test]
    fn sidecar_paths_are_deterministic_from_tmux_socket_path() {
        let tmux_socket = Path::new("/private/tmux-1000/default");
        let first = SidecarPaths::from_runtime_dir(tmux_socket, Some(Path::new("/run/user/1000")));
        let second = SidecarPaths::from_runtime_dir(tmux_socket, Some(Path::new("/run/user/2000")));
        let fallback = SidecarPaths::from_runtime_dir(tmux_socket, None);

        assert_eq!(
            first.socket_path.file_name(),
            second.socket_path.file_name(),
            "hash stem should depend only on the tmux socket path"
        );
        assert_eq!(first.lock_path.file_name(), second.lock_path.file_name());
        assert_eq!(first.pid_path.file_name(), second.pid_path.file_name());
        assert_eq!(
            first.runtime_dir,
            Path::new("/run/user/1000").join("tmux-sidecar")
        );
        assert_eq!(
            fallback.runtime_dir,
            Path::new("/private/tmux-1000").join("tmux-sidecar")
        );
    }

    #[test]
    fn client_kind_serializes_with_kebab_case_names() {
        let encoded = serde_json::to_string(&ClientKind::Control).expect("serialize client kind");
        assert_eq!(encoded, "\"control\"");
    }

    #[test]
    fn projection_state_counts_active_bell_alerts() {
        let state = super::ProjectionState {
            tmux_socket_path: Path::new("/tmp/tmux/default").to_path_buf(),
            sessions: vec![
                super::ProjectionSession {
                    id: String::from("$1"),
                    name: String::from("work"),
                    attached_count: 1,
                    active_window_id: Some(String::from("@1")),
                    windows: vec![
                        super::ProjectionWindow {
                            id: String::from("@1"),
                            index: 0,
                            name: String::from("shell"),
                            active: true,
                            activity: 0,
                            activity_flag: true,
                            bell_flag: false,
                            silence_flag: false,
                        },
                        super::ProjectionWindow {
                            id: String::from("@2"),
                            index: 1,
                            name: String::from("tests"),
                            active: false,
                            activity: 0,
                            activity_flag: false,
                            bell_flag: true,
                            silence_flag: false,
                        },
                    ],
                },
                super::ProjectionSession {
                    id: String::from("$2"),
                    name: String::from("notes"),
                    attached_count: 0,
                    active_window_id: Some(String::from("@3")),
                    windows: vec![super::ProjectionWindow {
                        id: String::from("@3"),
                        index: 0,
                        name: String::from("scratch"),
                        active: false,
                        activity: 0,
                        activity_flag: false,
                        bell_flag: true,
                        silence_flag: true,
                    }],
                },
            ],
            clients: Vec::new(),
        };

        assert_eq!(state.active_alert_count(), 2);
    }
}
