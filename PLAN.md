# MVP implementation plan

This plan is intended for a future implementation agent. It assumes the agent will read `README.md`, `DESIGN.md`, `RESEARCH.md`, and `ARCHITECTURE.md` before making code changes.

## Phase 0: Baseline and project setup

Goal: establish a clean Rust baseline and make future changes safe.

Tasks:

- Confirm the repository builds from the current skeleton.
- Add the selected dependencies from `RESEARCH.md`: `ratatui`, `crossterm`, `clap`, `anyhow`, `thiserror`, and test-only crates as needed.
- Keep the binary small and synchronous for MVP; do not introduce Tokio.
- Create the module skeleton from `ARCHITECTURE.md`.
- Add a minimal CLI with options for tmux socket name/path, target client override, poll interval, and a hidden/test snapshot command if useful.

Done when:

- `cargo check` passes.
- `tmux-sidecar --help` documents the supported flags.
- Empty module scaffolding is in place without implementing unrelated behavior.

## Phase 1: tmux command boundary

Goal: make tmux access safe, typed, and testable.

Tasks:

- Implement `Tmux` trait and `TmuxCli` production adapter.
- Execute tmux via `std::process::Command` with separate arguments only.
- Support isolated tmux sockets for tests with `-L` or `-S`.
- Implement startup checks: tmux exists, sessions exist, and a target client can be resolved.
- Parse `list-sessions`, `list-windows -a`, and `list-clients` using a unit separator format.
- Capture stable IDs, names, indexes, active state, attached clients, and window alert/notification flags.
- Return typed errors for command failures and parse failures.

Done when:

- Unit tests cover parser success and malformed output.
- Integration tests can create an isolated tmux server and read a correct snapshot.
- Startup failure paths print concise stderr errors and exit non-zero before TUI startup.

## Phase 2: domain model and app state

Goal: implement behavior as pure state transitions wherever possible.

Tasks:

- Define `AppState`, `TmuxState`, `Session`, `Window`, `Focus`, `Mode`, `TreeRow`, and action types.
- Build tree rows from snapshots, including top-level new-session row and per-session new-window rows.
- Implement focus movement, nearest-row focus recovery, and ID-preserving reconciliation after refresh.
- Implement the single-line input buffer for create/rename modes.
- Model alert/notification state separately from active and focused state.

Done when:

- Unit tests cover focus movement, focus recovery after external changes, edit buffer behavior, and alert state preservation.
- No UI code or tmux command code is needed to test core state behavior.

## Phase 3: terminal event loop

Goal: run a stable synchronous Ratatui/Crossterm application shell.

Tasks:

- Initialize alternate screen, raw mode, mouse capture, and panic-safe terminal restoration.
- Implement the event loop using `crossterm::event::poll(timeout)`.
- Add periodic tmux polling, defaulting to 500 ms.
- Refresh immediately after every tmux action.
- Handle quit, help modal toggle, keyboard navigation, and mouse row focus.

Done when:

- The app starts and exits cleanly without corrupting the terminal.
- Polling updates app state without user input.
- Terminal restoration works on normal quit and action errors.

## Phase 4: MVP rendering

Goal: implement the visual design from `DESIGN.md`.

Tasks:

- Implement the dark graphite theme with semantic style tokens.
- Render header, tree, footer, and centered help modal.
- Default to Nerd Font/Powerline glyphs with straight-line geometry.
- Add ASCII fallback rendering mode.
- Render focused, active, create, inline-edit, disabled, and alert/notification states distinctly.
- Show both active and alert badges when a window has both states.
- Keep rendering pure: no tmux calls from UI modules.

Done when:

- Manual runs match the intended design language.
- Help modal includes keybindings and the focused/active/alert legend.
- Render-focused tests or snapshots cover normal, help, inline edit, and alerted-window states.

## Phase 5: switch, create, and rename workflows

Goal: complete every user workflow in the README.

Tasks:

- Switch to focused session/window from keyboard and mouse activation.
- Create a new session from the top creation row and auto-switch to it.
- Create a new window from a session creation row and auto-switch to it.
- Enter inline naming immediately after create.
- Rename focused sessions/windows with `r`.
- Submit names with `Enter`; cancel with `Esc`.
- On create-name cancel, keep tmux's default name.
- On failed create, rename, or switch, refresh from tmux and restore accurate UI state.

Done when:

- End-to-end tests cover switching, creating sessions, creating windows, accepting names, canceling names, and failed rename refresh/revert behavior.
- Workflows pass both inside an isolated tmux server and, where practical, with an attached target client.

## Phase 6: external sync and alerts

Goal: prove tmux remains the source of truth when it changes outside tmux-sidecar.

Tasks:

- Detect externally created, renamed, closed, or re-indexed sessions/windows on the next poll.
- Detect external active-window/session changes on the next poll.
- Detect tmux window alert/notification flags on the next poll and show them in the tree.
- Preserve focus sensibly when the focused item still exists.
- Move focus predictably when an externally removed item disappears.

Done when:

- End-to-end tests mutate tmux outside the app/snapshot layer and verify the next refresh reflects the change.
- Alert/notification display is covered by a real tmux test where feasible, or a fake `Tmux` app-state test if tmux alert triggering is too environment-sensitive.

## Phase 7: hardening and release readiness

Goal: make the MVP reliable enough to hand to users.

Tasks:

- Run formatting, linting, tests, and any existing build checks.
- Review all tmux command targets for ID-based addressing and argument safety.
- Verify no code path shell-joins user-provided session/window names.
- Verify all fatal startup errors occur before raw mode.
- Verify runtime tmux errors do not leave speculative UI state.
- Update README with usage, keybindings, dependencies, font/fallback note, and testing instructions.
- Document known limitations, especially target-client requirements when launched outside tmux.

Done when:

- All tests pass.
- README describes how to run the MVP.
- The app satisfies every MVP item in `README.md`, including visual active markers, alert/notification display, external sync, create/rename/switch workflows, and help modal.
