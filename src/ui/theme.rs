use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub muted: Color,
    pub accent: Color,
    pub active: Color,
    pub warning: Color,
    pub alert: Color,
    pub danger: Color,
}

impl Theme {
    pub fn app(self) -> Style {
        Style::default()
    }

    pub fn header(self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }

    pub fn footer(self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }

    pub fn header_highlight(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn header_text(self) -> Style {
        Style::default()
    }

    pub fn row_base(self) -> Style {
        Style::default()
    }

    pub fn row_focused(self) -> Style {
        self.row_base().add_modifier(Modifier::BOLD)
    }

    pub fn row_disabled(self) -> Style {
        self.row_base().fg(self.muted)
    }

    pub fn row_inline_edit(self) -> Style {
        self.row_base()
            .fg(self.warning)
            .add_modifier(Modifier::BOLD)
    }

    pub fn row_inline_edit_focused(self) -> Style {
        self.row_inline_edit()
    }

    pub fn modal(self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }

    pub fn modal_border(self) -> Style {
        Style::default().fg(self.accent)
    }

    pub fn marker_focus(self) -> Style {
        Style::default().add_modifier(Modifier::BOLD)
    }

    pub fn marker_idle(self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn marker_create(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn jump_label(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    }

    pub fn badge_active(self) -> Style {
        Style::default()
            .fg(self.active)
            .add_modifier(Modifier::BOLD)
    }

    pub fn badge_activity(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn badge_alert(self) -> Style {
        Style::default().fg(self.alert).add_modifier(Modifier::BOLD)
    }

    pub fn danger(self) -> Style {
        Style::default()
            .fg(self.danger)
            .add_modifier(Modifier::BOLD)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            muted: Color::DarkGray,
            accent: Color::Cyan,
            active: Color::Green,
            warning: Color::Yellow,
            alert: Color::Yellow,
            danger: Color::Red,
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier, Style};

    use super::Theme;

    #[test]
    fn semantic_colors_use_terminal_palette_slots() {
        let theme = Theme::default();

        assert_eq!(theme.muted, Color::DarkGray);
        assert_eq!(theme.accent, Color::Cyan);
        assert_eq!(theme.active, Color::Green);
        assert_eq!(theme.warning, Color::Yellow);
        assert_eq!(theme.alert, Color::Yellow);
        assert_eq!(theme.danger, Color::Red);
    }

    #[test]
    fn surface_styles_use_terminal_attributes_instead_of_fixed_backgrounds() {
        let theme = Theme::default();
        let reversed = Style::default().add_modifier(Modifier::REVERSED);
        let bold = Style::default().add_modifier(Modifier::BOLD);

        assert_eq!(theme.app(), Style::default());
        assert_eq!(theme.header(), reversed);
        assert_eq!(theme.footer(), reversed);
        assert_eq!(
            theme.header_highlight(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(theme.header_text(), Style::default());
        assert_eq!(theme.row_focused(), bold);
        assert_eq!(theme.modal(), reversed);
        assert_eq!(theme.marker_focus(), bold);
        assert_eq!(
            theme.jump_label(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        );
        assert_eq!(
            theme.badge_activity(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            theme.row_inline_edit_focused(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        );
    }
}
