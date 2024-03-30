use {
    crate::{settings, wayland},
    clap::Parser,
    error_reporter::Report,
};

/// wl-tray-bridge.
///
/// Creates a bridge between applications implementing the StatusNotifierItem protocol
/// and compositors implementing the ext-tray-v1 protocol.
#[derive(Parser, Debug)]
struct Cli {
    /// Path to the config file.
    ///
    /// Defaults to `~/.config/wl-tray-bridge/config.toml`.
    #[clap(long)]
    config: Option<String>,
}

pub async fn run() {
    let cli = Cli::parse();

    settings::init(cli.config.as_deref());

    let Err(e) = wayland::run().await;
    log::error!("A fatal error occurred: {}", Report::new(e));
    std::process::exit(1);
}
