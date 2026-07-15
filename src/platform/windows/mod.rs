mod autostart;
mod hooks;
mod ime;
mod injector;

use std::{
    any::Any,
    ffi::c_void,
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
                SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
                CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
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
    core::{Decision, Engine, Platform},
    platform::{Autostart, ImeStateProvider, KeyInjector},
};

use self::{
    autostart::WindowsAutostart,
    hooks::{CaptureGuard, EventQueue, WindowsHooks},
    ime::{ComApartment, WindowsImeStateProvider},
    injector::WindowsKeyInjector,
};

const EVENT_QUEUE_CAPACITY: usize = 1024;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(1);
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static CLEANUP_EVENT: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
const CONSOLE_CLEANUP_WAIT_MS: u32 = 4_000;

pub(super) fn run(config: &Config) -> Result<()> {
    let mut message = MSG::default();
    // SAFETY: message points to valid storage. PM_NOREMOVE creates this thread's
    // message queue without removing or dispatching any message.
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };
    // SAFETY: GetCurrentThreadId has no preconditions.
    let hook_thread_id = unsafe { GetCurrentThreadId() };

    let event_queue = Arc::new(EventQueue::new(EVENT_QUEUE_CAPACITY));
    hooks::set_event_queue(Arc::clone(&event_queue))?;
    let console = ConsoleCtrlGuard::install(hook_thread_id)?;
    let worker = WorkerGuard::start(config.clone(), event_queue, hook_thread_id)?;
    let hooks = WindowsHooks::install()?;
    let capture = CaptureGuard::enable();

    log::info!("Windows hooks installed; CLIME is running");
    let loop_result = message_loop();

    // Disable callbacks first, then unhook, then stop and join the worker. The
    // same reverse order is guaranteed automatically if unwinding occurs.
    drop(capture);
    drop(hooks);
    let worker_result = worker.finish();
    console.signal_cleanup();
    drop(console);
    log::info!("Windows hooks removed; CLIME stopped");

    loop_result.and(worker_result)
}

pub(super) fn install_autostart() -> Result<()> {
    WindowsAutostart.install()
}

pub(super) fn uninstall_autostart() -> Result<()> {
    WindowsAutostart.uninstall()
}

pub(super) fn doctor() -> Result<()> {
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
            return Err(error).context("Windows hook diagnostic failed");
        }
    }

    let _com = ComApartment::initialize()?;
    let ime_window = WindowsImeStateProvider::has_foreground_ime_window();
    if ime_window {
        println!("Foreground IME window: OK");
    } else {
        println!(
            "Foreground IME window: unavailable (focus an IME-aware text field and retry; CLIME fails safe to Unknown)"
        );
    }
    let mut provider = WindowsImeStateProvider::new();
    let snapshot = provider.snapshot();
    println!(
        "IME snapshot: active={:?}, recognized_profile={}",
        snapshot.active,
        snapshot.ime_id.as_deref().unwrap_or("none")
    );
    Ok(())
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
    fn start(config: Config, event_queue: Arc<EventQueue>, hook_thread_id: u32) -> Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_error = Arc::clone(&error);
        let handle = thread::Builder::new()
            .name("clime-windows-worker".to_owned())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    worker_loop(config, event_queue, &worker_shutdown)
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

fn worker_loop(config: Config, event_queue: Arc<EventQueue>, shutdown: &AtomicBool) -> Result<()> {
    let _com = ComApartment::initialize()?;
    let mut provider = WindowsImeStateProvider::new();
    let mut injector = WindowsKeyInjector;
    let mut engine = Engine::new(&config, Platform::Windows);
    let clock = std::time::Instant::now();
    let mut generation = hooks::current_generation();

    while !shutdown.load(Ordering::Acquire) {
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
