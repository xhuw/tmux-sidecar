pub mod help;
pub mod theme;
pub mod tree;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::model::{AppState, Mode};

use self::{
    theme::Theme,
    tree::{GlyphMode, Glyphs, TreeView},
};

pub const TREE_START_ROW: u16 = 1;

pub fn tree_index_for_terminal_row(row: u16) -> Option<usize> {
    row.checked_sub(TREE_START_ROW).map(usize::from)
}

pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    render_with_options(frame, state, RenderOptions::from_env());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RenderOptions {
    pub glyph_mode: GlyphMode,
}

impl RenderOptions {
    pub fn from_env() -> Self {
        Self {
            glyph_mode: GlyphMode::from_env(),
        }
    }

    #[cfg(test)]
    pub const fn ascii() -> Self {
        Self {
            glyph_mode: GlyphMode::Ascii,
        }
    }
}

pub fn render_with_options(frame: &mut Frame<'_>, state: &AppState, options: RenderOptions) {
    let theme = Theme::default();
    let glyphs = Glyphs::from_mode(options.glyph_mode);
    let area = frame.area();
    frame.render_widget(Block::default().style(theme.app()), area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    let header = Paragraph::new(header_line(state, glyphs, theme)).style(theme.header());
    frame.render_widget(header, chunks[0]);

    let tree = TreeView::from_state(state, theme, glyphs);
    let body = Paragraph::new(tree.lines).style(theme.app());
    frame.render_widget(body, chunks[1]);

    let mut footer_spans = vec![Span::styled(help::key_hints(&state.mode), theme.footer())];
    if state.last_error.is_some() {
        footer_spans.push(Span::styled("  ", theme.footer()));
        footer_spans.push(Span::styled(
            "! action failed; state refreshed",
            theme.danger(),
        ));
    }
    let footer = Paragraph::new(Line::from(footer_spans)).style(theme.footer());
    frame.render_widget(footer, chunks[2]);

    if state.mode == Mode::Help {
        let lines = help::modal_lines(options.glyph_mode, theme);
        let help_area = centered_rect(area, 72, lines.len() as u16 + 2);
        frame.render_widget(Clear, help_area);
        frame.render_widget(
            Paragraph::new(lines).style(theme.modal()).block(
                Block::bordered()
                    .title(" Help ")
                    .border_style(theme.modal_border()),
            ),
            help_area,
        );
    }
}

fn centered_rect(area: Rect, max_width: u16, height: u16) -> Rect {
    let width = area.width.min(max_width);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;

    Rect {
        x,
        y,
        width,
        height: area.height.min(height),
    }
}

fn header_line(state: &AppState, glyphs: Glyphs, theme: Theme) -> Line<'static> {
    let target = state
        .target_client
        .as_ref()
        .map(|client| client.0.clone())
        .unwrap_or_else(|| "none".to_string());
    let active = active_target_label(state);

    Line::from(vec![
        Span::styled(
            format!(" {} tmux-sidecar ", glyphs.app_icon),
            theme.marker_focus(),
        ),
        Span::styled(format!("{} ", glyphs.separator), theme.marker_idle()),
        Span::styled("target ", theme.marker_idle()),
        Span::styled(target, theme.row_base()),
        Span::styled(format!(" {} ", glyphs.separator), theme.marker_idle()),
        Span::styled("active ", theme.marker_idle()),
        Span::styled(active, theme.badge_active()),
    ])
}

