use anyhow::{Context, Result};
use cl4se::{
    config::{Config, IdleAction},
    control::RestartRequest,
    platform,
};
use clap::{Parser, Subcommand, ValueEnum};

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
    /// Show or change persistent settings and restart a running instance.
    Setting {
        #[command(subcommand)]
        command: SettingCommand,
    },
}

#[derive(Debug, Subcommand)]
enum SettingCommand {
    /// Show the current persistent settings.
    Show,
    /// Set the action used when CL4SE knows composition is idle.
    IdleAction {
        #[arg(value_enum)]
        action: IdleActionArgument,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum IdleActionArgument {
    None,
    ShiftEnter,
    #[value(name = "capslock")]
    CapsLock,
}

impl From<IdleActionArgument> for IdleAction {
    fn from(value: IdleActionArgument) -> Self {
        match value {
            IdleActionArgument::None => Self::None,
            IdleActionArgument::ShiftEnter => Self::ShiftEnter,
            IdleActionArgument::CapsLock => Self::CapsLock,
        }
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run => run(),
        Command::InstallAutostart => platform::install_autostart(),
        Command::UninstallAutostart => platform::uninstall_autostart(),
        Command::Doctor => platform::doctor(),
        Command::Setting { command } => setting(command),
    }
}

fn run() -> Result<()> {
    let restart = RestartRequest::new()?;
    // A request left while CL4SE was stopped is already reflected in the
    // persisted config and must not cause an immediate extra restart.
    let _ = restart.take()?;
    let config = Config::load()?;
    init_logging(&config)?;
    log::info!("configuration loaded");
    platform::run(&config, &restart)?;

    if restart.take()? {
        log::info!("restart requested; platform cleanup completed");
        platform::restart_process()?;
    }
    Ok(())
}

fn setting(command: SettingCommand) -> Result<()> {
    let mut config = Config::load()?;
    match command {
        SettingCommand::Show => {
            println!("Config: {}", Config::path()?.display());
            println!("idle_action = \"{}\"", config.general.idle_action.as_str());
        }
        SettingCommand::IdleAction { action } => {
            let action = IdleAction::from(action);
            if config.general.idle_action == action {
                println!(
                    "idle_action is already \"{}\"; no restart requested.",
                    action.as_str()
                );
                return Ok(());
            }

            config.general.idle_action = action;
            let path = config.save()?;
            RestartRequest::new()?.request()?;
            println!("Updated {}", path.display());
            println!("idle_action = \"{}\"", action.as_str());
            println!(
                "Restart requested. A running CL4SE instance will restart automatically; otherwise the setting applies at next start."
            );
        }
    }
    Ok(())
}

fn init_logging(config: &Config) -> Result<()> {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(config.general.log_level.as_str()),
    )
    .try_init()
    .context("failed to initialize logging")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_idle_action_uses_kebab_case_cli_values() {
        let parsed = Cli::try_parse_from(["cl4se", "setting", "idle-action", "shift-enter"])
            .expect("setting command should parse");
        assert!(matches!(
            parsed.command,
            Command::Setting {
                command: SettingCommand::IdleAction {
                    action: IdleActionArgument::ShiftEnter
                }
            }
        ));
    }

    #[test]
    fn setting_show_parses_without_mutation_arguments() {
        let parsed =
            Cli::try_parse_from(["cl4se", "setting", "show"]).expect("setting show should parse");
        assert!(matches!(
            parsed.command,
            Command::Setting {
                command: SettingCommand::Show
            }
        ));
    }
}
