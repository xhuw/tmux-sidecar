use std::{
    env,
    fs::{self, OpenOptions},
    io::BufReader,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::{
    cli::HookArgs,
    ipc::{
        AckKind, Action, ActionRequest, ClientKind, ClientMessage, Hello, HookEvent,
        PROTOCOL_VERSION, ServerMessage, SidecarPaths, Subscribe, write_message,
    },
    tmux::command::{SocketOptions, run_tmux},
};

const SERVER_START_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub struct IpcClient {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

pub enum ReadStatus {
    Message(ServerMessage),
    Pending,
    Closed,
}

impl IpcClient {
    pub fn connect_or_spawn(tmux_socket_path: &Path, client_kind: ClientKind) -> Result<Self> {
        let stream = connect_or_spawn_stream(tmux_socket_path)?;
        let reader_stream = stream
            .try_clone()
            .context("failed to clone sidecar stream")?;
        let mut client = Self {
            reader: BufReader::new(reader_stream),
            writer: stream,
        };

        client.send(&ClientMessage::Hello(Hello {
            client_kind,
            protocol_version: PROTOCOL_VERSION,
        }))?;

        match client.read_required()? {
            ServerMessage::HelloAck(ack) if ack.protocol_version == PROTOCOL_VERSION => Ok(client),
            ServerMessage::HelloAck(ack) => bail!(
                "protocol mismatch: client expects {}, server replied with {}",
                PROTOCOL_VERSION,
                ack.protocol_version
            ),
            ServerMessage::Error(error) => bail!(error.message),
            message => bail!("unexpected handshake response: {message:?}"),
        }
    }

    pub fn send(&mut self, message: &ClientMessage) -> Result<()> {
        write_message(&mut self.writer, message).context("failed to send IPC message")
    }

    pub fn send_action_request(
        &mut self,
        target_client: Option<String>,
        action: Action,
    ) -> Result<String> {
        let request = ActionRequest::new(target_client, action);
        let request_id = request.request_id.clone();
        self.send(&ClientMessage::ActionRequest(request))?;
        Ok(request_id)
    }

    pub fn read(&mut self) -> Result<Option<ServerMessage>> {
        crate::ipc::read_message(&mut self.reader).context("failed to read IPC message")
    }

    pub fn read_status(&mut self) -> Result<ReadStatus> {
        match crate::ipc::read_message(&mut self.reader) {
            Ok(Some(message)) => Ok(ReadStatus::Message(message)),
            Ok(None) => Ok(ReadStatus::Closed),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(ReadStatus::Pending)
            }
            Err(error) => Err(error).context("failed to read IPC message"),
        }
    }

    pub fn read_required(&mut self) -> Result<ServerMessage> {
        self.read()?
            .ok_or_else(|| anyhow!("sidecar server closed the connection"))
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.reader
            .get_mut()
            .set_read_timeout(timeout)
            .context("failed to configure sidecar read timeout")
    }
}

pub fn ensure_server_running(tmux_socket_path: &Path) -> Result<()> {
    let _client = IpcClient::connect_or_spawn(tmux_socket_path, ClientKind::Control)?;
    Ok(())
}

pub fn run_hook(args: HookArgs) -> Result<()> {
    send_hook_event(
        &args.socket_path,
        HookEvent {
            tmux_socket_path: args.socket_path.clone(),
            event: args.event,
            session_id: args.session_id,
            window_id: args.window_id,
            window_index: args.window_index,
            pane_id: args.pane_id,
            client_name: args.client_name,
            timestamp_ms: args.timestamp_ms,
        },
    )
}

pub fn send_hook_event(tmux_socket_path: &Path, event: HookEvent) -> Result<()> {
    let mut client = IpcClient::connect_or_spawn(tmux_socket_path, ClientKind::Hook)?;
    client.send(&ClientMessage::HookEvent(event))?;

    match client.read_required()? {
        ServerMessage::Ack(ack) if ack.kind == AckKind::HookEvent => Ok(()),
        ServerMessage::Error(error) => bail!(error.message),
        message => bail!("unexpected hook acknowledgement: {message:?}"),
    }
}

pub fn subscribe(tmux_socket_path: &Path, target_client: Option<String>) -> Result<IpcClient> {
    let mut client = IpcClient::connect_or_spawn(tmux_socket_path, ClientKind::Ui)?;
    client.send(&ClientMessage::Subscribe(Subscribe { target_client }))?;
    Ok(client)
}

pub fn shutdown_server(tmux_socket_path: &Path) -> Result<()> {
    let mut client = IpcClient::connect_or_spawn(tmux_socket_path, ClientKind::Control)?;
    client.send(&ClientMessage::Shutdown)?;

    match client.read_required()? {
        ServerMessage::Ack(ack) if ack.kind == AckKind::Shutdown => Ok(()),
        ServerMessage::Error(error) => bail!(error.message),
        message => bail!("unexpected shutdown acknowledgement: {message:?}"),
    }
}

pub fn resolve_tmux_socket_path(
    socket_name: Option<String>,
    socket_path: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(socket_path) = socket_path {
        return Ok(socket_path);
    }

    let socket = SocketOptions::from_parts(socket_name, None);
    let output = run_tmux(&socket, ["display-message", "-p", "#{socket_path}"])
        .context("failed to resolve tmux socket path")?;
    let resolved = output.lines().next().map(str::trim).unwrap_or_default();

    if resolved.is_empty() {
        bail!("tmux returned an empty socket path");
    }

    Ok(PathBuf::from(resolved))
}

fn connect_or_spawn_stream(tmux_socket_path: &Path) -> Result<UnixStream> {
    let sidecar_paths = SidecarPaths::from_tmux_socket_path(tmux_socket_path);
    fs::create_dir_all(&sidecar_paths.runtime_dir)
        .context("failed to create sidecar runtime dir")?;

    if let Ok(stream) = UnixStream::connect(&sidecar_paths.socket_path) {
        return Ok(stream);
    }

    let spawn_lock = acquire_spawn_lock(&sidecar_paths.lock_path, &sidecar_paths.socket_path)?;

    if let Ok(stream) = UnixStream::connect(&sidecar_paths.socket_path) {
        return Ok(stream);
    }

    if sidecar_paths.socket_path.exists() {
        fs::remove_file(&sidecar_paths.socket_path).with_context(|| {
            format!(
                "failed to remove stale sidecar socket `{}`",
                sidecar_paths.socket_path.display()
            )
        })?;
    }

    if spawn_lock.held {
        spawn_server_process(tmux_socket_path)?;
    }

    wait_for_server_socket(&sidecar_paths.socket_path)
}

fn wait_for_server_socket(socket_path: &Path) -> Result<UnixStream> {
    let deadline = Instant::now() + SERVER_START_TIMEOUT;

    loop {
        match UnixStream::connect(socket_path) {
            Ok(stream) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionAborted
                ) && Instant::now() < deadline =>
            {
                thread::sleep(CONNECT_RETRY_DELAY);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to connect to sidecar socket `{}`",
                        socket_path.display()
                    )
                });
            }
        }
    }
}

