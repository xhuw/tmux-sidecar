# tmux-sidecar

`tmux-sidecar` is a terminal UI for managing tmux sessions/windows with keyboard or mouse.
It keeps the tree synced with tmux, supports inline create/rename, and highlights active + alerting windows.

## Requirements

- Linux
- `tmux` installed and available on `PATH`
- A running tmux server with at least one session
- A target tmux client (required for switching)

Optional:

- Nerd Font for rich glyphs (default rendering)
- Any monospace font + ASCII fallback mode

## Build

```bash
cargo build
```

## Run

Inside tmux:

```bash
cargo run --
```

Outside tmux (recommended explicit target client):

```bash
cargo run -- --target-client <client-name>
```

Use a non-default tmux socket:

```bash
cargo run -- --socket-name <name>
cargo run -- --socket-path <path>
```

Useful options:

- `--poll-interval-ms <millis>`: snapshot refresh interval (default `500`)
- `--print-snapshot`: debug helper, prints snapshot and exits

## Keybindings

Normal mode:

- `Up/Down` or `j/k`: move focus
- `Enter`: switch to focused session/window, or create focused `new ...` row
- `r`: rename focused session/window
- `?`: toggle help
- `q` or `Ctrl+c`: quit

Inline edit mode (rename/create naming):

- Type text directly
- `Enter`: accept
- `Esc`: cancel inline edit (keep current/default tmux name)
- `Ctrl+u`: clear
- `Left/Right/Home/End/Backspace/Delete`: cursor editing

Mouse:

- Left click row: focus + activate
- Wheel up/down: move focus

## Glyph/font fallback

If Nerd Font glyphs do not render correctly, force ASCII mode:

```bash
TMUX_SIDECAR_ASCII=1 cargo run --
# or
TMUX_SIDECAR_GLYPHS=ascii cargo run --
```

## Testing

Run standard checks:

```bash
cargo fmt --all --check
cargo check
cargo test
```

Notes:

- Integration tests use real tmux and are skipped automatically if tmux is unavailable.
- Tests use isolated tmux servers/sockets and do not require your existing tmux config.

## Known limitations / caveats

- When launched outside tmux, you should pass `--target-client` to avoid acting on an unintended client.
- tmux-sidecar polls tmux (`--poll-interval-ms`) rather than subscribing to tmux hooks.
- Error reporting is minimal by design (footer indicator + refresh to authoritative tmux state).
- If tmux has no sessions or no usable target client at startup, the app exits with an error.
