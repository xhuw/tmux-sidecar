# Hook/server rewrite plan

> Historical note: this file records the staged rewrite plan that produced the current architecture. For the live command surface and runtime behavior, prefer `README.md` and `ARCHITECTURE.md`.

This plan replaces the polling-centered MVP with the architecture in `ARCHITECTURE.md`. Treat the existing implementation as reference material and a source of reusable tests, not as the structure to preserve. Reuse small pieces only when they still fit the new server/client design, such as tmux format parsers, safe command execution, input editing, tree rendering, and focus reconciliation.

## Phase 0: confirm baseline and preserve behavior

Goal: make the rewrite safe by capturing current behavior and the failing alert scenario.

Tasks:

- Run the existing checks to establish the baseline: `cargo fmt --all --check`, `cargo check`, and `cargo test`.
- Add or update a failing integration test that reproduces bell alerts from a non-current session not reaching the UI reliably.
- Inventory reusable code: tmux command wrapper, snapshot parsers, domain row model, input buffer, rendering, and workflow tests.
- Decide which current modules will be deleted or replaced. The polling `App` loop and UI-owned tmux adapter should be considered throwaway.

Done when:

- The current failure is represented by a test or documented tmux reproduction.
- The rewrite boundaries are clear enough that later phases do not try to patch polling back into the app.

## Phase 1: protocol and daemon foundation

Goal: introduce the sidecar server without changing the TUI yet.

Tasks:

- Add IPC dependencies, likely `serde` and `serde_json`.
- Define protocol structs for `Hello`, `HookEvent`, `Subscribe`, `ActionRequest`, `StateUpdated`, `ActionResult`, and `Error`.
- Implement deterministic sidecar socket path derivation from tmux `socket_path`.
- Implement stale socket detection and an auto-spawn lock file.
- Add `tmux-sidecar server --socket-path <path>` with a blocking Unix-listener accept loop.
- Add `tmux-sidecar hook --socket-path <path> --event <name> ...` that connects, auto-spawns once if needed, sends an event, and exits.

Done when:

- A unit or integration test can start the server, send a synthetic hook event, and receive an acknowledgement.
- Concurrent hook invocations do not spawn multiple servers for one tmux socket.

## Phase 2: server state engine

Goal: move tmux state ownership into the server.

Tasks:

- Move or recreate snapshot collection inside the server path.
- Model windows by session-local winlink key `(session_id, window_id)` so linked windows can carry different active/alert state.
- Build `ServerState` with generation numbers and subscriber broadcasting.
- On bootstrap, collect full state with `list-sessions`, `list-windows -a`, and `list-clients`.
- Implement dirty marking and debounce hook bursts into full snapshot reconciliation.
- Apply alert hook payloads immediately when they include enough identity, then reconcile from tmux.
- Configure `monitor-bell on` for all discovered windows without changing user `monitor-activity` or `monitor-silence` settings.

Done when:

- Synthetic hook tests prove generation changes and subscriber broadcasts.
- Real tmux tests prove full snapshot reconciliation handles create, rename, close, link/unlink, client changes, and renumbering.

## Phase 3: plugin and hook installer

Goal: make tmux reliably feed the server.

Tasks:

- Implement `install-hooks` to install tmux-sidecar hooks into reserved indexed hook slots, for example `hook-name[900]`.
- Implement `uninstall-hooks` to remove only those reserved indexed hook slots.
- Ensure hook commands use `run-shell -b` and quote every tmux format with `#{q:...}`.
- Pass only stable IDs, indexes, booleans, event names, and socket paths through hook commands; never pass session/window names.
- Install hooks for sessions, windows/winlinks, alerts, clients, and command fallback hooks listed in `ARCHITECTURE.md`.
- Implement `init-plugin` to print a `.tmux.conf` snippet based on `run-shell -b 'tmux-sidecar install-hooks'`.

Done when:

- Reloading tmux config does not duplicate sidecar hooks.
- Existing unrelated user hooks remain installed and still fire.
- `alert-bell` hooks reach the sidecar server from an isolated tmux server.

## Phase 4: UI as a server subscriber

Goal: remove UI-owned tmux polling.

Tasks:

- Replace `poll_interval_ms` sync behavior with a server subscription.
- Keep focus, edit mode, help mode, jump state, and render state local to the UI.
- Reconcile incoming `StateUpdated` messages with existing focus recovery rules.
- Avoid periodic render ticks for tmux refreshes or activity animations.
- Exit cleanly if the server disconnects instead of rendering stale state.

Done when:

- External tmux changes update the UI through server messages.
- There is no periodic tmux snapshot call in the TUI event loop.

## Phase 5: route UI actions through the server

Goal: centralize all tmux mutations and post-action reconciliation.

Tasks:

- Add server-side actions for switch session/window, create session/window, rename session/window, close session, and close window.
- Update UI key/mouse handlers to send `ActionRequest` messages instead of calling tmux directly.
- Add session close support in the UI. Use the existing close key if acceptable, but distinguish session/window close behavior clearly in the footer/help.
- After every action, have the server reconcile from tmux and broadcast the resulting state.
- On action error, return `ActionResult` with a clear message and do not retain speculative UI state.

Done when:

- Existing switch/create/rename/close-window workflow tests pass through the server path.
- New close-session tests pass.
- Two UI subscribers both update after an action from either UI.

## Phase 6: alert correctness across sessions

Goal: prove the original issue is fixed.

Tasks:

- Add real tmux tests where a non-current session/window triggers bell alerts.
- Add linked-window tests where the same `window_id` appears in multiple sessions with different session-local alert and active state.
- Verify alert updates are pushed to subscribers without waiting for a UI poll.
- Verify selecting or switching a window lets tmux clear/preserve alerts according to normal tmux behavior, and sidecar follows the next hook/snapshot.

Done when:

- The failing alert test from Phase 0 passes.
- Cross-session alert state remains correct after create, link, unlink, switch, and close operations.

## Phase 7: hardening, docs, and migration

Goal: make the rewrite usable as the default implementation.

Tasks:

- Remove obsolete polling CLI flags and code, or hide compatibility flags with clear deprecation behavior.
- Update `README.md` so setup no longer claims zero config and documents the plugin snippet.
- Update troubleshooting docs for hook installation, stale sidecar sockets, missing `tmux-sidecar` on tmux's `PATH`, and server logs.
- Run `cargo fmt --all --check`, `cargo check`, and `cargo test`.
- Manually test real `.tmux.conf` reload, two simultaneous UI clients, server kill/restart, and tmux server shutdown.

Done when:

- The hook/server implementation is the normal `tmux-sidecar` path.
- Documentation matches the new plugin requirement.
- The test suite covers hook install, server IPC, state reconciliation, UI subscription, actions, and cross-session alerts.
