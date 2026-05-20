# Architecture

## Goals

The MVP should be small, modular, and testable against real tmux. tmux remains the source of truth; the app owns focus, edit mode, rendering, and command orchestration.

Core constraints:

- No shell-joined tmux commands.
- No persistent tmux config for MVP.
- Startup failures happen before terminal raw mode.
- Runtime tmux failures refresh/revert to accurate tmux state.
- Tests assert workflows and public behavior, not private widget internals.

## Proposed module layout

```text
src/
  main.rs
  cli.rs
  app.rs
  event.rs
  input.rs
  model.rs
  tmux/
    mod.rs
    command.rs
    parse.rs
    snapshot.rs
  ui/
    mod.rs
    theme.rs
    tree.rs
    help.rs
tests/
  e2e_tmux.rs
```

Responsibilities:

| Module | Responsibility |
| --- | --- |
| `main` | Parse CLI, run startup checks, initialize terminal, restore terminal on exit. |
| `cli` | `clap` options such as socket, target client, poll interval, and future `--init-tmux`. |
| `app` | Own `AppState`, dispatch input to actions, call tmux boundary, request redraws. |
| `event` | Convert Crossterm key/mouse/tick events into app-level events. |
| `input` | Single-line edit buffer for create/rename. |
| `model` | Pure data types for tmux state, tree rows, focus, and modes. |
| `tmux::command` | Safe wrapper around `std::process::Command`. |
| `tmux::parse` | Pure parsers for tmux formatted output. |
| `tmux::snapshot` | Build full `TmuxState` from tmux commands. |
| `ui` | Pure rendering from `AppState` to Ratatui frames. |

## Data model

```text
AppState
  tmux: TmuxState
  focus: Focus
  mode: Mode
  target_client: ClientName
  last_error: Option<ActionError>
  next_poll_at: Instant

TmuxState
  sessions: Vec<Session>
  clients: Vec<Client>

Client
  name: ClientName
  session_id: SessionId
  current_window_id: Option<WindowId>
  activity: u64
  tty: String

Session
  id: SessionId
  name: String
  attached_count: u32
  active_window_id: WindowId
  windows: Vec<Window>

Window
  id: WindowId
  index: u32
  name: String
  active: bool  // tmux session-local current window
  flags: String
```

Use tmux IDs (`$0`, `@1`) as stable identifiers. Names and indexes are display data and command targets only when tmux requires them.

`Mode` should be explicit:

```text
Normal
Help
RenameSession { id, original_name, input }
RenameWindow { id, original_name, input }
CreateSessionName { id, input }
CreateWindowName { id, input }
```

Creation modes reference the already-created tmux object. `Esc` exits the mode and keeps tmux's default name.

## tmux boundary

Define a trait so app logic can be tested without spawning tmux:

```text
trait Tmux {
    fn snapshot(&self) -> Result<TmuxState, TmuxError>;
    fn resolve_target_client(&self, cli_override: Option<&str>) -> Result<ClientName, TmuxError>;
    fn switch_to(&self, client: &ClientName, target: WindowTarget) -> Result<(), TmuxError>;
    fn create_session(&self) -> Result<SessionId, TmuxError>;
    fn create_window(&self, session: &SessionId) -> Result<WindowId, TmuxError>;
    fn rename_session(&self, session: &SessionId, name: &str) -> Result<(), TmuxError>;
    fn rename_window(&self, window: &WindowId, name: &str) -> Result<(), TmuxError>;
}
```

`TmuxCli` is the production implementation. It should accept optional socket arguments so tests can run against `tmux -L <isolated-name> -f /dev/null`.

All command execution should:

- pass arguments separately through `Command`
- capture stdout/stderr
- map non-zero exits to typed `TmuxError`
- avoid broad fallback behavior
- keep parsing separate from process execution

## Event loop

```text
startup checks
initial snapshot
resolve target client
enter alternate screen and raw mode

loop:
  draw AppState
  wait for crossterm event until next poll deadline
  if input event:
    update focus/mode or run tmux action
    after tmux action, refresh snapshot immediately
  if poll deadline reached:
    refresh snapshot
  if quit:
    restore terminal and exit
```

The event loop can remain synchronous. Use `crossterm::event::poll(timeout)` to interleave input with periodic tmux snapshots.

## Actions and reconciliation

Represent user intent separately from side effects:

```text
AppEvent -> Action -> optional Tmux call -> Snapshot -> AppState
```

Examples:

| User event | Action |
| --- | --- |
| `Enter` on window row | `Switch(WindowId)` |
| `Enter` on session `[+]` | `CreateSession` |
| `Enter` in rename mode | `SubmitRename` |
| `Esc` in create-name mode | `KeepDefaultName` |

After every snapshot:

1. Rebuild visible tree rows from `TmuxState`.
2. Preserve focus by focused row ID if still present.
3. If focused row disappeared, move to nearest row index.
4. If no rows remain, exit with a runtime error because the README requires sessions to manage.

## Rendering architecture

Rendering should be pure:

```text
fn render(frame: &mut Frame, state: &AppState)
```

Guidelines:

- Build a `Vec<TreeRow>` from state before rendering.
- Keep theme tokens in `ui::theme`.
- Keep key hints derived from `Mode`.
- Do not run tmux commands from UI code.
- Snapshot-test critical render states with Ratatui buffers if visual regressions become costly.

## Testing strategy

Unit tests:

- tmux output parsers
- focus reconciliation
- edit buffer behavior
- action reducer behavior with a fake `Tmux`

Integration tests against real tmux:

- use a unique socket name per test process
- start with `tmux -L <name> -f /dev/null new-session -d ...`
- clean up with `kill-server`
- serialize tests that share terminal or tmux resources
- skip with a clear message if `tmux` is unavailable

End-to-end workflows:

| Workflow | Expected assertion |
| --- | --- |
| Startup with no tmux | stderr error and non-zero exit. |
| Startup with no sessions | stderr error and non-zero exit. |
| Switch window | target client changes to selected window. |
| Create window, `Esc` | new window exists, active, default name kept. |
| Create window, rename | new window exists, active, requested name applied. |
| Rename session/window failure | UI refreshes to tmux's actual name. |
| External tmux change | next poll updates the target client's active marker and tree contents. |

The CLI binary can also expose a test-only or hidden `--print-snapshot` command if parser and tmux integration need stable black-box coverage without driving the full TUI.
