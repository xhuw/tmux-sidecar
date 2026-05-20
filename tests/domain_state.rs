#[path = "../src/input.rs"]
mod input;
#[path = "../src/model.rs"]
mod model;

use input::InputBuffer;
use model::{
    AppState, EditAction, Focus, FocusMove, FocusRecovery, Mode, Session, TmuxState, TreeRowKind,
    Window, WindowAlert,
};

fn session(id: &str, name: &str, active_window_id: Option<&str>, windows: Vec<Window>) -> Session {
    Session {
        id: id.to_string(),
        name: name.to_string(),
        attached_count: 0,
        active_window_id: active_window_id.map(str::to_string),
        windows,
    }
}

fn window(id: &str, index: u32, name: &str, active: bool, alert: WindowAlert) -> Window {
    Window {
        id: id.to_string(),
        index,
        name: name.to_string(),
        active,
        flags: String::new(),
        alert,
    }
}

#[test]
fn tree_rows_include_new_rows_and_focus_moves_by_visible_order() {
    let tmux = TmuxState {
        sessions: vec![
            session(
                "$1",
                "work",
                Some("@11"),
                vec![
                    window("@10", 0, "shell", false, WindowAlert::None),
                    window("@11", 1, "editor", true, WindowAlert::Activity),
                ],
            ),
            session(
                "$2",
                "notes",
                Some("@20"),
                vec![window("@20", 0, "scratch", true, WindowAlert::Bell)],
            ),
        ],
    };

    let rows = tmux.tree_rows();
    assert_eq!(rows.len(), 8);
    assert_eq!(rows[0].focus, Focus::CreateSession);
    assert!(matches!(rows[4].kind, TreeRowKind::CreateWindow { .. }));
    assert!(matches!(rows[7].kind, TreeRowKind::CreateWindow { .. }));

    let mut app = AppState::from_tmux(tmux);
    assert_eq!(app.focus, Focus::CreateSession);

    assert!(app.move_focus(FocusMove::Down));
    assert_eq!(app.focus, Focus::Session("$1".to_string()));
    assert!(app.move_focus(FocusMove::Down));
    assert_eq!(app.focus, Focus::Window("@10".to_string()));
    assert!(app.move_focus(FocusMove::Down));
    assert_eq!(app.focus, Focus::Window("@11".to_string()));
    assert!(app.move_focus(FocusMove::Up));
    assert_eq!(app.focus, Focus::Window("@10".to_string()));
}

#[test]
fn reconciliation_recovers_focus_to_nearest_row_when_target_disappears() {
    let initial = TmuxState {
        sessions: vec![
            session(
                "$1",
                "work",
                Some("@11"),
                vec![
                    window("@10", 0, "shell", false, WindowAlert::None),
                    window("@11", 1, "editor", true, WindowAlert::None),
                ],
            ),
            session("$2", "notes", None, vec![]),
        ],
    };

    let mut app = AppState::from_tmux(initial);
    app.focus = Focus::Window("@11".to_string());

    let refreshed = TmuxState {
        sessions: vec![
            session(
                "$1",
                "work",
                Some("@10"),
                vec![window("@10", 0, "shell", true, WindowAlert::None)],
            ),
            session("$2", "notes", None, vec![]),
        ],
    };

    let reconcile = app.reconcile_tmux(refreshed);
    assert_eq!(reconcile.recovery, FocusRecovery::NearestRow);
    assert_eq!(reconcile.row_index, 3);
    assert_eq!(app.focus, Focus::CreateWindow("$1".to_string()));
}

#[test]
fn edit_buffer_applies_single_line_actions_only_in_edit_modes() {
    let mut app = AppState::default();
    app.mode = Mode::RenameWindow {
        id: "@1".to_string(),
        original_name: "original".to_string(),
        input: InputBuffer::from_text("na\nme"),
    };

    assert_eq!(
        app.edit_buffer().map(|buffer| buffer.as_str()),
        Some("name")
    );
    assert!(app.apply_edit_action(EditAction::MoveLeft));
    assert!(app.apply_edit_action(EditAction::Backspace));
    assert_eq!(app.edit_buffer().map(|buffer| buffer.as_str()), Some("nae"));

    assert!(!app.apply_edit_action(EditAction::Insert('\n')));
    assert!(app.apply_edit_action(EditAction::Insert('x')));
    assert_eq!(
        app.edit_buffer().map(|buffer| buffer.as_str()),
        Some("naxe")
    );

    assert!(app.apply_edit_action(EditAction::MoveHome));
    assert!(app.apply_edit_action(EditAction::Delete));
    assert_eq!(app.edit_buffer().map(|buffer| buffer.as_str()), Some("axe"));

    assert!(app.apply_edit_action(EditAction::Clear));
    assert_eq!(app.edit_buffer().map(|buffer| buffer.as_str()), Some(""));

    app.mode = Mode::Normal;
    assert!(!app.apply_edit_action(EditAction::Insert('z')));
}

#[test]
fn alert_state_is_preserved_separately_from_active_and_focus_state() {
    let mut initial_window = window("@10", 0, "shell", true, WindowAlert::None);
    initial_window.set_flags("*");
    let mut alerted_window = window("@11", 1, "editor", true, WindowAlert::None);
    alerted_window.set_flags("*!#");

    let mut app = AppState::from_tmux(TmuxState {
        sessions: vec![session(
            "$1",
            "work",
            Some("@11"),
            vec![initial_window, alerted_window],
        )],
    });
    app.focus = Focus::Window("@11".to_string());

    let mut refreshed_alerted_window = window("@11", 1, "editor", false, WindowAlert::None);
    refreshed_alerted_window.set_flags("#");

    let reconcile = app.reconcile_tmux(TmuxState {
        sessions: vec![session(
            "$1",
            "work",
            Some("@10"),
            vec![
                window("@10", 0, "shell", true, WindowAlert::None),
                refreshed_alerted_window,
            ],
        )],
    });

    assert_eq!(reconcile.recovery, FocusRecovery::Preserved);
    assert_eq!(app.focus, Focus::Window("@11".to_string()));

    let alerted_row = app
        .tree_rows()
        .into_iter()
        .find(|row| row.focus == Focus::Window("@11".to_string()))
        .expect("expected alerted window row");

    assert_eq!(alerted_row.alert(), Some(WindowAlert::Activity));
    assert!(!alerted_row.active());
}

#[test]
fn alert_kind_maps_tmux_flags_for_activity_bell_and_silence() {
    let mut activity = window("@1", 0, "activity", false, WindowAlert::None);
    activity.set_flags("#");
    let mut bell = window("@2", 1, "bell", false, WindowAlert::None);
    bell.set_flags("!");
    let mut silence = window("@3", 2, "silence", false, WindowAlert::None);
    silence.set_flags("~");

    assert_eq!(activity.alert, WindowAlert::Activity);
    assert_eq!(bell.alert, WindowAlert::Bell);
    assert_eq!(silence.alert, WindowAlert::Silence);
}
