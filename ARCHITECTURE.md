# Architecture

## Current shape

tmux-sidecar now runs as a hook-driven, per-tmux-socket sidecar daemon.

- tmux is the durable source of truth.
- The daemon owns only an in-memory projection, reconcile scheduling, subscriber fan-out, and per-session workdir hints.
- The TUI subscribes to pushed state updates and routes all tmux mutations back through the daemon.
- Startup does not restore a persisted cache. The TUI shows a short loading placeholder until the initial subscribed snapshot arrives.

## Goals

- Track sessions, windows, clients, and bell alerts across the selected tmux server.
- Keep linked windows session-local: active state and alert state are keyed by `(session_id, window_id)`, not just `window_id`.
- Push updates to every tmux-sidecar UI without blind tmux polling in the UI.
- Route switch/create/rename/close operations through one tmux command boundary so hooks and user actions reconcile through the same server core.
- Keep hook installation shell-safe by passing only quoted IDs, indexes, client names, paths, and socket paths.
- Avoid persistent application state. State lives in tmux and in the daemon's memory only.

## Process model

```text
tmux server
  ├─ indexed tmux-sidecar hooks
  │    └─ run-shell -b "tmux-sidecar hook --socket-path #{q:socket_path} --event ..."
  │
  ├─ short-lived hook command
  │    ├─ connects to the sidecar Unix socket
  │    ├─ auto-spawns the daemon if missing
  │    └─ sends one HookEvent and waits for Ack
  │
  ├─ persistent tmux-sidecar daemon
  │    ├─ one owner thread for DomainState
  │    ├─ reconciles full tmux snapshots
  │    ├─ applies urgent bell overlays immediately when possible
  │    ├─ tracks per-session workdirs for create-window actions
  │    └─ broadcasts StateUpdated / ActionResult messages
  │
  └─ tmux-sidecar clients
       ├─ one or more subscribed TUIs
       └─ short-lived query / control clients
```

Run one daemon per tmux socket. The sidecar socket path is derived deterministically from the tmux socket path and normally lives under `$XDG_RUNTIME_DIR/tmux-sidecar/<hash>.sock`; if `XDG_RUNTIME_DIR` is unavailable, tmux-sidecar falls back to a sibling directory next to the tmux socket. A lock file next to the sidecar socket prevents duplicate auto-spawns.

## CLI surface

| Command | Purpose |
| --- | --- |
| `tmux-sidecar` | Start the TUI. Resolve the tmux socket and target client, ensure the daemon is running, subscribe, then render. |
| `tmux-sidecar setup [--socket-name/--socket-path]` | Install or refresh tmux-sidecar-managed hooks and bell monitoring for the selected tmux socket. |
| `tmux-sidecar teardown [--socket-name/--socket-path]` | Remove tmux-sidecar-managed indexed hook entries. |
| `tmux-sidecar init-plugin` | Print the recommended `run-shell -b 'tmux-sidecar setup'` tmux snippet. |
| `tmux-sidecar daemon --socket-path <path>` | Run the per-socket daemon in the foreground. This is normally auto-started. |
| `tmux-sidecar daemon --stop [--socket-name/--socket-path]` | Ask the running daemon for the selected tmux socket to shut down without spawning a replacement. |
| `tmux-sidecar hook --socket-path <path> --event <event> ...` | Internal hook entry point used by installed tmux hooks. |
| `tmux-sidecar query alerts [--socket-name/--socket-path]` | Print the number of active bell alerts tracked by the daemon. |

Compatibility aliases `install-hooks`, `uninstall-hooks`, `server`, and `daemon --kill` are still accepted for existing scripts, but `setup`, `teardown`, and `daemon --stop` are the documented names. The old `--poll-interval-ms` flag is no longer part of the documented interface.

## Hook installation and monitoring

tmux-sidecar installs hooks into reserved indexed slots so user hooks remain untouched. The current implementation reserves a small high range starting at `900` and always quotes tmux formats with `#{q:...}`.

Important rules:

- Pass only stable IDs, indexes, socket paths, pane paths, client names, and timestamps through `run-shell` hook commands.
- Never pass session or window names through shell-interpolated hook commands.
- Reinstall by unsetting tmux-sidecar's reserved indexes first, then writing the managed hook block again.
- Configure `monitor-bell on` globally and for existing windows so bell alerts are available immediately.

Installed hook coverage:

