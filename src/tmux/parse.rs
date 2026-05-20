use thiserror::Error;

use crate::model::ClientName;

pub const FIELD_SEPARATOR: char = '\u{1f}';

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: String,
    pub name: String,
    pub attached_count: u32,
    pub active_window_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRecord {
    pub session_id: String,
    pub session_name: String,
    pub id: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub flags: WindowFlags,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRecord {
    pub name: ClientName,
    pub session_id: String,
    pub current_window_id: Option<String>,
    pub activity: u64,
    pub tty: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowFlags {
    pub raw: String,
    pub has_activity: bool,
    pub has_bell: bool,
    pub has_silence: bool,
}

impl WindowFlags {
    pub fn from_raw(raw: String) -> Self {
        Self {
            has_activity: raw.contains('#'),
            has_bell: raw.contains('!'),
            has_silence: raw.contains('~'),
            raw,
        }
    }

    pub fn has_alert(&self) -> bool {
        self.has_activity || self.has_bell || self.has_silence
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ParseError {
    #[error("expected {expected} fields for {record}, got {actual}: {line}")]
    WrongFieldCount {
        record: &'static str,
        expected: usize,
        actual: usize,
        line: String,
    },
    #[error("invalid integer for {field}: {value}")]
    InvalidInteger { field: &'static str, value: String },
    #[error("invalid boolean for {field}: {value}")]
    InvalidBoolean { field: &'static str, value: String },
    #[error("missing required value for {field}")]
    MissingField { field: &'static str },
}

pub fn split_fields(line: &str) -> Vec<&str> {
    line.split(FIELD_SEPARATOR).collect()
}

pub fn session_format() -> String {
    join_with_separator(&[
        "#{session_id}",
        "#{session_name}",
        "#{session_attached}",
        "#{window_id}",
    ])
}

pub fn window_format() -> String {
    join_with_separator(&[
        "#{session_id}",
        "#{session_name}",
        "#{window_id}",
        "#{window_index}",
        "#{window_name}",
        "#{window_active}",
        "#{window_flags}",
    ])
}

pub fn client_format() -> String {
    join_with_separator(&[
        "#{client_name}",
        "#{session_id}",
        "#{window_id}",
        "#{client_activity}",
        "#{client_tty}",
    ])
}

pub fn parse_sessions(raw: &str) -> Result<Vec<SessionRecord>, ParseError> {
    parse_lines(raw, parse_session_line)
}

pub fn parse_windows(raw: &str) -> Result<Vec<WindowRecord>, ParseError> {
    parse_lines(raw, parse_window_line)
}

pub fn parse_clients(raw: &str) -> Result<Vec<ClientRecord>, ParseError> {
    parse_lines(raw, parse_client_line)
}

fn parse_lines<T, F>(raw: &str, mut parser: F) -> Result<Vec<T>, ParseError>
where
    F: FnMut(&str) -> Result<T, ParseError>,
{
    let mut values = Vec::new();

    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }

        values.push(parser(line)?);
    }

    Ok(values)
}

fn parse_session_line(line: &str) -> Result<SessionRecord, ParseError> {
    let fields = split_fields(line);
    let [id, name, attached_count, active_window_id] = expect_fields::<4>("session", fields, line)?;

    Ok(SessionRecord {
        id: required("session_id", id)?,
        name: name.to_owned(),
        attached_count: parse_u32("session_attached", attached_count)?,
        active_window_id: optional_id(active_window_id),
    })
}

fn parse_window_line(line: &str) -> Result<WindowRecord, ParseError> {
    let fields = split_fields(line);
    let [session_id, session_name, id, index, name, active, flags] =
        expect_fields::<7>("window", fields, line)?;

    Ok(WindowRecord {
        session_id: required("session_id", session_id)?,
        session_name: session_name.to_owned(),
        id: required("window_id", id)?,
        index: parse_u32("window_index", index)?,
        name: name.to_owned(),
        active: parse_bool("window_active", active)?,
        flags: WindowFlags::from_raw(flags.to_owned()),
    })
}

fn parse_client_line(line: &str) -> Result<ClientRecord, ParseError> {
    let fields = split_fields(line);
    let [name, session_id, current_window_id, activity, tty] =
        expect_fields::<5>("client", fields, line)?;

    Ok(ClientRecord {
        name: ClientName(required("client_name", name)?),
        session_id: required("client_session", session_id)?,
        current_window_id: optional_id(current_window_id),
        activity: parse_u64("client_activity", activity)?,
        tty: tty.to_owned(),
    })
}

fn expect_fields<'a, const N: usize>(
    record: &'static str,
    fields: Vec<&'a str>,
    line: &str,
) -> Result<[&'a str; N], ParseError> {
    fields
        .try_into()
        .map_err(|values: Vec<&str>| ParseError::WrongFieldCount {
            record,
            expected: N,
            actual: values.len(),
            line: line.to_owned(),
        })
}

fn parse_u32(field: &'static str, value: &str) -> Result<u32, ParseError> {
    value.parse().map_err(|_| ParseError::InvalidInteger {
        field,
        value: value.to_owned(),
    })
}

fn parse_u64(field: &'static str, value: &str) -> Result<u64, ParseError> {
    value.parse().map_err(|_| ParseError::InvalidInteger {
        field,
        value: value.to_owned(),
    })
}

fn parse_bool(field: &'static str, value: &str) -> Result<bool, ParseError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(ParseError::InvalidBoolean {
            field,
            value: value.to_owned(),
        }),
    }
}

fn required(field: &'static str, value: &str) -> Result<String, ParseError> {
    if value.is_empty() {
        return Err(ParseError::MissingField { field });
    }

    Ok(value.to_owned())
}

fn optional_id(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn join_with_separator(fields: &[&str]) -> String {
    fields.join(&FIELD_SEPARATOR.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        FIELD_SEPARATOR, ParseError, parse_clients, parse_sessions, parse_windows, split_fields,
    };

    #[test]
    fn splits_fields_on_unit_separator() {
        let line = format!("a{sep}b{sep}c", sep = FIELD_SEPARATOR);

        assert_eq!(split_fields(&line), vec!["a", "b", "c"]);
    }

    #[test]
    fn parses_sessions_successfully() {
        let raw = format!("$1{sep}dev{sep}2{sep}@1\n", sep = FIELD_SEPARATOR);

        let sessions = parse_sessions(&raw).expect("sessions should parse");

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "$1");
        assert_eq!(sessions[0].name, "dev");
        assert_eq!(sessions[0].attached_count, 2);
        assert_eq!(sessions[0].active_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn rejects_session_with_wrong_field_count() {
        let raw = format!("$1{sep}dev{sep}2\n", sep = FIELD_SEPARATOR);

        let error = parse_sessions(&raw).expect_err("parser must reject malformed output");

        assert!(matches!(
            error,
            ParseError::WrongFieldCount {
                record: "session",
                expected: 4,
                actual: 3,
                ..
            }
        ));
    }

    #[test]
    fn parses_windows_and_alert_flags() {
        let raw = format!(
            "$1{sep}dev{sep}@9{sep}4{sep}editor{sep}1{sep}!#*\n",
            sep = FIELD_SEPARATOR
        );

        let windows = parse_windows(&raw).expect("windows should parse");

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id, "@9");
        assert_eq!(windows[0].index, 4);
        assert!(windows[0].active);
        assert!(windows[0].flags.has_activity);
        assert!(windows[0].flags.has_bell);
        assert!(windows[0].flags.has_alert());
    }

    #[test]
    fn rejects_window_with_non_boolean_active_value() {
        let raw = format!(
            "$1{sep}dev{sep}@9{sep}4{sep}editor{sep}yes{sep}\n",
            sep = FIELD_SEPARATOR
        );

        let error = parse_windows(&raw).expect_err("parser must reject invalid active flag");

        assert!(matches!(
            error,
            ParseError::InvalidBoolean {
                field: "window_active",
                value
            } if value == "yes"
        ));
    }

    #[test]
    fn rejects_client_with_invalid_activity() {
        let raw = format!(
            "client-1{sep}$1{sep}@9{sep}not-a-number{sep}/dev/pts/2\n",
            sep = FIELD_SEPARATOR
        );

        let error = parse_clients(&raw).expect_err("parser must reject invalid client activity");

        assert!(matches!(
            error,
            ParseError::InvalidInteger {
                field: "client_activity",
                value
            } if value == "not-a-number"
        ));
    }

    #[test]
    fn parses_clients_with_current_window() {
        let raw = format!(
            "client-1{sep}$1{sep}@9{sep}42{sep}/dev/pts/2\n",
            sep = FIELD_SEPARATOR
        );

        let clients = parse_clients(&raw).expect("clients should parse");

        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].name.0, "client-1");
        assert_eq!(clients[0].session_id, "$1");
        assert_eq!(clients[0].current_window_id.as_deref(), Some("@9"));
        assert_eq!(clients[0].activity, 42);
    }
}
