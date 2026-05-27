use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::BufReader,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};

use crate::{
    ipc::{
        Ack, AckKind, Action, ActionOutcome, ActionRequest, ActionResult, ActionResultKind,
        ClientMessage, ErrorMessage, HelloAck, HookEvent, HookName, ProjectionSession,
        ProjectionState, ProjectionWindow, ServerMessage, SidecarPaths, StateUpdated,
    },
    model::WindowTarget,
    tmux::{Tmux, TmuxCli},
};

const IPC_WRITE_TIMEOUT: Duration = Duration::from_millis(100);
const ACTION_REFRESH_ATTEMPTS: usize = 10;
const ACTION_REFRESH_RETRY_DELAY: Duration = Duration::from_millis(25);
const HOOK_REFRESH_ATTEMPTS: usize = 6;
const HOOK_REFRESH_SETTLE_DELAY: Duration = Duration::from_millis(50);
const HOOK_REFRESH_RETRY_DELAY: Duration = Duration::from_millis(25);

pub struct ServerOptions {
    pub tmux_socket_path: PathBuf,
}

pub fn run(options: ServerOptions) -> Result<()> {
    let tmux_socket_path = options.tmux_socket_path;
    let tmux = Arc::new(LiveServerTmuxOps);
    let initial_state = tmux.snapshot_projection(&tmux_socket_path)?;
    let sidecar_paths = SidecarPaths::from_tmux_socket_path(&tmux_socket_path);

    Server::bind(sidecar_paths, tmux_socket_path, initial_state, true)?.run()
}

type SharedWriter = Arc<Mutex<UnixStream>>;

struct SharedState {
    server_id: String,
    tmux_socket_path: PathBuf,
    cache_path: PathBuf,
    generation: u64,
    state: ProjectionState,
    subscribers: BTreeMap<u64, SharedWriter>,
    next_subscriber_id: u64,
}

struct Server {
    listener: UnixListener,
    shared: Arc<Mutex<SharedState>>,
    refresh_lock: Arc<Mutex<()>>,
    shutdown: Arc<AtomicBool>,
    refresh_from_tmux: bool,
    tmux: Arc<dyn ServerTmuxOps>,
    cleanup: CleanupPaths,
}

struct CleanupPaths {
    socket_path: PathBuf,
    pid_path: PathBuf,
}

trait ServerTmuxOps: Send + Sync {
    fn snapshot_projection(&self, tmux_socket_path: &Path) -> Result<ProjectionState>;
    fn execute_action(
        &self,
        tmux_socket_path: &Path,
        request: &ActionRequest,
    ) -> Result<ActionEffect>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ActionEffect {
    SwitchedSession {
        client_name: String,
        session_id: String,
    },
    SwitchedWindow {
        client_name: String,
        session_id: String,
        window_id: String,
    },
    CreatedSession {
        session_id: String,
    },
    CreatedWindow {
        session_id: String,
        window_id: String,
    },
    RenamedSession {
        session_id: String,
        name: String,
    },
    RenamedWindow {
        window_id: String,
        name: String,
    },
    ClosedSession {
        session_id: String,
    },
    ClosedWindow {
        session_id: String,
        window_id: String,
    },
}

#[derive(Debug, Default)]
struct LiveServerTmuxOps;

impl ServerTmuxOps for LiveServerTmuxOps {
    fn snapshot_projection(&self, tmux_socket_path: &Path) -> Result<ProjectionState> {
        let tmux = tmux_client(tmux_socket_path);
        let snapshot = tmux.snapshot().context("failed to snapshot tmux state")?;
        Ok(ProjectionState::from_tmux(
            tmux_socket_path.to_path_buf(),
            snapshot,
        ))
    }

    fn execute_action(
        &self,
        tmux_socket_path: &Path,
        request: &ActionRequest,
    ) -> Result<ActionEffect> {
        execute_action_request(&tmux_client(tmux_socket_path), request)
    }
}

impl Drop for CleanupPaths {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_file(&self.pid_path);
    }
}

impl Server {
    fn bind(
        sidecar_paths: SidecarPaths,
        tmux_socket_path: PathBuf,
        initial_state: ProjectionState,
        refresh_from_tmux: bool,
    ) -> Result<Self> {
        Self::bind_with_tmux(
            sidecar_paths,
            tmux_socket_path,
            initial_state,
            refresh_from_tmux,
            Arc::new(LiveServerTmuxOps),
        )
    }

    fn bind_with_tmux(
        sidecar_paths: SidecarPaths,
        tmux_socket_path: PathBuf,
        initial_state: ProjectionState,
        refresh_from_tmux: bool,
        tmux: Arc<dyn ServerTmuxOps>,
    ) -> Result<Self> {
        fs::create_dir_all(&sidecar_paths.runtime_dir)
            .context("failed to create sidecar runtime dir")?;
        if sidecar_paths.socket_path.exists() {
            fs::remove_file(&sidecar_paths.socket_path).with_context(|| {
                format!(
                    "failed to remove existing sidecar socket `{}`",
                    sidecar_paths.socket_path.display()
                )
            })?;
        }

        let listener = UnixListener::bind(&sidecar_paths.socket_path).with_context(|| {
            format!(
                "failed to bind sidecar socket `{}`",
                sidecar_paths.socket_path.display()
            )
        })?;
        listener
            .set_nonblocking(true)
            .context("failed to configure sidecar listener")?;
        fs::write(&sidecar_paths.pid_path, format!("{}\n", std::process::id()))
            .context("failed to write sidecar pid file")?;
        store_state_cache(&sidecar_paths.cache_path, &initial_state);

        Ok(Self {
            listener,
            shared: Arc::new(Mutex::new(SharedState {
                server_id: format!("tmux-sidecar-{}", std::process::id()),
                tmux_socket_path,
                cache_path: sidecar_paths.cache_path,
                generation: 1,
                state: initial_state,
                subscribers: BTreeMap::new(),
                next_subscriber_id: 1,
            })),
            refresh_lock: Arc::new(Mutex::new(())),
            shutdown: Arc::new(AtomicBool::new(false)),
            refresh_from_tmux,
            tmux,
            cleanup: CleanupPaths {
                socket_path: sidecar_paths.socket_path,
                pid_path: sidecar_paths.pid_path,
            },
        })
    }

    fn run(self) -> Result<()> {
        while !self.shutdown.load(Ordering::SeqCst) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let shared = Arc::clone(&self.shared);
                    let refresh_lock = Arc::clone(&self.refresh_lock);
                    let shutdown = Arc::clone(&self.shutdown);
                    let refresh_from_tmux = self.refresh_from_tmux;
                    let tmux = Arc::clone(&self.tmux);
                    thread::spawn(move || {
                        let _ = handle_connection(
                            stream,
                            shared,
                            refresh_lock,
                            shutdown,
                            refresh_from_tmux,
                            tmux,
                        );
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error).context("failed while accepting sidecar client"),
            }
        }

        drop(self.cleanup);
        Ok(())
    }
}

