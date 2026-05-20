# Research

Research date: 2026-05-20.

## Decisions

| Area | Decision | Rationale |
| --- | --- | --- |
| TUI framework | `ratatui` 0.30.0 | Current, maintained successor to `tui-rs`; provides widgets, layout, styling, buffers, and first-class Crossterm support. |
| Terminal backend/events | `crossterm` 0.29.0 | Ratatui's default backend; supports keyboard, mouse capture, alternate screen, raw mode, and Linux terminals. |
| CLI parsing | `clap` 4.6.1 with derive | Well-maintained, standard Rust CLI parser; useful for future `--client`, `--socket`, `--init-tmux`, and test socket options. |
| Error types | `thiserror` 2.0.18 plus `anyhow` 1.0.102 | Typed errors inside tmux/domain modules; ergonomic context at the binary boundary. |
| Inline text input | Small in-app single-line editor for MVP | Rename/create needs one line plus cursor/backspace/delete; avoiding a widget dependency keeps behavior explicit. Revisit `tui-input` if editing grows. |
| Width/truncation | `unicode-width` 0.2.2 if custom truncation is needed | Prevents broken layout for non-ASCII session/window names; Ratatui handles many spans, but tree truncation may need explicit width calculation. |
| Async runtime | No runtime for MVP | Crossterm polling plus a timed tmux snapshot tick is enough; avoiding Tokio keeps startup, testing, and terminal restoration simpler. |
| Tests | `assert_cmd`, `predicates`, `tempfile`, `serial_test`; optional `insta` | CLI assertions, isolated tmux sockets, serialized real-tmux tests, and optional Ratatui buffer snapshots. |

## TUI library comparison

| Option | Notes | Decision |
| --- | --- | --- |
| `ratatui` | Active fork of `tui-rs`; strong docs, examples, and Crossterm backend. | Choose. |
| Direct `crossterm` drawing | Lowest dependency count but requires hand-written layout, clipping, and style diffing. | Reject for MVP because visual quality and testability suffer. |
| `termion`/`termwiz` backend | Useful alternatives, but Linux-only MVP does not need backend abstraction beyond Ratatui. | Defer. |

## tmux findings

The local development environment has tmux 3.0a. The MVP should avoid features newer than tmux 3.0 unless the implementation explicitly gates them.

Useful tmux behavior verified against an isolated socket:

- `list-sessions -F` and `list-windows -a -F` provide enough data for a session/window tree.
- `select-window -t <session>:<window>` works without a current tmux client and updates the active window for that session.
- `switch-client` without a current or targeted client fails with `no current client`, so the app must resolve a target client before activation can satisfy the README's switch semantics.
- `list-clients -F` is empty when all sessions are detached; outside-tmux activation therefore needs either an attached client, an explicit `--client`, or a documented fatal startup error.

tmux control mode research:

- `tmux -C` exposes a text protocol with `%begin`, `%end`, and `%error` command blocks.
- Control mode emits notifications such as `%session-changed`, `%session-window-changed`, `%sessions-changed`, `%window-add`, `%window-close`, and `%window-renamed`.
- `refresh-client -C` can set control-client size.

tmux hook research:

- tmux hooks run commands on triggers via `set-hook`.
- Control-mode notifications also exist as hooks, except `%exit`.
- Hooks would require app-installed config, a script, or IPC plumbing, and cleanup/versioning would become part of the product surface.

## Update mechanism

Use polling for the MVP:

1. Take a full tmux snapshot on startup.
2. Poll tmux every 500 ms while idle.
3. Refresh immediately after every create, rename, or switch action.
4. Preserve focus by stable tmux IDs (`session_id`, `window_id`) across snapshots.
5. If an object disappears externally, move focus to the nearest visible row.

Why polling:

- Requires no tmux config or `--init-tmux` for MVP.
- Works the same inside and outside tmux.
- Handles missed events naturally because every tick is a complete state reconciliation.
- Keeps tests deterministic by allowing explicit refreshes.

Rejected for MVP:

| Mechanism | Reason |
| --- | --- |
| tmux hooks | Requires persistent user/server configuration and app-owned IPC. Good future optimization, unnecessary for MVP. |
| tmux control mode | Event-driven but adds a persistent protocol client, parsing complexity, and edge cases around which session/window the control client observes. Better suited after the command model is stable. |

## tmux command model

Prefer `std::process::Command` with separate arguments; never shell-join user-provided names.

Recommended snapshot commands use a control-character field separator supplied as an actual byte in the `-F` argument:

```text
list-sessions -F "#{session_id}<US>#{session_name}<US>#{session_attached}<US>#{session_windows}<US>#{session_activity}"
list-windows -a -F "#{session_id}<US>#{session_name}<US>#{window_id}<US>#{window_index}<US>#{window_name}<US>#{window_active}<US>#{window_flags}"
list-clients -F "#{client_name}<US>#{client_session}<US>#{client_activity}<US>#{client_tty}"
```

Use ASCII unit separator (`0x1f`) for fields and parse line-by-line. tmux names should not normally contain control characters; if they do, the parser should return a typed parse error rather than guessing.

Action commands:

| Action | tmux command |
| --- | --- |
| Switch window | `select-window -t <window_id>` to make the window active in its session, then `switch-client -c <client> -t <session_id>`. |
| Create window | `new-window -P -F "#{window_id}" -t <session_id>:` then switch to the created window's session. |
| Rename window | `rename-window -t <window_id> <name>`. |
| Create session | `new-session -d -P -F "#{session_id}"` then switch target client to it. |
| Rename session | `rename-session -t <session_id> <name>`. |

Target client resolution:

1. If launched inside tmux, resolve the current client from `TMUX_PANE`.
2. If launched outside tmux and `--client` is provided, use it.
3. Otherwise choose the attached client with the highest `client_activity`.
4. If no target client exists, exit non-zero at startup because switch and auto-switch workflows cannot be satisfied.

## Startup and failure behavior

Startup checks:

1. `tmux -V` succeeds.
2. `list-sessions` succeeds.
3. At least one session exists.
4. A target client can be resolved.

If any startup check fails, print a concise error to stderr and exit non-zero before entering alternate screen.

Runtime action failures:

- Do not keep speculative UI state.
- Refresh from tmux immediately.
- Restore focus by ID where possible.
- Extra error messaging is optional for MVP; a brief footer flash is acceptable if it does not complicate the model.
