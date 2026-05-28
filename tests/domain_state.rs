#[path = "../src/domain/mod.rs"]
mod domain;
#[path = "../src/input.rs"]
mod input;
#[path = "../src/model.rs"]
mod model;

use input::InputBuffer;
use model::{
    AppState, Client, ClientName, EditAction, Focus, FocusMove, FocusRecovery, Mode, Session,
    TmuxState, TreeRowKind, Window, WindowAlert, WinlinkKey,
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
        activity: 0,
        activity_flag: false,
        silence_flag: false,
    }
}

fn window_with_activity(
    id: &str,
    index: u32,
    name: &str,
    active: bool,
    alert: WindowAlert,
    activity: u64,
) -> Window {
    Window {
        activity,
        ..window(id, index, name, active, alert)
    }
}

fn client(name: &str, session_id: &str, current_window_id: Option<&str>, activity: u64) -> Client {
    Client {
        name: ClientName(name.to_string()),
        session_id: session_id.to_string(),
        current_window_id: current_window_id.map(str::to_string),
        activity,
        tty: format!("/dev/pts/{activity}"),
    }
}

fn window_focus(session_id: &str, window_id: &str) -> Focus {
    Focus::window(session_id, window_id)
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
                    window("@11", 1, "editor", true, WindowAlert::None),
                ],
            ),
            session(
                "$2",
                "notes",
                Some("@20"),
                vec![window("@20", 0, "scratch", true, WindowAlert::Bell)],
            ),
        ],
        clients: vec![client("client-1", "$1", Some("@11"), 10)],
    };

    let rows = tmux.tree_rows();
    assert_eq!(rows.len(), 8);
    assert!(matches!(rows[0].kind, TreeRowKind::Session { .. }));
    assert!(matches!(rows[3].kind, TreeRowKind::CreateWindow { .. }));
    assert!(matches!(rows[6].kind, TreeRowKind::CreateWindow { .. }));
    assert_eq!(rows[7].focus, Focus::CreateSession);

    let mut app = AppState::from_tmux(tmux);
    assert_eq!(app.focus, Focus::CreateSession);
    assert_eq!(app.focused_row_index(), Some(7));

    assert!(!app.move_focus(FocusMove::Down));
    assert!(app.move_focus(FocusMove::Up));
    assert_eq!(app.focus, Focus::CreateWindow("$2".to_string()));
    assert!(app.move_focus(FocusMove::Up));
    assert_eq!(app.focus, window_focus("$2", "@20"));
    app.focus = Focus::Session("$1".to_string());
    assert!(app.move_focus(FocusMove::Down));
    assert_eq!(app.focus, window_focus("$1", "@10"));
    assert!(app.move_focus(FocusMove::Down));
    assert_eq!(app.focus, window_focus("$1", "@11"));
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
        clients: vec![client("client-1", "$1", Some("@11"), 10)],
    };

    let mut app = AppState::from_tmux(initial);
    app.focus = window_focus("$1", "@11");

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
        clients: vec![client("client-1", "$1", Some("@10"), 10)],
    };

    let reconcile = app.reconcile_tmux(refreshed);
    assert_eq!(reconcile.recovery, FocusRecovery::NearestRow);
    assert_eq!(reconcile.row_index, 2);
    assert_eq!(app.focus, Focus::CreateWindow("$1".to_string()));
}

#[test]
fn edit_buffer_applies_single_line_actions_only_in_edit_modes() {
    let mut app = AppState::default();
    app.mode = Mode::RenameWindow {
        session_id: "$1".to_string(),
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
        clients: vec![client("client-1", "$1", Some("@11"), 10)],
    });
    app.focus = window_focus("$1", "@11");

    let mut refreshed_alerted_window = window("@11", 1, "editor", false, WindowAlert::None);
    refreshed_alerted_window.set_flags("!");

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
        clients: vec![client("client-1", "$1", Some("@10"), 10)],
    });

    assert_eq!(reconcile.recovery, FocusRecovery::Preserved);
    assert_eq!(app.focus, window_focus("$1", "@11"));

    let alerted_row = app
        .tree_rows()
        .into_iter()
        .find(|row| row.focus == window_focus("$1", "@11"))
        .expect("expected alerted window row");

    assert_eq!(alerted_row.alert(), Some(WindowAlert::Bell));
    assert!(!alerted_row.active());
}