fn handle_connection(
    stream: UnixStream,
    shared: Arc<Mutex<SharedState>>,
    refresh_lock: Arc<Mutex<()>>,
    shutdown: Arc<AtomicBool>,
    refresh_from_tmux: bool,
    tmux: Arc<dyn ServerTmuxOps>,
) -> Result<()> {
    stream
        .set_write_timeout(Some(IPC_WRITE_TIMEOUT))
        .context("failed to configure IPC write timeout")?;
    let reader_stream = stream.try_clone().context("failed to clone IPC stream")?;
    let writer = Arc::new(Mutex::new(stream));
    let mut reader = BufReader::new(reader_stream);
    let mut hello_complete = false;
    let mut subscriber_id = None;

    while let Some(message) = crate::ipc::read_message(&mut reader)? {
        if !hello_complete {
            match message {
                ClientMessage::Hello(_hello) => {
                    let server_id = {
                        let guard = shared.lock().expect("shared state poisoned");
                        guard.server_id.clone()
                    };
                    send_message(
                        &writer,
                        &ServerMessage::HelloAck(HelloAck {
                            protocol_version: crate::ipc::PROTOCOL_VERSION,
                            server_id,
                        }),
                    )?;
                    hello_complete = true;
                }
                _ => {
                    send_error(&writer, "expected hello before any other IPC message")?;
                    break;
                }
            }
            continue;
        }

        match message {
            ClientMessage::Hello(_) => {
                send_error(&writer, "duplicate hello is not allowed")?;
            }
            ClientMessage::HookEvent(event) => {
                let expected_tmux_socket = {
                    let guard = shared.lock().expect("shared state poisoned");
                    guard.tmux_socket_path.clone()
                };
                if event.tmux_socket_path != expected_tmux_socket {
                    send_error(
                        &writer,
                        "hook event tmux socket path did not match this server",
                    )?;
                    continue;
                }

                refresh_state_for_hook(
                    &shared,
                    &refresh_lock,
                    refresh_from_tmux,
                    tmux.as_ref(),
                    &event,
                )?;
                send_message(
                    &writer,
                    &ServerMessage::Ack(Ack {
                        kind: AckKind::HookEvent,
                    }),
                )?;
            }
            ClientMessage::Subscribe(_) => {
                if subscriber_id.is_none() {
                    subscriber_id = Some(register_subscriber(&shared, Arc::clone(&writer)));
                }
                send_message(
                    &writer,
                    &ServerMessage::StateUpdated(current_state_update(&shared)),
                )?;
            }
            ClientMessage::SnapshotRequest => {
                match refresh_state_for_snapshot_request(
                    &shared,
                    &refresh_lock,
                    refresh_from_tmux,
                    tmux.as_ref(),
                ) {
                    Ok(update) => {
                        send_message(&writer, &ServerMessage::StateUpdated(update))?;
                    }
                    Err(error) => {
                        send_error(&writer, format!("failed to refresh state: {error:#}"))?;
                    }
                }
            }
            ClientMessage::ActionRequest(request) => {
                let request_id = request.request_id.clone();
                let result = handle_action_request(
                    &shared,
                    &refresh_lock,
                    refresh_from_tmux,
                    tmux.as_ref(),
                    &request,
                );
                send_message(
                    &writer,
                    &ServerMessage::ActionResult(ActionResult { request_id, result }),
                )?;
            }
            ClientMessage::Shutdown => {
                shutdown.store(true, Ordering::SeqCst);
                send_message(
                    &writer,
                    &ServerMessage::Ack(Ack {
                        kind: AckKind::Shutdown,
                    }),
                )?;
                break;
            }
        }
    }

    if let Some(subscriber_id) = subscriber_id {
        let mut guard = shared.lock().expect("shared state poisoned");
        guard.subscribers.remove(&subscriber_id);
    }

    Ok(())
}

fn register_subscriber(shared: &Arc<Mutex<SharedState>>, writer: SharedWriter) -> u64 {
    let mut guard = shared.lock().expect("shared state poisoned");
    let subscriber_id = guard.next_subscriber_id;
    guard.next_subscriber_id += 1;
    guard.subscribers.insert(subscriber_id, writer);
    subscriber_id
}

fn current_state_update(shared: &Arc<Mutex<SharedState>>) -> StateUpdated {
    let guard = shared.lock().expect("shared state poisoned");
    StateUpdated {
        generation: guard.generation,
        state: guard.state.clone(),
    }
}

fn refresh_state(
    shared: &Arc<Mutex<SharedState>>,
    refresh_lock: &Arc<Mutex<()>>,
    refresh_from_tmux: bool,
    tmux: &dyn ServerTmuxOps,
) -> Result<StateUpdated> {
    refresh_state_with_policy(
        shared,
        refresh_lock,
        refresh_from_tmux,
        tmux,
        RefreshPolicy::Immediate,
    )
}

fn refresh_state_for_hook(
    shared: &Arc<Mutex<SharedState>>,
    refresh_lock: &Arc<Mutex<()>>,
    refresh_from_tmux: bool,
    tmux: &dyn ServerTmuxOps,
    event: &HookEvent,
) -> Result<StateUpdated> {
    refresh_state_with_policy(
        shared,
        refresh_lock,
        refresh_from_tmux,
        tmux,
        RefreshPolicy::Hook(event),
    )
}

fn refresh_state_for_action(
    shared: &Arc<Mutex<SharedState>>,
    refresh_lock: &Arc<Mutex<()>>,
    refresh_from_tmux: bool,
    tmux: &dyn ServerTmuxOps,
    effect: &ActionEffect,
) -> Result<StateUpdated> {
    refresh_state_with_policy(
        shared,
        refresh_lock,
        refresh_from_tmux,
        tmux,
        RefreshPolicy::Action(effect),
    )
}

fn refresh_state_for_snapshot_request(
    shared: &Arc<Mutex<SharedState>>,
    refresh_lock: &Arc<Mutex<()>>,
    refresh_from_tmux: bool,
    tmux: &dyn ServerTmuxOps,
) -> Result<StateUpdated> {
    let _refresh_guard = refresh_lock.lock().expect("refresh lock poisoned");
    let (tmux_socket_path, previous_state) = {
        let guard = shared.lock().expect("shared state poisoned");
        (guard.tmux_socket_path.clone(), guard.state.clone())
    };

    let mut next_state = if refresh_from_tmux {
        tmux.snapshot_projection(&tmux_socket_path)?
    } else {
        previous_state.clone()
    };
    preserve_cached_bell_flags(&previous_state, &mut next_state, None);

    if next_state == previous_state {
        return Ok(current_state_update(shared));
    }

    Ok(publish_state_update(shared, next_state, false))
}

