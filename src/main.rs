use anyhow::{Context, Result};
use cl4se::{config::Config, platform};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "cl4se", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run CL4SE in the foreground.
    Run,
    /// Register CL4SE to start at login.
    InstallAutostart,
    /// Remove CL4SE from login startup.
    UninstallAutostart,
    /// Diagnose permissions, dependencies, and IME detection.
    Doctor,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run => run(),
        Command::InstallAutostart => platform::install_autostart(),
        Command::UninstallAutostart => platform::uninstall_autostart(),
        Command::Doctor => platform::doctor(),
    }
}

fn run() -> Result<()> {
    let config = Config::load()?;
    init_logging(&config)?;
    log::info!("configuration loaded");
    platform::run(&config)
}

fn init_logging(config: &Config) -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(config.general.log_level.as_str()),
    )
    .try_init()
    .context("failed to initialize logging")
}
