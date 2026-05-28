use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Result, anyhow};

use crate::{
    client::{IpcClient, ReadStatus},
    event::{self, AppEvent},
    ipc::ServerMessage,
};

const THREAD_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub enum UiEvent {
    Terminal(AppEvent),
    Server(ServerMessage),
    ServerDisconnected,
    RuntimeError(String),
}

#[derive(Debug)]
pub struct UiRuntime {
    tmux_socket_path: PathBuf,
    events: Receiver<UiEvent>,
    pending_events: VecDeque<UiEvent>,
    stop: Arc<AtomicBool>,
    input_handle: Option<JoinHandle<()>>,
    server_handle: Option<JoinHandle<()>>,
}

impl UiRuntime {
    pub fn spawn(subscription: IpcClient, tmux_socket_path: PathBuf) -> Self {
        let (sender, events) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));

        Self {
            tmux_socket_path,
            events,
            pending_events: VecDeque::new(),
            input_handle: Some(spawn_terminal_thread(sender.clone(), Arc::clone(&stop))),
            server_handle: Some(spawn_server_thread(subscription, sender, Arc::clone(&stop))),
            stop,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(tmux_socket_path: PathBuf, events: Receiver<UiEvent>) -> Self {
        Self {
            tmux_socket_path,
            events,
            pending_events: VecDeque::new(),
            stop: Arc::new(AtomicBool::new(false)),
            input_handle: None,
            server_handle: None,
        }
    }

    pub fn tmux_socket_path(&self) -> &Path {
        &self.tmux_socket_path
    }

    pub fn recv(&mut self) -> Result<UiEvent> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(event);
        }

        self.events
            .recv()
            .map_err(|_| anyhow!("ui runtime event channel closed"))
    }

    pub fn recv_timeout(&mut self, timeout: Duration) -> Result<Option<UiEvent>> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(Some(event));
        }

        match self.events.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(anyhow!("ui runtime event channel closed")),
        }
    }

    pub(crate) fn prepend_pending(&mut self, mut events: VecDeque<UiEvent>) {
        if events.is_empty() {
            return;
        }

        events.append(&mut self.pending_events);
        self.pending_events = events;
    }
}

impl Drop for UiRuntime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);

        if let Some(handle) = self.input_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.join();
        }
    }
}

fn spawn_terminal_thread(sender: Sender<UiEvent>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match crossterm::event::poll(THREAD_POLL_INTERVAL) {
                Ok(true) => match event::read_next() {
                    Ok(event) => {
                        if sender.send(UiEvent::Terminal(event)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(UiEvent::RuntimeError(format!(
                            "failed to read terminal event: {error:#}"
                        )));
                        break;
                    }
                },
                Ok(false) => {}
                Err(error) => {
                    let _ = sender.send(UiEvent::RuntimeError(format!(
                        "failed to poll terminal events: {error:#}"
                    )));
                    break;
                }
            }
        }
    })
}

fn spawn_server_thread(
    mut subscription: IpcClient,
    sender: Sender<UiEvent>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        if let Err(error) = subscription.set_read_timeout(Some(THREAD_POLL_INTERVAL)) {
            let _ = sender.send(UiEvent::RuntimeError(format!(
                "failed to configure sidecar event reader: {error:#}"
            )));
            return;
        }

        while !stop.load(Ordering::Relaxed) {
            match subscription.read_status() {
                Ok(ReadStatus::Message(message)) => {
                    if sender.send(UiEvent::Server(message)).is_err() {
                        break;
                    }
                }
                Ok(ReadStatus::Pending) => {}
                Ok(ReadStatus::Closed) => {
                    let _ = sender.send(UiEvent::ServerDisconnected);
                    break;
                }
                Err(error) => {
                    let _ = sender.send(UiEvent::RuntimeError(format!(
                        "failed to read sidecar event: {error:#}"
                    )));
                    break;
                }
            }
        }

        let _ = subscription.set_read_timeout(None);
    })
}
