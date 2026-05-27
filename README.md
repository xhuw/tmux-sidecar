<div align="center">

#  tmux-sidecar

**The tmux session manager you didn't know you were missing.**

*Blazing-fast. Keyboard-native. Always in sync.*

[![Rust](https://img.shields.io/badge/built%20with-Rust-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![tmux](https://img.shields.io/badge/requires-tmux-green?style=flat-square)](https://github.com/tmux/tmux)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)

</div>

---

Stop fumbling with `tmux ls`, `tmux switch-client`, and half-remembered key sequences.
**tmux-sidecar** drops a beautiful, always-live session tree right into your terminal — every session, every window, every bell alert, one keystroke away.

---

## Screenshot

```
 tmux-sidecar  │ target /dev/pts/3  │ active work:2.editor
────────────────────────────────────────────────────────────────
  work (1 attached)
  ├─ 0 shell
▶ ├─ 2 editor                                          ● active
  ├─ 3 tests                                           󰂞 alert
  └─  new window
  notes
  ├─ 0 scratch
  └─  new window
  side-project
  ├─ 0 main
  └─  new window
   new session
────────────────────────────────────────────────────────────────
Enter switch  s new session  S jump  c new window  gg top  G bottom  r rename  x close  ? help  q quit
```

> Nerd Font glyphs shown above. An ASCII fallback mode is available for any monospace font.

---

## Why tmux-sidecar?

- **Instant context** — see all your sessions and windows at a glance in a UI subscribed to tmux-sidecar state updates
- **One-keystroke everything** — switch, create, rename, or close without leaving the keyboard
- **Mouse? Sure.** — click any row to jump straight to it
- **Hook-driven sync** — tmux hooks feed a local per-socket sidecar server, so the UI stays current without querying tmux for every refresh
- **Plugin-friendly setup** — install or refresh the hook wiring with one `run-shell -b 'tmux-sidecar install-hooks'`
- **Bell alerts at a glance** — bell alerts stay visible so nothing gets lost in a busy session
- **Inline rename** — rename sessions and windows without dropping to a tmux prompt, with full cursor editing
- **Survives chaos** — if something changes in tmux behind your back, the sidecar refreshes state and focus recovers gracefully
- **Fast** — written in Rust with lightweight local IPC and cheap renders

---

## Install

### Prerequisites

- Linux
- Rust toolchain (`cargo`) — [install rustup](https://rustup.rs/)
- `tmux` on your `PATH`
- *(Optional)* A [Nerd Font](https://www.nerdfonts.com/) for rich glyphs

### Build from source

```bash
git clone https://github.com/xhuw/tmux-sidecar
cd tmux-sidecar
cargo build --release
```

The binary lands at `target/release/tmux-sidecar`. Copy it anywhere on your `PATH`:

```bash
cp target/release/tmux-sidecar ~/.local/bin/
```

---

## Quick start

**Inside tmux** — just run it:

```bash
tmux-sidecar
```

On startup, tmux-sidecar reuses or auto-starts a local sidecar for the current tmux socket, refreshes hooks with `install-hooks`, and subscribes the UI to server-pushed state updates.

For launcher-style behavior that exits as soon as you choose the destination:

```bash
tmux-sidecar --auto-quit
```

**Outside tmux** — tell it which client to drive:

```bash
tmux-sidecar --target-client <client-name>
```

Not sure of the client name? `tmux list-clients` will tell you.

The session tree opens full-screen and stays live through the local sidecar server.

### Recommended tmux setup

Add this to `~/.tmux.conf` so tmux installs or refreshes the hooks whenever the server starts or you reload the config:

```tmux
run-shell -b 'tmux-sidecar install-hooks'
```

If you want the snippet generated for you:

```bash
tmux-sidecar init-plugin
```

Launching `tmux-sidecar` also refreshes the hooks automatically, but the tmux config snippet is the recommended always-on setup.

---

## Usage

### Keybindings

| Key | Action |
|-----|--------|
| `↑` / `↓` or `k` / `j` | Move focus up/down the tree |
| `gg` / `G` | Jump to the first / last visible row |
| `Enter` | Switch to focused session/window, or create from a `[+]` row |
| `s` | Start the new-session inline create flow |
| `S` | Show jump labels for visible rows, then switch immediately after choosing one |
| `c` | Start the new-window inline create flow for the focused session, or for the focused window's session |
| `r` | Rename the focused session or window (inline, no prompts) |
| `x` | Close the focused session or window immediately |
| `?` | Open/close the help modal |
| `q` or `Ctrl+c` | Quit |

**In rename/create mode:**

| Key | Action |
|-----|--------|
| Type | Edit the name directly |
| `Enter` | Rename the focused item, or create a new item after confirming the optional name |
| `Esc` | Cancel the inline edit or pre-create prompt |
| `Ctrl+u` | Clear the input |
| `Left` / `Right` / `Home` / `End` / `Backspace` / `Delete` | Cursor editing |

**Mouse:**

| Gesture | Action |
|---------|--------|
| Left click | Focus + activate that row |
| Scroll wheel | Move focus up/down |

### Commands

| Command | Action |
|---------|--------|
| `install-hooks` | Install or refresh tmux-sidecar-managed hooks for the selected tmux socket |
| `uninstall-hooks` | Remove tmux-sidecar-managed hooks from the selected tmux socket |
| `init-plugin` | Print the recommended `run-shell -b 'tmux-sidecar install-hooks'` snippet |
| `server` | Run the local per-socket sidecar server (normally auto-started); use `server --kill` to stop the running server for the selected tmux socket |
| `hook` | Send one tmux hook event to the sidecar server (normally used by installed hooks) |

### Options

| Option | Action |
|--------|--------|
| `--target-client <name>` | Use a specific tmux client for switching |
| `--socket-name <name>` | Connect to a named tmux socket (`tmux -L`) |
| `--socket-path <path>` | Connect to a socket by path (`tmux -S`) |
| `--poll-interval-ms <ms>` | Control the UI render/input poll tick while the TUI is open (default: `500`) |
| `--auto-quit` | Exit immediately after selecting a session or window |

### ASCII / font fallback

No Nerd Font? No problem:

```bash
TMUX_SIDECAR_GLYPHS=ascii tmux-sidecar
# or
TMUX_SIDECAR_ASCII=1 tmux-sidecar
```

---

## Contributing

Bug reports, ideas, and PRs are welcome. Run the test suite before submitting:

```bash
cargo fmt --all --check
cargo check
cargo test
```

Integration tests spin up real isolated tmux servers and are skipped automatically if tmux is unavailable — your existing tmux sessions are never touched.
