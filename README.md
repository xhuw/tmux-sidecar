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
**tmux-sidecar** drops a beautiful, always-live session tree right into your terminal — every session, every window, every burst of activity, every bell alert, one keystroke away.

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

- **Instant context** — see all your sessions and windows at a glance, always live-synced to tmux
- **One-keystroke everything** — switch, create, rename, or close without leaving the keyboard
- **Mouse? Sure.** — click any row to jump straight to it
- **Zero config** — attach inside tmux and it just works; no plugins, no hooks, no config changes required
- **Activity and alerts at a glance** — current activity animates in-place and bell alerts stay visible so nothing gets lost in a busy session
- **Inline rename** — rename sessions and windows without dropping to a tmux prompt, with full cursor editing
- **Survives chaos** — if something changes in tmux behind your back, the tree re-syncs and focus recovers gracefully
- **Fast** — written in Rust; sub-millisecond renders, 500 ms background polling

---

## Install

### Prerequisites

- Linux
- Rust toolchain (`cargo`) — [install rustup](https://rustup.rs/)
- `tmux` on your `PATH`
- *(Optional)* A [Nerd Font](https://www.nerdfonts.com/) for rich glyphs

### Build from source

```bash
git clone https://github.com/youruser/tmux-sidecar
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

For launcher-style behavior that exits as soon as you choose the destination:

```bash
tmux-sidecar --auto-quit
```

**Outside tmux** — tell it which client to drive:

```bash
tmux-sidecar --target-client <client-name>
```

Not sure of the client name? `tmux list-clients` will tell you.

That's it. The session tree opens full-screen and stays live.

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
| `x` | Close the focused window immediately |
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

### Options

```
tmux-sidecar [OPTIONS]

Options:
  --target-client <name>      Use a specific tmux client for switching
  --socket-name <name>        Connect to a named tmux socket (-L)
  --socket-path <path>        Connect to a socket by path (-S)
  --poll-interval-ms <ms>     Live-sync interval in milliseconds (default: 500)
  --auto-quit                 Exit immediately after selecting a session or window
  --print-snapshot            Print the session/window tree and exit (debug)
```

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