enum RefreshPolicy<'a> {
    Immediate,
    Hook(&'a HookEvent),
    Action(&'a ActionEffect),
}

fn refresh_state_with_policy(
    shared: &Arc<Mutex<SharedState>>,
    refresh_lock: &Arc<Mutex<()>>,
    refresh_from_tmux: bool,
    tmux: &dyn ServerTmuxOps,
    policy: RefreshPolicy<'_>,
) -> Result<StateUpdated> {
    let _refresh_guard = refresh_lock.lock().expect("refresh lock poisoned");
    let (tmux_socket_path, previous_state) = {
        let guard = shared.lock().expect("shared state poisoned");
        (guard.tmux_socket_path.clone(), guard.state.clone())
    };

    let attempts = if refresh_from_tmux {
        policy.max_attempts()
    } else {
        1
    };
    let mut last_state = previous_state.clone();
    let mut last_satisfied = false;

    for attempt in 0..attempts {
        if refresh_from_tmux {
            if attempt == 0 {
                if let Some(delay) = policy.initial_delay() {
                    thread::sleep(delay);
                }
            } else if let Some(delay) = policy.retry_delay() {
                thread::sleep(delay);
            }
        }

        let mut next_state = if refresh_from_tmux {
            tmux.snapshot_projection(&tmux_socket_path)?
        } else {
            previous_state.clone()
        };
        let hook_event = policy.hook_event();
        preserve_cached_bell_flags(&previous_state, &mut next_state, hook_event);
        if let Some(event) = hook_event {
            apply_hook_event_overlay(&mut next_state, event);
        }

        last_satisfied = policy.is_satisfied(&previous_state, &next_state);
        last_state = next_state;
        if last_satisfied || attempt + 1 == attempts {
            break;
        }
    }

    let update = publish_state_update(shared, last_state, true);
    if let RefreshPolicy::Action(effect) = policy {
        if !last_satisfied {
            anyhow::bail!("{}", effect.unsatisfied_message());
        }
    }

    Ok(update)
}

fn publish_state_update(
    shared: &Arc<Mutex<SharedState>>,
    next_state: ProjectionState,
    broadcast_update: bool,
) -> StateUpdated {
    let (cache_path, update) = {
        let mut guard = shared.lock().expect("shared state poisoned");
        guard.generation += 1;
        guard.state = next_state;
        (
            guard.cache_path.clone(),
            StateUpdated {
                generation: guard.generation,
                state: guard.state.clone(),
            },
        )
    };
    store_state_cache(&cache_path, &update.state);

    if broadcast_update {
        broadcast(shared, ServerMessage::StateUpdated(update.clone()));
    }
    update
}

impl RefreshPolicy<'_> {
    fn max_attempts(&self) -> usize {
        match self {
            RefreshPolicy::Immediate => 1,
            RefreshPolicy::Hook(event) if hook_may_change_projection(event) => {
                HOOK_REFRESH_ATTEMPTS
            }
            RefreshPolicy::Hook(_) => 1,
            RefreshPolicy::Action(_) => ACTION_REFRESH_ATTEMPTS,
        }
    }

    fn initial_delay(&self) -> Option<Duration> {
        match self {
            RefreshPolicy::Hook(event) if hook_may_change_projection(event) => {
                Some(HOOK_REFRESH_SETTLE_DELAY)
            }
            RefreshPolicy::Immediate | RefreshPolicy::Action(_) => None,
            RefreshPolicy::Hook(_) => None,
        }
    }

    fn retry_delay(&self) -> Option<Duration> {
        match self {
            RefreshPolicy::Hook(event) if hook_may_change_projection(event) => {
                Some(HOOK_REFRESH_RETRY_DELAY)
            }
            RefreshPolicy::Action(_) => Some(ACTION_REFRESH_RETRY_DELAY),
            RefreshPolicy::Immediate | RefreshPolicy::Hook(_) => None,
        }
    }

    fn hook_event(&self) -> Option<&HookEvent> {
        match self {
            RefreshPolicy::Hook(event) => Some(event),
            RefreshPolicy::Immediate | RefreshPolicy::Action(_) => None,
        }
    }

    fn is_satisfied(&self, previous_state: &ProjectionState, next_state: &ProjectionState) -> bool {
        match self {
            RefreshPolicy::Immediate => true,
            RefreshPolicy::Hook(event) => {
                !hook_may_change_projection(event)
                    || hook_refresh_satisfied(event, previous_state, next_state)
            }
            RefreshPolicy::Action(effect) => effect.is_satisfied(next_state),
        }
    }
}

fn hook_may_change_projection(event: &HookEvent) -> bool {
    matches!(
        event.event,
        HookName::SessionCreated
            | HookName::SessionClosed
            | HookName::WindowLinked
            | HookName::WindowUnlinked
            | HookName::AfterNewSession
            | HookName::AfterNewWindow
            | HookName::AfterKillPane
    )
}

fn hook_refresh_satisfied(
    event: &HookEvent,
    previous_state: &ProjectionState,
    next_state: &ProjectionState,
) -> bool {
    match event.event {
        HookName::SessionCreated | HookName::AfterNewSession => {
            next_state.sessions.len() > previous_state.sessions.len()
        }
        HookName::SessionClosed => next_state.sessions.len() < previous_state.sessions.len(),
        HookName::WindowLinked | HookName::AfterNewWindow => {
            projection_window_count(next_state) > projection_window_count(previous_state)
        }
        HookName::WindowUnlinked => {
            projection_window_count(next_state) < projection_window_count(previous_state)
                || next_state.sessions.len() < previous_state.sessions.len()
        }
        HookName::AfterKillPane => {
            projection_window_count(next_state) < projection_window_count(previous_state)
                || next_state.sessions.len() < previous_state.sessions.len()
                || next_state != previous_state
        }
        _ => true,
    }
}

fn projection_window_count(state: &ProjectionState) -> usize {
    state
        .sessions
        .iter()
        .map(|session| session.windows.len())
        .sum()
}

impl ActionEffect {
    fn outcome(&self) -> Option<ActionOutcome> {
        match self {
            ActionEffect::CreatedSession { session_id } => Some(ActionOutcome::CreatedSession {
                session_id: session_id.clone(),
            }),
            ActionEffect::CreatedWindow {
                session_id,
                window_id,
            } => Some(ActionOutcome::CreatedWindow {
                session_id: session_id.clone(),
                window_id: window_id.clone(),
            }),
            ActionEffect::SwitchedSession { .. }
            | ActionEffect::SwitchedWindow { .. }
            | ActionEffect::RenamedSession { .. }
            | ActionEffect::RenamedWindow { .. }
            | ActionEffect::ClosedSession { .. }
            | ActionEffect::ClosedWindow { .. } => None,
        }
    }

    fn is_satisfied(&self, state: &ProjectionState) -> bool {
        match self {
            ActionEffect::SwitchedSession {
                client_name,
                session_id,
            } => state
                .clients
                .iter()
                .any(|client| client.name == *client_name && client.session_id == *session_id),
            ActionEffect::SwitchedWindow {
                client_name,
                session_id,
                window_id,
            } => state.clients.iter().any(|client| {
                client.name == *client_name
                    && client.session_id == *session_id
                    && client.current_window_id.as_deref() == Some(window_id.as_str())
            }),
            ActionEffect::CreatedSession { session_id } => state
                .sessions
                .iter()
                .any(|session| session.id == *session_id),
            ActionEffect::CreatedWindow {
                session_id,
                window_id,
            } => projection_session(state, session_id)
                .map(|session| session.windows.iter().any(|window| window.id == *window_id))
                .unwrap_or(false),
            ActionEffect::RenamedSession { session_id, name } => {
                projection_session(state, session_id)
                    .map(|session| session.name == *name)
                    .unwrap_or(false)
            }
            ActionEffect::RenamedWindow { window_id, name } => state
                .sessions
                .iter()
                .flat_map(|session| session.windows.iter())
                .any(|window| window.id == *window_id && window.name == *name),
            ActionEffect::ClosedSession { session_id } => {
                projection_session(state, session_id).is_none()
            }
            ActionEffect::ClosedWindow {
                session_id,
                window_id,
            } => projection_session(state, session_id)
                .map(|session| !session.windows.iter().any(|window| window.id == *window_id))
                .unwrap_or(true),
        }
    }

    fn unsatisfied_message(&self) -> String {
        match self {
            ActionEffect::SwitchedSession {
                client_name,
                session_id,
            } => format!(
                "refreshed state did not show client `{client_name}` in session `{session_id}`"
            ),
            ActionEffect::SwitchedWindow {
                client_name,
                session_id,
                window_id,
            } => format!(
                "refreshed state did not show client `{client_name}` on window `{window_id}` in session `{session_id}`"
            ),
            ActionEffect::CreatedSession { session_id } => {
                format!("refreshed state did not include created session `{session_id}`")
            }
            ActionEffect::CreatedWindow {
                session_id,
                window_id,
            } => format!(
                "refreshed state did not include created window `{window_id}` in session `{session_id}`"
            ),
            ActionEffect::RenamedSession { session_id, name } => {
                format!("refreshed state did not show session `{session_id}` renamed to `{name}`")
            }
            ActionEffect::RenamedWindow { window_id, name } => {
                format!("refreshed state did not show window `{window_id}` renamed to `{name}`")
            }
            ActionEffect::ClosedSession { session_id } => {
                format!("refreshed state still included closed session `{session_id}`")
            }
            ActionEffect::ClosedWindow {
                session_id,
                window_id,
            } => format!(
                "refreshed state still included closed window `{window_id}` in session `{session_id}`"
            ),
        }
    }
}

fn projection_session<'a>(
    state: &'a ProjectionState,
    session_id: &str,
) -> Option<&'a ProjectionSession> {
    state
        .sessions
        .iter()
        .find(|session| session.id == session_id)
}

