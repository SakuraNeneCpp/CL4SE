mod autostart;
mod hooks;
mod ime;
mod injector;
mod taskbar;

use std::{
    any::Any,
    env,
    ffi::c_void,
    os::windows::process::CommandExt,
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use windows::{
    core::BOOL,
    Win32::{
        Foundation::{CloseHandle, HANDLE, LPARAM, WPARAM},
        System::{
            Console::{
                GetConsoleWindow, SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT,
                CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
            },
            Threading::{CreateEventW, GetCurrentThreadId, SetEvent, WaitForSingleObject},
        },
        UI::WindowsAndMessaging::{
            DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW, TranslateMessage, MSG,
            PM_NOREMOVE, WM_QUIT,
        },
    },
};

use crate::{
    config::Config,
    control::{ControlAction, ControlRequests, ControlWatcher},
    core::{Decision, Engine, ImeGuess, Platform},
    platform::{Autostart, BackgroundProcess, ImeStateProvider, KeyInjector, RunOutcome},
};

use self::{
    autostart::WindowsAutostart,
    hooks::{CaptureGuard, EventQueue, WindowsHooks},
    ime::{ComApartment, WindowsImeStateProvider},
    injector::WindowsKeyInjector,
};

const EVENT_QUEUE_CAPACITY: usize = 1024;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(1);
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static CLEANUP_EVENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static CONSOLE_STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
const CONSOLE_CLEANUP_WAIT_MS: u32 = 4_000;

pub(super) fn run(config: &Config, control: &ControlRequests) -> Result<RunOutcome> {
    let mut message = MSG::default();
    // SAFETY: message points to valid storage. PM_NOREMOVE creates this thread's
    // message queue without removing or dispatching any message.
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
    // SAFETY: GetCurrentThreadId has no preconditions.
    let hook_thread_id = unsafe { GetCurrentThreadId() };

    let event_queue = hooks::shared_event_queue(EVENT_QUEUE_CAPACITY);
    CONSOLE_STOP_REQUESTED.store(false, Ordering::Release);
    let console = if console_available() {
        Some(ConsoleCtrlGuard::install(hook_thread_id)?)
    } else {
        log::debug!("no Windows console attached; stop is available through `cl4se stop`");
        None
    };
    let worker = WorkerGuard::start(
        config.clone(),
        event_queue,
        hook_thread_id,
        control.watcher(),
    )?;
    let hooks = WindowsHooks::install()?;
    let capture = CaptureGuard::enable();

    log::info!("Windows hooks installed; CL4SE is running");
    let loop_result = message_loop();
    let console_stop_requested = CONSOLE_STOP_REQUESTED.load(Ordering::Acquire);

    // Disable callbacks first, then unhook, then stop and join the worker. The
    // same reverse order is guaranteed automatically if unwinding occurs.
    drop(capture);
    drop(hooks);
    let worker_result = worker.finish();
    if let Some(console) = console.as_ref() {
        console.signal_cleanup();
    }
    drop(console);
    log::info!("Windows hooks removed; CL4SE stopped");

    loop_result.and(worker_result)?;
    let stop_pending = control.stop_pending()?;
    if !console_stop_requested && !stop_pending && control.restart_pending()? {
        Ok(RunOutcome::RestartRequested)
    } else {
        Ok(RunOutcome::Stopped)
    }
}

pub(super) fn start_background() -> Result<BackgroundProcess> {
    let executable = env::current_exe().context("failed to locate cl4se executable")?;
    let mut command = Command::new(executable);
    command.arg("run");
    let log_path = super::configure_background_command(&mut command)?;
    let child = command
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .spawn()
        .context("failed to start CL4SE in the background")?;
    Ok(BackgroundProcess {
        pid: child.id(),
        log_path,
    })
}

pub(super) fn install_autostart() -> Result<()> {
    WindowsAutostart.install()
}

pub(super) fn uninstall_autostart() -> Result<()> {
    WindowsAutostart.uninstall()
}

pub(super) fn doctor() -> Result<()> {
    println!("CL4SE doctor (Windows)");
    let mut has_error = false;
    let mut message = MSG::default();
    // SAFETY: See run; this only creates the doctor thread's message queue.
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };

    match WindowsHooks::install() {
        Ok(hooks) => {
            println!("Windows hooks: OK (keyboard, mouse, foreground)");
            drop(hooks);
        }
        Err(error) => {
            println!("Windows hooks: ERROR: {error:#}");
            println!(
                "Fix: close other keyboard-hook tools, allow cl4se.exe in security software, and retry; administrator privileges are not normally required."
            );
            has_error = true;
        }
    }

    match ComApartment::initialize() {
        Ok(_com) => {
            let ime_window = WindowsImeStateProvider::has_foreground_ime_window();
            if ime_window {
                println!("Foreground IME window: OK");
            } else {
                println!("Foreground IME window: WARN unavailable in this terminal");
                println!(
                    "Check: run `cl4se run` and perform README T1 in an IME-aware editor; unsupported applications safely remain inactive."
                );
            }

            let mut provider = WindowsImeStateProvider::new();
            println!("Taskbar IME mode: {:?}", provider.taskbar_mode_for_doctor());
            let snapshot = provider.snapshot();
            let profile = snapshot.ime_id.as_deref().unwrap_or("none");
            println!(
                "IME snapshot: active={:?}, recognized_profile={profile}",
                snapshot.active
            );
            if snapshot.active == ImeGuess::Unknown {
                println!(
                    "IME query: WARN state is Unknown in this terminal; CL4SE safely will not inject for Unknown."
                );
            }
            if snapshot.ime_id.is_none() {
                println!(
                    "Commit key: INFO unrecognized profile; commit_key=auto safely falls back to Enter."
                );
            }
        }
        Err(error) => {
            has_error = true;
            println!("COM/TSF: ERROR: {error:#}");
            println!("Fix: sign out of Windows, sign back in, and rerun `cl4se doctor`.");
        }
    }

    if has_error {
        println!("Result: ERROR. Apply the fixes above, then rerun `cl4se doctor`.");
        bail!("Windows doctor found setup problems")
    } else {
        println!("Result: OK. Next: run `cl4se install-autostart`.");
        Ok(())
    }
}

