# Design language

## Product feel

tmux-sidecar should feel like a calm, fast control panel for tmux: dense enough to be useful, quiet enough to keep context, and obvious about what will happen when the user presses `Enter`.

Design principles:

- **Sidecar, not shell replacement:** the UI should expose tmux structure without hiding tmux concepts.
- **Keyboard-first, mouse-capable:** every workflow must be fast from the keyboard; mouse support should map to the same focus and activation model.
- **State over decoration:** focused row, active tmux target, edit mode, and failed/reverted actions must be visually distinct.
- **Minimal chrome:** use one tree, one footer, one help modal, and avoid secondary panels for the MVP.
- **Modern terminal-native glyphs:** default to Nerd Font/Powerline glyphs with straight-line geometry; provide an ASCII fallback mode for terminals without patched fonts.

## Layout

Use the alternate screen with a single full-screen view:

```text
  tmux-sidecar   target /dev/pts/7   active work:2.editor
 ────────────────────────────────────────────────────────
   work
 ├─ 0 shell
 ├─ 2 editor                            ● active
 ├─ 3 tests                             󰂞 alert
 └─ 󰐕 new window
 ▶ notes
   ├─ 0 scratch
   └─ 󰐕 new window
 󰐕 new session

 ────────────────────────────────────────────────────────
 Enter switch/create  s new session  c new window  r rename  x close window  ? help  q quit
```

Regions:

| Region | Purpose |
| --- | --- |
| Header | App name, target client, current active session/window. |
| Tree | Sessions and windows, plus explicit creation rows. |
| Footer | Contextual key hints; changes in rename/create mode. |
| Modal | Centered help overlay opened with `?`. |

The MVP should default to a Nerd Font/Powerline presentation using straight separators and box drawing. Avoid rounded borders, emoji, heavy ornamentation, and dense iconography. Provide an ASCII fallback profile for users whose terminal cannot render patched glyphs cleanly.

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
| Alerted window | `!` | `alert` foreground badge on the right side of the window row. |
| Inline edit | `[...]` | Warning prompt text; if also focused, combine with reverse-video emphasis. |
| Disabled action | none | Muted foreground only. |

The table shows ASCII markers for clarity; the default rendered UI should use the glyph system above.

Focus, active tmux state, and alert/notification state are different concepts. If a row has multiple states, focus owns the background, active owns the first right-side badge, and alert owns the next right-side badge. An active window with an alert must show both states.

## Implementation plan

1. Replace the hard-coded RGB palette in `ui::theme` with terminal-default foreground/background handling and ANSI palette colors for semantic states.
2. Render header, footer, help modal, and focused rows with terminal-native attributes such as reverse video and bold instead of custom surface fills.
3. Keep semantic theme helpers in `ui::theme` so tree, header, and help rendering remain decoupled from palette choices.
4. Add or update theme-focused tests and run the existing formatting, check, and test commands after the migration.

## Alerts and notifications

Window rows must show tmux bell alert state when tmux reports it. Activity and silence flags from tmux are parsed by the snapshot layer but are not rendered as sidecar alert badges. Alerts are visual only in the MVP; selecting or switching to a window lets tmux clear or preserve the alert according to normal tmux behavior.

Alert display rules:

- Show alerts only on window rows, not session rows, unless a later phase adds aggregated session badges.
- Use the `alert` token and `󰂞` badge by default.
- Do not replace the active marker with the alert marker; show both.
- Preserve alerts during external polling refreshes.
- Include the alert badge in the help modal legend.

## Interactions

Keyboard defaults:

| Key | Behavior |
| --- | --- |
| `Up`/`Down`, `k`/`j` | Move focus by visible row. |
| `Enter` | Activate focused session/window, or start creation from a `[+]` row. |
| `s` | Start the new-session inline create flow. |
| `c` | Start the new-window inline create flow for the focused session, or the focused window's session. |
| `r` | Rename focused session/window. |
| `x` | Close the focused window immediately. |
| `Esc` | Cancel rename; for newly-created items, keep tmux's default name. |
| `?` | Open/close help modal. |
| `q` | Quit. |

Mouse defaults:

| Gesture | Behavior |
| --- | --- |
| Click row | Move focus to row. |
| Double-click session/window | Activate row. |
| Click `[+]` row | Start create flow. |
| Scroll | Move through the tree when content exceeds height. |

Double-click rename is intentionally not used, because it conflicts with activation.

## Create and rename flow

Creation is optimistic only for focus, not for naming:

1. Create the tmux object immediately and auto-switch to it.
2. Focus the created row and enter inline edit mode.
3. `Enter` submits the entered name through tmux.
4. `Esc` leaves the tmux default name in place.
5. If tmux rejects a submitted name, refresh from tmux and restore accurate state.

Rename follows the same inline editor, except `Esc` restores the previous displayed name without issuing a tmux command.

## Help modal

The help modal should be centered, no wider than 72 columns, and include:

- navigation keys
- create/rename keys
- active/focused/alert marker legend
- failure behavior summary: failed actions refresh from tmux

The modal should not obscure terminal restoration or fatal errors; fatal startup errors are printed to stderr before the TUI starts.
