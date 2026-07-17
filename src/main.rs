use std::time::Duration;

use anyhow::{bail, Context, Result};
use cl4se::{
    config::{Config, IdleAction},
    control::ControlRequests,
    platform::{self, RunOutcome},
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
    /// Start CL4SE in the background, or resume it after a stop.
    Start,
    /// Stop a background CL4SE instance after normal cleanup.
    Stop,
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
        Command::Start => start(),
        Command::Stop => stop(),
        Command::InstallAutostart => platform::install_autostart(),
        Command::UninstallAutostart => platform::uninstall_autostart(),
        Command::Doctor => platform::doctor(),
        Command::Setting { command } => setting(command),
    }
}

fn run() -> Result<()> {
    let control = ControlRequests::new()?;
    // Requests left while CL4SE was stopped must not affect the next start.
    control.clear_startup_requests()?;
    let config = Config::load()?;
    init_logging(&config)?;
    let run_result = run_backend_loop(
        config,
        |config| platform::run(config, &control),
        || {
            let _ = control.complete_restart()?;
            Config::load().context("failed to reload configuration for restart")
        },
    );
    let stop_completion = control.complete_stop().map(|_| ());
    run_result.and(stop_completion)
}

fn start() -> Result<()> {
    let control = ControlRequests::new()?;
    if control.probe_running(Duration::from_millis(750))? {
        println!("CL4SE is already running in the background.");
        return Ok(());
    }

    let process = platform::start_background()?;
    if !control.probe_running(Duration::from_secs(5))? {
        bail!(
            "CL4SE background startup was not acknowledged; inspect {}",
            process.log_path.display()
        );
    }

    println!("CL4SE started in the background (PID {}).", process.pid);
    println!("Log: {}", process.log_path.display());
    Ok(())
}

fn stop() -> Result<()> {
    let control = ControlRequests::new()?;
    if control.stop_running(Duration::from_millis(750), Duration::from_secs(15))? {
        println!("CL4SE stopped after cleanup completed.");
    } else {
        println!("CL4SE is not running.");
    }
    Ok(())
}

fn run_backend_loop<B, R>(mut config: Config, mut backend: B, mut reload: R) -> Result<()>
where
    B: FnMut(&Config) -> Result<RunOutcome>,
    R: FnMut() -> Result<Config>,
{
    loop {
        log::info!("configuration loaded");
        match backend(&config)? {
            RunOutcome::Stopped => return Ok(()),
            RunOutcome::RestartRequested => {
                log::info!(
                    "restart requested; platform cleanup completed; reinitializing in the current process"
                );
                config = reload()?;
            }
        }
    }
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
            ControlRequests::new()?.request_restart()?;
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

    #[test]
    fn background_start_and_stop_commands_parse() {
        assert!(matches!(
            Cli::try_parse_from(["cl4se", "start"])
                .expect("start should parse")
                .command,
            Command::Start
        ));
        assert!(matches!(
            Cli::try_parse_from(["cl4se", "stop"])
                .expect("stop should parse")
                .command,
            Command::Stop
        ));
    }

    #[test]
    fn restart_reloads_config_without_leaving_the_foreground_process() {
        let initial = Config::default();
        let mut updated = Config::default();
        updated.general.idle_action = IdleAction::ShiftEnter;
        let mut observed = Vec::new();
        let mut runs = 0;

        run_backend_loop(
            initial,
            |config| {
                observed.push(config.general.idle_action);
                runs += 1;
                Ok(if runs == 1 {
                    RunOutcome::RestartRequested
                } else {
                    RunOutcome::Stopped
                })
            },
            || Ok(updated.clone()),
        )
        .expect("in-process restart loop should finish");

        assert_eq!(observed, vec![IdleAction::None, IdleAction::ShiftEnter]);
    }
}