#[test]
fn linked_window_rows_keep_session_local_active_and_alert_state() {
    let shared_current = window("@shared", 0, "shared", true, WindowAlert::None);
    let shared_alerted = window("@shared", 5, "shared", false, WindowAlert::Bell);
    let app = AppState {
        tmux: TmuxState {
            sessions: vec![
                session("$1", "current", Some("@shared"), vec![shared_current]),
                session(
                    "$2",
                    "other",
                    Some("@20"),
                    vec![
                        window("@20", 0, "own", true, WindowAlert::None),
                        shared_alerted,
                    ],
                ),
            ],
            clients: vec![client("client-1", "$1", Some("@shared"), 10)],
        },
        target_client: Some(ClientName("client-1".to_string())),
        ..AppState::default()
    };

    let rows = app.tree_rows();
    let current_row = rows
        .iter()
        .find(|row| row.focus == window_focus("$1", "@shared"))
        .expect("expected current-session shared row");
    let alerted_row = rows
        .iter()
        .find(|row| row.focus == window_focus("$2", "@shared"))
        .expect("expected other-session shared row");

    assert!(current_row.active());
    assert_eq!(current_row.alert(), None);
    assert!(!alerted_row.active());
    assert_eq!(alerted_row.alert(), Some(WindowAlert::Bell));
    assert_ne!(current_row.focus, alerted_row.focus);

    let session_states = app.tmux.session_states();
    let current_key = WinlinkKey::new("$1", "@shared");
    let alerted_key = WinlinkKey::new("$2", "@shared");
    let current_window = session_states
        .get("$1")
        .and_then(|session| session.windows.get(&current_key))
        .expect("expected current-session winlink state");
    let alerted_window = session_states
        .get("$2")
        .and_then(|session| session.windows.get(&alerted_key))
        .expect("expected alerted-session winlink state");

    assert_eq!(
        app.tmux.visible_window_key(app.target_client.as_ref()),
        Some(current_key)
    );
    assert!(current_window.active);
    assert!(!current_window.bell_flag);
    assert!(!alerted_window.active);
    assert!(alerted_window.bell_flag);
}

#[test]
fn reconciliation_does_not_synthesize_activity_alerts() {
    let mut app = AppState {
        tmux: TmuxState {
            sessions: vec![
                session(
                    "$1",
                    "visible",
                    Some("@10"),
                    vec![window_with_activity(
                        "@10",
                        0,
                        "visible",
                        true,
                        WindowAlert::None,
                        10,
                    )],
                ),
                session(
                    "$2",
                    "other",
                    Some("@20"),
                    vec![window_with_activity(
                        "@20",
                        0,
                        "other",
                        true,
                        WindowAlert::None,
                        20,
                    )],
                ),
            ],
            clients: vec![client("client-1", "$1", Some("@10"), 10)],
        },
        target_client: Some(ClientName("client-1".to_string())),
        ..AppState::default()
    };
    app.reconcile_tmux(app.tmux.clone());

    app.reconcile_tmux(TmuxState {
        sessions: vec![
            session(
                "$1",
                "visible",
                Some("@10"),
                vec![window_with_activity(
                    "@10",
                    0,
                    "visible",
                    true,
                    WindowAlert::None,
                    10,
                )],
            ),
            session(
                "$2",
                "other",
                Some("@20"),
                vec![window_with_activity(
                    "@20",
                    0,
                    "other",
                    true,
                    WindowAlert::None,
                    21,
                )],
            ),
        ],
        clients: vec![client("client-1", "$1", Some("@10"), 10)],
    });

    let alerted_row = app
        .tree_rows()
        .into_iter()
        .find(|row| row.focus == window_focus("$2", "@20"))
        .expect("expected other session row");
    assert_eq!(alerted_row.alert(), None);
}

