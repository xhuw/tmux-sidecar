use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use crate::{
    domain::{DomainState, WinlinkKey},
    ipc::{
        Ack, AckKind, Action, ActionOutcome, ActionRequest, ActionResult, ActionResultKind,
        ClientMessage, ErrorMessage, HelloAck, HookEvent, HookName, ProjectionState, ServerMessage,
        SidecarPaths, StateUpdated,
    },
    model::{ClientName, WindowTarget},
    tmux::{Tmux, TmuxCli, WindowWorkdir},
};

const IPC_WRITE_TIMEOUT: Duration = Duration::from_millis(100);
const ACTION_RECONCILE_ATTEMPTS: usize = 10;
const ACTION_RECONCILE_RETRY_DELAY: Duration = Duration::from_millis(25);
const HOOK_RECONCILE_ATTEMPTS: usize = 6;
const HOOK_RECONCILE_SETTLE_DELAY: Duration = Duration::from_millis(50);
const HOOK_RECONCILE_RETRY_DELAY: Duration = Duration::from_millis(25);

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

type ServerReply<T> = mpsc::Sender<T>;

struct ServerCoreState {
    tmux_socket_path: PathBuf,
    generation: u64,
    state: DomainState,
    workdirs: SessionWorkdirTracker,
    pending_reconcile: Option<PendingReconcile>,
    subscribers: BTreeMap<u64, SharedWriter>,
    next_subscriber_id: u64,
}

struct ServerCore {
    state: ServerCoreState,
    refresh_from_tmux: bool,
    tmux: Arc<dyn ServerTmuxOps>,
    bell_notifier: Arc<dyn BellNotifier>,
    shutdown: Arc<AtomicBool>,
}

#[derive(Clone)]
struct ServerCoreHandle {
    sender: mpsc::Sender<ServerInput>,
}

enum ServerInput {
    Subscribe {
        writer: SharedWriter,
        reply: ServerReply<(u64, StateUpdated)>,
    },
    CurrentState {
        reply: ServerReply<StateUpdated>,
    },
    HookEvent {
        event: HookEvent,
        reply: ServerReply<Result<()>>,
    },
    SnapshotRequest {
        reply: ServerReply<Result<StateUpdated>>,
    },
    ActionRequest {
        request: ActionRequest,
        reply: ServerReply<ActionResult>,
    },
    Unsubscribe {
        subscriber_id: u64,
    },
    Shutdown {
        reply: ServerReply<()>,
    },
}

struct Server {
    listener: UnixListener,
    server_id: String,
    tmux_socket_path: PathBuf,
    core: ServerCoreHandle,
    core_thread: thread::JoinHandle<Result<()>>,
    shutdown: Arc<AtomicBool>,
    cleanup: CleanupPaths,
}

struct CleanupPaths {
    socket_path: PathBuf,
    pid_path: PathBuf,
}

trait ServerTmuxOps: Send + Sync {
    fn snapshot_projection(&self, tmux_socket_path: &Path) -> Result<ProjectionState>;
    fn inspect_session_workdirs(
        &self,
        tmux_socket_path: &Path,
        session_id: &str,
    ) -> Result<Vec<WindowWorkdir>>;
    fn execute_action(
        &self,
        tmux_socket_path: &Path,
        request: &ActionRequest,
        options: &ActionExecutionOptions,
    ) -> Result<ActionEffect>;
}