fn message_loop() -> Result<()> {
    let mut message = MSG::default();
    loop {
        // SAFETY: message is valid writable storage and this thread owns the
        // hook message loop. No HWND filter is used.
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        match result.0 {
            -1 => return Err(windows::core::Error::from_thread().into()),
            0 => return Ok(()),
            _ => {
                // SAFETY: message was initialized by a successful GetMessageW.
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }
}

struct WorkerGuard {
    shutdown: Arc<AtomicBool>,
    error: Arc<Mutex<Option<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl WorkerGuard {
    fn start(
        config: Config,
        event_queue: Arc<EventQueue>,
        hook_thread_id: u32,
        control: ControlWatcher,
    ) -> Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_error = Arc::clone(&error);
        let handle = thread::Builder::new()
            .name("cl4se-windows-worker".to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker_loop(
                        config,
                        event_queue,
                        &worker_shutdown,
                        hook_thread_id,
                        control,
                    )
                }));
                let failure = match outcome {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(format!("{error:#}")),
                    Err(payload) => Some(format!(
                        "Windows worker panicked: {}",
                        panic_message(payload.as_ref())
                    )),
                };
                if let Some(failure) = failure {
                    if let Ok(mut slot) = worker_error.lock() {
                        *slot = Some(failure);
                    }
                    // SAFETY: hook_thread_id identifies the run thread whose
                    // message queue was created before this worker started.
                    let _ = unsafe {
                        PostThreadMessageW(hook_thread_id, WM_QUIT, WPARAM(0), LPARAM(0))
                    };
                }
            })
            .context("failed to start Windows event worker")?;

        Ok(Self {
            shutdown,
            error,
            handle: Some(handle),
        })
    }

    fn finish(mut self) -> Result<()> {
        self.stop();
        let error = self
            .error
            .lock()
            .map_err(|_| anyhow!("Windows worker error lock was poisoned"))?
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
                    *slot = Some("Windows worker escaped its panic boundary".to_owned());
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
    event_queue: Arc<EventQueue>,
    shutdown: &AtomicBool,
    hook_thread_id: u32,
    mut control: ControlWatcher,
) -> Result<()> {
    let _com = ComApartment::initialize()?;
    let mut provider = WindowsImeStateProvider::new();
    let mut injector = WindowsKeyInjector;
    let mut engine = Engine::new(&config, Platform::Windows);
    let clock = std::time::Instant::now();
    let mut generation = hooks::current_generation();

    while !shutdown.load(Ordering::Acquire) {
        match control.poll() {
            Some(ControlAction::Stop) => {
                log::info!("background stop request received");
                // SAFETY: hook_thread_id identifies the run thread whose
                // message queue was created before this worker started.
                unsafe { PostThreadMessageW(hook_thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }
                    .context("failed to stop the Windows message loop")?;
                return Ok(());
            }
            Some(ControlAction::Restart) => {
                log::info!("configuration restart request received");
                // SAFETY: hook_thread_id identifies the run thread whose
                // message queue was created before this worker started.
                unsafe { PostThreadMessageW(hook_thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }
                    .context("failed to stop the Windows message loop for restart")?;
                return Ok(());
            }
            None => {}
        }

        let Some(queued) = event_queue.pop() else {
            thread::sleep(WORKER_POLL_INTERVAL);
            continue;
        };

        if queued.generation != generation {
            generation = queued.generation;
            engine.reset_composition();
            log::debug!("hook event loss detected; composition state reset");
        }

        match engine.handle_event(queued.event, &mut provider, clock.elapsed()) {
            Decision::InjectCommitKey(key) => {
                log::debug!("injecting commit key: {key:?}");
                injector.inject_commit_key(key)?;
            }
            Decision::InjectShiftEnter => {
                log::debug!("injecting opt-in Shift+Enter idle action");
                injector.inject_shift_enter()?;
            }
            Decision::PassThroughCapsLock => {
                log::debug!("injecting marked CapsLock pass-through");
                injector.inject_capslock()?;
            }
            Decision::Suppress | Decision::Ignore => {}
        }
    }

    Ok(())
}

struct ConsoleCtrlGuard {
    cleanup_event: HANDLE,
}

impl ConsoleCtrlGuard {
    fn install(thread_id: u32) -> Result<Self> {
        // SAFETY: No security attributes or name are supplied. The returned
        // manual-reset event is owned by this guard.
        let cleanup_event = unsafe { CreateEventW(None, true, false, None) }
            .context("failed to create console cleanup event")?;
        CONSOLE_STOP_REQUESTED.store(false, Ordering::Release);
        CLEANUP_EVENT.store(cleanup_event.0, Ordering::Release);
        HOOK_THREAD_ID.store(thread_id, Ordering::Release);
        // SAFETY: console_ctrl_handler has the required system ABI and static
        // lifetime. Its only wait is the bounded shutdown-completion wait.
        if let Err(error) = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), true) } {
            HOOK_THREAD_ID.store(0, Ordering::Release);
            CLEANUP_EVENT.store(std::ptr::null_mut(), Ordering::Release);
            // SAFETY: cleanup_event was created successfully above and has not
            // been published to a running handler after installation failed.
            let _ = unsafe { CloseHandle(cleanup_event) };
            return Err(error).context("failed to install console control handler");
        }
        Ok(Self { cleanup_event })
    }

    fn signal_cleanup(&self) {
        // SAFETY: cleanup_event is owned by this guard and remains open.
        let _ = unsafe { SetEvent(self.cleanup_event) };
    }
}

