use clap::Parser;

fn main() {
    let cli = tmux_sidecar::cli::Cli::parse();

    if let Err(error) = tmux_sidecar::run(cli) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
