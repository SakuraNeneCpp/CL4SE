use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    thread::{self, JoinHandle},
};

use anyhow::{anyhow, bail, Context, Result};
use core_foundation::runloop::CFRunLoop;

const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;
const F_GETFL: i32 = 3;
const F_SETFL: i32 = 4;
const O_NONBLOCK: i32 = 0x0004;
const SIGNAL_ERROR: *mut c_void = usize::MAX as *mut c_void;

static SIGNAL_WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static SIGNAL_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn pipe(file_descriptors: *mut i32) -> i32;
    fn read(file_descriptor: i32, buffer: *mut c_void, count: usize) -> isize;
    fn write(file_descriptor: i32, buffer: *const c_void, count: usize) -> isize;
    fn close(file_descriptor: i32) -> i32;
    fn fcntl(file_descriptor: i32, command: i32, ...) -> i32;
    fn signal(signal: i32, handler: *mut c_void) -> *mut c_void;
}

pub(crate) struct SignalGuard {
    read_fd: i32,
    write_fd: i32,
    previous_sigint: *mut c_void,
    previous_sigterm: *mut c_void,
    handle: Option<JoinHandle<()>>,
}

impl SignalGuard {
    pub(crate) fn install(run_loop: CFRunLoop) -> Result<Self> {
        let mut file_descriptors = [-1; 2];
        // SAFETY: file_descriptors points to writable storage for two ints.
        if unsafe { pipe(file_descriptors.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error()).context("failed to create signal pipe");
        }
        let [read_fd, write_fd] = file_descriptors;

        if let Err(error) = make_nonblocking(write_fd) {
            close_fd(read_fd);
            close_fd(write_fd);
            return Err(error);
        }

        SIGNAL_REQUESTED.store(false, Ordering::Release);
        if SIGNAL_WRITE_FD
            .compare_exchange(-1, write_fd, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            close_fd(read_fd);
            close_fd(write_fd);
            bail!("macOS signal cleanup handler is already installed");
        }

        let handler = signal_handler as *const () as *mut c_void;
        // SAFETY: signal_handler has C ABI and static lifetime. Its body only
        // stores atomics and performs a nonblocking async-signal-safe write.
        let previous_sigint = unsafe { signal(SIGINT, handler) };
        if previous_sigint == SIGNAL_ERROR {
            SIGNAL_WRITE_FD.store(-1, Ordering::Release);
            close_fd(read_fd);
            close_fd(write_fd);
            return Err(std::io::Error::last_os_error())
                .context("failed to install SIGINT handler");
        }

        // SAFETY: Same handler contract as the successful SIGINT install.
        let previous_sigterm = unsafe { signal(SIGTERM, handler) };
        if previous_sigterm == SIGNAL_ERROR {
            restore_signal(SIGINT, previous_sigint);
            SIGNAL_WRITE_FD.store(-1, Ordering::Release);
            close_fd(read_fd);
            close_fd(write_fd);
            return Err(std::io::Error::last_os_error())
                .context("failed to install SIGTERM handler");
        }

        let handle = match thread::Builder::new()
            .name("clime-macos-signal".to_owned())
            .spawn(move || signal_thread(read_fd, run_loop))
        {
            Ok(handle) => handle,
            Err(error) => {
                restore_signal(SIGTERM, previous_sigterm);
                restore_signal(SIGINT, previous_sigint);
                SIGNAL_WRITE_FD.store(-1, Ordering::Release);
                close_fd(read_fd);
                close_fd(write_fd);
                return Err(error).context("failed to start macOS signal cleanup thread");
            }
        };

        Ok(Self {
            read_fd,
            write_fd,
            previous_sigint,
            previous_sigterm,
            handle: Some(handle),
        })
    }

    pub(crate) fn stop_requested() -> bool {
        SIGNAL_REQUESTED.load(Ordering::Acquire)
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        restore_signal(SIGTERM, self.previous_sigterm);
        restore_signal(SIGINT, self.previous_sigint);
        SIGNAL_WRITE_FD.store(-1, Ordering::Release);

        let shutdown = [0_u8];
        // SAFETY: write_fd remains open until after the listener joins, and
        // shutdown points to one initialized byte.
        let _ = unsafe { write(self.write_fd, shutdown.as_ptr().cast(), shutdown.len()) };
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        close_fd(self.read_fd);
        close_fd(self.write_fd);
    }
}

fn make_nonblocking(file_descriptor: i32) -> Result<()> {
    // SAFETY: F_GETFL accepts no variadic argument and returns the descriptor's
    // current flags or -1.
    let flags = unsafe { fcntl(file_descriptor, F_GETFL) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error()).context("failed to read signal-pipe flags");
    }
    // SAFETY: F_SETFL accepts one int flag argument. O_NONBLOCK is valid for a
    // pipe descriptor and preserves all existing flags.
    if unsafe { fcntl(file_descriptor, F_SETFL, flags | O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error())
            .context("failed to make signal pipe nonblocking");
    }
    Ok(())
}

fn signal_thread(read_fd: i32, run_loop: CFRunLoop) {
    loop {
        let mut byte = 0_u8;
        // SAFETY: read_fd is kept open by SignalGuard and byte is writable for
        // exactly one byte.
        let received = unsafe { read(read_fd, (&mut byte as *mut u8).cast(), 1) };
        if received == 1 {
            if byte != 0 {
                run_loop.stop();
            }
            break;
        }
        if received == 0 {
            break;
        }
        // A signal can interrupt read before the handler's pipe write becomes
        // visible. Retrying handles EINTR without allocating or polling.
    }
}

// SAFETY: Installed only through SignalGuard. This handler is async-signal-safe:
// it performs lock-free atomic operations and a best-effort nonblocking write.
unsafe extern "C" fn signal_handler(_signal: i32) {
    SIGNAL_REQUESTED.store(true, Ordering::Release);
    let write_fd = SIGNAL_WRITE_FD.load(Ordering::Acquire);
    if write_fd >= 0 {
        let byte = [1_u8];
        // SAFETY: write is async-signal-safe, the descriptor is published only
        // while open, and byte points to one initialized byte.
        let _ = unsafe { write(write_fd, byte.as_ptr().cast(), byte.len()) };
    }
}

fn restore_signal(signal_number: i32, previous: *mut c_void) {
    if previous == SIGNAL_ERROR {
        return;
    }
    // SAFETY: previous is the disposition returned by a successful signal()
    // call for this same signal number.
    let _ = unsafe { signal(signal_number, previous) };
}

fn close_fd(file_descriptor: i32) {
    if file_descriptor < 0 {
        return;
    }
    // SAFETY: This helper is called once for each successfully created pipe
    // descriptor after no handler or listener can use it.
    let result = unsafe { close(file_descriptor) };
    if result != 0 {
        log::warn!(
            "failed to close macOS signal-pipe descriptor: {}",
            anyhow!(std::io::Error::last_os_error())
        );
    }
}
