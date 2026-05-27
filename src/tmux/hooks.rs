#![allow(dead_code)]

use std::collections::BTreeSet;

use super::{
    TmuxError,
    command::{self, SocketOptions},
};

pub const DEFAULT_HOOK_BINARY: &str = "tmux-sidecar";
pub const RESERVED_HOOK_INDEX_START: u16 = 900;

const TMUX_SOCKET_PATH_FORMAT: &str = "#{q:socket_path}";
const SESSION_ID_FORMAT: &str = "#{q:session_id}";
const WINDOW_ID_FORMAT: &str = "#{q:window_id}";
const WINDOW_INDEX_FORMAT: &str = "#{q:window_index}";
const PANE_ID_FORMAT: &str = "#{q:pane_id}";
const CLIENT_NAME_FORMAT: &str = "#{q:client_name}";

const SESSION_ID_ARG: HookArgument = HookArgument::new("--session-id", SESSION_ID_FORMAT);
const WINDOW_ID_ARG: HookArgument = HookArgument::new("--window-id", WINDOW_ID_FORMAT);
const WINDOW_INDEX_ARG: HookArgument = HookArgument::new("--window-index", WINDOW_INDEX_FORMAT);
const PANE_ID_ARG: HookArgument = HookArgument::new("--pane-id", PANE_ID_FORMAT);
const CLIENT_NAME_ARG: HookArgument = HookArgument::new("--client-name", CLIENT_NAME_FORMAT);

const SESSION_HOOKS: &[HookDefinition] = &[
    HookDefinition::new("session-created", &[SESSION_ID_ARG]),
    HookDefinition::new("session-closed", &[SESSION_ID_ARG]),
    HookDefinition::new("session-renamed", &[SESSION_ID_ARG]),
    HookDefinition::new(
        "session-window-changed",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG],
    ),
];

const WINDOW_HOOKS: &[HookDefinition] = &[
    HookDefinition::new(
        "window-linked",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG],
    ),
    HookDefinition::new(
        "window-unlinked",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG],
    ),
    HookDefinition::new(
        "window-renamed",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG],
    ),
    HookDefinition::new(
        "window-pane-changed",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG, PANE_ID_ARG],
    ),
    HookDefinition::new(
        "window-layout-changed",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG],
    ),
];

const ALERT_HOOKS: &[HookDefinition] = &[
    HookDefinition::disabled(
        "alert-activity",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG, PANE_ID_ARG],
    ),
    HookDefinition::new(
        "alert-bell",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG, PANE_ID_ARG],
    ),
    HookDefinition::disabled(
        "alert-silence",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG, PANE_ID_ARG],
    ),
];

const CLIENT_HOOKS: &[HookDefinition] = &[
    HookDefinition::new(
        "client-attached",
        &[CLIENT_NAME_ARG, SESSION_ID_ARG, WINDOW_ID_ARG],
    ),
    HookDefinition::new(
        "client-detached",
        &[CLIENT_NAME_ARG, SESSION_ID_ARG, WINDOW_ID_ARG],
    ),
    HookDefinition::new(
        "client-session-changed",
        &[CLIENT_NAME_ARG, SESSION_ID_ARG, WINDOW_ID_ARG],
    ),
];