fn preserve_cached_bell_flags(
    previous_state: &ProjectionState,
    next_state: &mut ProjectionState,
    hook_event: Option<&HookEvent>,
) {
    let viewed_windows = viewed_projection_windows(next_state);

    for previous_session in &previous_state.sessions {
        let Some(next_session) = next_state
            .sessions
            .iter_mut()
            .find(|session| session.id == previous_session.id)
        else {
            continue;
        };

        for previous_window in previous_session
            .windows
            .iter()
            .filter(|window| window.bell_flag)
        {
            let Some(next_window) = next_session
                .windows
                .iter_mut()
                .find(|window| window.id == previous_window.id)
            else {
                continue;
            };

            if next_window.bell_flag
                || viewed_windows
                    .contains(&(previous_session.id.clone(), previous_window.id.clone()))
                || hook_event_clears_cached_bell(
                    hook_event,
                    &previous_session.id,
                    &previous_window.id,
                    previous_window.index,
                )
            {
                continue;
            }

            next_window.bell_flag = true;
        }
    }
}

fn viewed_projection_windows(state: &ProjectionState) -> BTreeSet<(String, String)> {
    let active_window_by_session: BTreeMap<&str, &str> = state
        .sessions
        .iter()
        .filter_map(|session| Some((session.id.as_str(), session.active_window_id.as_deref()?)))
        .collect();

    state
        .clients
        .iter()
        .filter_map(|client| {
            let window_id = client.current_window_id.as_deref().or_else(|| {
                active_window_by_session
                    .get(client.session_id.as_str())
                    .copied()
            })?;
            Some((client.session_id.clone(), window_id.to_owned()))
        })
        .collect()
}

fn hook_event_clears_cached_bell(
    hook_event: Option<&HookEvent>,
    session_id: &str,
    window_id: &str,
    window_index: u32,
) -> bool {
    let Some(event) = hook_event else {
        return false;
    };
    if !matches!(
        event.event,
        HookName::AfterSelectWindow
            | HookName::ClientSessionChanged
            | HookName::SessionWindowChanged
    ) {
        return false;
    }
    if non_empty(event.session_id.as_deref()) != Some(session_id) {
        return false;
    }

    non_empty(event.window_id.as_deref()) == Some(window_id)
        || event.window_index == Some(window_index)
}

fn apply_hook_event_overlay(state: &mut ProjectionState, event: &HookEvent) {
    let Some(window) = projection_window_for_hook_event(state, event) else {
        return;
    };

    match event.event {
        HookName::AlertBell => {
            window.bell_flag = true;
            update_activity_timestamp(window, event);
        }
        _ => {}
    }
}

fn projection_window_for_hook_event<'a>(
    state: &'a mut ProjectionState,
    event: &HookEvent,
) -> Option<&'a mut ProjectionWindow> {
    let session_id = non_empty(event.session_id.as_deref())?;
    let window_id = non_empty(event.window_id.as_deref());
    let window_index = event.window_index;
    if window_id.is_none() && window_index.is_none() {
        return None;
    }

    let session = state
        .sessions
        .iter_mut()
        .find(|session| session.id == session_id)?;
    if let Some(window_id) = window_id {
        return session
            .windows
            .iter_mut()
            .find(|window| window.id == window_id);
    }

    session.windows.iter_mut().find(|window| {
        window_index
            .map(|index| window.index == index)
            .unwrap_or(false)
    })
}

