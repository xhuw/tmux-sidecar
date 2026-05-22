use std::env;

use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::model::{AppState, Focus, Mode, TreeRowKind, WindowAlert};

use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlyphMode {
    #[default]
    Nerd,
    Ascii,
}

impl GlyphMode {
    pub fn from_env() -> Self {
        if env_truthy("TMUX_SIDECAR_ASCII") {
            return Self::Ascii;
        }

        match env::var("TMUX_SIDECAR_GLYPHS") {
            Ok(value) if value.eq_ignore_ascii_case("ascii") => Self::Ascii,
            _ => Self::Nerd,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    pub app_icon: &'static str,
    pub separator: &'static str,
    pub tree_branch: &'static str,
    pub tree_last: &'static str,
    pub focus: &'static str,
    pub active: &'static str,
    pub create: &'static str,
    pub alert: &'static str,
    pub rename: &'static str,
    pub inline_prefix: &'static str,
}

impl Glyphs {
    pub fn from_mode(mode: GlyphMode) -> Self {
        match mode {
            GlyphMode::Nerd => Self {
                app_icon: "",
                separator: "",
                tree_branch: "├─",
                tree_last: "└─",
                focus: "▶",
                active: "●",
                create: "󰐕",
                alert: "󰂞",
                rename: "󰑕",
                inline_prefix: "󰑕",
            },
            GlyphMode::Ascii => Self {
                app_icon: "tmux",
                separator: "|",
                tree_branch: "|--",
                tree_last: "+--",
                focus: ">",
                active: "*",
                create: "[+]",
                alert: "!",
                rename: "[...]",
                inline_prefix: "[...]",
            },
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TreeView {
    pub lines: Vec<Line<'static>>,
}

impl TreeView {
    pub fn from_state(state: &AppState, theme: Theme, glyphs: Glyphs) -> Self {
        let rows = state.tree_rows();
        let focused = state.focused_row_index();
        let inline_edit = inline_edit_target(&state.mode);
        let mut lines = Vec::with_capacity(rows.len());

        for (index, row) in rows.iter().enumerate() {
            let is_focused = focused == Some(index);
            let is_inline_edit = inline_edit
                .as_ref()
                .map(|edit| edit.focus == row.focus)
                .unwrap_or(false);
            let disabled = !matches!(state.mode, Mode::Normal) && !is_inline_edit;

            let mut row_style = theme.row_base();
            if disabled {
                row_style = theme.row_disabled();
            }
            if is_focused {
                row_style = theme.row_focused();
            }
            if is_inline_edit {
                row_style = theme.row_inline_edit();
                if is_focused {
                    row_style = theme.row_inline_edit_focused();
                }
            }

            let mut spans = Vec::new();
            spans.push(if is_focused {
                Span::styled(glyphs.focus, theme.marker_focus())
            } else {
                Span::styled(" ", theme.marker_idle())
            });
            spans.push(Span::raw(" "));

            let branch = match &row.kind {
                TreeRowKind::Window { .. } => Some(glyphs.tree_branch),
                TreeRowKind::CreateWindow { .. } => Some(glyphs.tree_last),
                TreeRowKind::CreateSession | TreeRowKind::Session { .. } => None,
            };
            if let Some(branch) = branch {
                spans.push(Span::styled(branch, theme.marker_idle()));
                spans.push(Span::raw(" "));
            }

            if is_inline_edit {
                let edit = inline_edit.as_ref().expect("inline edit must exist");
                spans.extend(inline_label(
                    &row.kind,
                    edit,
                    glyphs,
                    if disabled {
                        theme.row_disabled()
                    } else {
                        row_style
                    },
                    theme,
                ));
            } else {
                spans.extend(label_spans(&row.kind, glyphs, row_style, theme));
            }

            let mut badges = Vec::new();
            if row.active() {
                badges.push(Span::styled(
                    format!(" {} active", glyphs.active),
                    theme.badge_active(),
                ));
            }
            if let Some(alert) = row.alert() {
                badges.push(Span::styled(
                    format!(" {} {}", glyphs.alert, alert_label(alert)),
                    theme.badge_alert(),
                ));
            }
            spans.extend(badges);

            lines.push(Line::from(spans).style(row_style));
        }

        Self { lines }
    }
}

#[derive(Debug)]
struct InlineEdit<'a> {
    focus: Focus,
    input: &'a crate::input::InputBuffer,
    create: bool,
}

fn inline_edit_target(mode: &Mode) -> Option<InlineEdit<'_>> {
    match mode {
        Mode::RenameSession { id, input, .. } => Some(InlineEdit {
            focus: Focus::Session(id.clone()),
            input,
            create: false,
        }),
        Mode::RenameWindow { id, input, .. } => Some(InlineEdit {
            focus: Focus::Window(id.clone()),
            input,
            create: false,
        }),
        Mode::CreateSessionName { input } => Some(InlineEdit {
            focus: Focus::CreateSession,
            input,
            create: true,
        }),
        Mode::CreateWindowName { session_id, input } => Some(InlineEdit {
            focus: Focus::CreateWindow(session_id.clone()),
            input,
            create: true,
        }),
        Mode::Normal | Mode::Help => None,
    }
}

fn label_spans(
    kind: &TreeRowKind,
    glyphs: Glyphs,
    row_style: Style,
    theme: Theme,
) -> Vec<Span<'static>> {
    match kind {
        TreeRowKind::CreateSession => vec![
            Span::styled(glyphs.create, theme.marker_create()),
            Span::styled(" new session", row_style.fg(theme.muted)),
        ],
        TreeRowKind::Session {
            name,
            attached_count,
            ..
        } => {
            let mut spans = vec![Span::styled(name.clone(), row_style)];
            if *attached_count > 0 {
                spans.push(Span::styled(
                    format!(" ({attached_count} attached)"),
                    row_style.fg(theme.muted),
                ));
            }
            spans
        }
        TreeRowKind::Window { index, name, .. } => {
            vec![Span::styled(format!("{index} {name}"), row_style)]
        }
        TreeRowKind::CreateWindow { .. } => vec![
            Span::styled(glyphs.create, theme.marker_create()),
            Span::styled(" new window", row_style.fg(theme.muted)),
        ],
    }
}

fn inline_label(
    kind: &TreeRowKind,
    inline_edit: &InlineEdit<'_>,
    glyphs: Glyphs,
    row_style: Style,
    theme: Theme,
) -> Vec<Span<'static>> {
    let noun = match kind {
        TreeRowKind::Session { .. } => "session",
        TreeRowKind::Window { .. } => "window",
        TreeRowKind::CreateSession => "session",
        TreeRowKind::CreateWindow { .. } => "window",
    };
    let action = if inline_edit.create {
        format!("new {noun} name")
    } else {
        format!("rename {noun}")
    };
    let cursor_text = render_with_cursor(inline_edit.input);

    vec![
        Span::styled(glyphs.inline_prefix, theme.marker_create()),
        Span::styled(format!(" {action}: "), row_style),
        Span::styled(cursor_text, row_style.fg(theme.warning)),
    ]
}

fn render_with_cursor(input: &crate::input::InputBuffer) -> String {
    let text = input.as_str();
    let cursor = input.cursor().min(text.len());
    let (head, tail) = text.split_at(cursor);
    format!("{head}|{tail}")
}

fn alert_label(alert: WindowAlert) -> &'static str {
    match alert {
        WindowAlert::None => "alert",
        WindowAlert::Activity => "alert",
        WindowAlert::Bell => "alert",
        WindowAlert::Silence => "alert",
    }
}

fn env_truthy(name: &str) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}
