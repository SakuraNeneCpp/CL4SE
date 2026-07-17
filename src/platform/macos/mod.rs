mod autostart;
mod diagnostics;
mod event_tap;
mod hidutil;
mod ime;
mod injector;
mod modifier_lock;
mod signal;

use std::{
    any::Any,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{anyhow, bail, Result};
use core_foundation::runloop::CFRunLoop;

use crate::{
    config::Config,
    core::{Decision, Engine, Platform},
    platform::{Autostart, KeyInjector},
};

use self::{
    autostart::MacOsAutostart,
    event_tap::{EventQueue, MacOsEventTap},
    hidutil::HidutilRemapGuard,
    ime::MacOsImeStateProvider,
    injector::MacOsKeyInjector,
    modifier_lock::ModifierLock,
    signal::SignalGuard,
};

const EVENT_QUEUE_CAPACITY: usize = 1024;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(1);

pub(super) fn run(config: &Config) -> Result<()> {
    let tcc = diagnostics::tcc_status();
    if !tcc.input_monitoring || !tcc.event_posting || !tcc.accessibility {
        log::warn!(
            "macOS TCC access is incomplete; run manually once and grant Input Monitoring and Accessibility"
        );
    }

    let queue = Arc::new(EventQueue::new(EVENT_QUEUE_CAPACITY));
    let mut runtime = RuntimeGuards::default();
    runtime.event_tap = Some(MacOsEventTap::install(Arc::clone(&queue))?);
    let run_loop = runtime
        .event_tap
        .as_ref()
        .ok_or_else(|| anyhow!("macOS event tap disappeared during startup"))?
        .run_loop();
    runtime.signal = Some(SignalGuard::install(run_loop.clone())?);
    runtime.worker = Some(WorkerGuard::start(config.clone(), queue, run_loop)?);
    runtime.remap = Some(HidutilRemapGuard::install()?);

    log::info!("Caps Lock mapped to F18; macOS event tap installed; CL4SE is running");
    if !SignalGuard::stop_requested() {
        if let Some(event_tap) = runtime.event_tap.as_ref() {
            event_tap.run();
        }
    }
    let result = runtime.finish();
    log::info!("macOS event tap removed and hidutil mapping restored; CL4SE stopped");
    result
}

pub(super) fn install_autostart() -> Result<()> {
    MacOsAutostart.install()
}

pub(super) fn uninstall_autostart() -> Result<()> {
    MacOsAutostart.uninstall()
}

pub(super) fn doctor() -> Result<()> {
    println!("CL4SE doctor (macOS)");
    let mut has_error = false;
    let tcc = diagnostics::tcc_status();
    println!(
        "Input Monitoring: {}",
        diagnostic_label(tcc.input_monitoring)
    );
    println!("Accessibility: {}", diagnostic_label(tcc.accessibility));
    println!("CGEvent posting: {}", diagnostic_label(tcc.event_posting));
    if !tcc.input_monitoring || !tcc.accessibility || !tcc.event_posting {
        has_error = true;
        println!("Fix TCC permissions:");
        println!("  1. Run `cl4se run` manually once, then stop it with Ctrl+C.");
        println!(
            "  2. In System Settings > Privacy & Security, enable CL4SE under Input Monitoring and Accessibility."
        );
        println!("  3. Rerun `cl4se doctor`; install autostart only after it reports OK.");
        println!(
            "If the cl4se binary path changes, remove the old TCC entries and grant both permissions again."
        );
    }

    match MacOsImeStateProvider::current_source_id() {
        Some(source_id) => println!(
            "Current input source: {source_id} (Japanese={})",
            source_id.contains("Japanese")
        ),
        None => {
            has_error = true;
            println!("Current input source: ERROR (TIS input source ID unavailable)");
            println!(
                "Fix: add and select a keyboard input source in System Settings > Keyboard > Text Input, then retry."
            );
        }
    }

    match hidutil::cl4se_mapping_is_active() {
        Ok(true) => match hidutil::restore_residual_mapping() {
            Ok(()) => println!("hidutil mapping: residual CL4SE mapping found and restored"),
            Err(error) => {
                has_error = true;
                println!("hidutil mapping: ERROR restoring residual mapping: {error:#}");
            }
        },
        Ok(false) => println!("hidutil mapping: OK (no residual CL4SE mapping)"),
        Err(error) => {
            has_error = true;
            println!("hidutil mapping: ERROR: {error:#}");
        }
    }
    println!(
        "If Caps Lock remains remapped after a crash, recover with: /usr/bin/hidutil property --set '{{\"UserKeyMapping\":[]}}'"
    );

    match ModifierLock::open().and_then(|modifier| modifier.current_caps_lock_state()) {
        Ok(_) => println!("Shift+CapsLock pass-through: available (IOHID modifier lock API)"),
        Err(error) => println!(
            "Shift+CapsLock pass-through: WARN unsupported; Caps Lock will remain suppressed: {error:#}"
        ),
    }
    println!(
        "Runtime note: Terminal Secure Keyboard Entry disables event taps; turn it off while using CL4SE."
    );

    if has_error {
        println!("Result: ERROR. Apply the fixes above, then rerun `cl4se doctor`.");
        bail!("macOS doctor found setup problems")
    } else {
        println!("Result: OK. Next: run `cl4se install-autostart`.");
        Ok(())
    }
}

const fn diagnostic_label(ok: bool) -> &'static str {
    if ok {
        "OK"
    } else {
        "MISSING"
    }
}

