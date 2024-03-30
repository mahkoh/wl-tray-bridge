#![expect(
    clippy::collapsible_else_if,
    clippy::field_reassign_with_default,
    clippy::single_match
)]

use log::LevelFilter;

mod cli;
mod settings;
mod sni;
mod wayland;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::builder()
        .default_format()
        .filter_level(LevelFilter::Info)
        .parse_default_env()
        .init();
    cli::run().await;
}
