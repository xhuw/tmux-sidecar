# Design language

## Product feel

tmux-sidecar should feel like a calm, fast control panel for tmux: dense enough to be useful, quiet enough to keep context, and obvious about what will happen when the user presses `Enter`.

Design principles:

- **Sidecar, not shell replacement:** the UI should expose tmux structure without hiding tmux concepts.
- **Keyboard-first, mouse-capable:** every workflow must be fast from the keyboard; mouse support should map to the same focus and activation model.
- **State over decoration:** focused row, active tmux target, edit mode, and failed/reverted actions must be visually distinct.
- **Minimal chrome:** use one tree, one footer, and one help modal; avoid secondary panels.
- **Modern terminal-native glyphs:** default to Nerd Font/Powerline glyphs with straight-line geometry; provide an ASCII fallback mode for terminals without patched fonts.

## Layout

Use the alternate screen with a single full-screen view:

```text
  tmux-sidecar   target /dev/pts/7   active work:2.editor
 ────────────────────────────────────────────────────────
   work
 ├─ 0 shell
 ├─ 2 editor                            ● active
 ├─ 3 tests                         [1] 󰂞 alert
 └─ 󰐕 new window
 ▶ notes
   ├─ 0 scratch
   └─ 󰐕 new window
 󰐕 new session

 ────────────────────────────────────────────────────────
 Enter switch  1-9/0 alert  n session  / filter  s jump  c window  gg/G  r rename  x close  ? help  q quit
```

Regions:

| Region | Purpose |
| --- | --- |
| Header | App name, target client, current active session/window. |
| Tree | Sessions and windows, plus explicit creation rows. |
| Footer | Contextual key hints; changes in rename/create mode and error states. |
| Modal | Centered help overlay opened with `?`. |

The current build defaults to a Nerd Font/Powerline presentation using straight separators and box drawing. Avoid rounded borders, emoji, heavy ornamentation, and dense iconography. Provide an ASCII fallback profile for users whose terminal cannot render patched glyphs cleanly.

## Glyph system

Use glyphs sparingly and only where they encode state or improve scanning.

| Purpose | Default glyph | ASCII fallback |
| --- | --- | --- |
| App icon | `` | `tmux` |
| Header separator | `` | `\|` |
| Horizontal rule | `─` | `-` |
| Tree branch | `├─`, `└─` | `\|--`, `+--` |
| Focus marker | `▶` | `>` |
| Active tmux target | `●` | `*` |
| Create action | `󰐕` | `[+]` |
| Alert/notification | `󰂞` | `!` |
| Rename/edit | `󰑕` | `[...]` |

Powerline glyphs should be used as subtle separators, not large blocks. The default shape language is straight, compact, and grid-aligned.

## Visual system

Use the terminal's native foreground/background and ANSI palette slots instead of a tmux-sidecar-owned RGB palette. Prefer semantic style names in code rather than hard-coded colors in widgets, and prefer terminal-native attributes such as reverse video and bold when separating surfaces or focus.

| Token | Terminal-native mapping | Usage |
| --- | --- | --- |
| `bg` | terminal default background | App background and tree body. |
| `surface` | reverse video on terminal defaults | Header, footer, and modal surface treatment. |
| `surface_high` | reverse video with stronger emphasis | Focused row treatment when a full-row highlight is needed. |
| `text` | terminal default foreground | Primary labels. |
| `muted` | bright black / dim secondary text | Secondary metadata and inactive markers. |
| `accent` | cyan | Focus marker, selected text, primary affordances. |
| `active` | green | tmux's active window. |
| `warning` | yellow | Pending rename/create hint. |
| `alert` | yellow | tmux window bell state. |
| `danger` | red | Fatal or transient failed action indicator. |

The UI should not assume a dark background. Light and dark terminal themes must both inherit cleanly from the host terminal without introducing custom fill colors that clash with the rest of the session.

## Row states

