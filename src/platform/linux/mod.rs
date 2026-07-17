mod autostart;
mod diagnostics;
mod ime;
mod injector;
mod interceptor;
mod signal;
mod xkb;

use std::{env, os::unix::process::CommandExt, process::Command, thread, time::Duration};

use anyhow::{bail, Context, Result};

use crate::{
    config::Config,
    control::{ControlAction, ControlRequests},
    core::{Decision, Engine, Platform},
    platform::{Autostart, BackgroundProcess, KeyInjector, RunOutcome},
};

use self::{
    autostart::LinuxAutostart, ime::LinuxImeStateProvider, injector::LinuxKeyInjector,
    interceptor::LinuxInputMonitor, signal::SignalGuard,
};

const POLL_INTERVAL: Duration = Duration::from_millis(5);

pub(super) fn run(config: &Config, control: &ControlRequests) -> Result<RunOutcome> {
    if let Err(error) = xkb::reapply_if_managed() {
        log::warn!(
            "failed to reapply managed caps:none setting: {error:#}; physical Caps Lock may toggle"
        );
    }

    let _signals = SignalGuard::install()?;
    let mut injector = LinuxKeyInjector::new()?;
    let mut monitor = LinuxInputMonitor::new()?;
    let mut provider = LinuxImeStateProvider::new();
    let mut engine = Engine::new(config, Platform::Linux);
    let clock = std::time::Instant::now();
    let mut control = control.watcher();
    let mut outcome = RunOutcome::Stopped;

    log::info!("Linux evdev monitor and uinput injector initialized; CL4SE is running");
    while !SignalGuard::stop_requested() {
        match control.poll() {
            Some(ControlAction::Stop) => {
                log::info!("background stop request received");
                break;
            }
            Some(ControlAction::Restart) => {
                log::info!("configuration restart request received");
                outcome = RunOutcome::RestartRequested;
                break;
            }
            None => {}
        }

        let batch = monitor.poll();
        if batch.state_uncertain {
            engine.reset_composition();
            log::debug!("evdev device loss detected; composition state reset");
        }

        for event in batch.events {
            match engine.handle_event(event, &mut provider, clock.elapsed()) {
                Decision::InjectCommitKey(key) => {
                    log::debug!("injecting commit key: {key:?}");
                    let result = injector.inject_commit_key(key);
                    // The CL4SE uinput device is deliberately excluded from
                    // observation, so reset explicitly after the marked
                    // sequence instead of waiting to observe our own event.
                    engine.reset_composition();
                    result?;
                }
                Decision::InjectShiftEnter => {
                    log::debug!("injecting opt-in Shift+Enter idle action");
                    let result = injector.inject_shift_enter();
                    // The CL4SE uinput device is excluded from observation, so
                    // keep the core state explicit after our marked sequence.
                    engine.reset_composition();
                    result?;
                }
                Decision::PassThroughCapsLock => {
                    log::debug!("injecting CapsLock pass-through through uinput");
                    injector.inject_capslock()?;
                }
                Decision::Suppress | Decision::Ignore => {}
            }
        }
        thread::sleep(POLL_INTERVAL);
    }

    // VirtualDevice is an RAII handle. Returning through this path, early
    // errors, and panic unwinding all destroy it before platform::run catches
    // a panic at the application boundary.
    log::info!("event loop stopped; dropping uinput device and evdev handles");
    Ok(outcome)
}

pub(super) fn start_background() -> Result<BackgroundProcess> {
    let executable = env::current_exe().context("failed to locate cl4se executable")?;
    let mut command = Command::new(executable);
    command.arg("run").process_group(0);
    let log_path = super::configure_background_command(&mut command)?;
    let child = command
        .spawn()
        .context("failed to start CL4SE in the background")?;
    Ok(BackgroundProcess {
        pid: child.id(),
        log_path,
    })
}

pub(super) fn install_autostart() -> Result<()> {
    LinuxAutostart.install()
}

pub(super) fn uninstall_autostart() -> Result<()> {
    LinuxAutostart.uninstall()
}

pub(super) fn doctor() -> Result<()> {
    println!("CL4SE doctor (Linux)");
    let input = diagnostics::inspect_input_access();
    let uinput = diagnostics::inspect_uinput_access();
    let ime = LinuxImeStateProvider::new().probe();
    let xkb = xkb::inspect();
    let report = diagnostics::build_report(input, uinput, &ime, &xkb);
    for line in report.lines {
        println!("{line}");
    }
    if report.has_error {
        bail!("Linux doctor found setup problems")
    } else {
        Ok(())
    }
}
