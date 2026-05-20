use ratatui::text::{Line, Span};

use crate::model::Mode;

use super::{
    theme::Theme,
    tree::{GlyphMode, Glyphs},
};

pub fn key_hints(mode: &Mode) -> &'static str {
    match mode {
        Mode::Normal => "Enter switch/create  r rename  x close window  ? help  q quit",
        Mode::Help => "Esc close help  ? close help  q quit",
        Mode::RenameSession { .. } | Mode::RenameWindow { .. } => {
            "Enter accept  Esc revert  Ctrl+u clear"
        }
        Mode::CreateSessionName { .. } | Mode::CreateWindowName { .. } => {
            "Enter accept  Esc keep default  Ctrl+u clear"
        }
    }
}

pub fn modal_lines(glyph_mode: GlyphMode, theme: Theme) -> Vec<Line<'static>> {
    let glyphs = Glyphs::from_mode(glyph_mode);

    vec![
        Line::from(Span::styled("Keybindings", theme.marker_focus())),
        Line::from("Up/Down or j/k  move focus"),
        Line::from("Enter           switch or create"),
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