fn active_target_label(state: &AppState) -> String {
    if let Some((session, window)) = state.tmux.visible_window(state.target_client.as_ref()) {
        return format!("{}:{}.{}", session.name, window.index, window.name);
    }

    if let Some(session) = state.tmux.visible_session(state.target_client.as_ref()) {
        return session.name.clone();
    }

    "none".to_string()
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use crate::{
        input::InputBuffer,
        model::{
            AppState, Client, ClientName, Focus, Mode, Session, TmuxState, Window, WindowAlert,
        },
    };

    use super::{RenderOptions, render_with_options};

    #[test]
    fn normal_render_snapshot_includes_header_tree_footer() {
        let state = sample_state();
        let output = render_ascii(&state, 96, 16);

        assert!(output.contains("tmux-sidecar"));
        assert!(output.contains("> [+] new session"));
        assert!(output.contains("* active ! alert"));
        assert!(output.contains("Enter switch/create  r rename  ? help  q quit"));
    }

    #[test]
    fn new_session_renders_after_session_rows() {
        let output = render_ascii(&sample_state(), 96, 16);
        let lines: Vec<_> = output.lines().collect();
        let new_window = lines
            .iter()
            .position(|line| line.contains("+-- [+] new window"))
            .expect("expected new window row");
        let new_session = lines
            .iter()
            .position(|line| line.contains("[+] new session"))
            .expect("expected new session row");

        assert!(new_window < new_session);
        assert_eq!(lines[new_session].trim(), "> [+] new session");
    }

    #[test]
    fn help_modal_snapshot_is_centered_and_includes_legend() {
        let mut state = sample_state();
        state.mode = Mode::Help;

        let output = render_ascii(&state, 96, 20);
        assert!(output.contains("Help"));
        assert!(output.contains("> focused"));
        assert!(output.contains("* active"));
        assert!(output.contains("! alert"));
        assert!(output.contains("Failed actions refresh from tmux."));
    }

    #[test]
    fn inline_edit_snapshot_shows_cursor_and_footer_hints() {
        let mut state = sample_state();
        state.focus = Focus::Window("@11".to_string());
        state.mode = Mode::RenameWindow {
            id: "@11".to_string(),
            original_name: "editor".to_string(),
            input: InputBuffer::from_text("editor"),
        };

        let output = render_ascii(&state, 96, 16);

        assert!(output.contains("[...] rename window: editor|"));
        assert!(output.contains("Enter accept  Esc revert  Ctrl+u clear"));
    }

    #[test]
    fn session_rows_do_not_render_active_badges() {
        let output = render_ascii(&sample_state(), 96, 16);
        let session_line = output
            .lines()
            .find(|line| line.contains("work (1 attached)"))
            .expect("expected session row");

        assert!(!session_line.contains("* active"));
        assert!(!session_line.contains("! alert"));
    }

    #[test]
    fn alerted_window_snapshot_shows_active_and_alert_badges_together() {
        let mut state = sample_state();
        state.focus = Focus::Window("@11".to_string());

        let output = render_ascii(&state, 96, 16);
        let alert_line = output
            .lines()
            .find(|line| line.contains("|-- 1 editor"))
            .expect("expected editor row");

        assert!(alert_line.contains("* active"));
        assert!(alert_line.contains("! alert"));
    }

    #[test]
    fn render_marks_only_target_clients_visible_window_active() {
        let mut state = sample_state();
        state.tmux.sessions.push(Session {
            id: "$2".to_string(),
            name: "notes".to_string(),
            attached_count: 1,
            active_window_id: Some("@20".to_string()),
            windows: vec![Window {
                id: "@20".to_string(),
                index: 0,
                name: "scratch".to_string(),
                active: true,
                flags: String::new(),
                alert: WindowAlert::None,
            }],
        });
        state.tmux.clients = vec![Client {
            name: ClientName("client-2".to_string()),
            session_id: "$2".to_string(),
            current_window_id: Some("@20".to_string()),
            activity: 99,
            tty: "/dev/pts/2".to_string(),
        }];
        state.target_client = Some(ClientName("client-2".to_string()));

        let output = render_ascii(&state, 96, 16);
        let work_line = output
            .lines()
            .find(|line| line.contains("work (1 attached)"))
            .expect("expected work session row");
        let editor_line = output
            .lines()
            .find(|line| line.contains("|-- 1 editor"))
            .expect("expected editor row");
        let notes_line = output
            .lines()
            .find(|line| line.contains("notes (1 attached)"))
            .expect("expected notes session row");
        let scratch_line = output
            .lines()
            .find(|line| line.contains("|-- 0 scratch"))
            .expect("expected scratch row");

        assert!(!work_line.contains("* active"));
        assert!(!editor_line.contains("* active"));
        assert!(!notes_line.contains("* active"));
        assert!(scratch_line.contains("* active"));
        assert!(output.contains("active notes:0.scratch"));
    }

    fn render_ascii(state: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test backend must initialize");
        terminal
            .draw(|frame| render_with_options(frame, state, RenderOptions::ascii()))
            .expect("draw should succeed");

        let buffer = terminal.backend().buffer();
        let mut lines = Vec::new();
        for y in 0..height {
            let mut row = String::new();
            for x in 0..width {
                row.push_str(buffer[(x, y)].symbol());
            }
            lines.push(row.trim_end().to_string());
        }

        lines.join("\n")
    }

    fn sample_state() -> AppState {
        AppState {
            tmux: TmuxState {
                sessions: vec![Session {
                    id: "$1".to_string(),
                    name: "work".to_string(),
                    attached_count: 1,
                    active_window_id: Some("@11".to_string()),
                    windows: vec![
                        Window {
                            id: "@10".to_string(),
                            index: 0,
                            name: "shell".to_string(),
                            active: false,
                            flags: String::new(),
                            alert: WindowAlert::None,
                        },
                        Window {
                            id: "@11".to_string(),
                            index: 1,
                            name: "editor".to_string(),
                            active: true,
                            flags: "*!".to_string(),
                            alert: WindowAlert::Bell,
                        },
                    ],
                }],
                clients: vec![Client {
                    name: ClientName("client-1".to_string()),
                    session_id: "$1".to_string(),
                    current_window_id: Some("@11".to_string()),
                    activity: 42,
                    tty: "/dev/pts/1".to_string(),
                }],
            },
            target_client: Some(ClientName("client-1".to_string())),
            ..AppState::default()
        }
    }
}