fn update_activity_timestamp(window: &mut ProjectionWindow, event: &HookEvent) {
    if let Some(timestamp_ms) = event.timestamp_ms {
        window.activity = timestamp_ms / 1000;
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn broadcast(shared: &Arc<Mutex<SharedState>>, message: ServerMessage) {
    let subscribers: Vec<(u64, SharedWriter)> = {
        let guard = shared.lock().expect("shared state poisoned");
        guard
            .subscribers
            .iter()
            .map(|(subscriber_id, writer)| (*subscriber_id, Arc::clone(writer)))
            .collect()
    };

    let mut stale_subscribers = Vec::new();
    for (subscriber_id, writer) in subscribers {
        if send_message(&writer, &message).is_err() {
            stale_subscribers.push(subscriber_id);
        }
    }

    if stale_subscribers.is_empty() {
        return;
    }

    let mut guard = shared.lock().expect("shared state poisoned");
    for subscriber_id in stale_subscribers {
        guard.subscribers.remove(&subscriber_id);
    }
}

fn send_message(writer: &SharedWriter, message: &ServerMessage) -> Result<()> {
    let mut stream = writer.lock().expect("sidecar writer poisoned");
    crate::ipc::write_message(&mut *stream, message).context("failed to write sidecar response")
}

fn send_error(writer: &SharedWriter, message: impl Into<String>) -> Result<()> {
    send_message(
        writer,
        &ServerMessage::Error(ErrorMessage {
            message: message.into(),
        }),
    )
}

fn store_state_cache(cache_path: &Path, state: &ProjectionState) {
    // The cache only accelerates the next UI launch; live tmux synchronization must not depend on it.
    let _ = crate::state_cache::store_path(cache_path, state);
}

fn handle_action_request(
    shared: &Arc<Mutex<SharedState>>,
    refresh_lock: &Arc<Mutex<()>>,
    refresh_from_tmux: bool,
    tmux: &dyn ServerTmuxOps,
    request: &ActionRequest,
) -> ActionResultKind {
    let tmux_socket_path = {
        let guard = shared.lock().expect("shared state poisoned");
        guard.tmux_socket_path.clone()
    };
    let action_result = tmux.execute_action(&tmux_socket_path, request);
    let refresh_result = match &action_result {
        Ok(effect) => {
            refresh_state_for_action(shared, refresh_lock, refresh_from_tmux, tmux, effect)
        }
        Err(_) => refresh_state(shared, refresh_lock, refresh_from_tmux, tmux),
    };

    match (action_result, refresh_result) {
        (Ok(effect), Ok(_)) => ActionResultKind::Ok {
            outcome: effect.outcome(),
        },
        (Err(action_error), Ok(_)) => ActionResultKind::Error {
            message: action_error.to_string(),
        },
        (Ok(_), Err(refresh_error)) => ActionResultKind::Error {
            message: format!("action succeeded but failed to refresh state: {refresh_error:#}"),
        },
        (Err(action_error), Err(refresh_error)) => ActionResultKind::Error {
            message: format!(
                "{action_error:#}; additionally failed to refresh state: {refresh_error:#}"
            ),
        },
    }
}

fn tmux_client(tmux_socket_path: &Path) -> TmuxCli {
    TmuxCli {
        socket_name: None,
        socket_path: Some(tmux_socket_path.to_path_buf()),
    }
}

fn execute_action_request(tmux: &impl Tmux, request: &ActionRequest) -> Result<ActionEffect> {
    let effect = match &request.action {
        Action::SwitchSession { session_id } => {
            let client = tmux
                .resolve_target_client(request.target_client.as_deref())
                .context("failed to resolve target tmux client")?;
            tmux.switch_to(&client, WindowTarget::Session(session_id.clone()))
                .with_context(|| {
                    format!(
                        "failed to switch client `{}` to session `{session_id}`",
                        client.0
                    )
                })?;
            ActionEffect::SwitchedSession {
                client_name: client.0,
                session_id: session_id.clone(),
            }
        }
        Action::SwitchWindow {
            session_id,
            window_id,
        } => {
            let client = tmux
                .resolve_target_client(request.target_client.as_deref())
                .context("failed to resolve target tmux client")?;
            tmux.switch_to(
                &client,
                WindowTarget::Window {
                    session_id: session_id.clone(),
                    window_id: window_id.clone(),
                },
            )
            .with_context(|| {
                format!(
                    "failed to switch client `{}` to window `{window_id}` in session `{session_id}`",
                    client.0
                )
            })?;
            ActionEffect::SwitchedWindow {
                client_name: client.0,
                session_id: session_id.clone(),
                window_id: window_id.clone(),
            }
        }
        Action::CreateSession { name } => {
            let session_id = tmux
                .create_session(name.as_deref())
                .with_context(|| match name {
                    Some(name) => format!("failed to create session `{name}`"),
                    None => String::from("failed to create session"),
                })?;
            ActionEffect::CreatedSession { session_id }
        }
        Action::CreateWindow { session_id, name } => {
            let window_id = tmux
                .create_window(session_id, name.as_deref())
                .with_context(|| match name {
                    Some(name) => {
                        format!("failed to create window `{name}` in session `{session_id}`")
                    }
                    None => format!("failed to create window in session `{session_id}`"),
                })?;
            ActionEffect::CreatedWindow {
                session_id: session_id.clone(),
                window_id,
            }
        }
        Action::RenameSession { session_id, name } => {
            tmux.rename_session(session_id, name)
                .with_context(|| format!("failed to rename session `{session_id}` to `{name}`"))?;
            ActionEffect::RenamedSession {
                session_id: session_id.clone(),
                name: name.clone(),
            }
        }
        Action::RenameWindow { window_id, name } => {
            tmux.rename_window(window_id, name)
                .with_context(|| format!("failed to rename window `{window_id}` to `{name}`"))?;
            ActionEffect::RenamedWindow {
                window_id: window_id.clone(),
                name: name.clone(),
            }
        }
        Action::CloseSession { session_id } => {
            tmux.close_session(session_id)
                .with_context(|| format!("failed to close session `{session_id}`"))?;
            ActionEffect::ClosedSession {
                session_id: session_id.clone(),
            }
        }
        Action::CloseWindow {
            session_id,
            window_id,
        } => {
            tmux.close_window(session_id, window_id).with_context(|| {
                format!("failed to close window `{window_id}` in session `{session_id}`")
            })?;
            ActionEffect::ClosedWindow {
                session_id: session_id.clone(),
                window_id: window_id.clone(),
            }
        }
    };

    Ok(effect)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        fs,
        io::BufReader,
        os::unix::net::UnixStream,
        path::{Path, PathBuf},
        sync::Mutex,
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use anyhow::{Context, Result, anyhow};

    use super::{ActionEffect, Server, ServerTmuxOps, execute_action_request};
    use crate::ipc::{
        Ack, AckKind, Action, ActionOutcome, ActionRequest, ActionResult, ActionResultKind,
        ClientKind, ClientMessage, Hello, HookEvent, HookName, PROTOCOL_VERSION, ProjectionClient,
        ProjectionSession, ProjectionState, ProjectionWindow, ServerMessage, SidecarPaths,
        StateUpdated,
    };
    use crate::{
        model::{ClientName, SessionId, WindowId, WindowTarget},
        tmux::{Tmux, TmuxError},
    };

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Result<Self> {
            let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(".rt")
                .join(format!("{name:.3}-{unique:x}"));
            fs::create_dir_all(&path)?;
            Ok(Self { path })
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    struct RawClient {
        reader: BufReader<UnixStream>,
        writer: UnixStream,
    }

    impl RawClient {
        fn connect(socket_path: &Path, client_kind: ClientKind) -> Result<Self> {
            let writer = wait_for_stream(socket_path)?;
            let reader_stream = writer.try_clone()?;
            let mut client = Self {
                reader: BufReader::new(reader_stream),
                writer,
            };
            client.send(&ClientMessage::Hello(Hello {
                client_kind,
                protocol_version: PROTOCOL_VERSION,
            }))?;

            match client.recv()? {
                ServerMessage::HelloAck(_) => Ok(client),
                other => Err(anyhow::anyhow!("unexpected hello response: {other:?}")),
            }
        }

        fn send(&mut self, message: &ClientMessage) -> Result<()> {
            crate::ipc::write_message(&mut self.writer, message)
                .context("failed to write test client message")
        }

        fn recv(&mut self) -> Result<ServerMessage> {
            crate::ipc::read_message(&mut self.reader)
                .context("failed to read test client message")?
                .context("server closed test connection")
        }
    }

    fn wait_for_stream(socket_path: &Path) -> Result<UnixStream> {
        for _ in 0..40 {
            if let Ok(stream) = UnixStream::connect(socket_path) {
                return Ok(stream);
            }
            thread::sleep(Duration::from_millis(25));
        }

        Err(anyhow::anyhow!(
            "timed out waiting for sidecar socket `{}`",
            socket_path.display()
        ))
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum RecordedTmuxCall {
        ResolveTargetClient(Option<String>),
        SwitchTo {
            client: String,
            target: WindowTarget,
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

    #[derive(Debug, Default)]
    struct RecordingTmux {
        calls: Mutex<Vec<RecordedTmuxCall>>,
    }

    impl RecordingTmux {
        fn calls(&self) -> Vec<RecordedTmuxCall> {
            self.calls.lock().expect("tmux call log poisoned").clone()
        }
    }

    impl Tmux for RecordingTmux {
        fn snapshot(&self) -> Result<crate::model::TmuxState, TmuxError> {
            panic!("snapshot should not be called in this test");
        }

        fn resolve_target_client(
            &self,
            cli_override: Option<&str>,
        ) -> Result<ClientName, TmuxError> {
            self.calls.lock().expect("tmux call log poisoned").push(
                RecordedTmuxCall::ResolveTargetClient(cli_override.map(str::to_owned)),
            );
            Ok(ClientName(String::from("resolved-client")))
        }

        fn switch_to(&self, client: &ClientName, target: WindowTarget) -> Result<(), TmuxError> {
            self.calls
                .lock()
                .expect("tmux call log poisoned")
                .push(RecordedTmuxCall::SwitchTo {
                    client: client.0.clone(),
                    target,
                });
            Ok(())
        }

        fn create_session(&self, name: Option<&str>) -> Result<SessionId, TmuxError> {
            self.calls.lock().expect("tmux call log poisoned").push(
                RecordedTmuxCall::CreateSession {
                    name: name.map(str::to_owned),
                },
            );
            Ok(String::from("$new"))
        }

        fn create_window(
            &self,
            session: &SessionId,
            name: Option<&str>,
        ) -> Result<WindowId, TmuxError> {
            self.calls.lock().expect("tmux call log poisoned").push(
                RecordedTmuxCall::CreateWindow {
                    session_id: session.clone(),
                    name: name.map(str::to_owned),
                },
            );
            Ok(String::from("@new"))
        }

        fn close_session(&self, session: &SessionId) -> Result<(), TmuxError> {
            self.calls.lock().expect("tmux call log poisoned").push(
                RecordedTmuxCall::CloseSession {
                    session_id: session.clone(),
                },
            );
            Ok(())
        }

        fn close_window(&self, session: &SessionId, window: &WindowId) -> Result<(), TmuxError> {
            self.calls.lock().expect("tmux call log poisoned").push(
                RecordedTmuxCall::CloseWindow {
                    session_id: session.clone(),
                    window_id: window.clone(),
                },
            );
            Ok(())
        }

        fn rename_session(&self, session: &SessionId, name: &str) -> Result<(), TmuxError> {
            self.calls.lock().expect("tmux call log poisoned").push(
                RecordedTmuxCall::RenameSession {
                    session_id: session.clone(),
                    name: name.to_owned(),
                },
            );
            Ok(())
        }

        fn rename_window(&self, window: &WindowId, name: &str) -> Result<(), TmuxError> {
            self.calls.lock().expect("tmux call log poisoned").push(
                RecordedTmuxCall::RenameWindow {
                    window_id: window.clone(),
                    name: name.to_owned(),
                },
            );
            Ok(())
        }
    }

    #[derive(Debug)]
    struct MockServerTmux {
        snapshots: Mutex<VecDeque<ProjectionState>>,
        action_results: Mutex<VecDeque<std::result::Result<ActionEffect, String>>>,
        requests: Mutex<Vec<ActionRequest>>,
    }

    impl MockServerTmux {
        fn new(
            snapshots: impl Into<VecDeque<ProjectionState>>,
            action_results: impl Into<VecDeque<std::result::Result<ActionEffect, String>>>,
        ) -> Self {
            Self {
                snapshots: Mutex::new(snapshots.into()),
                action_results: Mutex::new(action_results.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<ActionRequest> {
            self.requests
                .lock()
                .expect("tmux requests poisoned")
                .clone()
        }
    }

    impl ServerTmuxOps for MockServerTmux {
        fn snapshot_projection(&self, _tmux_socket_path: &Path) -> Result<ProjectionState> {
            self.snapshots
                .lock()
                .expect("tmux snapshots poisoned")
                .pop_front()
                .context("missing mock snapshot response")
        }

        fn execute_action(
            &self,
            _tmux_socket_path: &Path,
            request: &ActionRequest,
        ) -> Result<ActionEffect> {
            self.requests
                .lock()
                .expect("tmux requests poisoned")
                .push(request.clone());
            match self
                .action_results
                .lock()
                .expect("tmux action results poisoned")
                .pop_front()
                .context("missing mock action response")?
            {
                Ok(effect) => Ok(effect),
                Err(message) => Err(anyhow!(message)),
            }
        }
    }

    fn projection_state_with_session(
        tmux_socket_path: &Path,
        session_id: &str,
        session_name: &str,
    ) -> ProjectionState {
        ProjectionState {
            tmux_socket_path: tmux_socket_path.to_path_buf(),
            sessions: vec![ProjectionSession {
                id: session_id.to_owned(),
                name: session_name.to_owned(),
                attached_count: 1,
                active_window_id: None,
                windows: Vec::new(),
            }],
            clients: Vec::new(),
        }
    }

    fn projection_state_with_window(
        tmux_socket_path: &Path,
        session_id: &str,
        session_name: &str,
        window_id: &str,
        window_name: &str,
    ) -> ProjectionState {
        ProjectionState {
            tmux_socket_path: tmux_socket_path.to_path_buf(),
            sessions: vec![ProjectionSession {
                id: session_id.to_owned(),
                name: session_name.to_owned(),
                attached_count: 0,
                active_window_id: Some(window_id.to_owned()),
                windows: vec![ProjectionWindow {
                    id: window_id.to_owned(),
                    index: 0,
                    name: window_name.to_owned(),
                    active: true,
                    activity: 0,
                    activity_flag: false,
                    bell_flag: false,
                    silence_flag: false,
                }],
            }],
            clients: Vec::new(),
        }
    }

    fn add_projection_window(
        state: &mut ProjectionState,
        session_id: &str,
        window_id: &str,
        window_name: &str,
        index: u32,
        active: bool,
    ) {
        let session = state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("missing projection session");
        if active {
            session.active_window_id = Some(window_id.to_owned());
            for window in &mut session.windows {
                window.active = false;
            }
        }
        session.windows.push(ProjectionWindow {
            id: window_id.to_owned(),
            index,
            name: window_name.to_owned(),
            active,
            activity: 0,
            activity_flag: false,
            bell_flag: false,
            silence_flag: false,
        });
    }

    fn add_projection_client(
        state: &mut ProjectionState,
        name: &str,
        session_id: &str,
        window_id: &str,
    ) {
        if let Some(session) = state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.attached_count = session.attached_count.max(1);
        }
        state.clients.push(ProjectionClient {
            name: name.to_owned(),
            session_id: session_id.to_owned(),
            current_window_id: Some(window_id.to_owned()),
            activity: 1,
            tty: String::from("/dev/pts/1"),
        });
    }

    fn shutdown_server(socket_path: &Path) -> Result<()> {
        let mut shutdown = RawClient::connect(socket_path, ClientKind::Control)?;
        shutdown.send(&ClientMessage::Shutdown)?;
        assert_eq!(
            shutdown.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::Shutdown,
            })
        );
        Ok(())
    }

    #[test]
    fn server_handles_subscribe_hook_and_shutdown_flows() -> Result<()> {
        let sandbox = TestDir::new("server-ipc")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let server = Server::bind(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            ProjectionState::empty(&tmux_socket_path),
            false,
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut subscriber = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        subscriber.send(&ClientMessage::Subscribe(Default::default()))?;
        let initial_update = subscriber.recv()?;
        assert_eq!(
            initial_update,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 1,
                state: ProjectionState::empty(&tmux_socket_path),
            })
        );

        let mut hook = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Hook)?;
        hook.send(&ClientMessage::HookEvent(HookEvent {
            tmux_socket_path: tmux_socket_path.clone(),
            event: HookName::AlertBell,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@1")),
            window_index: Some(1),
            pane_id: Some(String::from("%1")),
            client_name: None,
            timestamp_ms: Some(42),
        }))?;
        assert_eq!(
            hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );
        assert_eq!(
            subscriber.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: ProjectionState::empty(&tmux_socket_path),
            })
        );

        drop(hook);
        drop(subscriber);

        let mut shutdown = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Control)?;
        shutdown.send(&ClientMessage::Shutdown)?;
        assert_eq!(
            shutdown.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::Shutdown,
            })
        );
        drop(shutdown);

        server_thread.join().expect("server thread panicked")?;
        assert!(!sidecar_paths.socket_path.exists());
        assert!(!sidecar_paths.pid_path.exists());

        Ok(())
    }

    #[test]
    fn snapshot_request_returns_current_state_without_registering_subscriber() -> Result<()> {
        let sandbox = TestDir::new("server-query")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "build");
        let server = Server::bind(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state.clone(),
            false,
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut subscriber = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        subscriber.send(&ClientMessage::Subscribe(Default::default()))?;
        assert_eq!(
            subscriber.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 1,
                state: initial_state.clone(),
            })
        );

        let mut query = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Control)?;
        query.send(&ClientMessage::SnapshotRequest)?;
        assert_eq!(
            query.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 1,
                state: initial_state.clone(),
            })
        );

        let mut hook = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Hook)?;
        hook.send(&ClientMessage::HookEvent(HookEvent {
            tmux_socket_path: tmux_socket_path.clone(),
            event: HookName::AlertBell,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@1")),
            window_index: Some(0),
            pane_id: Some(String::from("%1")),
            client_name: None,
            timestamp_ms: Some(1_000),
        }))?;

        let mut alerted_state = initial_state;
        alerted_state.sessions[0].windows[0].bell_flag = true;
        alerted_state.sessions[0].windows[0].activity = 1;
        assert_eq!(
            subscriber.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: alerted_state,
            })
        );
        assert_eq!(
            hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );

        drop(query);
        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;

        Ok(())
    }

    #[test]
    fn shutdown_existing_server_at_stops_running_server_without_spawning() -> Result<()> {
        let sandbox = TestDir::new("server-kill")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let server = Server::bind(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            ProjectionState::empty(&tmux_socket_path),
            false,
        )?;

        let server_thread = thread::spawn(move || server.run());

        crate::client::shutdown_existing_server_at(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;
        assert!(!sidecar_paths.socket_path.exists());
        assert!(!sidecar_paths.pid_path.exists());

        Ok(())
    }

    #[test]
    fn alert_hook_payload_marks_refreshed_projection_when_snapshot_has_no_alert() -> Result<()> {
        let sandbox = TestDir::new("server-alert-overlay")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let snapshot_without_alert =
            projection_state_with_window(&tmux_socket_path, "$1", "detached", "@1", "build");
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![snapshot_without_alert.clone()],
            vec![],
        ));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            snapshot_without_alert.clone(),
            true,
            tmux,
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut subscriber = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        subscriber.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = subscriber.recv()?;

        let mut hook = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Hook)?;
        hook.send(&ClientMessage::HookEvent(HookEvent {
            tmux_socket_path: tmux_socket_path.clone(),
            event: HookName::AlertBell,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@1")),
            window_index: Some(0),
            pane_id: Some(String::from("%1")),
            client_name: None,
            timestamp_ms: Some(42_000),
        }))?;

        let mut expected_state = snapshot_without_alert;
        expected_state.sessions[0].windows[0].bell_flag = true;
        expected_state.sessions[0].windows[0].activity = 42;
        assert_eq!(
            subscriber.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: expected_state,
            })
        );
        assert_eq!(
            hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;

        Ok(())
    }

    #[test]
    fn cached_bell_alert_survives_later_non_alert_hook_refresh() -> Result<()> {
        let sandbox = TestDir::new("server-alert-preserve")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "detached", "@1", "build");
        initial_state.sessions[0].windows[0].bell_flag = true;
        let snapshot_without_alert =
            projection_state_with_window(&tmux_socket_path, "$1", "detached", "@1", "build");
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![snapshot_without_alert.clone()],
            vec![],
        ));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state.clone(),
            true,
            tmux,
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut subscriber = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        subscriber.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = subscriber.recv()?;

        let mut hook = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Hook)?;
        hook.send(&ClientMessage::HookEvent(HookEvent {
            tmux_socket_path: tmux_socket_path.clone(),
            event: HookName::SessionRenamed,
            session_id: Some(String::from("$1")),
            window_id: None,
            window_index: None,
            pane_id: None,
            client_name: None,
            timestamp_ms: None,
        }))?;

        let mut expected_state = snapshot_without_alert;
        expected_state.sessions[0].windows[0].bell_flag = true;
        assert_eq!(
            subscriber.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: expected_state,
            })
        );
        assert_eq!(
            hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;

        Ok(())
    }

    #[test]
    fn cached_bell_alert_clears_on_select_hook_when_snapshot_has_no_alert() -> Result<()> {
        let sandbox = TestDir::new("server-alert-clear")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "detached", "@1", "build");
        initial_state.sessions[0].windows[0].bell_flag = true;
        let snapshot_without_alert =
            projection_state_with_window(&tmux_socket_path, "$1", "detached", "@1", "build");
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![snapshot_without_alert.clone()],
            vec![],
        ));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state,
            true,
            tmux,
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut subscriber = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        subscriber.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = subscriber.recv()?;

        let mut hook = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Hook)?;
        hook.send(&ClientMessage::HookEvent(HookEvent {
            tmux_socket_path: tmux_socket_path.clone(),
            event: HookName::AfterSelectWindow,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@1")),
            window_index: Some(0),
            pane_id: Some(String::from("%1")),
            client_name: None,
            timestamp_ms: None,
        }))?;

        assert_eq!(
            subscriber.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: snapshot_without_alert,
            })
        );
        assert_eq!(
            hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;

        Ok(())
    }

    #[test]
    fn cached_bell_alert_clears_on_action_refresh_when_client_views_window() -> Result<()> {
        let sandbox = TestDir::new("server-alert-action-clear")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "build");
        initial_state.sessions[0].windows[0].bell_flag = true;
        let mut snapshot_without_alert =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "build");
        add_projection_client(&mut snapshot_without_alert, "client-1", "$1", "@1");
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![snapshot_without_alert.clone()],
            vec![Ok(ActionEffect::SwitchedWindow {
                client_name: String::from("client-1"),
                session_id: String::from("$1"),
                window_id: String::from("@1"),
            })],
        ));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state,
            true,
            tmux,
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut client = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        client.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = client.recv()?;

        let request = ActionRequest {
            request_id: String::from("req-switch"),
            target_client: Some(String::from("client-1")),
            action: Action::SwitchWindow {
                session_id: String::from("$1"),
                window_id: String::from("@1"),
            },
        };
        client.send(&ClientMessage::ActionRequest(request.clone()))?;

        assert_eq!(
            client.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: snapshot_without_alert,
            })
        );
        assert_eq!(
            client.recv()?,
            ServerMessage::ActionResult(ActionResult {
                request_id: request.request_id,
                result: ActionResultKind::Ok { outcome: None },
            })
        );

        shutdown_server(&sidecar_paths.socket_path)?;
        drop(client);
        server_thread.join().expect("server thread panicked")?;

        Ok(())
    }

    #[test]
    fn cached_bell_alert_clears_on_hook_refresh_when_client_views_window() -> Result<()> {
        let sandbox = TestDir::new("server-alert-client-view-clear")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "build");
        initial_state.sessions[0].windows[0].bell_flag = true;
        let mut snapshot_without_alert =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "build");
        add_projection_client(&mut snapshot_without_alert, "client-1", "$1", "@1");
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![snapshot_without_alert.clone()],
            vec![],
        ));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state,
            true,
            tmux,
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut subscriber = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        subscriber.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = subscriber.recv()?;

        let mut hook = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Hook)?;
        hook.send(&ClientMessage::HookEvent(HookEvent {
            tmux_socket_path: tmux_socket_path.clone(),
            event: HookName::ClientAttached,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@1")),
            window_index: Some(0),
            pane_id: None,
            client_name: Some(String::from("client-1")),
            timestamp_ms: None,
        }))?;

        assert_eq!(
            subscriber.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: snapshot_without_alert,
            })
        );
        assert_eq!(
            hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;

        Ok(())
    }

    #[test]
    fn execute_action_request_dispatches_supported_actions() -> Result<()> {
        let tmux = RecordingTmux::default();
        let requests = [
            ActionRequest {
                request_id: String::from("req-1"),
                target_client: Some(String::from("client-1")),
                action: Action::SwitchSession {
                    session_id: String::from("$1"),
                },
            },
            ActionRequest {
                request_id: String::from("req-2"),
                target_client: None,
                action: Action::SwitchWindow {
                    session_id: String::from("$2"),
                    window_id: String::from("@2"),
                },
            },
            ActionRequest {
                request_id: String::from("req-3"),
                target_client: None,
                action: Action::CreateSession {
                    name: Some(String::from("dev")),
                },
            },
            ActionRequest {
                request_id: String::from("req-4"),
                target_client: None,
                action: Action::CreateWindow {
                    session_id: String::from("$2"),
                    name: Some(String::from("shell")),
                },
            },
            ActionRequest {
                request_id: String::from("req-5"),
                target_client: None,
                action: Action::RenameSession {
                    session_id: String::from("$2"),
                    name: String::from("renamed"),
                },
            },
            ActionRequest {
                request_id: String::from("req-6"),
                target_client: None,
                action: Action::RenameWindow {
                    window_id: String::from("@2"),
                    name: String::from("editor"),
                },
            },
            ActionRequest {
                request_id: String::from("req-7"),
                target_client: None,
                action: Action::CloseSession {
                    session_id: String::from("$3"),
                },
            },
            ActionRequest {
                request_id: String::from("req-8"),
                target_client: None,
                action: Action::CloseWindow {
                    session_id: String::from("$4"),
                    window_id: String::from("@4"),
                },
            },
        ];

        let effects = requests
            .iter()
            .map(|request| execute_action_request(&tmux, request))
            .collect::<Result<Vec<_>>>()?;

        assert_eq!(
            tmux.calls(),
            vec![
                RecordedTmuxCall::ResolveTargetClient(Some(String::from("client-1"))),
                RecordedTmuxCall::SwitchTo {
                    client: String::from("resolved-client"),
                    target: WindowTarget::Session(String::from("$1")),
                },
                RecordedTmuxCall::ResolveTargetClient(None),
                RecordedTmuxCall::SwitchTo {
                    client: String::from("resolved-client"),
                    target: WindowTarget::Window {
                        session_id: String::from("$2"),
                        window_id: String::from("@2"),
                    },
                },
                RecordedTmuxCall::CreateSession {
                    name: Some(String::from("dev")),
                },
                RecordedTmuxCall::CreateWindow {
                    session_id: String::from("$2"),
                    name: Some(String::from("shell")),
                },
                RecordedTmuxCall::RenameSession {
                    session_id: String::from("$2"),
                    name: String::from("renamed"),
                },
                RecordedTmuxCall::RenameWindow {
                    window_id: String::from("@2"),
                    name: String::from("editor"),
                },
                RecordedTmuxCall::CloseSession {
                    session_id: String::from("$3"),
                },
                RecordedTmuxCall::CloseWindow {
                    session_id: String::from("$4"),
                    window_id: String::from("@4"),
                },
            ]
        );
        assert_eq!(
            effects,
            vec![
                ActionEffect::SwitchedSession {
                    client_name: String::from("resolved-client"),
                    session_id: String::from("$1"),
                },
                ActionEffect::SwitchedWindow {
                    client_name: String::from("resolved-client"),
                    session_id: String::from("$2"),
                    window_id: String::from("@2"),
                },
                ActionEffect::CreatedSession {
                    session_id: String::from("$new"),
                },
                ActionEffect::CreatedWindow {
                    session_id: String::from("$2"),
                    window_id: String::from("@new"),
                },
                ActionEffect::RenamedSession {
                    session_id: String::from("$2"),
                    name: String::from("renamed"),
                },
                ActionEffect::RenamedWindow {
                    window_id: String::from("@2"),
                    name: String::from("editor"),
                },
                ActionEffect::ClosedSession {
                    session_id: String::from("$3"),
                },
                ActionEffect::ClosedWindow {
                    session_id: String::from("$4"),
                    window_id: String::from("@4"),
                },
            ]
        );

        Ok(())
    }

    #[test]
    fn action_refresh_retries_until_created_window_is_projected() -> Result<()> {
        let sandbox = TestDir::new("server-create-window-retry")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "shell");
        let stale_state = initial_state.clone();
        let mut refreshed_state = initial_state.clone();
        add_projection_window(&mut refreshed_state, "$1", "@2", "created", 1, false);
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![stale_state, refreshed_state.clone()],
            vec![Ok(ActionEffect::CreatedWindow {
                session_id: String::from("$1"),
                window_id: String::from("@2"),
            })],
        ));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state.clone(),
            true,
            tmux.clone(),
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut client = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        client.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = client.recv()?;

        let request = ActionRequest {
            request_id: String::from("req-create-window"),
            target_client: None,
            action: Action::CreateWindow {
                session_id: String::from("$1"),
                name: Some(String::from("created")),
            },
        };
        client.send(&ClientMessage::ActionRequest(request.clone()))?;

        assert_eq!(
            client.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: refreshed_state,
            })
        );
        assert_eq!(
            client.recv()?,
            ServerMessage::ActionResult(ActionResult {
                request_id: request.request_id.clone(),
                result: ActionResultKind::Ok {
                    outcome: Some(ActionOutcome::CreatedWindow {
                        session_id: String::from("$1"),
                        window_id: String::from("@2"),
                    }),
                },
            })
        );

        shutdown_server(&sidecar_paths.socket_path)?;
        drop(client);
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(tmux.requests(), vec![request]);

        Ok(())
    }

    #[test]
    fn action_refresh_retries_until_closed_window_leaves_projected_session() -> Result<()> {
        let sandbox = TestDir::new("server-close-window-retry")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "shell");
        let stale_state = initial_state.clone();
        let refreshed_state = projection_state_with_session(&tmux_socket_path, "$1", "work");
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![stale_state, refreshed_state.clone()],
            vec![Ok(ActionEffect::ClosedWindow {
                session_id: String::from("$1"),
                window_id: String::from("@1"),
            })],
        ));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state.clone(),
            true,
            tmux.clone(),
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut client = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        client.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = client.recv()?;

        let request = ActionRequest {
            request_id: String::from("req-close-window"),
            target_client: None,
            action: Action::CloseWindow {
                session_id: String::from("$1"),
                window_id: String::from("@1"),
            },
        };
        client.send(&ClientMessage::ActionRequest(request.clone()))?;

        assert_eq!(
            client.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: refreshed_state,
            })
        );
        assert_eq!(
            client.recv()?,
            ServerMessage::ActionResult(ActionResult {
                request_id: request.request_id.clone(),
                result: ActionResultKind::Ok { outcome: None },
            })
        );

        shutdown_server(&sidecar_paths.socket_path)?;
        drop(client);
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(tmux.requests(), vec![request]);

        Ok(())
    }

    #[test]
    fn hook_refresh_retries_when_first_lifecycle_snapshot_is_unchanged() -> Result<()> {
        let sandbox = TestDir::new("server-hook-retry")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "shell");
        let stale_state = initial_state.clone();
        let refreshed_state = projection_state_with_session(&tmux_socket_path, "$1", "work");
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![stale_state, refreshed_state.clone()],
            vec![],
        ));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state,
            true,
            tmux,
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut subscriber = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        subscriber.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = subscriber.recv()?;

        let mut hook = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Hook)?;
        hook.send(&ClientMessage::HookEvent(HookEvent {
            tmux_socket_path: tmux_socket_path.clone(),
            event: HookName::WindowUnlinked,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@0")),
            window_index: Some(0),
            pane_id: Some(String::from("%1")),
            client_name: None,
            timestamp_ms: None,
        }))?;

        assert_eq!(
            subscriber.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: refreshed_state,
            })
        );
        assert_eq!(
            hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;

        Ok(())
    }

    #[test]
    fn server_broadcasts_refreshed_state_before_action_success() -> Result<()> {
        let sandbox = TestDir::new("server-action-success")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let refreshed_state = projection_state_with_session(&tmux_socket_path, "$9", "created");
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![refreshed_state.clone()],
            vec![Ok(ActionEffect::CreatedSession {
                session_id: String::from("$9"),
            })],
        ));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            ProjectionState::empty(&tmux_socket_path),
            true,
            tmux.clone(),
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut client = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        client.send(&ClientMessage::Subscribe(Default::default()))?;
        assert_eq!(
            client.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 1,
                state: ProjectionState::empty(&tmux_socket_path),
            })
        );

        let request = ActionRequest {
            request_id: String::from("req-create"),
            target_client: None,
            action: Action::CreateSession {
                name: Some(String::from("created")),
            },
        };
        client.send(&ClientMessage::ActionRequest(request.clone()))?;

        assert_eq!(
            client.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: refreshed_state,
            })
        );
        assert_eq!(
            client.recv()?,
            ServerMessage::ActionResult(ActionResult {
                request_id: request.request_id.clone(),
                result: ActionResultKind::Ok {
                    outcome: Some(ActionOutcome::CreatedSession {
                        session_id: String::from("$9"),
                    }),
                },
            })
        );

        shutdown_server(&sidecar_paths.socket_path)?;
        drop(client);
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(tmux.requests(), vec![request]);

        Ok(())
    }

    #[test]
    fn server_refreshes_and_reports_action_errors() -> Result<()> {
        let sandbox = TestDir::new("server-action-error")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let refreshed_state = projection_state_with_session(&tmux_socket_path, "$1", "current");
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![refreshed_state.clone()],
            vec![Err(String::from("rename-session failed"))],
        ));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            ProjectionState::empty(&tmux_socket_path),
            true,
            tmux.clone(),
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut client = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        client.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = client.recv()?;

        let request = ActionRequest {
            request_id: String::from("req-rename"),
            target_client: None,
            action: Action::RenameSession {
                session_id: String::from("$1"),
                name: String::from("renamed"),
            },
        };
        client.send(&ClientMessage::ActionRequest(request.clone()))?;

        assert_eq!(
            client.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: refreshed_state,
            })
        );
        match client.recv()? {
            ServerMessage::ActionResult(ActionResult {
                request_id,
                result: ActionResultKind::Error { message },
            }) => {
                assert_eq!(request_id, request.request_id);
                assert!(message.contains("rename-session failed"));
            }
            other => panic!("unexpected action response: {other:?}"),
        }

        shutdown_server(&sidecar_paths.socket_path)?;
        drop(client);
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(tmux.requests(), vec![request]);

        Ok(())
    }
}