const FALLBACK_HOOKS: &[HookDefinition] = &[
    HookDefinition::new(
        "after-new-session",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG, PANE_ID_ARG],
    ),
    HookDefinition::new(
        "after-new-window",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG, PANE_ID_ARG],
    ),
    HookDefinition::new("after-rename-session", &[SESSION_ID_ARG]),
    HookDefinition::new(
        "after-rename-window",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG],
    ),
    HookDefinition::new(
        "after-kill-pane",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG, PANE_ID_ARG],
    ),
    HookDefinition::new(
        "after-select-window",
        &[SESSION_ID_ARG, WINDOW_ID_ARG, WINDOW_INDEX_ARG, PANE_ID_ARG],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HookArgument {
    flag: &'static str,
    quoted_tmux_format: &'static str,
}

impl HookArgument {
    const fn new(flag: &'static str, quoted_tmux_format: &'static str) -> Self {
        Self {
            flag,
            quoted_tmux_format,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HookDefinition {
    name: &'static str,
    args: &'static [HookArgument],
    install: bool,
}

impl HookDefinition {
    const fn new(name: &'static str, args: &'static [HookArgument]) -> Self {
        Self {
            name,
            args,
            install: true,
        }
    }

    const fn disabled(name: &'static str, args: &'static [HookArgument]) -> Self {
        Self {
            name,
            args,
            install: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCommandProgram {
    argv: Vec<String>,
}

impl HookCommandProgram {
    pub fn new(argv: Vec<String>) -> Self {
        assert!(
            !argv.is_empty(),
            "hook command program argv must not be empty"
        );
        Self { argv }
    }

    pub fn from_executable(executable: impl Into<String>) -> Self {
        Self::new(vec![executable.into()])
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    pub fn install_hooks_shell_command(&self) -> String {
        shell_join(
            self.argv
                .iter()
                .cloned()
                .chain([String::from("install-hooks")]),
        )
    }
}

impl Default for HookCommandProgram {
    fn default() -> Self {
        Self::from_executable(DEFAULT_HOOK_BINARY)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledHook {
    pub name: &'static str,
    pub index: u16,
    pub command: String,
}

impl InstalledHook {
    pub fn qualified_name(&self) -> String {
        format!("{}[{}]", self.name, self.index)
    }
}

pub fn installed_hooks(program: &HookCommandProgram) -> Vec<InstalledHook> {
    hook_definitions()
        .iter()
        .enumerate()
        .filter(|(_, definition)| definition.install)
        .map(|(offset, definition)| InstalledHook {
            name: definition.name,
            index: RESERVED_HOOK_INDEX_START + offset as u16,
            command: hook_command(program, definition),
        })
        .collect()
}

pub fn install_hooks(
    socket: &SocketOptions,
    program: &HookCommandProgram,
) -> Result<(), TmuxError> {
    uninstall_hooks(socket)?;
    configure_global_window_monitoring(socket)?;
    configure_existing_window_monitoring(socket)?;

    for hook in installed_hooks(program) {
        command::run_tmux(
            socket,
            vec![
                String::from("set-hook"),
                String::from("-g"),
                hook.qualified_name(),
                hook.command,
            ],
        )?;
    }

    Ok(())
}

pub fn uninstall_hooks(socket: &SocketOptions) -> Result<(), TmuxError> {
    for hook in managed_hooks(&HookCommandProgram::default()) {
        command::run_tmux(
            socket,
            vec![
                String::from("set-hook"),
                String::from("-gu"),
                hook.qualified_name(),
            ],
        )?;
    }

    Ok(())
}

fn managed_hooks(program: &HookCommandProgram) -> Vec<InstalledHook> {
    hook_definitions()
        .iter()
        .enumerate()
        .map(|(offset, definition)| InstalledHook {
            name: definition.name,
            index: RESERVED_HOOK_INDEX_START + offset as u16,
            command: hook_command(program, definition),
        })
        .collect()
}

pub fn configure_existing_window_monitoring(socket: &SocketOptions) -> Result<(), TmuxError> {
    for window_id in list_window_ids(socket)? {
        configure_window_monitoring(socket, &window_id)?;
    }

    Ok(())
}

pub fn configure_window_monitoring(
    socket: &SocketOptions,
    window_id: &str,
) -> Result<(), TmuxError> {
    for (option, value) in window_monitoring_options() {
        command::run_tmux(
            socket,
            ["set-window-option", "-q", "-t", window_id, option, value],
        )?;
    }

    Ok(())
}

pub fn init_plugin_snippet(program: &HookCommandProgram) -> String {
    let command = program.install_hooks_shell_command();
    let quoted = if command.contains('\'') {
        format!("\"{}\"", command.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("'{command}'")
    };

    format!(
        "# tmux-sidecar hook/server rewrite\n# Installs or refreshes tmux-sidecar hooks for the current tmux server.\nrun-shell -b {quoted}\n"
    )
}

fn configure_global_window_monitoring(socket: &SocketOptions) -> Result<(), TmuxError> {
    for (option, value) in window_monitoring_options() {
        command::run_tmux(socket, ["set-window-option", "-gq", option, value])?;
    }

    Ok(())
}

fn hook_command(program: &HookCommandProgram, definition: &HookDefinition) -> String {
    let mut words = program
        .argv()
        .iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<_>>();
    words.push(String::from("hook"));
    words.push(String::from("--socket-path"));
    words.push(String::from(TMUX_SOCKET_PATH_FORMAT));
    words.push(String::from("--event"));
    words.push(shell_quote(definition.name));

    for arg in definition.args {
        words.push(arg.flag.to_owned());
        words.push(arg.quoted_tmux_format.to_owned());
    }

    format!("run-shell -b \"{}\"", words.join(" "))
}

fn hook_definitions() -> Vec<HookDefinition> {
    SESSION_HOOKS
        .iter()
        .chain(WINDOW_HOOKS)
        .chain(ALERT_HOOKS)
        .chain(CLIENT_HOOKS)
        .chain(FALLBACK_HOOKS)
        .copied()
        .collect()
}

fn list_window_ids(socket: &SocketOptions) -> Result<Vec<String>, TmuxError> {
    let output = command::run_tmux(socket, ["list-windows", "-a", "-F", "#{window_id}"])?;

    Ok(output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

fn window_monitoring_options() -> [(&'static str, &'static str); 1] {
    [("monitor-bell", "on")]
}

fn shell_join(words: impl IntoIterator<Item = String>) -> String {
    words
        .into_iter()
        .map(|word| shell_quote(&word))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(word: &str) -> String {
    if !word.is_empty() && word.bytes().all(is_shell_safe) {
        return word.to_owned();
    }

    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

fn is_shell_safe(byte: u8) -> bool {
    matches!(
        byte,
        b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'_'
            | b'@'
            | b'%'
            | b'+'
            | b'='
            | b':'
            | b','
            | b'.'
            | b'/'
            | b'-'
    )
}

#[cfg(test)]
mod tests {
    use super::{
        CLIENT_NAME_FORMAT, HookCommandProgram, PANE_ID_FORMAT, SESSION_ID_FORMAT,
        TMUX_SOCKET_PATH_FORMAT, WINDOW_ID_FORMAT, WINDOW_INDEX_FORMAT, init_plugin_snippet,
        installed_hooks, managed_hooks,
    };

    #[test]
    fn installed_hooks_cover_expected_names_and_reserved_indices() {
        let hooks = installed_hooks(&HookCommandProgram::default());
        let names: Vec<_> = hooks.iter().map(|hook| hook.name).collect();

        assert_eq!(
            names,
            vec![
                "session-created",
                "session-closed",
                "session-renamed",
                "session-window-changed",
                "window-linked",
                "window-unlinked",
                "window-renamed",
                "window-pane-changed",
                "window-layout-changed",
                "alert-bell",
                "client-attached",
                "client-detached",
                "client-session-changed",
                "after-new-session",
                "after-new-window",
                "after-rename-session",
                "after-rename-window",
                "after-kill-pane",
                "after-select-window",
            ]
        );

        let indices: Vec<_> = hooks.iter().map(|hook| hook.index).collect();
        assert_eq!(
            indices,
            vec![
                900, 901, 902, 903, 904, 905, 906, 907, 908, 910, 912, 913, 914, 915, 916, 917,
                918, 919, 920,
            ]
        );
        assert!(indices.last().copied().unwrap_or_default() <= 949);
    }

    #[test]
    fn managed_hooks_include_disabled_activity_slots_for_cleanup() {
        let hooks = managed_hooks(&HookCommandProgram::default());

        assert!(
            hooks
                .iter()
                .any(|hook| hook.qualified_name() == "alert-activity[909]")
        );
        assert!(
            hooks
                .iter()
                .any(|hook| hook.qualified_name() == "alert-silence[911]")
        );
        assert!(
            !installed_hooks(&HookCommandProgram::default())
                .iter()
                .any(|hook| matches!(hook.name, "alert-activity" | "alert-silence"))
        );
    }

    #[test]
    fn alert_hook_commands_use_raw_q_tmux_formats() {
        let hooks = installed_hooks(&HookCommandProgram::default());
        let alert_hook = hooks
            .iter()
            .find(|hook| hook.name == "alert-bell")
            .expect("missing alert-bell hook");

        assert!(alert_hook.command.contains(TMUX_SOCKET_PATH_FORMAT));
        assert!(alert_hook.command.contains(SESSION_ID_FORMAT));
        assert!(alert_hook.command.contains(WINDOW_ID_FORMAT));
        assert!(alert_hook.command.contains(WINDOW_INDEX_FORMAT));
        assert!(alert_hook.command.contains(PANE_ID_FORMAT));
        assert!(
            !alert_hook
                .command
                .contains(&format!("'{TMUX_SOCKET_PATH_FORMAT}'"))
        );
        assert!(
            !alert_hook
                .command
                .contains(&format!("'{SESSION_ID_FORMAT}'"))
        );
        assert!(
            !alert_hook
                .command
                .contains(&format!("'{WINDOW_ID_FORMAT}'"))
        );
        assert!(
            !alert_hook
                .command
                .contains(&format!("'{WINDOW_INDEX_FORMAT}'"))
        );
        assert!(!alert_hook.command.contains(&format!("'{PANE_ID_FORMAT}'")));
        assert!(!alert_hook.command.contains("#{socket_path}"));
        assert!(!alert_hook.command.contains("#{session_id}"));
        assert!(!alert_hook.command.contains("#{window_id}"));
        assert!(!alert_hook.command.contains("#{window_index}"));
        assert!(!alert_hook.command.contains("#{pane_id}"));
    }

    #[test]
    fn client_hook_commands_use_q_client_name() {
        let hooks = installed_hooks(&HookCommandProgram::default());
        let client_hook = hooks
            .iter()
            .find(|hook| hook.name == "client-attached")
            .expect("missing client-attached hook");

        assert!(client_hook.command.contains(CLIENT_NAME_FORMAT));
        assert!(!client_hook.command.contains("#{client_name}"));
    }

    #[test]
    fn init_plugin_snippet_prints_run_shell_install_command() {
        let snippet = init_plugin_snippet(&HookCommandProgram::default());

        assert_eq!(
            snippet,
            "# tmux-sidecar hook/server rewrite\n# Installs or refreshes tmux-sidecar hooks for the current tmux server.\nrun-shell -b 'tmux-sidecar install-hooks'\n"
        );
    }

    #[test]
    fn hook_program_shell_quotes_custom_binary_path() {
        let program =
            HookCommandProgram::new(vec![String::from("/opt/tmux sidecar/bin/tmux-sidecar")]);
        let hooks = installed_hooks(&program);
        let hook = hooks
            .iter()
            .find(|hook| hook.name == "session-created")
            .expect("missing session-created hook");

        assert!(
            hook.command
                .contains("'/opt/tmux sidecar/bin/tmux-sidecar' hook")
        );
    }
}