#[test]
fn app_tree_rows_only_mark_target_clients_visible_window_active() {
    let mut app = AppState::from_tmux(TmuxState {
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
            session(
                "$2",
                "notes",
                Some("@20"),
                vec![window("@20", 0, "scratch", true, WindowAlert::None)],
            ),
        ],
        clients: vec![client("client-2", "$2", Some("@20"), 50)],
    });
    app.target_client = Some(ClientName("client-2".to_string()));

    let rows = app.tree_rows();
    let work_session = rows
        .iter()
        .find(|row| row.focus == Focus::Session("$1".to_string()))
        .expect("expected work session row");
    let editor_window = rows
        .iter()
        .find(|row| row.focus == window_focus("$1", "@11"))
        .expect("expected editor row");
    let notes_session = rows
        .iter()
        .find(|row| row.focus == Focus::Session("$2".to_string()))
        .expect("expected notes session row");
    let scratch_window = rows
        .iter()
        .find(|row| row.focus == window_focus("$2", "@20"))
        .expect("expected scratch row");

    assert!(!work_session.active());
    assert!(!editor_window.active());
    assert!(!notes_session.active());
    assert!(scratch_window.active());
}

#[test]
fn focus_visible_target_moves_focus_to_target_clients_current_window() {
    let mut app = AppState::from_tmux(TmuxState {
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
            session(
                "$2",
                "notes",
                Some("@20"),
                vec![window("@20", 0, "scratch", true, WindowAlert::None)],
            ),
        ],
        clients: vec![client("client-2", "$2", Some("@20"), 50)],
    });
    app.target_client = Some(ClientName("client-2".to_string()));

    assert!(app.focus_visible_target());
    assert_eq!(app.focus, window_focus("$2", "@20"));
    assert_eq!(app.focused_row_index(), Some(5));
    assert!(!app.focus_visible_target());
}

#[test]
fn vim_and_jump_navigation_follow_visible_row_order() {
    let tmux = TmuxState {
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
            session(
                "$2",
                "notes",
                Some("@20"),
                vec![window("@20", 0, "scratch", true, WindowAlert::Bell)],
            ),
        ],
        clients: vec![client("client-1", "$1", Some("@11"), 10)],
    };
    let mut app = AppState::from_tmux(tmux);
    app.focus = window_focus("$1", "@10");

    assert!(app.focus_first_row());
    assert_eq!(app.focus, Focus::Session("$1".to_string()));
    assert!(app.focus_last_row());
    assert_eq!(app.focus, Focus::CreateSession);

    assert!(app.start_jump());
    let targets = app.jump_targets();
    assert_eq!(targets.len(), 8);

    let notes_label = targets
        .iter()
        .find(|target| target.focus == Focus::Session("$2".to_string()))
        .map(|target| target.label)
        .expect("expected notes label");
    assert!(app.focus_jump_label(notes_label));
    assert_eq!(app.focus, Focus::Session("$2".to_string()));

    let focused_before_invalid = app.focus.clone();
    assert!(!app.focus_jump_label('!'));
    assert_eq!(app.focus, focused_before_invalid);
}

#[test]
fn alert_kind_only_marks_bell_flags_as_alerts() {
    let mut activity = window("@1", 0, "activity", false, WindowAlert::None);
    activity.set_flags("#");
    let mut bell = window("@2", 1, "bell", false, WindowAlert::None);
    bell.set_flags("!");
    let mut silence = window("@3", 2, "silence", false, WindowAlert::None);
    silence.set_flags("~");
    let mut current_and_alert = window("@4", 3, "build", false, WindowAlert::None);
    current_and_alert.set_flags("#!");

    assert_eq!(activity.alert, WindowAlert::None);
    assert_eq!(bell.alert, WindowAlert::Bell);
    assert_eq!(silence.alert, WindowAlert::None);
    assert_eq!(current_and_alert.alert, WindowAlert::Bell);
}

#[test]
fn tree_rows_ignore_activity_flags_but_keep_bell_alerts() {
    let mut active_window = window("@11", 1, "editor", true, WindowAlert::None);
    active_window.set_flags("#!");
    let app = AppState::from_tmux(TmuxState {
        sessions: vec![session("$1", "work", Some("@11"), vec![active_window])],
        clients: vec![client("client-1", "$1", Some("@11"), 10)],
    });

    let row = app
        .tree_rows()
        .into_iter()
        .find(|row| row.focus == window_focus("$1", "@11"))
        .expect("expected editor row");
    assert!(row.active());
    assert!(row.active());
    assert_eq!(row.alert(), Some(WindowAlert::Bell));
}
