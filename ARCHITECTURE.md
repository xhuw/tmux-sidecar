# Architecture

## Direction

tmux-sidecar should be rebuilt around tmux hooks and a local state server. The current polling UI was useful for the MVP, but it is the wrong center of gravity for reliable cross-session alerts: hooks should report tmux changes as they happen, a persistent local server should maintain the live projection of tmux state, and UI instances should subscribe to that server.

tmux remains the durable source of truth. The sidecar server owns only an in-memory cache, subscriptions, and command orchestration. If the server dies, hooks or the next UI invocation auto-spawn it and it reconstructs state from tmux.

## Goals

- Track sessions, windows, clients, and bell alerts across all sessions in the tmux server.
- Keep linked windows session-local: active state and alert state are keyed by the session/winlink, not just `window_id`.
- Push updates to every tmux-sidecar UI without requiring blind polling in the UI.
- Route UI actions through one tmux command boundary so create, close, rename, and switch operations reconcile through the same state engine as hook updates.
- Keep setup simple enough for `.tmux.conf` while avoiding hook commands that pass user-controlled names through a shell.
- Avoid persistent application state. State lives in tmux and in the server's memory only.

## Process model

```text
tmux server
  ├─ global tmux-sidecar hooks
  │    └─ run-shell -b "tmux-sidecar hook --event ... --socket-path ..."
  │
  ├─ short-lived hook command
  │    ├─ connects to sidecar Unix socket
  │    ├─ auto-spawns server if missing
  │    └─ sends HookEvent, exits
  │
  ├─ persistent tmux-sidecar server
  │    ├─ owns in-memory ServerState for one tmux socket
  │    ├─ bootstraps from full tmux snapshots
  │    ├─ debounces hook bursts into snapshot refreshes
  │    ├─ applies urgent alert events immediately when possible
  │    ├─ executes UI-requested tmux actions
  │    └─ broadcasts StateUpdated messages to UIs
  │
  └─ one or more tmux-sidecar UIs
       ├─ subscribe to server state
       ├─ keep local focus/edit/help/navigation state
       └─ send ActionRequest messages to the server
```

Run one server per tmux socket. The IPC socket path should be deterministic from the tmux socket path, for example:

```text
$XDG_RUNTIME_DIR/tmux-sidecar/<hash-of-tmux-socket-path>.sock
```

Use a lock file next to the socket during auto-spawn so simultaneous hook invocations do not start multiple servers. The server should remove stale sockets only after a failed connection proves no process is listening.

## CLI surface

| Command | Purpose |
| --- | --- |
| `tmux-sidecar` | Start the TUI. Resolve the tmux socket, ensure the server is running, subscribe, then render. |
| `tmux-sidecar server --socket-path <path>` | Internal persistent daemon for one tmux server. Bootstraps state, listens on the sidecar Unix socket, and exits when the tmux server disappears or after an idle timeout. |
| `tmux-sidecar server --kill [--socket-name <name>\|--socket-path <path>]` | Ask an existing sidecar server for the selected tmux socket to shut down without spawning a replacement. |
| `tmux-sidecar hook --socket-path <path> --event <name> [ids...]` | Short-lived hook entry point. Connects to the server, auto-spawns it if needed, sends one hook event, and exits quickly. |
| `tmux-sidecar install-hooks [--socket-path <path>]` | Installs or refreshes the tmux hook block and monitoring options for the current tmux server. Intended for `run-shell` from `.tmux.conf`. |
| `tmux-sidecar uninstall-hooks [--socket-path <path>]` | Removes only tmux-sidecar's indexed hook entries. |
| `tmux-sidecar init-plugin` | Prints the tmux config snippet users should add to `.tmux.conf`. |

The setup should prefer a tmux-native one-liner:

```tmux
run-shell -b 'tmux-sidecar install-hooks'
```

`source $(tmux-sidecar init-plugin)` should not be documented because tmux config files do not perform shell command substitution for `source-file`. `init-plugin` can instead print the `run-shell` line above plus comments, or users can run it from a shell to append the snippet to their config.

## Hook installation

Use indexed hook entries so tmux-sidecar does not clobber user hooks. tmux 3.0 supports hook array indexes, for example:

