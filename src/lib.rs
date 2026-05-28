pub mod app;
pub mod cli;
pub mod client;
pub mod domain;
pub mod event;
pub mod input;
pub mod ipc;
pub mod model;
pub mod projection;
pub mod query;
pub mod server;
pub mod tmux;
pub mod ui;
mod ui_app;

use std::{env, path::PathBuf};

use anyhow::{Context, Result, bail};

pub fn run(cli: cli::Cli) -> Result<()> {
    match cli.command.clone() {
        Some(cli::CliCommand::Setup(command)) => setup(&command),
        Some(cli::CliCommand::Teardown(command)) => teardown(&command),
        Some(cli::CliCommand::InitPlugin) => {
            print!(
                "{}",
                tmux::TmuxCli::init_plugin_snippet(&tmux::hooks::HookCommandProgram::default())
            );
            Ok(())
        }
        Some(cli::CliCommand::Daemon(command)) => run_daemon(&command),
        Some(cli::CliCommand::Hook(command)) => client::run_hook(command),
        Some(cli::CliCommand::Query(command)) => run_query(&cli, &command),
        None => run_app(cli),
    }
}

fn run_app(cli: cli::Cli) -> Result<()> {
    let mut app = app::App::new(cli);
    app.run()
}

fn setup(command: &cli::SetupArgs) -> Result<()> {
    tmux_cli(command.socket_name.clone(), command.socket_path.clone())
        .install_hooks(&hook_command_program()?)?;
    Ok(())
}

fn teardown(command: &cli::TeardownArgs) -> Result<()> {
    tmux_cli(command.socket_name.clone(), command.socket_path.clone()).uninstall_hooks()?;
    Ok(())
}

fn run_query(cli: &cli::Cli, command: &cli::QueryArgs) -> Result<()> {
    let tmux_socket_path = client::resolve_tmux_socket_path(
        command
            .socket_name
            .clone()
            .or_else(|| cli.socket_name.clone()),
        command
            .socket_path
            .clone()
            .or_else(|| cli.socket_path.clone()),
    )?;
    let state = client::request_snapshot(&tmux_socket_path)?;
    query::write_result(command, &state, &mut std::io::stdout())
}

fn run_daemon(command: &cli::DaemonArgs) -> Result<()> {
    if command.stop {
        let tmux_socket_path = client::resolve_tmux_socket_path(
            command.socket_name.clone(),
            command.socket_path.clone(),
        )?;
        return client::shutdown_existing_server(&tmux_socket_path);
    }

    let Some(socket_path) = command.socket_path.clone() else {
        bail!("daemon requires --socket-path unless --stop is set");
    };
    if command.socket_name.is_some() {
        bail!("daemon can only be started with --socket-path");
    }

    server::run(server::ServerOptions {
        tmux_socket_path: socket_path,
    })
}

fn tmux_cli(socket_name: Option<String>, socket_path: Option<PathBuf>) -> tmux::TmuxCli {
    tmux::TmuxCli {
        socket_name,
        socket_path,
    }
}

fn hook_command_program() -> Result<tmux::hooks::HookCommandProgram> {
    Ok(tmux::hooks::HookCommandProgram::new(vec![
        hook_program_path()?.display().to_string(),
    ]))
}

fn hook_program_path() -> Result<PathBuf> {
    Ok(env::var_os("CARGO_BIN_EXE_tmux-sidecar")
        .map(PathBuf::from)
        .unwrap_or(env::current_exe().context("failed to resolve current executable")?))
}