fn spawn_server_process(tmux_socket_path: &Path) -> Result<()> {
    let executable = env::var_os("CARGO_BIN_EXE_tmux-sidecar")
        .map(PathBuf::from)
        .unwrap_or(env::current_exe().context("failed to resolve current executable")?);

    Command::new(executable)
        .arg("server")
        .arg("--socket-path")
        .arg(tmux_socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn sidecar server")?;

    Ok(())
}

struct SpawnLock {
    path: PathBuf,
    held: bool,
}

impl Drop for SpawnLock {
    fn drop(&mut self) {
        if self.held {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn acquire_spawn_lock(lock_path: &Path, socket_path: &Path) -> Result<SpawnLock> {
    let deadline = Instant::now() + SERVER_START_TIMEOUT;

    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(_) => {
                return Ok(SpawnLock {
                    path: lock_path.to_path_buf(),
                    held: true,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if let Ok(stream) = UnixStream::connect(socket_path) {
                    drop(stream);
                    return Ok(SpawnLock {
                        path: lock_path.to_path_buf(),
                        held: false,
                    });
                }

                if Instant::now() >= deadline {
                    let _ = fs::remove_file(lock_path);
                    continue;
                }

                thread::sleep(CONNECT_RETRY_DELAY);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to acquire `{}`", lock_path.display()));
            }
        }
    }
}
