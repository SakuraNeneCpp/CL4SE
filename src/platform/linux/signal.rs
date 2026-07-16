use std::{
    ffi::c_int,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{bail, Result};

const SIGINT: c_int = 2;
const SIGTERM: c_int = 15;
const SIG_ERR: usize = usize::MAX;

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn signal(signal: c_int, handler: usize) -> usize;
}

pub(crate) struct SignalGuard {
    previous_sigint: usize,
    previous_sigterm: usize,
}

impl SignalGuard {
    pub(crate) fn install() -> Result<Self> {
        STOP_REQUESTED.store(false, Ordering::Release);
        // SAFETY: linux_signal_handler has the C signal-handler ABI and only
        // stores to a lock-free atomic. SIGINT is a valid Linux signal number.
        let previous_sigint = unsafe { signal(SIGINT, linux_signal_handler as *const () as usize) };
        if previous_sigint == SIG_ERR {
            bail!("failed to install SIGINT handler");
        }

        // SAFETY: The same handler constraints apply to SIGTERM.
        let previous_sigterm =
            unsafe { signal(SIGTERM, linux_signal_handler as *const () as usize) };
        if previous_sigterm == SIG_ERR {
            // SAFETY: previous_sigint was returned by signal for SIGINT and is
            // restored immediately because SIGTERM installation failed.
            unsafe { signal(SIGINT, previous_sigint) };
            bail!("failed to install SIGTERM handler");
        }

        Ok(Self {
            previous_sigint,
            previous_sigterm,
        })
    }

    pub(crate) fn stop_requested() -> bool {
        STOP_REQUESTED.load(Ordering::Acquire)
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        // SAFETY: Both values came from successful signal calls for these
        // exact signals. Restoration happens after the event loop has stopped.
        unsafe {
            signal(SIGTERM, self.previous_sigterm);
            signal(SIGINT, self.previous_sigint);
        }
    }
}

extern "C" fn linux_signal_handler(_signal: c_int) {
    STOP_REQUESTED.store(true, Ordering::Release);
}