```tmux
set-hook -g 'alert-bell[900]' 'run-shell -b "tmux-sidecar hook --socket-path #{q:socket_path} --event alert-bell --session-id #{q:session_id} --window-id #{q:window_id} --window-index #{q:window_index} --pane-id #{q:pane_id}"'
```

Installation should first unset tmux-sidecar's reserved indexes, then set them again. Reserve a small high range such as `900..949`.

Use `#{q:...}` for every tmux format interpolated into `run-shell`; otherwise tmux IDs like `$1` will be expanded by the shell. Hook commands must pass only stable IDs, indexes, event names, booleans, and socket paths. Do not pass session or window names through hook shell commands.

Install hooks for these state-changing classes:

| Hook class | Hooks |
| --- | --- |
| Sessions | `session-created`, `session-closed`, `session-renamed`, `session-window-changed` |
| Windows/winlinks | `window-linked`, `window-unlinked`, `window-renamed`, `window-pane-changed`, `window-layout-changed` |
| Alerts | `alert-bell` |
| Clients | `client-attached`, `client-detached`, `client-session-changed` |
| Command fallback | `after-new-session`, `after-new-window`, `after-rename-session`, `after-rename-window`, `after-kill-pane`, `after-select-window` |

Some hooks overlap. That is acceptable because the server debounces updates and reconciles from snapshots.

`install-hooks` should also configure bell monitoring for existing and future windows:

```text
monitor-bell on
```

Apply this at bootstrap for every window and after any hook that may introduce a new winlink/window. If tmux supports global defaults reliably for the target version, set those too, but do not rely on defaults alone.

## IPC protocol

Keep IPC local and simple: newline-delimited JSON over Unix domain sockets is enough. Add `serde` and `serde_json`; avoid an async runtime unless the blocking implementation becomes unmanageable.

Messages from clients to the server:

```text
Hello { client_kind, protocol_version }
HookEvent { tmux_socket_path, event, session_id?, window_id?, window_index?, pane_id?, client_name?, timestamp? }
Subscribe { target_client? }
ActionRequest { request_id, target_client?, action }
SnapshotRequest
Shutdown
```

Messages from the server to clients:

```text
HelloAck { protocol_version, server_id }
StateUpdated { generation, state }
ActionResult { request_id, result }
Error { message }
```

The TUI should keep one subscription connection open. Hook commands should open a connection, send one `HookEvent`, wait for a small acknowledgement, and exit. Action requests can use the subscription connection or a short-lived request connection; prefer the subscription connection if that keeps ordering easier.

## Server state

```text
ServerState
  tmux_socket: TmuxSocket
  generation: u64
  last_full_snapshot_at: Instant
  sessions: BTreeMap<SessionId, SessionState>
  clients: BTreeMap<ClientName, ClientState>
  dirty: DirtySet

SessionState
  id: SessionId
  name: String
  attached_count: u32
  active_window_id: Option<WindowId>
  windows: BTreeMap<WinlinkKey, WindowState>

WinlinkKey
  session_id: SessionId
  window_id: WindowId

WindowState
  id: WindowId
  index: u32
  name: String
  active: bool
  activity: u64
  activity_flag: bool
  bell_flag: bool
  silence_flag: bool

ClientState
  name: ClientName
  session_id: SessionId
  current_window_id: Option<WindowId>
  activity: u64
  tty: String
```

State lives in memory only. Do not persist snapshots, alerts, focus, or UI state to disk. The only filesystem artifacts are the Unix socket and lock/pid files needed to run the daemon.

The server's cache is a projection, not an authority. Full snapshots from tmux overwrite cached names, indexes, active windows, clients, and alert flags. Hook events are used to wake the server immediately and to make urgent alert state visible before the next debounced snapshot completes.

## State update algorithm

Bootstrap:

1. Resolve tmux socket path.
2. Start or connect to the per-socket sidecar server.
3. Server runs a full snapshot using `list-sessions`, `list-windows -a`, and `list-clients`.
4. Server configures monitoring on all windows.
5. Server broadcasts generation `1`.

Hook handling:

