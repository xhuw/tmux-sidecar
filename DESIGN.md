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
 󰐕 new session
 ▶ work                                  ●
 ├─ 0 shell
 ├─ 2 editor                            ● active
 ├─ 3 tests                             󰂞 alert
 └─ 󰐕 new window
   notes
   ├─ 0 scratch
   └─ 󰐕 new window

 ────────────────────────────────────────────────────────
 Enter switch  r rename  ? help  q quit
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

Use a dark graphite palette with one bright accent. Prefer semantic style names in code rather than hard-coded colors in widgets.

| Token | True-color value | 16-color fallback | Usage |
| --- | --- | --- | --- |
| `bg` | `#0b0f14` | black | App background. |
| `surface` | `#111820` | black | Header, footer, modal. |
| `surface_high` | `#1b2633` | bright black | Focused row background. |
| `text` | `#d6deeb` | white | Primary labels. |
| `muted` | `#7d8590` | bright black | Secondary metadata and inactive markers. |
| `accent` | `#7dd3fc` | cyan | Focus marker, selected text, primary affordances. |
| `active` | `#a7f3d0` | green | tmux's active session/window. |
| `warning` | `#facc15` | yellow | Pending rename/create hint. |
| `alert` | `#fbbf24` | yellow | tmux window activity, bell, silence, or notification state. |
| `danger` | `#f87171` | red | Fatal or transient failed action indicator. |

## Row states

| State | Marker | Styling |
| --- | --- | --- |
| Focused row | `>` | Accent marker and `surface_high` background. |
| Active tmux window | `*` | `active` foreground; label remains readable if also focused. |
| Creation row | `[+]` | Muted label, accent plus sign. |
| Alerted window | `!` | `alert` foreground badge on the right side of the window row. |
| Inline edit | `[...]` | Warning border or prompt prefix; footer switches to `Enter accept  Esc keep/revert`. |
| Disabled action | none | Muted foreground only. |

The table shows ASCII markers for clarity; the default rendered UI should use the glyph system above.

Focus, active tmux state, and alert/notification state are different concepts. If a row has multiple states, focus owns the background, active owns the first right-side badge, and alert owns the next right-side badge. An active window with an alert must show both states.

## Alerts and notifications

Window rows must show tmux alert/notification state when tmux reports it, including activity, bell, silence, or equivalent window flags exposed by the snapshot layer. Alerts are visual only in the MVP; selecting or switching to a window lets tmux clear or preserve the alert according to normal tmux behavior.

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
| `r` | Rename focused session/window. |
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