| State | Marker | Styling |
| --- | --- | --- |
| Focused row | `>` | Accent marker and reverse-video row emphasis. |
| Active tmux window | `*` | `active` foreground; label remains readable if also focused. |
| Creation row | `[+]` | Muted label, accent plus sign. |
| Alerted window | `[1] !` | `alert` foreground badge on the right side of the window row, with a numbered shortcut marker on the first 10 alert rows. |
| Inline edit | `[...]` | Warning prompt text; if also focused, combine with reverse-video emphasis. |
| Disabled action | none | Muted foreground only. |

The table shows ASCII markers for clarity; the default rendered UI uses the glyph system above. Session rows do not render active badges; active state belongs to window rows only.

Focus, active tmux state, and alert state are different concepts. If a row has multiple states, focus owns the background, active owns the first right-side badge, and the alert shortcut marker (when present) sits immediately before the alert badge. A window can show active and alert states together.

## Implementation notes

- `ui::theme` uses terminal-native colors and attributes instead of custom RGB fills.
- Rendering stays a pure `AppState -> frame` transform in `ui::*`.
- The first paint can show a loading placeholder while the initial subscribed tree arrives from the sidecar daemon.
- Snapshot-style UI tests cover the normal tree, help modal, jump labels, alerts, loading state, and startup toast.

## Alerts and notifications

Window rows show tmux bell alert state when tmux reports it. tmux-sidecar configures `monitor-bell on`; it does not configure `monitor-activity` or `monitor-silence`. Alerts are visual only in the current build; selecting or switching to a window lets tmux clear or preserve the alert according to normal tmux behavior.

Alert display rules:

- Show alerts only on window rows, not session rows.
- Number the first 10 alert rows in visible tree order as `1`-`9`, then `0`.
- Render the alert shortcut marker immediately before the alert badge.
- Use the `alert` token and `󰂞` badge by default.
- Do not replace the active marker with the alert marker; show all applicable badges.
- Preserve alerts across server reconciliation snapshots until tmux reports that they cleared.
- Include the alert badge in the help modal legend.

## Interactions

Keyboard defaults:

| Key | Behavior |
| --- | --- |
| `Up`/`Down`, `k`/`j` | Move focus by visible row. |
| `gg` / `G` | Jump to the first / last visible row. |
| `Enter` | Activate the focused session/window, or confirm the focused create flow. |
| `1`-`9`, `0` | Activate the numbered alert window from the first 10 alert rows in visible order (`0` is the tenth alert). |
| `n` | Start the new-session inline create flow. |
| `s` | Show jump labels for visible rows, then activate the chosen row immediately. |
| `c` | Start the new-window inline create flow for the focused session. |
| `r` | Rename the focused session or window. |
| `x` | Close the focused session or window immediately. |
| `?` | Open/close help modal. |
| `q`, `Ctrl+c` | Quit. |

Edit-mode defaults:

| Key | Behavior |
| --- | --- |
| Type | Edit the name directly. |
| `Enter` | Submit the rename or create request. |
| `Esc` | Cancel the inline edit or pre-create prompt. |
| `Ctrl+u` | Clear the input. |
| `Left` / `Right` / `Home` / `End` / `Backspace` / `Delete` | Cursor editing. |

Mouse defaults:

| Gesture | Behavior |
| --- | --- |
| Left click | Focus and activate that row. |
| Scroll wheel | Move focus up/down the visible tree. |

Double-click actions are intentionally unused; single-click activation keeps mouse behavior aligned with keyboard `Enter`.

## Create and rename flow

Creation is confirmed before tmux mutates state:

1. Focus a `[+]` row or press `n` / `c`.
2. Enter an optional name in the inline editor.
3. `Enter` sends the create action through the sidecar daemon.
4. The UI reconciles to the daemon's pushed result and focuses the created row.
5. `Esc` cancels before any tmux object is created.

Rename uses the same inline editor, except the focused row already exists. `Enter` sends the rename action through the daemon, and `Esc` restores the previously displayed name without issuing a tmux command.

## Help modal

The help modal should be centered, no wider than 72 columns, and include:

- navigation keys, including `gg` / `G`
- create, rename, close, jump, and alert shortcut keys
- active / focused / alert marker legend
- failure behavior summary: failed actions refresh from tmux

The modal should not obscure terminal restoration or fatal errors; fatal startup errors are printed to stderr before the TUI starts.
