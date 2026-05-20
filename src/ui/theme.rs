use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub surface: Color,
    pub surface_high: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub active: Color,
    pub warning: Color,
    pub alert: Color,
    pub danger: Color,
}

impl Theme {
    pub fn app(self) -> Style {
        Style::default().bg(self.bg).fg(self.text)
    }

    pub fn header(self) -> Style {
        Style::default().bg(self.surface).fg(self.text)
    }

    pub fn footer(self) -> Style {
        Style::default().bg(self.surface).fg(self.muted)
    }

    pub fn row_base(self) -> Style {
        Style::default().bg(self.bg).fg(self.text)
    }

    pub fn row_focused(self) -> Style {
        self.row_base().bg(self.surface_high)
    }

    pub fn row_disabled(self) -> Style {
        self.row_base().fg(self.muted)
    }

    pub fn row_inline_edit(self) -> Style {
        self.row_base()
            .fg(self.warning)
            .add_modifier(Modifier::BOLD)
    }

    pub fn modal(self) -> Style {
        Style::default().bg(self.surface).fg(self.text)
    }

    pub fn modal_border(self) -> Style {
        Style::default().fg(self.accent)
    }

    pub fn marker_focus(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn marker_idle(self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn marker_create(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn badge_active(self) -> Style {
        Style::default()
            .fg(self.active)
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
            bg: Color::Rgb(0x0b, 0x0f, 0x14),
            surface: Color::Rgb(0x11, 0x18, 0x20),
            surface_high: Color::Rgb(0x1b, 0x26, 0x33),
            text: Color::Rgb(0xd6, 0xde, 0xeb),
            muted: Color::Rgb(0x7d, 0x85, 0x90),
            accent: Color::Rgb(0x7d, 0xd3, 0xfc),
            active: Color::Rgb(0xa7, 0xf3, 0xd0),
            warning: Color::Rgb(0xfa, 0xcc, 0x15),
            alert: Color::Rgb(0xfb, 0xbf, 0x24),
            danger: Color::Rgb(0xf8, 0x71, 0x71),
        }
    }
}
