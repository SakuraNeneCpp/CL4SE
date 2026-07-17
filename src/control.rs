//! Cross-process control requests for a running CL4SE instance.

use std::{
    fs,
    io::ErrorKind,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use crate::config::config_directory;

const RESTART_REQUEST_FILE_NAME: &str = ".restart-request";
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct RestartRequest {
    path: PathBuf,
}

impl RestartRequest {
    pub fn new() -> Result<Self> {
        Ok(Self::from_path(
            config_directory()?.join(RESTART_REQUEST_FILE_NAME),
        ))
    }

    pub fn request(&self) -> Result<()> {
        let parent = self.path.parent().ok_or_else(|| {
            anyhow::anyhow!(
                "restart request path has no parent directory: {}",
                self.path.display()
            )
        })?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create restart request directory: {}",
                parent.display()
            )
        })?;
        fs::write(&self.path, b"restart\n")
            .with_context(|| format!("failed to write restart request: {}", self.path.display()))
    }

    /// Removes and acknowledges a pending request.
    pub fn take(&self) -> Result<bool> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to acknowledge restart request: {}",
                    self.path.display()
                )
            }),
        }
    }

    pub fn watcher(&self) -> RestartWatcher {
        RestartWatcher {
            request: self.clone(),
            next_check: Instant::now(),
            error_reported: false,
        }
    }

    fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    fn is_pending(&self) -> Result<bool> {
        self.path
            .try_exists()
            .with_context(|| format!("failed to inspect restart request: {}", self.path.display()))
    }
}

pub struct RestartWatcher {
    request: RestartRequest,
    next_check: Instant,
    error_reported: bool,
}

impl RestartWatcher {
    /// Performs at most one filesystem check per control interval.
    pub fn poll(&mut self) -> bool {
        let now = Instant::now();
        if now < self.next_check {
            return false;
        }
        self.next_check = now.checked_add(CONTROL_POLL_INTERVAL).unwrap_or(now);

        match self.request.is_pending() {
            Ok(pending) => {
                self.error_reported = false;
                pending
            }
            Err(error) => {
                if !self.error_reported {
                    log::warn!("restart control is temporarily unavailable: {error:#}");
                    self.error_reported = true;
                }
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_request() -> Result<(PathBuf, RestartRequest)> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "cl4se-control-test-{}-{unique}",
            std::process::id()
        ));
        let request = RestartRequest::from_path(directory.join(RESTART_REQUEST_FILE_NAME));
        Ok((directory, request))
    }

    #[test]
    fn restart_request_round_trip_is_idempotent() -> Result<()> {
        let (directory, request) = temporary_request()?;
        assert!(!request.take()?);

        request.request()?;
        assert!(request.watcher().poll());
        assert!(request.take()?);
        assert!(!request.take()?);

        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