trait BellNotifier: Send + Sync {
    fn notify(&self, tty_paths: &[PathBuf], repeat_count: usize) -> Result<()>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ActionExecutionOptions {
    create_window_path: Option<PathBuf>,
    close_session_client_switch: Option<CloseSessionClientSwitch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CloseSessionClientSwitch {
    fallback_session_id: Option<String>,
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
        current_path: Option<PathBuf>,
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

#[derive(Debug, Default)]
struct TtyBellNotifier;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SessionWorkdirTracker {
    next_observed_order: u64,
    sessions: BTreeMap<String, TrackedSessionWorkdirs>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TrackedSessionWorkdirs {
    windows: BTreeMap<String, TrackedWindowWorkdir>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedWindowWorkdir {
    path: PathBuf,
    window_index: Option<u32>,
    observed_order: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DirtyScopes {
    all: bool,
    alerts: bool,
    clients: bool,
    sessions: BTreeSet<String>,
    winlinks: BTreeSet<WinlinkKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DirtyScope {
    All,
    Alerts,
    Clients,
    Session(String),
    Winlink(WinlinkKey),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BellClearHint {
    session_id: String,
    window_id: Option<String>,
    window_index: Option<u32>,
}

#[derive(Debug, Clone)]
struct PendingReconcile {
    dirty_scopes: DirtyScopes,
    clear_bell_hints: BTreeSet<BellClearHint>,
    deadline: Instant,
    attempts_remaining: usize,
}

#[derive(Debug, Clone)]
struct ReconcileResult {
    update: StateUpdated,
    changed: bool,
    snapshot_changed: bool,
}

impl ServerTmuxOps for LiveServerTmuxOps {
    fn snapshot_projection(&self, tmux_socket_path: &Path) -> Result<ProjectionState> {
        let tmux = tmux_client(tmux_socket_path);
        let snapshot = tmux.snapshot().context("failed to snapshot tmux state")?;
        Ok(ProjectionState::from_tmux(
            tmux_socket_path.to_path_buf(),
            snapshot,
        ))
    }

    fn inspect_session_workdirs(
        &self,
        tmux_socket_path: &Path,
        session_id: &str,
    ) -> Result<Vec<WindowWorkdir>> {
        tmux_client(tmux_socket_path)
            .session_window_workdirs(&session_id.to_owned())
            .with_context(|| format!("failed to inspect workdirs for session `{session_id}`"))
    }

    fn execute_action(
        &self,
        tmux_socket_path: &Path,
        request: &ActionRequest,
        options: &ActionExecutionOptions,
    ) -> Result<ActionEffect> {
        execute_action_request(&tmux_client(tmux_socket_path), request, options)
    }
}

impl BellNotifier for TtyBellNotifier {
    fn notify(&self, tty_paths: &[PathBuf], repeat_count: usize) -> Result<()> {
        if tty_paths.is_empty() || repeat_count == 0 {
            return Ok(());
        }

        let payload = vec![b'\x07'; repeat_count];
        let mut failures = Vec::new();
        for tty_path in tty_paths {
            if let Err(error) = write_bel_to_tty(tty_path, &payload) {
                failures.push(format!("{}: {error:#}", tty_path.display()));
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "failed to emit BEL to one or more tmux client ttys: {}",
                failures.join("; ")
            );
        }
    }
}

impl ServerCoreHandle {
    fn subscribe(&self, writer: SharedWriter) -> Result<(u64, StateUpdated)> {
        self.call(|reply| ServerInput::Subscribe { writer, reply })
    }

    fn current_state_update(&self) -> Result<StateUpdated> {
        self.call(|reply| ServerInput::CurrentState { reply })
    }

    fn handle_hook_event(&self, event: HookEvent) -> Result<()> {
        self.call(|reply| ServerInput::HookEvent { event, reply })?
    }

    fn snapshot_request(&self) -> Result<StateUpdated> {
        self.call(|reply| ServerInput::SnapshotRequest { reply })?
    }

    fn handle_action_request(&self, request: ActionRequest) -> Result<ActionResult> {
        self.call(|reply| ServerInput::ActionRequest { request, reply })
    }

    fn unsubscribe(&self, subscriber_id: u64) {
        let _ = self.sender.send(ServerInput::Unsubscribe { subscriber_id });
    }

    fn shutdown(&self) -> Result<()> {
        self.call(|reply| ServerInput::Shutdown { reply })
    }

    fn call<T: Send + 'static>(
        &self,
        build_input: impl FnOnce(ServerReply<T>) -> ServerInput,
    ) -> Result<T> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.sender
            .send(build_input(reply_tx))
            .context("failed to send request to server core")?;
        reply_rx
            .recv()
            .context("server core stopped before replying")
    }
}

impl ServerCore {
    fn spawn(
        state: ServerCoreState,
        refresh_from_tmux: bool,
        tmux: Arc<dyn ServerTmuxOps>,
        bell_notifier: Arc<dyn BellNotifier>,
        shutdown: Arc<AtomicBool>,
    ) -> (ServerCoreHandle, thread::JoinHandle<Result<()>>) {
        let (sender, receiver) = mpsc::channel();
        let handle = ServerCoreHandle { sender };
        let join_handle = thread::spawn(move || {
            Self {
                state,
                refresh_from_tmux,
                tmux,
                bell_notifier,
                shutdown,
            }
            .run(receiver)
        });
        (handle, join_handle)
    }

    fn run(mut self, receiver: mpsc::Receiver<ServerInput>) -> Result<()> {
        loop {
            let next_input = if let Some(timeout) = self.reconcile_timeout() {
                match receiver.recv_timeout(timeout) {
                    Ok(input) => Some(input),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match receiver.recv() {
                    Ok(input) => Some(input),
                    Err(_) => break,
                }
            };

            if let Some(input) = next_input {
                if self.handle_input(input) {
                    break;
                }
                continue;
            }

            self.run_scheduled_reconcile()?;
        }
        Ok(())
    }

    fn handle_input(&mut self, input: ServerInput) -> bool {
        match input {
            ServerInput::Subscribe { writer, reply } => {
                let _ = reply.send(self.register_subscriber(writer));
            }
            ServerInput::CurrentState { reply } => {
                let _ = reply.send(self.current_state_update());
            }
            ServerInput::HookEvent { event, reply } => {
                let _ = reply.send(self.handle_hook_event(&event));
            }
            ServerInput::SnapshotRequest { reply } => {
                let _ = reply.send(self.refresh_state_for_snapshot_request());
            }
            ServerInput::ActionRequest { request, reply } => {
                let _ = reply.send(self.handle_action_request(&request));
            }
            ServerInput::Unsubscribe { subscriber_id } => {
                self.state.subscribers.remove(&subscriber_id);
            }
            ServerInput::Shutdown { reply } => {
                self.shutdown.store(true, Ordering::SeqCst);
                let _ = reply.send(());
                return true;
            }
        }
        false
    }

    fn reconcile_timeout(&self) -> Option<Duration> {
        let deadline = self.state.pending_reconcile.as_ref()?.deadline;
        Some(deadline.saturating_duration_since(Instant::now()))
    }
}

impl SessionWorkdirTracker {
    fn record_hook(&mut self, event: &HookEvent) {
        let Some(session_id) = non_empty(event.session_id.as_deref()) else {
            return;
        };
        let Some(window_id) = non_empty(event.window_id.as_deref()) else {
            return;
        };
        let Some(path) = non_empty_path(event.pane_current_path.as_deref()) else {
            return;
        };
        let observed_order = self.next_observed_order();

        self.record_window(
            session_id,
            window_id,
            event.window_index,
            path.to_path_buf(),
            observed_order,
        );
    }

    fn record_session_workdirs(&mut self, session_id: &str, workdirs: &[WindowWorkdir]) {
        if workdirs.is_empty() {
            return;
        }

        let observed_order = self.next_observed_order();
        for workdir in workdirs {
            self.record_window(
                session_id,
                &workdir.window_id,
                Some(workdir.window_index),
                workdir.path.clone(),
                observed_order,
            );
        }
    }

    fn record_created_window(&mut self, session_id: &str, window_id: &str, path: PathBuf) {
        let observed_order = self.next_observed_order();
        self.record_window(session_id, window_id, None, path, observed_order);
    }

    fn resolve_session_path(
        &self,
        state: &DomainState,
        session_id: &str,
        live_workdirs: &[WindowWorkdir],
    ) -> Option<PathBuf> {
        if let Some(path) = resolve_path_from_live_workdirs(live_workdirs) {
            return Some(path);
        }

        let session = state.session(session_id);
        let tracked_session = self.sessions.get(session_id)?;
        let session_window_ids = session.map(|session| {
            state
                .session_windows(&session.id)
                .into_iter()
                .map(|(key, _)| key.window_id)
                .collect::<BTreeSet<_>>()
        });
        resolve_path_from_tracked_workdirs(
            tracked_session,
            session.and_then(|session| session.active_window_id.as_deref()),
            session_window_ids.as_ref(),
        )
    }

    fn prune_to_projection(&mut self, state: &DomainState) {
        let mut valid_windows_by_session: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for key in state.winlinks.keys() {
            valid_windows_by_session
                .entry(key.session_id.as_str())
                .or_default()
                .insert(key.window_id.as_str());
        }

        self.sessions.retain(|session_id, tracked_session| {
            let Some(valid_windows) = valid_windows_by_session.get(session_id.as_str()) else {
                return false;
            };

            tracked_session
                .windows
                .retain(|window_id, _| valid_windows.contains(window_id.as_str()));
            !tracked_session.windows.is_empty()
        });
    }

    fn next_observed_order(&mut self) -> u64 {
        let next = self.next_observed_order;
        self.next_observed_order = self.next_observed_order.saturating_add(1);
        next
    }

    fn record_window(
        &mut self,
        session_id: &str,
        window_id: &str,
        window_index: Option<u32>,
        path: PathBuf,
        observed_order: u64,
    ) {
        let tracked_session = self.sessions.entry(session_id.to_owned()).or_default();
        tracked_session.windows.insert(
            window_id.to_owned(),
            TrackedWindowWorkdir {
                path,
                window_index,
                observed_order,
            },
        );
    }
}

fn resolve_path_from_live_workdirs(workdirs: &[WindowWorkdir]) -> Option<PathBuf> {
    if let Some(active_path) = workdirs
        .iter()
        .find(|workdir| workdir.active)
        .map(|workdir| workdir.path.clone())
    {
        return Some(active_path);
    }

    let candidates = workdirs.iter().map(|workdir| WorkdirCandidate {
        window_id: workdir.window_id.as_str(),
        window_index: Some(workdir.window_index),
        path: &workdir.path,
        observed_order: 0,
    });
    choose_workdir(None, candidates)
}

fn resolve_path_from_tracked_workdirs(
    tracked_session: &TrackedSessionWorkdirs,
    active_window_id: Option<&str>,
    session_window_ids: Option<&BTreeSet<String>>,
) -> Option<PathBuf> {
    let candidates = tracked_session
        .windows
        .iter()
        .filter(|(window_id, _)| {
            session_window_ids
                .map(|window_ids| window_ids.contains(window_id.as_str()))
                .unwrap_or(true)
        })
        .map(|(window_id, tracked_window)| WorkdirCandidate {
            window_id: window_id.as_str(),
            window_index: tracked_window.window_index,
            path: &tracked_window.path,
            observed_order: tracked_window.observed_order,
        });
    choose_workdir(active_window_id, candidates)
}

#[derive(Clone, Copy)]
struct WorkdirCandidate<'a> {
    window_id: &'a str,
    window_index: Option<u32>,
    path: &'a Path,
    observed_order: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkdirPathScore {
    count: usize,
    observed_order: u64,
    lowest_window_index: Option<u32>,
}

fn choose_workdir<'a>(
    active_window_id: Option<&str>,
    candidates: impl IntoIterator<Item = WorkdirCandidate<'a>>,
) -> Option<PathBuf> {
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| non_empty_path(Some(candidate.path)).is_some())
        .collect();
    if candidates.is_empty() {
        return None;
    }

    if let Some(active_path) = active_window_id.and_then(|active_window_id| {
        candidates
            .iter()
            .find(|candidate| candidate.window_id == active_window_id)
            .map(|candidate| candidate.path.to_path_buf())
    }) {
        return Some(active_path);
    }

    let mut scores = BTreeMap::<PathBuf, WorkdirPathScore>::new();
    for candidate in &candidates {
        let score = scores
            .entry(candidate.path.to_path_buf())
            .or_insert_with(|| WorkdirPathScore {
                count: 0,
                observed_order: candidate.observed_order,
                lowest_window_index: candidate.window_index,
            });
        score.count += 1;
        score.observed_order = score.observed_order.max(candidate.observed_order);
        score.lowest_window_index = match (score.lowest_window_index, candidate.window_index) {
            (Some(current), Some(next)) => Some(current.min(next)),
            (current @ Some(_), None) => current,
            (None, next) => next,
        };
    }

    scores
        .into_iter()
        .max_by(|(left_path, left_score), (right_path, right_score)| {
            left_score
                .count
                .cmp(&right_score.count)
                .then(left_score.observed_order.cmp(&right_score.observed_order))
                .then_with(|| {
                    right_score
                        .lowest_window_index
                        .unwrap_or(u32::MAX)
                        .cmp(&left_score.lowest_window_index.unwrap_or(u32::MAX))
                })
                .then_with(|| right_path.cmp(left_path))
        })
        .map(|(path, _)| path)
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
        Self::bind_with_dependencies(
            sidecar_paths,
            tmux_socket_path,
            initial_state,
            refresh_from_tmux,
            tmux,
            Arc::new(TtyBellNotifier),
        )
    }

    fn bind_with_dependencies(
        sidecar_paths: SidecarPaths,
        tmux_socket_path: PathBuf,
        initial_state: ProjectionState,
        refresh_from_tmux: bool,
        tmux: Arc<dyn ServerTmuxOps>,
        bell_notifier: Arc<dyn BellNotifier>,
    ) -> Result<Self> {
        let initial_domain = initial_state.clone().into_domain_state();
        let server_id = format!("tmux-sidecar-{}", std::process::id());
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
        let shutdown = Arc::new(AtomicBool::new(false));
        let (core, core_thread) = ServerCore::spawn(
            ServerCoreState {
                tmux_socket_path: tmux_socket_path.clone(),
                generation: 1,
                state: initial_domain,
                workdirs: SessionWorkdirTracker::default(),
                pending_reconcile: None,
                subscribers: BTreeMap::new(),
                next_subscriber_id: 1,
            },
            refresh_from_tmux,
            tmux,
            bell_notifier,
            Arc::clone(&shutdown),
        );

        Ok(Self {
            listener,
            server_id,
            tmux_socket_path,
            core,
            core_thread,
            shutdown,
            cleanup: CleanupPaths {
                socket_path: sidecar_paths.socket_path,
                pid_path: sidecar_paths.pid_path,
            },
        })
    }

    fn run(self) -> Result<()> {
        let Self {
            listener,
            server_id,
            tmux_socket_path,
            core,
            core_thread,
            shutdown,
            cleanup,
        } = self;

        while !shutdown.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let server_id = server_id.clone();
                    let tmux_socket_path = tmux_socket_path.clone();
                    let core = core.clone();
                    thread::spawn(move || {
                        let _ = handle_connection(stream, server_id, tmux_socket_path, core);
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error).context("failed while accepting sidecar client"),
            }
        }

        drop(core);
        core_thread
            .join()
            .map_err(|_| anyhow::anyhow!("server core thread panicked"))??;
        drop(cleanup);
        Ok(())
    }
}

fn handle_connection(
    stream: UnixStream,
    server_id: String,
    tmux_socket_path: PathBuf,
    core: ServerCoreHandle,
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
                    send_message(
                        &writer,
                        &ServerMessage::HelloAck(HelloAck {
                            protocol_version: crate::ipc::PROTOCOL_VERSION,
                            server_id: server_id.clone(),
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
                if event.tmux_socket_path != tmux_socket_path {
                    send_error(
                        &writer,
                        "hook event tmux socket path did not match this server",
                    )?;
                    continue;
                }

                core.handle_hook_event(event)?;
                send_message(
                    &writer,
                    &ServerMessage::Ack(Ack {
                        kind: AckKind::HookEvent,
                    }),
                )?;
            }
            ClientMessage::Subscribe(_) => {
                if subscriber_id.is_none() {
                    let (registered_id, update) = core.subscribe(Arc::clone(&writer))?;
                    subscriber_id = Some(registered_id);
                    send_message(&writer, &ServerMessage::StateUpdated(update))?;
                    continue;
                }
                send_message(
                    &writer,
                    &ServerMessage::StateUpdated(core.current_state_update()?),
                )?;
            }
            ClientMessage::SnapshotRequest => match core.snapshot_request() {
                Ok(update) => {
                    send_message(&writer, &ServerMessage::StateUpdated(update))?;
                }
                Err(error) => {
                    send_error(&writer, format!("failed to refresh state: {error:#}"))?;
                }
            },
            ClientMessage::ActionRequest(request) => match core.handle_action_request(request) {
                Ok(result) => {
                    send_message(&writer, &ServerMessage::ActionResult(result))?;
                }
                Err(error) => {
                    send_error(
                        &writer,
                        format!("failed to process action request: {error:#}"),
                    )?;
                }
            },
            ClientMessage::Shutdown => {
                core.shutdown()?;
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
        core.unsubscribe(subscriber_id);
    }

    Ok(())
}

impl ServerCore {
    fn register_subscriber(&mut self, writer: SharedWriter) -> (u64, StateUpdated) {
        let subscriber_id = self.state.next_subscriber_id;
        self.state.next_subscriber_id += 1;
        self.state.subscribers.insert(subscriber_id, writer);
        (subscriber_id, self.current_state_update())
    }

    fn current_state_update(&self) -> StateUpdated {
        StateUpdated {
            generation: self.state.generation,
            state: ProjectionState::from_domain(&self.state.state),
        }
    }

    fn handle_hook_event(&mut self, event: &HookEvent) -> Result<()> {
        self.state.workdirs.record_hook(event);
        self.apply_hook_overlay(event);
        self.schedule_hook_reconciliation(event);
        Ok(())
    }

    fn refresh_state(&mut self) -> Result<StateUpdated> {
        let clear_bell_hints = self
            .take_pending_reconcile()
            .map(|pending| pending.clear_bell_hints)
            .unwrap_or_default();
        self.reconcile_from_snapshot(&clear_bell_hints, true, false)
            .map(|result| result.update)
    }

    fn refresh_state_for_snapshot_request(&mut self) -> Result<StateUpdated> {
        let clear_bell_hints = self
            .take_pending_reconcile()
            .map(|pending| pending.clear_bell_hints)
            .unwrap_or_default();
        self.reconcile_from_snapshot(&clear_bell_hints, false, false)
            .map(|result| result.update)
    }

    fn run_scheduled_reconcile(&mut self) -> Result<()> {
        let Some(mut pending) = self.take_pending_reconcile() else {
            return Ok(());
        };

        let result = self.reconcile_from_snapshot(&pending.clear_bell_hints, true, true)?;
        if !result.changed
            && !result.snapshot_changed
            && pending.dirty_scopes.requires_confirmation()
            && pending.attempts_remaining > 1
        {
            pending.attempts_remaining -= 1;
            pending.deadline = Instant::now() + HOOK_RECONCILE_RETRY_DELAY;
            self.state.pending_reconcile = Some(pending);
        }
        Ok(())
    }

    fn reconcile_after_action(&mut self, effect: &ActionEffect) -> Result<StateUpdated> {
        let clear_bell_hints = self
            .take_pending_reconcile()
            .map(|pending| pending.clear_bell_hints)
            .unwrap_or_default();
        if !self.refresh_from_tmux {
            return Ok(self.current_state_update());
        }

        for attempt in 0..ACTION_RECONCILE_ATTEMPTS {
            if attempt > 0 {
                thread::sleep(ACTION_RECONCILE_RETRY_DELAY);
            }

            let result = self.reconcile_from_snapshot(&clear_bell_hints, true, false)?;
            if effect.is_satisfied(&self.state.state) {
                return Ok(result.update);
            }
        }

        anyhow::bail!("{}", effect.unsatisfied_message())
    }

    fn reconcile_from_snapshot(
        &mut self,
        clear_bell_hints: &BTreeSet<BellClearHint>,
        broadcast_update: bool,
        notify_bells: bool,
    ) -> Result<ReconcileResult> {
        if !self.refresh_from_tmux {
            return Ok(ReconcileResult {
                update: self.current_state_update(),
                changed: false,
                snapshot_changed: false,
            });
        }

        let previous_state = self.state.state.clone();
        let tmux_socket_path = self.state.tmux_socket_path.clone();
        let mut next_state = self
            .tmux
            .snapshot_projection(&tmux_socket_path)?
            .into_domain_state();
        let snapshot_changed = next_state != previous_state;
        preserve_cached_bell_overlays(&previous_state, &mut next_state, clear_bell_hints);
        let (update, changed) =
            self.publish_state_update(next_state, broadcast_update, notify_bells);
        Ok(ReconcileResult {
            update,
            changed,
            snapshot_changed,
        })
    }

    fn take_pending_reconcile(&mut self) -> Option<PendingReconcile> {
        self.state.pending_reconcile.take()
    }

    fn apply_hook_overlay(&mut self, event: &HookEvent) {
        let mut next_state = self.state.state.clone();
        if !apply_hook_event_overlay(&mut next_state, event) {
            return;
        }
        let _ = self.publish_state_update(next_state, true, true);
    }

    fn schedule_hook_reconciliation(&mut self, event: &HookEvent) {
        if !self.refresh_from_tmux {
            return;
        }

        let dirty_scopes = dirty_scopes_for_hook(event);
        if dirty_scopes.is_empty() {
            return;
        }

        let deadline = Instant::now() + HOOK_RECONCILE_SETTLE_DELAY;
        let clear_bell_hints = bell_clear_hint_for_hook(event).into_iter().collect();
        match &mut self.state.pending_reconcile {
            Some(pending) => {
                pending.dirty_scopes.merge(dirty_scopes);
                pending.clear_bell_hints.extend(clear_bell_hints);
                pending.deadline = pending.deadline.max(deadline);
                pending.attempts_remaining = HOOK_RECONCILE_ATTEMPTS;
            }
            None => {
                self.state.pending_reconcile = Some(PendingReconcile {
                    dirty_scopes,
                    clear_bell_hints,
                    deadline,
                    attempts_remaining: HOOK_RECONCILE_ATTEMPTS,
                });
            }
        }
    }

    fn publish_state_update(
        &mut self,
        next_state: DomainState,
        broadcast_update: bool,
        notify_bells: bool,
    ) -> (StateUpdated, bool) {
        if next_state == self.state.state {
            return (self.current_state_update(), false);
        }

        if notify_bells {
            self.notify_new_bell_transitions(&self.state.state, &next_state);
        }

        self.state.generation += 1;
        self.state.workdirs.prune_to_projection(&next_state);
        self.state.state = next_state;
        let update = StateUpdated {
            generation: self.state.generation,
            state: ProjectionState::from_domain(&self.state.state),
        };

        if broadcast_update {
            self.broadcast(ServerMessage::StateUpdated(update.clone()));
        }
        (update, true)
    }

    fn notify_new_bell_transitions(&self, previous_state: &DomainState, next_state: &DomainState) {
        let repeat_count = count_new_bell_transitions(previous_state, next_state);
        if repeat_count == 0 {
            return;
        }

        let tty_paths = bell_notification_ttys(next_state);
        if tty_paths.is_empty() {
            return;
        }

        if let Err(error) = self.bell_notifier.notify(&tty_paths, repeat_count) {
            eprintln!("{error:#}");
        }
    }

    fn broadcast(&mut self, message: ServerMessage) {
        let mut stale_subscribers = Vec::new();
        for (subscriber_id, writer) in &self.state.subscribers {
            if send_message(writer, &message).is_err() {
                stale_subscribers.push(*subscriber_id);
            }
        }

        for subscriber_id in stale_subscribers {
            self.state.subscribers.remove(&subscriber_id);
        }
    }

    fn handle_action_request(&mut self, request: &ActionRequest) -> ActionResult {
        let tmux_socket_path = self.state.tmux_socket_path.clone();
        let result = match &request.action {
            Action::CreateWindow { session_id, .. } => {
                match self.resolve_create_window_path(&tmux_socket_path, session_id) {
                    Ok(create_window_path) => self.handle_action_request_with_options(
                        request,
                        tmux_socket_path,
                        ActionExecutionOptions {
                            create_window_path,
                            ..ActionExecutionOptions::default()
                        },
                    ),
                    Err(error) => ActionResultKind::Error {
                        message: format!("failed to resolve working directory: {error:#}"),
                    },
                }
            }
            Action::CloseSession { session_id } => {
                let close_session_client_switch =
                    self.resolve_close_session_client_switch(request, session_id);
                self.handle_action_request_with_options(
                    request,
                    tmux_socket_path,
                    ActionExecutionOptions {
                        create_window_path: None,
                        close_session_client_switch,
                    },
                )
            }
            _ => self.handle_action_request_with_options(
                request,
                tmux_socket_path,
                ActionExecutionOptions::default(),
            ),
        };

        ActionResult {
            request_id: request.request_id.clone(),
            generation: self.state.generation,
            result,
        }
    }

    fn handle_action_request_with_options(
        &mut self,
        request: &ActionRequest,
        tmux_socket_path: PathBuf,
        action_options: ActionExecutionOptions,
    ) -> ActionResultKind {
        let action_result = self
            .tmux
            .execute_action(&tmux_socket_path, request, &action_options);
        if let Ok(ActionEffect::CreatedWindow {
            session_id,
            window_id,
            current_path: Some(current_path),
        }) = &action_result
        {
            self.state
                .workdirs
                .record_created_window(session_id, window_id, current_path.clone());
        };
        let refresh_result = match &action_result {
            Ok(effect) => self.reconcile_after_action(effect),
            Err(_) => self.refresh_state(),
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

    fn resolve_create_window_path(
        &mut self,
        tmux_socket_path: &Path,
        session_id: &str,
    ) -> Result<Option<PathBuf>> {
        let live_workdirs = self
            .tmux
            .inspect_session_workdirs(tmux_socket_path, session_id)?;
        self.state
            .workdirs
            .record_session_workdirs(session_id, &live_workdirs);
        Ok(self
            .state
            .workdirs
            .resolve_session_path(&self.state.state, session_id, &live_workdirs))
    }

    fn resolve_close_session_client_switch(
        &self,
        request: &ActionRequest,
        closing_session_id: &str,
    ) -> Option<CloseSessionClientSwitch> {
        let target_client = request
            .target_client
            .as_ref()
            .map(|name| ClientName(name.clone()))?;
        let target_client_state = self.state.state.visible_client(Some(&target_client))?;
        if target_client_state.session_id != closing_session_id {
            return None;
        }

        let fallback_session_id = self
            .state
            .state
            .clients
            .values()
            .filter(|client| client.session_id != closing_session_id)
            .max_by_key(|client| (client.activity, client.order))
            .map(|client| client.session_id.clone())
            .or_else(|| {
                self.state
                    .state
                    .ordered_sessions()
                    .into_iter()
                    .find(|session| session.id != closing_session_id)
                    .map(|session| session.id.clone())
            });

        Some(CloseSessionClientSwitch {
            fallback_session_id,
        })
    }
}

impl DirtyScopes {
    fn is_empty(&self) -> bool {
        !self.all
            && !self.alerts
            && !self.clients
            && self.sessions.is_empty()
            && self.winlinks.is_empty()
    }

    fn requires_confirmation(&self) -> bool {
        self.all || self.clients || !self.sessions.is_empty() || !self.winlinks.is_empty()
    }

    fn mark(&mut self, scope: DirtyScope) {
        if self.all {
            return;
        }

        match scope {
            DirtyScope::All => {
                self.all = true;
                self.alerts = false;
                self.clients = false;
                self.sessions.clear();
                self.winlinks.clear();
            }
            DirtyScope::Alerts => self.alerts = true,
            DirtyScope::Clients => self.clients = true,
            DirtyScope::Session(session_id) => {
                self.sessions.insert(session_id);
            }
            DirtyScope::Winlink(key) => {
                self.winlinks.insert(key);
            }
        }
    }

    fn merge(&mut self, other: Self) {
        if self.all {
            return;
        }
        if other.all {
            self.mark(DirtyScope::All);
            return;
        }

        self.alerts |= other.alerts;
        self.clients |= other.clients;
        self.sessions.extend(other.sessions);
        self.winlinks.extend(other.winlinks);
    }
}

impl BellClearHint {
    fn matches(&self, key: &WinlinkKey, window_index: u32) -> bool {
        if self.session_id != key.session_id {
            return false;
        }

        self.window_id.as_deref() == Some(key.window_id.as_str())
            || self.window_index == Some(window_index)
    }
}

fn dirty_scopes_for_hook(event: &HookEvent) -> DirtyScopes {
    let mut dirty = DirtyScopes::default();
    match event.event {
        HookName::AlertBell => {
            dirty.mark(DirtyScope::Alerts);
        }
        HookName::ClientAttached | HookName::ClientDetached | HookName::ClientSessionChanged => {
            dirty.mark(DirtyScope::Clients);
            if let Some(session_id) = non_empty(event.session_id.as_deref()) {
                dirty.mark(DirtyScope::Session(session_id.to_owned()));
            }
            if let Some(key) = hook_window_key(event) {
                dirty.mark(DirtyScope::Winlink(key));
            }
        }
        HookName::SessionWindowChanged | HookName::AfterSelectWindow => {
            dirty.mark(DirtyScope::Clients);
            if let Some(key) = hook_window_key(event) {
                dirty.mark(DirtyScope::Winlink(key));
            } else if let Some(session_id) = non_empty(event.session_id.as_deref()) {
                dirty.mark(DirtyScope::Session(session_id.to_owned()));
            } else {
                dirty.mark(DirtyScope::All);
            }
        }
        HookName::SessionRenamed | HookName::AfterRenameSession => {
            if let Some(session_id) = non_empty(event.session_id.as_deref()) {
                dirty.mark(DirtyScope::Session(session_id.to_owned()));
            } else {
                dirty.mark(DirtyScope::All);
            }
        }
        HookName::WindowRenamed | HookName::AfterRenameWindow => {
            if let Some(key) = hook_window_key(event) {
                dirty.mark(DirtyScope::Winlink(key));
            } else if let Some(session_id) = non_empty(event.session_id.as_deref()) {
                dirty.mark(DirtyScope::Session(session_id.to_owned()));
            } else {
                dirty.mark(DirtyScope::All);
            }
        }
        HookName::SessionCreated
        | HookName::SessionClosed
        | HookName::WindowLinked
        | HookName::WindowUnlinked
        | HookName::AfterNewSession
        | HookName::AfterNewWindow
        | HookName::AfterKillPane => {
            dirty.mark(DirtyScope::All);
        }
        HookName::WindowPaneChanged
        | HookName::WindowLayoutChanged
        | HookName::AlertActivity
        | HookName::AlertSilence => {}
    }
    dirty
}

fn bell_clear_hint_for_hook(event: &HookEvent) -> Option<BellClearHint> {
    if !matches!(
        event.event,
        HookName::AfterSelectWindow
            | HookName::ClientSessionChanged
            | HookName::SessionWindowChanged
    ) {
        return None;
    }

    let session_id = non_empty(event.session_id.as_deref())?.to_owned();
    let window_id = non_empty(event.window_id.as_deref()).map(str::to_owned);
    let window_index = event.window_index;
    if window_id.is_none() && window_index.is_none() {
        return None;
    }

    Some(BellClearHint {
        session_id,
        window_id,
        window_index,
    })
}

fn hook_window_key(event: &HookEvent) -> Option<WinlinkKey> {
    Some(WinlinkKey::new(
        non_empty(event.session_id.as_deref())?.to_owned(),
        non_empty(event.window_id.as_deref())?.to_owned(),
    ))
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
                ..
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

    fn is_satisfied(&self, state: &DomainState) -> bool {
        match self {
            ActionEffect::SwitchedSession {
                client_name,
                session_id,
            } => state
                .clients
                .values()
                .any(|client| client.name.0 == *client_name && client.session_id == *session_id),
            ActionEffect::SwitchedWindow {
                client_name,
                session_id,
                window_id,
            } => state.clients.values().any(|client| {
                client.name.0 == *client_name
                    && client.session_id == *session_id
                    && client.current_window_id.as_deref() == Some(window_id.as_str())
            }),
            ActionEffect::CreatedSession { session_id } => state.session(session_id).is_some(),
            ActionEffect::CreatedWindow {
                session_id,
                window_id,
                ..
            } => state.session_window(session_id, window_id).is_some(),
            ActionEffect::RenamedSession { session_id, name } => state
                .session(session_id)
                .map(|session| session.name == *name)
                .unwrap_or(false),
            ActionEffect::RenamedWindow { window_id, name } => state
                .winlinks
                .values()
                .any(|window| window.id == *window_id && window.name == *name),
            ActionEffect::ClosedSession { session_id } => state.session(session_id).is_none(),
            ActionEffect::ClosedWindow {
                session_id,
                window_id,
            } => state.session_window(session_id, window_id).is_none(),
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
                ..
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

fn count_new_bell_transitions(previous_state: &DomainState, next_state: &DomainState) -> usize {
    next_state
        .winlinks
        .iter()
        .filter(|(key, next_window)| {
            next_window.bell_flag
                && previous_state
                    .winlinks
                    .get(*key)
                    .map(|window| window.bell_flag)
                    .unwrap_or(false)
                    == false
        })
        .count()
}

fn bell_notification_ttys(state: &DomainState) -> Vec<PathBuf> {
    state
        .clients
        .values()
        .filter_map(|client| non_empty(Some(client.tty.as_str())).map(PathBuf::from))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn preserve_cached_bell_overlays(
    previous_state: &DomainState,
    next_state: &mut DomainState,
    clear_bell_hints: &BTreeSet<BellClearHint>,
) {
    let viewed_windows = next_state.viewed_window_keys();

    for (key, previous_window) in previous_state
        .winlinks
        .iter()
        .filter(|(_, window)| window.bell_flag)
    {
        let Some(next_window) = next_state.winlinks.get_mut(key) else {
            continue;
        };

        if next_window.bell_flag
            || viewed_windows.contains(key)
            || clear_bell_hints
                .iter()
                .any(|hint| hint.matches(key, previous_window.index))
        {
            continue;
        }

        next_window.bell_flag = true;
        next_window.activity = next_window.activity.max(previous_window.activity);
    }
}

fn apply_hook_event_overlay(state: &mut DomainState, event: &HookEvent) -> bool {
    let Some(window) = domain_window_for_hook_event(state, event) else {
        return false;
    };

    match event.event {
        HookName::AlertBell => {
            let mut changed = false;
            if !window.bell_flag {
                window.bell_flag = true;
                changed = true;
            }
            let activity_changed = update_activity_timestamp(window, event);
            changed || activity_changed
        }
        _ => false,
    }
}

fn domain_window_for_hook_event<'a>(
    state: &'a mut DomainState,
    event: &HookEvent,
) -> Option<&'a mut crate::domain::WindowState> {
    let session_id = non_empty(event.session_id.as_deref())?;
    let window_id = non_empty(event.window_id.as_deref());
    let window_index = event.window_index;
    if window_id.is_none() && window_index.is_none() {
        return None;
    }

    if let Some(window_id) = window_id {
        return state.session_window_mut(session_id, window_id);
    }

    state.session_window_by_index_mut(session_id, window_index?)
}

fn update_activity_timestamp(window: &mut crate::domain::WindowState, event: &HookEvent) -> bool {
    if let Some(timestamp_ms) = event.timestamp_ms {
        let activity = timestamp_ms / 1000;
        if window.activity != activity {
            window.activity = activity;
            return true;
        }
    }
    false
}

fn write_bel_to_tty(tty_path: &Path, payload: &[u8]) -> Result<()> {
    let mut tty = OpenOptions::new()
        .write(true)
        .open(tty_path)
        .with_context(|| format!("failed to open tmux client tty `{}`", tty_path.display()))?;
    tty.write_all(payload).with_context(|| {
        format!(
            "failed to write BEL to tmux client tty `{}`",
            tty_path.display()
        )
    })?;
    tty.flush().with_context(|| {
        format!(
            "failed to flush BEL to tmux client tty `{}`",
            tty_path.display()
        )
    })
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn non_empty_path(path: Option<&Path>) -> Option<&Path> {
    path.filter(|path| !path.as_os_str().is_empty())
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

fn tmux_client(tmux_socket_path: &Path) -> TmuxCli {
    TmuxCli {
        socket_name: None,
        socket_path: Some(tmux_socket_path.to_path_buf()),
    }
}

fn execute_action_request(
    tmux: &impl Tmux,
    request: &ActionRequest,
    options: &ActionExecutionOptions,
) -> Result<ActionEffect> {
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
                .create_window(
                    session_id,
                    name.as_deref(),
                    options.create_window_path.as_deref(),
                )
                .with_context(|| match name {
                    Some(name) => {
                        format!("failed to create window `{name}` in session `{session_id}`")
                    }
                    None => format!("failed to create window in session `{session_id}`"),
                })?;
            ActionEffect::CreatedWindow {
                session_id: session_id.clone(),
                window_id,
                current_path: options.create_window_path.clone(),
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
            if let Some(close_session_client_switch) = &options.close_session_client_switch {
                let client = tmux
                    .resolve_target_client(request.target_client.as_deref())
                    .context("failed to resolve target tmux client")?;
                if tmux.switch_client_to_last_session(&client).is_err() {
                    if let Some(fallback_session_id) =
                        close_session_client_switch.fallback_session_id.as_deref()
                    {
                        tmux.switch_to(&client, WindowTarget::Session(fallback_session_id.to_owned()))
                            .with_context(|| {
                                format!(
                                    "failed to switch client `{}` away from closing session `{session_id}`",
                                    client.0
                                )
                            })?;
                    }
                }
            }
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

    use super::{
        ActionEffect, ActionExecutionOptions, BellNotifier, CloseSessionClientSwitch, Server,
        ServerTmuxOps, execute_action_request,
    };
    use crate::ipc::{
        Ack, AckKind, Action, ActionOutcome, ActionRequest, ActionResult, ActionResultKind,
        ClientKind, ClientMessage, Hello, HookEvent, HookName, PROTOCOL_VERSION, ProjectionClient,
        ProjectionSession, ProjectionState, ProjectionWindow, ServerMessage, SidecarPaths,
        StateUpdated,
    };
    use crate::{
        model::{ClientName, SessionId, WindowId, WindowTarget},
        tmux::{Tmux, TmuxError, WindowWorkdir},
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

        fn recv_timeout(&mut self, timeout: Duration) -> Result<Option<ServerMessage>> {
            self.reader
                .get_mut()
                .set_read_timeout(Some(timeout))
                .context("failed to configure test client read timeout")?;
            let result = match crate::ipc::read_message(&mut self.reader) {
                Ok(message) => Ok(message),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error).context("failed to read test client message"),
            };
            let reset = self
                .reader
                .get_mut()
                .set_read_timeout(None)
                .context("failed to reset test client read timeout");

            match (result, reset) {
                (Err(error), _) => Err(error),
                (Ok(_), Err(error)) => Err(error),
                (Ok(message), Ok(())) => Ok(message),
            }
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
        SwitchClientToLastSession {
            client: String,
        },
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
            current_path: Option<PathBuf>,
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

        fn switch_client_to_last_session(&self, client: &ClientName) -> Result<(), TmuxError> {
            self.calls.lock().expect("tmux call log poisoned").push(
                RecordedTmuxCall::SwitchClientToLastSession {
                    client: client.0.clone(),
                },
            );
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
            current_path: Option<&Path>,
        ) -> Result<WindowId, TmuxError> {
            self.calls.lock().expect("tmux call log poisoned").push(
                RecordedTmuxCall::CreateWindow {
                    session_id: session.clone(),
                    name: name.map(str::to_owned),
                    current_path: current_path.map(Path::to_path_buf),
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
        snapshot_calls: Mutex<usize>,
        session_workdirs: Mutex<VecDeque<std::result::Result<Vec<WindowWorkdir>, String>>>,
        action_results: Mutex<VecDeque<std::result::Result<ActionEffect, String>>>,
        requests: Mutex<Vec<ActionRequest>>,
        action_options: Mutex<Vec<ActionExecutionOptions>>,
    }

    impl MockServerTmux {
        fn new(
            snapshots: impl Into<VecDeque<ProjectionState>>,
            action_results: impl Into<VecDeque<std::result::Result<ActionEffect, String>>>,
        ) -> Self {
            Self {
                snapshots: Mutex::new(snapshots.into()),
                snapshot_calls: Mutex::new(0),
                session_workdirs: Mutex::new(VecDeque::new()),
                action_results: Mutex::new(action_results.into()),
                requests: Mutex::new(Vec::new()),
                action_options: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<ActionRequest> {
            self.requests
                .lock()
                .expect("tmux requests poisoned")
                .clone()
        }

        fn action_options(&self) -> Vec<ActionExecutionOptions> {
            self.action_options
                .lock()
                .expect("tmux action options poisoned")
                .clone()
        }

        fn snapshot_calls(&self) -> usize {
            *self
                .snapshot_calls
                .lock()
                .expect("tmux snapshot call counter poisoned")
        }

        fn push_session_workdirs(&self, result: std::result::Result<Vec<WindowWorkdir>, String>) {
            self.session_workdirs
                .lock()
                .expect("tmux session workdirs poisoned")
                .push_back(result);
        }
    }

    impl ServerTmuxOps for MockServerTmux {
        fn snapshot_projection(&self, _tmux_socket_path: &Path) -> Result<ProjectionState> {
            *self
                .snapshot_calls
                .lock()
                .expect("tmux snapshot call counter poisoned") += 1;
            self.snapshots
                .lock()
                .expect("tmux snapshots poisoned")
                .pop_front()
                .context("missing mock snapshot response")
        }

        fn inspect_session_workdirs(
            &self,
            _tmux_socket_path: &Path,
            _session_id: &str,
        ) -> Result<Vec<WindowWorkdir>> {
            match self
                .session_workdirs
                .lock()
                .expect("tmux session workdirs poisoned")
                .pop_front()
            {
                Some(Ok(workdirs)) => Ok(workdirs),
                Some(Err(message)) => Err(anyhow!("{message}")),
                None => Ok(Vec::new()),
            }
        }

        fn execute_action(
            &self,
            _tmux_socket_path: &Path,
            request: &ActionRequest,
            options: &ActionExecutionOptions,
        ) -> Result<ActionEffect> {
            self.requests
                .lock()
                .expect("tmux requests poisoned")
                .push(request.clone());
            self.action_options
                .lock()
                .expect("tmux action options poisoned")
                .push(options.clone());
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

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedBellNotification {
        tty_paths: Vec<PathBuf>,
        repeat_count: usize,
    }

    #[derive(Debug, Default)]
    struct MockBellNotifier {
        calls: Mutex<Vec<RecordedBellNotification>>,
        failures: Mutex<VecDeque<String>>,
    }

    impl MockBellNotifier {
        fn calls(&self) -> Vec<RecordedBellNotification> {
            self.calls
                .lock()
                .expect("bell notifier calls poisoned")
                .clone()
        }

        fn push_failure(&self, message: &str) {
            self.failures
                .lock()
                .expect("bell notifier failures poisoned")
                .push_back(message.to_owned());
        }
    }

    impl BellNotifier for MockBellNotifier {
        fn notify(&self, tty_paths: &[PathBuf], repeat_count: usize) -> Result<()> {
            self.calls
                .lock()
                .expect("bell notifier calls poisoned")
                .push(RecordedBellNotification {
                    tty_paths: tty_paths.to_vec(),
                    repeat_count,
                });

            match self
                .failures
                .lock()
                .expect("bell notifier failures poisoned")
                .pop_front()
            {
                Some(message) => Err(anyhow!(message)),
                None => Ok(()),
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
        add_projection_client_with_tty(state, name, session_id, window_id, "/dev/pts/1");
    }

    fn add_projection_client_with_tty(
        state: &mut ProjectionState,
        name: &str,
        session_id: &str,
        window_id: &str,
        tty: &str,
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
            tty: tty.to_owned(),
        });
    }

    fn window_workdir(
        path: &Path,
        window_id: &str,
        window_index: u32,
        active: bool,
    ) -> WindowWorkdir {
        WindowWorkdir {
            window_id: window_id.to_owned(),
            window_index,
            active,
            path: path.to_path_buf(),
        }
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
    fn alert_hook_rings_all_unique_client_ttys_on_new_bell_transition() -> Result<()> {
        let sandbox = TestDir::new("server-bell-notify-alert-hook")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "build");
        add_projection_client_with_tty(&mut initial_state, "client-1", "$1", "@1", "/dev/pts/1");
        add_projection_client_with_tty(&mut initial_state, "client-2", "$1", "@1", "/dev/pts/1");
        add_projection_client_with_tty(&mut initial_state, "client-3", "$1", "@1", "/dev/pts/2");
        let bell_notifier = std::sync::Arc::new(MockBellNotifier::default());
        let server = Server::bind_with_dependencies(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state.clone(),
            false,
            std::sync::Arc::new(MockServerTmux::new(Vec::<ProjectionState>::new(), vec![])),
            bell_notifier.clone(),
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
            pane_current_path: None,
            client_name: None,
            timestamp_ms: None,
        }))?;

        let mut alerted_state = initial_state;
        alerted_state.sessions[0].windows[0].bell_flag = true;
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

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(
            bell_notifier.calls(),
            vec![RecordedBellNotification {
                tty_paths: vec![PathBuf::from("/dev/pts/1"), PathBuf::from("/dev/pts/2")],
                repeat_count: 1,
            }]
        );

        Ok(())
    }

    #[test]
    fn alert_hook_does_not_ring_for_already_latched_bell() -> Result<()> {
        let sandbox = TestDir::new("server-bell-notify-latched")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "build");
        initial_state.sessions[0].windows[0].bell_flag = true;
        add_projection_client(&mut initial_state, "client-1", "$1", "@1");
        let bell_notifier = std::sync::Arc::new(MockBellNotifier::default());
        let server = Server::bind_with_dependencies(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state,
            false,
            std::sync::Arc::new(MockServerTmux::new(Vec::<ProjectionState>::new(), vec![])),
            bell_notifier.clone(),
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
            pane_current_path: None,
            client_name: None,
            timestamp_ms: None,
        }))?;

        assert_eq!(
            hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );
        assert_eq!(subscriber.recv_timeout(Duration::from_millis(150))?, None);

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;
        assert!(bell_notifier.calls().is_empty());

        Ok(())
    }

    #[test]
    fn snapshot_request_does_not_ring_for_snapshot_discovered_bells() -> Result<()> {
        let sandbox = TestDir::new("server-bell-notify-snapshot-query")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "build");
        add_projection_client(&mut initial_state, "client-1", "$1", "@1");
        let mut refreshed_state = initial_state.clone();
        refreshed_state.sessions[0].windows[0].bell_flag = true;
        let bell_notifier = std::sync::Arc::new(MockBellNotifier::default());
        let server = Server::bind_with_dependencies(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state,
            true,
            std::sync::Arc::new(MockServerTmux::new(vec![refreshed_state.clone()], vec![])),
            bell_notifier.clone(),
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut query = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Control)?;
        query.send(&ClientMessage::SnapshotRequest)?;
        assert_eq!(
            query.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: refreshed_state,
            })
        );

        drop(query);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;
        assert!(bell_notifier.calls().is_empty());

        Ok(())
    }

    #[test]
    fn hook_reconcile_rings_once_per_newly_alerted_window() -> Result<()> {
        let sandbox = TestDir::new("server-bell-notify-reconcile")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "build");
        add_projection_window(&mut initial_state, "$1", "@2", "tests", 1, false);
        add_projection_client(&mut initial_state, "client-1", "$1", "@1");
        let mut refreshed_state = initial_state.clone();
        refreshed_state.sessions[0].windows[0].bell_flag = true;
        refreshed_state.sessions[0].windows[1].bell_flag = true;
        let bell_notifier = std::sync::Arc::new(MockBellNotifier::default());
        let server = Server::bind_with_dependencies(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state,
            true,
            std::sync::Arc::new(MockServerTmux::new(vec![refreshed_state.clone()], vec![])),
            bell_notifier.clone(),
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
            pane_current_path: None,
            client_name: None,
            timestamp_ms: None,
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
                state: refreshed_state,
            })
        );

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(
            bell_notifier.calls(),
            vec![RecordedBellNotification {
                tty_paths: vec![PathBuf::from("/dev/pts/1")],
                repeat_count: 2,
            }]
        );

        Ok(())
    }

    #[test]
    fn bell_notifier_failures_do_not_block_hook_updates() -> Result<()> {
        let sandbox = TestDir::new("server-bell-notify-failure")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "build");
        add_projection_client(&mut initial_state, "client-1", "$1", "@1");
        let bell_notifier = std::sync::Arc::new(MockBellNotifier::default());
        bell_notifier.push_failure("tty write failed");
        let server = Server::bind_with_dependencies(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state.clone(),
            false,
            std::sync::Arc::new(MockServerTmux::new(Vec::<ProjectionState>::new(), vec![])),
            bell_notifier.clone(),
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
            pane_current_path: None,
            client_name: None,
            timestamp_ms: None,
        }))?;

        let mut alerted_state = initial_state;
        alerted_state.sessions[0].windows[0].bell_flag = true;
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

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(
            bell_notifier.calls(),
            vec![RecordedBellNotification {
                tty_paths: vec![PathBuf::from("/dev/pts/1")],
                repeat_count: 1,
            }]
        );

        Ok(())
    }

    fn assert_hook_refresh_retries_until_active_window_projected(event: HookName) -> Result<()> {
        let sandbox = TestDir::new("server-active-window-hook-retry")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "shell");
        add_projection_window(&mut initial_state, "$1", "@2", "editor", 1, false);
        add_projection_client(&mut initial_state, "client-1", "$1", "@1");
        let stale_state = initial_state.clone();
        let mut refreshed_state = initial_state.clone();
        refreshed_state.sessions[0].active_window_id = Some(String::from("@2"));
        refreshed_state.sessions[0].windows[0].active = false;
        refreshed_state.sessions[0].windows[1].active = true;
        refreshed_state.clients[0].current_window_id = Some(String::from("@2"));
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
            event,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@2")),
            window_index: Some(1),
            pane_id: Some(String::from("%2")),
            pane_current_path: None,
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
            pane_current_path: None,
            client_name: None,
            timestamp_ms: Some(42),
        }))?;
        assert_eq!(
            hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );
        assert_eq!(subscriber.recv_timeout(Duration::from_millis(150))?, None);

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
            pane_current_path: None,
            client_name: None,
            timestamp_ms: Some(1_000),
        }))?;

        let mut alerted_state = initial_state;
        alerted_state.sessions[0].windows[0].bell_flag = true;
        alerted_state.sessions[0].windows[0].activity = 1;
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
                state: alerted_state,
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
            tmux.clone(),
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
            pane_current_path: None,
            client_name: None,
            timestamp_ms: Some(42_000),
        }))?;

        let mut expected_state = snapshot_without_alert;
        expected_state.sessions[0].windows[0].bell_flag = true;
        expected_state.sessions[0].windows[0].activity = 42;
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
                state: expected_state,
            })
        );
        assert_eq!(subscriber.recv_timeout(Duration::from_millis(150))?, None);

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(tmux.snapshot_calls(), 1);

        Ok(())
    }

    #[test]
    fn retained_bell_alert_survives_later_non_alert_hook_refresh() -> Result<()> {
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
            tmux.clone(),
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
            pane_current_path: None,
            client_name: None,
            timestamp_ms: None,
        }))?;

        assert_eq!(
            hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );
        assert_eq!(subscriber.recv_timeout(Duration::from_millis(150))?, None);

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(tmux.snapshot_calls(), 1);

        Ok(())
    }

    #[test]
    fn retained_bell_alert_clears_on_select_hook_when_snapshot_has_no_alert() -> Result<()> {
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
            pane_current_path: None,
            client_name: None,
            timestamp_ms: None,
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
                state: snapshot_without_alert,
            })
        );

        drop(hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;

        Ok(())
    }

    #[test]
    fn retained_bell_alert_clears_on_action_refresh_when_client_views_window() -> Result<()> {
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
                generation: 2,
                result: ActionResultKind::Ok { outcome: None },
            })
        );

        shutdown_server(&sidecar_paths.socket_path)?;
        drop(client);
        server_thread.join().expect("server thread panicked")?;

        Ok(())
    }

    #[test]
    fn retained_bell_alert_clears_on_hook_refresh_when_client_views_window() -> Result<()> {
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
            pane_current_path: None,
            client_name: Some(String::from("client-1")),
            timestamp_ms: None,
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
                state: snapshot_without_alert,
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
            .map(|request| {
                execute_action_request(&tmux, request, &ActionExecutionOptions::default())
            })
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
                    current_path: None,
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
                    current_path: None,
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
    fn execute_action_request_switches_target_client_before_closing_session() -> Result<()> {
        let tmux = RecordingTmux::default();
        let request = ActionRequest {
            request_id: String::from("req-close-active"),
            target_client: Some(String::from("client-1")),
            action: Action::CloseSession {
                session_id: String::from("$1"),
            },
        };

        let effect = execute_action_request(
            &tmux,
            &request,
            &ActionExecutionOptions {
                create_window_path: None,
                close_session_client_switch: Some(CloseSessionClientSwitch {
                    fallback_session_id: Some(String::from("$2")),
                }),
            },
        )?;

        assert_eq!(
            tmux.calls(),
            vec![
                RecordedTmuxCall::ResolveTargetClient(Some(String::from("client-1"))),
                RecordedTmuxCall::SwitchClientToLastSession {
                    client: String::from("resolved-client"),
                },
                RecordedTmuxCall::CloseSession {
                    session_id: String::from("$1"),
                },
            ]
        );
        assert_eq!(
            effect,
            ActionEffect::ClosedSession {
                session_id: String::from("$1"),
            }
        );

        Ok(())
    }

    #[test]
    fn tracked_session_workdirs_prefer_active_window_paths() {
        let tmux_socket_path = Path::new("/tmp/tmux.sock");
        let mut tracker = super::SessionWorkdirTracker::default();
        let mut projection =
            projection_state_with_window(tmux_socket_path, "$1", "work", "@1", "shell");
        add_projection_window(&mut projection, "$1", "@2", "editor", 1, true);
        tracker.record_hook(&HookEvent {
            tmux_socket_path: tmux_socket_path.to_path_buf(),
            event: HookName::AfterSelectWindow,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@1")),
            window_index: Some(0),
            pane_id: Some(String::from("%1")),
            pane_current_path: Some(Path::new("/tmp/one").to_path_buf()),
            client_name: None,
            timestamp_ms: None,
        });
        tracker.record_hook(&HookEvent {
            tmux_socket_path: tmux_socket_path.to_path_buf(),
            event: HookName::AfterSelectWindow,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@2")),
            window_index: Some(1),
            pane_id: Some(String::from("%2")),
            pane_current_path: Some(Path::new("/tmp/two").to_path_buf()),
            client_name: None,
            timestamp_ms: None,
        });

        let domain = projection.to_domain_state();
        assert_eq!(
            tracker.resolve_session_path(&domain, "$1", &[]),
            Some(Path::new("/tmp/two").to_path_buf())
        );
    }

    #[test]
    fn create_window_actions_use_resolved_session_workdirs() -> Result<()> {
        let sandbox = TestDir::new("server-create-window-workdir")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "shell");
        add_projection_window(&mut initial_state, "$1", "@2", "tests", 1, true);
        let mut refreshed_state = initial_state.clone();
        add_projection_window(&mut refreshed_state, "$1", "@3", "created", 2, false);
        let tmux = std::sync::Arc::new(MockServerTmux::new(
            vec![refreshed_state.clone()],
            vec![Ok(ActionEffect::CreatedWindow {
                session_id: String::from("$1"),
                window_id: String::from("@3"),
                current_path: Some(Path::new("/tmp/active").to_path_buf()),
            })],
        ));
        tmux.push_session_workdirs(Ok(vec![
            window_workdir(Path::new("/tmp/other"), "@1", 0, false),
            window_workdir(Path::new("/tmp/active"), "@2", 1, true),
        ]));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state,
            true,
            tmux.clone(),
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut client = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        client.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = client.recv()?;

        let request = ActionRequest {
            request_id: String::from("req-create-window-workdir"),
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
                request_id: request.request_id,
                generation: 2,
                result: ActionResultKind::Ok {
                    outcome: Some(ActionOutcome::CreatedWindow {
                        session_id: String::from("$1"),
                        window_id: String::from("@3"),
                    }),
                },
            })
        );

        shutdown_server(&sidecar_paths.socket_path)?;
        drop(client);
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(
            tmux.action_options(),
            vec![ActionExecutionOptions {
                create_window_path: Some(Path::new("/tmp/active").to_path_buf()),
                close_session_client_switch: None,
            }]
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
                current_path: None,
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
                generation: 2,
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
                generation: 2,
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
            pane_current_path: None,
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
    fn hook_reconciliation_coalesces_multiple_dirty_scopes() -> Result<()> {
        let sandbox = TestDir::new("server-hook-coalesce")?;
        let tmux_socket_path = sandbox.path.join("tmux.sock");
        let sidecar_paths = SidecarPaths::from_runtime_dir(&tmux_socket_path, Some(&sandbox.path));
        let mut initial_state =
            projection_state_with_window(&tmux_socket_path, "$1", "work", "@1", "shell");
        add_projection_window(&mut initial_state, "$1", "@2", "editor", 1, false);
        add_projection_client(&mut initial_state, "client-1", "$1", "@1");
        let mut reconciled_state = initial_state.clone();
        reconciled_state.sessions[0].name = String::from("renamed");
        reconciled_state.sessions[0].active_window_id = Some(String::from("@2"));
        reconciled_state.sessions[0].windows[0].active = false;
        reconciled_state.sessions[0].windows[1].active = true;
        reconciled_state.clients[0].current_window_id = Some(String::from("@2"));
        let tmux = std::sync::Arc::new(MockServerTmux::new(vec![reconciled_state.clone()], vec![]));
        let server = Server::bind_with_tmux(
            sidecar_paths.clone(),
            tmux_socket_path.clone(),
            initial_state,
            true,
            tmux.clone(),
        )?;

        let server_thread = thread::spawn(move || server.run());

        let mut subscriber = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Ui)?;
        subscriber.send(&ClientMessage::Subscribe(Default::default()))?;
        let _ = subscriber.recv()?;

        let mut rename_hook = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Hook)?;
        rename_hook.send(&ClientMessage::HookEvent(HookEvent {
            tmux_socket_path: tmux_socket_path.clone(),
            event: HookName::SessionRenamed,
            session_id: Some(String::from("$1")),
            window_id: None,
            window_index: None,
            pane_id: None,
            pane_current_path: None,
            client_name: None,
            timestamp_ms: None,
        }))?;

        let mut select_hook = RawClient::connect(&sidecar_paths.socket_path, ClientKind::Hook)?;
        select_hook.send(&ClientMessage::HookEvent(HookEvent {
            tmux_socket_path: tmux_socket_path.clone(),
            event: HookName::AfterSelectWindow,
            session_id: Some(String::from("$1")),
            window_id: Some(String::from("@2")),
            window_index: Some(1),
            pane_id: Some(String::from("%2")),
            pane_current_path: None,
            client_name: None,
            timestamp_ms: None,
        }))?;

        assert_eq!(
            rename_hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );
        assert_eq!(
            select_hook.recv()?,
            ServerMessage::Ack(Ack {
                kind: AckKind::HookEvent,
            })
        );
        assert_eq!(
            subscriber.recv()?,
            ServerMessage::StateUpdated(StateUpdated {
                generation: 2,
                state: reconciled_state,
            })
        );
        assert_eq!(subscriber.recv_timeout(Duration::from_millis(150))?, None);

        drop(rename_hook);
        drop(select_hook);
        drop(subscriber);
        shutdown_server(&sidecar_paths.socket_path)?;
        server_thread.join().expect("server thread panicked")?;
        assert_eq!(tmux.snapshot_calls(), 1);

        Ok(())
    }

    #[test]
    fn after_select_window_hook_retries_until_active_window_is_projected() -> Result<()> {
        assert_hook_refresh_retries_until_active_window_projected(HookName::AfterSelectWindow)
    }

    #[test]
    fn session_window_changed_hook_retries_until_active_window_is_projected() -> Result<()> {
        assert_hook_refresh_retries_until_active_window_projected(HookName::SessionWindowChanged)
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
                generation: 2,
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
                generation: 2,
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