1. Receive `HookEvent`.
2. If it is an alert hook and includes session/window identity, patch that winlink's alert flags immediately and broadcast a new generation.
3. Mark the relevant dirty scope: all state, one session, one window/winlink, or clients.
4. Schedule a debounced reconciliation, usually 25-100 ms later.
5. Reconcile by taking a full snapshot first. Partial refreshes can be added later only if tests prove they are necessary.
6. Apply monitoring options to newly discovered windows.
7. Broadcast only if the projected state changed.

The full snapshot after hooks is deliberate. It handles missed hooks, hook ordering differences between tmux versions, linked windows, renumbering, command failures, and user changes made outside sidecar.

## tmux command boundary

All tmux commands should remain behind one production adapter that passes arguments separately through `std::process::Command`. The hook installer is the exception because tmux's `run-shell` necessarily invokes a shell; minimize that risk by passing only quoted tmux IDs and socket paths.

Server action commands:

| Action | tmux command behavior |
| --- | --- |
| Switch session | `switch-client -c <client> -t <session_id>` |
| Switch window | `select-window -t <session_id>:<window_id>`, then `switch-client -c <client> -t <session_id>` |
| Create session | `new-session -d -P -F "#{session_id}" [-s <name>]`, then optionally switch target client |
| Create window | `new-window -d -P -F "#{window_id}" -t <session_id>: [-n <name>]`, then optionally switch target client |
| Rename session | `rename-session -t <session_id> <name>` |
| Rename window | `rename-window -t <window_id> <name>` |
| Close session | `kill-session -t <session_id>` |
| Close window | `kill-window -t <session_id>:<window_id>` |

After every successful or failed action, the server should reconcile from tmux and send an `ActionResult`. The UI must not keep speculative state after an action failure.

## UI architecture

The UI should become a client of the state server, not a tmux polling loop.

Local UI state:

```text
UiState
  projection: TmuxState
  target_client: Option<ClientName>
  focus: Focus
  mode: Mode
  navigation: NavigationState
  last_error: Option<ActionError>
```

The server owns tmux state. The UI owns focus, edit buffers, help mode, jump labels, mouse mapping, and terminal rendering. Rendering should remain pure from `UiState`.

Event loop:

```text
connect to server
subscribe and receive initial StateUpdated
enter alternate screen/raw mode

loop:
  draw UiState
  wait for terminal input, resize, or server message
  terminal input -> update local state or send ActionRequest
  server StateUpdated -> reconcile projection and preserve focus by stable row ID
  ActionResult error -> show footer error and rely on following StateUpdated
```

No UI timer should be required for tmux sync.

## Startup and failure behavior

- Fatal startup checks that do not need raw mode should still run before entering alternate screen.
- If the server is absent, UI and hooks should auto-spawn it.
- If the server cannot be spawned or contacted, print a concise error and exit non-zero.
- If tmux is unavailable or has no sessions, the server reports an error and exits.
- If the server connection drops while the UI is running, the UI should leave raw mode, report the disconnection, and exit rather than showing stale state.
- If a hook command cannot contact the server, it should try one auto-spawn and then exit non-zero quietly enough not to disrupt tmux.

## Testing strategy

Unit tests:

- IPC message encoding/decoding.
- Hook command generation, especially `#{q:...}` use and reserved hook indexes.
- Snapshot-to-state projection keyed by `(session_id, window_id)`.
- Debounce behavior and generation increments.
- UI focus reconciliation after server-pushed updates.

Integration tests against real isolated tmux servers:

- `install-hooks` installs indexed hooks without removing an unrelated user hook.
- Hook invocation auto-spawns the server.
- Creating, renaming, closing, linking, and unlinking sessions/windows cause state updates without UI polling.
- Bell alerts in a non-current session reach the server and UI subscription.
- Linked windows keep alert and active state session-local.
- UI actions for switch/create/rename/close reconcile through the server.

Manual release checks:

- Add the `run-shell -b 'tmux-sidecar install-hooks'` snippet to a real `.tmux.conf`, reload it, and verify hooks survive reload without duplicates.
- Start two UI instances attached to the same tmux server and confirm both update from one action/hook.
- Kill the sidecar server while tmux remains alive and verify the next hook or UI invocation reconstructs state.
