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
            "Enter switch  1-9/0 alert  n session  / filter  s jump  c window  gg/G  r rename  x close  ? help  q quit"
        }
        Mode::Help => "Esc close help  ? close help  q quit",
        Mode::RenameSession { .. } | Mode::RenameWindow { .. } => {
            "Enter accept  Esc revert  Ctrl+u clear"
        }
        Mode::CreateSessionName { .. } | Mode::CreateWindowName { .. } => {
            "Enter create  Esc cancel  Ctrl+u clear"
        }
        Mode::FilterSessions { .. } => "Filter: Enter keep  Esc restore  Backspace edit",
    }
}

pub fn modal_lines(glyph_mode: GlyphMode, theme: Theme) -> Vec<Line<'static>> {
    let glyphs = Glyphs::from_mode(glyph_mode);

    vec![
        Line::from(Span::styled("Keybindings", theme.marker_focus())),
        Line::from("gg / G          first / last row"),
        Line::from("Up/Down or j/k  move focus"),
        Line::from("Enter           switch or create"),
        Line::from("/               filter sessions by substring"),
        Line::from("1-9,0           jump to numbered alert"),
        Line::from("n               start new session"),
        Line::from("s               jump to row label"),
        Line::from("c               new window in focused session"),
        Line::from("r               rename focused session/window"),
        Line::from("x               close focused session/window"),
        Line::from("?               toggle help"),
        Line::from("q               quit"),
        Line::from(""),
        Line::from(Span::styled("Legend", theme.marker_focus())),
        Line::from(vec![
            Span::styled(format!("{} focused", glyphs.focus), theme.marker_focus()),
            Span::raw("   "),
            Span::styled(format!("{} active", glyphs.active), theme.badge_active()),
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
