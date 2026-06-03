<div align="center">

#  tmux-sidecar

**The tmux session manager you didn't know you were missing.**

*Blazing-fast. Keyboard-native. Always in sync.*

[![Rust](https://img.shields.io/badge/built%20with-Rust-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![tmux](https://img.shields.io/badge/requires-tmux-green?style=flat-square)](https://github.com/tmux/tmux)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/xhuw/tmux-sidecar)

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
  ├─ 3 tests                                       [1] 󰂞 alert
  └─  new window
  notes
  ├─ 0 scratch
  └─  new window
  side-project
  ├─ 0 main
  └─  new window
   new session
────────────────────────────────────────────────────────────────
Enter switch  1-9/0 alert  n session  s jump  c window  gg/G  r rename  x close  ? help  q quit
```

> Nerd Font glyphs shown above. An ASCII fallback mode is available for any monospace font.

---

## Why tmux-sidecar?

- **Instant context** — see all your sessions and windows at a glance in a UI subscribed to tmux-sidecar state updates
- **One-keystroke everything** — switch, create, rename, or close without leaving the keyboard
- **Mouse? Sure.** — click any row to jump straight to it
- **Hook-driven sync** — tmux hooks feed a local per-socket sidecar daemon, so the UI stays current without querying tmux for every refresh
- **Plugin-friendly setup** — install or refresh the hook wiring with one `run-shell -b 'tmux-sidecar setup'`
- **Bell alerts at a glance** — bell alerts stay visible, and the daemon can ring attached tmux clients on new bell transitions even when the TUI is closed
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

On startup, tmux-sidecar reuses or auto-starts a local sidecar daemon for the current tmux socket, refreshes hooks with `setup`, and subscribes the UI to server-pushed state updates. There is no persisted startup cache; the TUI shows a brief loading state until the initial sidecar snapshot arrives.
If the backing tmux server for that socket exits, the sidecar daemon now exits automatically as well.

That server/client architecture is more complicated than tmux `>= 3.2` strictly needs. It mainly exists to keep tmux-sidecar working on tmux `>= 3.0, < 3.2`, where the bug fixed by tmux commit [`d8b6560cbf`](https://github.com/tmux/tmux/commit/d8b6560cbfb5677223982e4b27be92b2fcd034df) ("Set alert flag for the current window if the session is unattached") was still unresolved.

When tmux reports a new bell alert, the running daemon writes a BEL control character directly to each attached tmux client tty path (deduplicated by tty), so audible alerts still work without an open TUI.

For launcher-style behavior that exits as soon as you choose the destination:

```bash
tmux-sidecar --auto-quit
```

**Outside tmux** — tell it which client to drive:

```bash
tmux-sidecar --target-client <client-name>
```

Not sure of the client name? `tmux list-clients` will tell you.

The session tree opens full-screen and stays live through the local sidecar daemon.

### Recommended tmux setup

Add this to `~/.tmux.conf` so tmux installs or refreshes the hooks whenever the daemon starts or you reload the config:

```tmux
run-shell -b 'tmux-sidecar setup'
```

If you want the snippet generated for you:

```bash
tmux-sidecar init-plugin
```

Launching `tmux-sidecar` also refreshes the hooks automatically, but the tmux config snippet is the recommended always-on setup.

### Status line alert count

For a dynamic tmux status line count of active bell alerts, install the hooks and add `query alerts` to a `#(...)` status command:

```tmux
run-shell -b 'tmux-sidecar setup'
set -g status-interval 1
set -g status-right '#[fg=red]alerts #(tmux-sidecar query --socket-path #{q:socket_path} alerts)#[default] | %H:%M'
```

---

## Usage

### Keybindings

| Key | Action |
|-----|--------|
| `↑` / `↓` or `k` / `j` | Move focus up/down the tree |
| `gg` / `G` | Jump to the first / last visible row |
| `Enter` | Switch to focused session/window, or create from a `[+]` row |
| `1`-`9`, `0` | Jump directly to the numbered alert window (`0` is the tenth alert) |
| `n` | Start the new-session inline create flow |
| `s` | Show jump labels for visible rows, then switch immediately after choosing one |
| `c` | Start the new-window inline create flow for the focused session, or for the focused window's session |
| `r` | Rename the focused session or window (inline, no prompts) |
| `x` | Close the focused session or window immediately |
| `?` | Open/close the help modal |
| `q` or `Ctrl+c` | Quit |

The first 10 alert rows in visible tree order are numbered `1`-`9`, then `0`, and the number is rendered beside the alert badge.

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
| `setup` | Install or refresh tmux-sidecar-managed hooks for the selected tmux socket |
| `teardown` | Remove tmux-sidecar-managed hooks from the selected tmux socket |
| `init-plugin` | Print the recommended `run-shell -b 'tmux-sidecar setup'` snippet |
| `daemon` | Run the local per-socket sidecar daemon (normally auto-started); use `daemon --stop` to stop the running daemon for the selected tmux socket |
| `hook` | Send one tmux hook event to the sidecar daemon (normally used by installed hooks) |
| `query alerts` | Print the number of active bell alerts tracked by the sidecar daemon, suitable for tmux `#(...)` status lines |
| `query all` | Print the current sidecar projection snapshot as JSON, including sessions, windows, and clients |

Compatibility aliases `install-hooks`, `uninstall-hooks`, `server`, and `daemon --kill` are still accepted for existing scripts, but `setup`, `teardown`, and `daemon --stop` are the documented names.

### Options

| Option | Action |
|--------|--------|
| `--target-client <name>` | Use a specific tmux client for switching |
| `--socket-name <name>` | Connect to a named tmux socket (`tmux -L`) |
| `--socket-path <path>` | Connect to a socket by path (`tmux -S`) |
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
