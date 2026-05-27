use ratatui::text::{Line, Span};

use crate::model::{AppState, Mode};

use super::{
    theme::Theme,
    tree::{GlyphMode, Glyphs},
};

pub fn key_hints(state: &AppState) -> &'static str {
    if state.is_tree_loading() {
        return "Loading tmux tree...";
    }

    if state.navigation.jumping {
        return "Jump: type label to switch  invalid key cancels";
    }

    match &state.mode {
        Mode::Normal => {
            "Enter switch  s session  S jump  c window  gg top  G bottom  r rename  x close  ? help  q quit"
        }
        Mode::Help => "Esc close help  ? close help  q quit",
        Mode::RenameSession { .. } | Mode::RenameWindow { .. } => {
            "Enter accept  Esc revert  Ctrl+u clear"
        }
        Mode::CreateSessionName { .. } | Mode::CreateWindowName { .. } => {
            "Enter create  Esc cancel  Ctrl+u clear"
        }
    }
}

pub fn modal_lines(glyph_mode: GlyphMode, theme: Theme) -> Vec<Line<'static>> {
    let glyphs = Glyphs::from_mode(glyph_mode);

    vec![
        Line::from(Span::styled("Keybindings", theme.marker_focus())),
        Line::from("gg / G          first / last row"),
        Line::from("Up/Down or j/k  move focus"),
        Line::from("Enter           switch or create"),
        Line::from("s               start new session"),
        Line::from("S               jump to row label"),
        Line::from("c               new window in focused session"),
        Line::from("r               rename focused session/window"),
        Line::from("x               close focused window"),
        Line::from("?               toggle help"),
        Line::from("q               quit"),
        Line::from(""),
        Line::from(Span::styled("Legend", theme.marker_focus())),
        Line::from(vec![
            Span::styled(format!("{} focused", glyphs.focus), theme.marker_focus()),
            Span::raw("   "),
            Span::styled(format!("{} active", glyphs.active), theme.badge_active()),
            Span::raw("   "),
            Span::styled("... activity", theme.badge_activity()),
            Span::raw("   "),
            Span::styled(format!("{} alert", glyphs.alert), theme.badge_alert()),
        ]),
        Line::from(format!(
            "{} inline-edit",
            match glyph_mode {
                GlyphMode::Nerd => glyphs.rename,
                GlyphMode::Ascii => glyphs.rename,
            }
        )),
        Line::from("Failed actions refresh from tmux."),
    ]
}
