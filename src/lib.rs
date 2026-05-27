pub mod app;
pub mod cli;
pub mod client;
pub mod event;
pub mod input;
pub mod ipc;
pub mod model;
pub mod server;
pub mod state_cache;
pub mod tmux;
pub mod ui;

use std::{env, path::PathBuf};

use anyhow::{Context, Result, bail};

pub fn run(cli: cli::Cli) -> Result<()> {
    match cli.command.clone() {
        Some(cli::CliCommand::InstallHooks(command)) => install_hooks(&command),
        Some(cli::CliCommand::UninstallHooks(command)) => uninstall_hooks(&command),
        Some(cli::CliCommand::InitPlugin) => {
            print!(
                "{}",
                tmux::TmuxCli::init_plugin_snippet(&tmux::hooks::HookCommandProgram::default())
            );
            Ok(())
        }
        Some(cli::CliCommand::Server(command)) => run_server(&command),
        Some(cli::CliCommand::Hook(command)) => client::run_hook(command),
        None => run_app(cli),
    }
}

fn run_app(cli: cli::Cli) -> Result<()> {
    let mut app = app::App::new(cli);
    app.run()
}

fn install_hooks(command: &cli::InstallHooksArgs) -> Result<()> {
    tmux_cli(command.socket_name.clone(), command.socket_path.clone())
        .install_hooks(&hook_command_program()?)?;
    Ok(())
}

fn uninstall_hooks(command: &cli::UninstallHooksArgs) -> Result<()> {
    tmux_cli(command.socket_name.clone(), command.socket_path.clone()).uninstall_hooks()?;
    Ok(())
}

fn run_server(command: &cli::ServerArgs) -> Result<()> {
    if command.kill {
        let tmux_socket_path = client::resolve_tmux_socket_path(
            command.socket_name.clone(),
            command.socket_path.clone(),
        )?;
        return client::shutdown_existing_server(&tmux_socket_path);
    }

    let Some(socket_path) = command.socket_path.clone() else {
        bail!("server requires --socket-path unless --kill is set");
    };
    if command.socket_name.is_some() {
        bail!("server can only be started with --socket-path");
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
