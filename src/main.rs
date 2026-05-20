mod app;
mod cli;
mod event;
mod input;
mod model;
mod tmux;
mod ui;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    let mut app = app::App::new(cli);

    if let Err(error) = app.run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
