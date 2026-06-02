use std::io::Write;

use anyhow::{Context, Result};

use crate::{
    cli::{QueryArgs, QueryCommand},
    ipc::ProjectionState,
};

pub fn write_result(
    args: &QueryArgs,
    state: &ProjectionState,
    writer: &mut impl Write,
) -> Result<()> {
    match args.command_or_default() {
        QueryCommand::Alerts => writeln!(writer, "{}", state.active_alert_count())
            .context("failed to write query output"),
        QueryCommand::All => {
            serde_json::to_writer(&mut *writer, state)
                .context("failed to serialize query output")?;
            writeln!(writer).context("failed to write query output")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::write_result;
    use crate::{
        cli::{QueryArgs, QueryCommand},
        ipc::{ProjectionSession, ProjectionState, ProjectionWindow},
    };

    #[test]
    fn query_alerts_writes_plain_count_for_status_lines() {
        let args = QueryArgs {
            command: Some(QueryCommand::Alerts),
            ..QueryArgs::default()
        };
        let state = ProjectionState {
            tmux_socket_path: Path::new("/tmp/tmux/default").to_path_buf(),
            sessions: vec![ProjectionSession {
                id: String::from("$1"),
                name: String::from("work"),
                attached_count: 1,
                active_window_id: None,
                windows: vec![
                    ProjectionWindow {
                        id: String::from("@1"),
                        index: 0,
                        name: String::from("shell"),
                        active: true,
                        activity: 0,
                        activity_flag: true,
                        bell_flag: false,
                        silence_flag: false,
                    },
                    ProjectionWindow {
                        id: String::from("@2"),
                        index: 1,
                        name: String::from("tests"),
                        active: false,
                        activity: 0,
                        activity_flag: false,
                        bell_flag: true,
                        silence_flag: false,
                    },
                ],
            }],
            clients: Vec::new(),
        };
        let mut output = Vec::new();

        write_result(&args, &state, &mut output).expect("write query result");

        assert_eq!(String::from_utf8(output).expect("utf8 output"), "1\n");
    }

    #[test]
    fn query_all_writes_projection_snapshot_as_json() {
        let args = QueryArgs {
            command: Some(QueryCommand::All),
            ..QueryArgs::default()
        };
        let state = ProjectionState {
            tmux_socket_path: Path::new("/tmp/tmux/default").to_path_buf(),
            sessions: vec![ProjectionSession {
                id: String::from("$1"),
                name: String::from("work"),
                attached_count: 1,
                active_window_id: Some(String::from("@1")),
                windows: vec![
                    ProjectionWindow {
                        id: String::from("@1"),
                        index: 0,
                        name: String::from("shell"),
                        active: true,
                        activity: 10,
                        activity_flag: false,
                        bell_flag: false,
                        silence_flag: false,
                    },
                    ProjectionWindow {
                        id: String::from("@2"),
                        index: 1,
                        name: String::from("tests"),
                        active: false,
                        activity: 20,
                        activity_flag: false,
                        bell_flag: true,
                        silence_flag: false,
                    },
                ],
            }],
            clients: vec![crate::ipc::ProjectionClient {
                name: String::from("client-1"),
                session_id: String::from("$1"),
                current_window_id: Some(String::from("@1")),
                activity: 20,
                tty: String::from("/dev/pts/1"),
            }],
        };
        let mut output = Vec::new();

        write_result(&args, &state, &mut output).expect("write query result");

        let output = String::from_utf8(output).expect("utf8 output");
        let decoded: ProjectionState =
            serde_json::from_str(output.trim_end()).expect("decode query json");
        assert_eq!(decoded, state);
        assert!(output.ends_with('\n'));
    }
}