impl Drop for ConsoleCtrlGuard {
    fn drop(&mut self) {
        self.signal_cleanup();
        HOOK_THREAD_ID.store(0, Ordering::Release);
        CLEANUP_EVENT.store(std::ptr::null_mut(), Ordering::Release);
        // SAFETY: This removes the exact handler installed by this guard.
        let _ = unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), false) };
        // SAFETY: cleanup_event is owned by this guard and is closed exactly
        // once, after the handler is removed and completion is signaled.
        let _ = unsafe { CloseHandle(self.cleanup_event) };
    }
}

// SAFETY: Windows invokes this function with the PHANDLER_ROUTINE ABI. It only
// performs atomics, posts the shutdown message, and waits at most four seconds
// for RAII cleanup to complete on close-class console signals.
unsafe extern "system" fn console_ctrl_handler(control: u32) -> BOOL {
    if !matches!(
        control,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    ) {
        return BOOL(0);
    }
    CONSOLE_STOP_REQUESTED.store(true, Ordering::Release);
    let thread_id = HOOK_THREAD_ID.load(Ordering::Acquire);
    if thread_id == 0 {
        return BOOL(0);
    }
    // SAFETY: thread_id is published only after the run thread creates its
    // message queue. PostThreadMessageW is nonblocking in this handler.
    if unsafe { PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }.is_err() {
        return BOOL(0);
    }

    let cleanup_event = CLEANUP_EVENT.load(Ordering::Acquire);
    if !cleanup_event.is_null() {
        // SAFETY: The guard keeps the manual-reset event open until it signals
        // completion and removes this handler. The bounded wait lets Ctrl+C and
        // console-close paths observe hook and worker cleanup before returning.
        let _ = unsafe { WaitForSingleObject(HANDLE(cleanup_event), CONSOLE_CLEANUP_WAIT_MS) };
    }
    BOOL(1)
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

fn console_available() -> bool {
    // SAFETY: GetConsoleWindow has no preconditions and returns a borrowed HWND
    // or null when this background process has no attached console.
    !unsafe { GetConsoleWindow() }.0.is_null()
}