| Hook class | Hooks |
| --- | --- |
| Sessions | `session-created`, `session-closed`, `session-renamed`, `session-window-changed` |
| Windows / winlinks | `window-linked`, `window-unlinked`, `window-renamed`, `window-pane-changed`, `window-layout-changed` |
| Alerts | `alert-bell` |
| Clients | `client-attached`, `client-detached`, `client-session-changed` |
| Action fallback | `after-new-session`, `after-new-window`, `after-rename-session`, `after-rename-window`, `after-kill-pane`, `after-select-window` |

`alert-activity` and `alert-silence` stay out of the installed hook block. The daemon still preserves reserved cleanup slots for those names, but only bell alerts drive UI badges.

## IPC protocol

The runtime uses local newline-delimited JSON over Unix domain sockets.

Current protocol version: `2`

```text
ClientMessage
  Hello { client_kind, protocol_version }
  HookEvent { tmux_socket_path, event, session_id?, window_id?, window_index?, pane_id?, pane_current_path?, client_name?, timestamp_ms? }
  Subscribe { target_client? }
  ActionRequest { request_id, target_client?, action }
  SnapshotRequest
  Shutdown

ServerMessage
  HelloAck { protocol_version, server_id }
  Ack { kind }
  StateUpdated { generation, state }
  ActionResult { request_id, generation, result }
  Error { message }
```

`ProjectionState` is the serialization boundary for IPC. Internally, the daemon converts snapshots into normalized `DomainState` and only then derives projections for IPC and UI consumers.

## State ownership

```text
DomainState
  tmux_socket_path: PathBuf
  sessions: BTreeMap<SessionId, SessionNode>
  winlinks: BTreeMap<WinlinkKey, WindowState>
  clients: BTreeMap<ClientName, ClientNode>

WinlinkKey
  session_id: SessionId
  window_id: WindowId
```

Important invariants:

- Linked windows are session-local winlinks keyed by `(session_id, window_id)`.
- The visible target is derived from the selected client first, then the session's active window.
- Bell alerts are the only alert kind rendered in the UI.
- The daemon tracks recent pane paths per session so `create-window` can reuse the most relevant working directory.
- UI focus, edit buffers, jump labels, and toasts stay local to the TUI and are never part of `DomainState`.

## Reconciliation and action flow

Bootstrap:

1. Resolve the tmux socket path and target client.
2. Start or connect to the per-socket daemon.
3. The daemon snapshots tmux, derives `DomainState`, configures bell monitoring, and starts listening on the sidecar socket.
4. The TUI renders a loading placeholder until the initial `StateUpdated` arrives.

Hook handling:

1. A tmux hook runs `tmux-sidecar hook ...`.
2. The hook client connects to the daemon, sends one `HookEvent`, waits for `Ack`, and exits.
3. Bell hooks patch the matching winlink immediately when enough identity is available.
4. The daemon marks dirty scopes, debounces bursty hook traffic, then reconciles from a full tmux snapshot.
5. Newly discovered windows get `monitor-bell on` before the updated projection is broadcast.

Action handling:

1. The TUI sends `ActionRequest` over the subscription connection.
2. The daemon executes the tmux command through one adapter.
3. The daemon reconciles from tmux after both success and failure.
4. The daemon broadcasts `StateUpdated` before returning the final `ActionResult`, so the UI never relies on speculative local state.

## UI runtime

The TUI is a subscriber to daemon state, not a tmux snapshot loop.

```text
terminal input thread  ----\
server reader thread    ----> UiEvent channel ---> AppState reducer ---> render if dirty
local timers           ----/
```

Key runtime properties:

- Startup checks that do not need raw mode run before entering the alternate screen.
- The first paint uses a transient loading state, not a disk-backed cache.
- A server reader thread blocks on the sidecar subscription and forwards pushed messages into the UI event channel.
- A terminal input thread forwards key, mouse, and resize events into the same channel.
- The main UI thread owns `AppState`, reconciles focus by stable row ID, and redraws immediately when state changes.
- The only timers left are local UI concerns such as toast expiry; tmux synchronization itself is server-pushed.

## Failure and recovery behavior

- If the daemon is absent, UI and hook clients auto-spawn it.
- If a connected daemon speaks the wrong protocol version, tmux-sidecar treats it as stale, restarts it, and reconnects.
- If the daemon cannot be spawned or contacted, tmux-sidecar prints a concise error and exits non-zero.
- If the daemon connection drops while the UI is running, the UI leaves raw mode, reports the disconnection, and exits instead of rendering stale state.
- If tmux is unavailable or no target client can be resolved, startup fails before raw mode.

## Validation

The rewrite is validated with the repository's standard suite:

```bash
cargo fmt --all --check
cargo check
cargo test
```

Coverage includes protocol round-trips, hook installation, linked-window invariants, server reconciliation, startup hardening, pushed UI updates, and end-to-end tmux workflows.