#[derive(Default)]
struct RuntimeGuards {
    event_tap: Option<MacOsEventTap>,
    worker: Option<WorkerGuard>,
    remap: Option<HidutilRemapGuard>,
    signal: Option<SignalGuard>,
}

impl RuntimeGuards {
    fn finish(mut self) -> Result<()> {
        // Cleanup order is safety-sensitive: first stop interception, then
        // join the engine worker, restore hidutil, and finally remove signal
        // handling after restoration is complete.
        drop(self.event_tap.take());
        let worker_result = self.worker.take().map_or(Ok(()), WorkerGuard::finish);
        let remap_result = self.remap.take().map_or(Ok(()), HidutilRemapGuard::restore);
        drop(self.signal.take());
        worker_result.and(remap_result)
    }
}

impl Drop for RuntimeGuards {
    fn drop(&mut self) {
        // This is the panic/early-error path and mirrors finish's ordering.
        drop(self.event_tap.take());
        drop(self.worker.take());
        drop(self.remap.take());
        drop(self.signal.take());
    }
}

struct WorkerGuard {
    shutdown: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl WorkerGuard {
    fn start(config: Config, queue: Arc<EventQueue>, run_loop: CFRunLoop) -> Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_error = Arc::clone(&error);
        let handle = thread::Builder::new()
            .name("cl4se-macos-worker".to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker_loop(config, queue, &worker_shutdown, startup_tx)
                }));
                let failure = match outcome {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(format!("{error:#}")),
                    Err(payload) => Some(format!(
                        "macOS worker panicked: {}",
                        panic_message(payload.as_ref())
                    )),
                };
                if let Some(failure) = failure {
                    if let Ok(mut slot) = worker_error.lock() {
                        *slot = Some(failure);
                    }
                    run_loop.stop();
                }
            })?;

        let mut guard = Self {
            shutdown,
            error,
            handle: Some(handle),
        };
        match startup_rx.recv() {
            Ok(Ok(())) => Ok(guard),
            Ok(Err(error)) => {
                guard.stop();
                bail!(error)
            }
            Err(_) => {
                guard.stop();
                let error = guard
                    .error
                    .lock()
                    .ok()
                    .and_then(|mut error| error.take())
                    .unwrap_or_else(|| {
                        "macOS worker ended before reporting startup status".to_owned()
                    });
                bail!(error)
            }
        }
    }

    fn finish(mut self) -> Result<()> {
        self.stop();
        let error = self
            .error
            .lock()
            .map_err(|_| anyhow!("macOS worker error lock was poisoned"))?
            .take();
        if let Some(error) = error {
            bail!(error);
        }
        Ok(())
    }

    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                if let Ok(mut slot) = self.error.lock() {
                    *slot = Some("macOS worker escaped its panic boundary".to_owned());
                }
            }
        }
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker_loop(
    config: Config,
    queue: Arc<EventQueue>,
    shutdown: &AtomicBool,
    startup: mpsc::SyncSender<std::result::Result<(), String>>,
) -> Result<()> {
    let mut provider = MacOsImeStateProvider;
    let mut injector = match MacOsKeyInjector::new() {
        Ok(injector) => injector,
        Err(error) => {
            let message = format!("failed to initialize macOS key injection: {error:#}");
            let _ = startup.send(Err(message));
            return Err(error);
        }
    };
    let mut engine = Engine::new(&config, Platform::MacOs);
    let clock = std::time::Instant::now();
    let mut generation = 0;
    startup
        .send(Ok(()))
        .map_err(|_| anyhow!("macOS worker startup receiver was dropped"))?;

    while !shutdown.load(Ordering::Acquire) {
        let Some(queued) = queue.pop() else {
            thread::sleep(WORKER_POLL_INTERVAL);
            continue;
        };

        if queued.generation != generation {
            generation = queued.generation;
            engine.reset_composition();
            log::debug!("event-tap disable or event loss detected; composition state reset");
        }

        match engine.handle_event(queued.event, &mut provider, clock.elapsed()) {
            Decision::InjectCommitKey(key) => {
                log::debug!("injecting commit key: {key:?}");
                injector.inject_commit_key(key)?;
            }
            Decision::PassThroughCapsLock => {
                log::debug!("attempting macOS Caps Lock modifier-state pass-through");
                injector.inject_capslock()?;
            }
            Decision::Suppress | Decision::Ignore => {}
        }
    }

    Ok(())
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "non-string panic payload"
    }
}
