//! Cross-process control requests for a running CL4SE instance.

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};

use crate::config::config_directory;

const RESTART_REQUEST_FILE_NAME: &str = ".restart-request";
const STOP_REQUEST_FILE_NAME: &str = ".stop-request";
const STATUS_REQUEST_FILE_NAME: &str = ".status-request";
const STATUS_ACK_FILE_NAME: &str = ".status-ack";
const CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(100);
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(25);
static STATUS_TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    Restart,
    Stop,
}

#[derive(Debug, Clone)]
pub struct ControlRequests {
    restart: PathBuf,
    stop: PathBuf,
    status_request: PathBuf,
    status_ack: PathBuf,
}

impl ControlRequests {
    pub fn new() -> Result<Self> {
        Ok(Self::from_directory(config_directory()?))
    }

    pub fn clear_startup_requests(&self) -> Result<()> {
        remove_if_exists(&self.restart, "stale restart request")?;
        remove_if_exists(&self.stop, "stale stop request")?;
        remove_if_exists(&self.status_ack, "stale status acknowledgement")?;
        Ok(())
    }

    pub fn request_restart(&self) -> Result<()> {
        write_request(&self.restart, b"restart\n", "restart request")
    }

    pub fn complete_restart(&self) -> Result<bool> {
        remove_if_exists(&self.restart, "restart request")
    }

    pub fn complete_stop(&self) -> Result<bool> {
        remove_if_exists(&self.stop, "stop request")
    }

    pub fn stop_running(&self, probe_timeout: Duration, stop_timeout: Duration) -> Result<bool> {
        if !self.probe_running(probe_timeout)? {
            return Ok(false);
        }

        remove_if_exists(&self.stop, "stale stop request")?;
        write_request(&self.stop, b"stop\n", "stop request")?;
        if wait_until(stop_timeout, || Ok(!path_exists(&self.stop)?))? {
            return Ok(true);
        }

        let _ = remove_if_exists(&self.stop, "timed-out stop request");
        bail!("running CL4SE did not finish cleanup within {stop_timeout:?}")
    }

    pub fn probe_running(&self, timeout: Duration) -> Result<bool> {
        remove_if_exists(&self.status_request, "stale status request")?;
        remove_if_exists(&self.status_ack, "stale status acknowledgement")?;
        let token = status_token()?;
        write_request(&self.status_request, token.as_bytes(), "status request")?;

        let acknowledged = wait_until(timeout, || match fs::read(&self.status_ack) {
            Ok(value) => Ok(value == token.as_bytes()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to read status acknowledgement: {}",
                    self.status_ack.display()
                )
            }),
        });

        let acknowledged = acknowledged?;
        remove_if_exists(&self.status_request, "status request")?;
        remove_if_exists(&self.status_ack, "status acknowledgement")?;
        Ok(acknowledged)
    }

    pub fn watcher(&self) -> ControlWatcher {
        ControlWatcher {
            requests: self.clone(),
            next_check: Instant::now(),
            error_reported: false,
        }
    }

    pub fn restart_pending(&self) -> Result<bool> {
        path_exists(&self.restart)
    }

    pub fn stop_pending(&self) -> Result<bool> {
        path_exists(&self.stop)
    }

    fn from_directory(directory: PathBuf) -> Self {
        Self {
            restart: directory.join(RESTART_REQUEST_FILE_NAME),
            stop: directory.join(STOP_REQUEST_FILE_NAME),
            status_request: directory.join(STATUS_REQUEST_FILE_NAME),
            status_ack: directory.join(STATUS_ACK_FILE_NAME),
        }
    }

    fn acknowledge_status(&self) -> Result<()> {
        let token = match fs::read(&self.status_request) {
            Ok(token) => token,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to read status request: {}",
                        self.status_request.display()
                    )
                });
            }
        };

        write_request(&self.status_ack, &token, "status acknowledgement")?;
        remove_if_exists(&self.status_request, "status request")?;
        Ok(())
    }
}

pub struct ControlWatcher {
    requests: ControlRequests,
    next_check: Instant,
    error_reported: bool,
}

impl ControlWatcher {
    /// Performs at most one filesystem control pass per interval.
    pub fn poll(&mut self) -> Option<ControlAction> {
        let now = Instant::now();
        if now < self.next_check {
            return None;
        }
        self.next_check = now.checked_add(CONTROL_POLL_INTERVAL).unwrap_or(now);

        match self.poll_inner() {
            Ok(action) => {
                self.error_reported = false;
                action
            }
            Err(error) => {
                if !self.error_reported {
                    log::warn!("process control is temporarily unavailable: {error:#}");
                    self.error_reported = true;
                }
                None
            }
        }
    }

    fn poll_inner(&self) -> Result<Option<ControlAction>> {
        self.requests.acknowledge_status()?;
        if self.requests.stop_pending()? {
            Ok(Some(ControlAction::Stop))
        } else if self.requests.restart_pending()? {
            Ok(Some(ControlAction::Restart))
        } else {
            Ok(None)
        }
    }
}

fn status_token() -> Result<String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_nanos();
    let counter = STATUS_TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(format!("{}-{timestamp}-{counter}\n", std::process::id()))
}

fn write_request(path: &Path, contents: &[u8], description: &str) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "{description} path has no parent directory: {}",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create process control directory: {}",
            parent.display()
        )
    })?;
    fs::write(path, contents)
        .with_context(|| format!("failed to write {description}: {}", path.display()))
}

fn remove_if_exists(path: &Path, description: &str) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove {description}: {}", path.display())),
    }
}

fn path_exists(path: &Path) -> Result<bool> {
    path.try_exists()
        .with_context(|| format!("failed to inspect process control path: {}", path.display()))
}

fn wait_until<F>(timeout: Duration, mut predicate: F) -> Result<bool>
where
    F: FnMut() -> Result<bool>,
{
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        if predicate()? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(WAIT_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn temporary_requests() -> Result<(PathBuf, ControlRequests)> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "cl4se-control-test-{}-{unique}",
            std::process::id()
        ));
        let requests = ControlRequests::from_directory(directory.clone());
        Ok((directory, requests))
    }

    #[test]
    fn stop_has_priority_over_restart() -> Result<()> {
        let (directory, requests) = temporary_requests()?;
        requests.request_restart()?;
        write_request(&requests.stop, b"stop\n", "test stop request")?;

        assert_eq!(requests.watcher().poll(), Some(ControlAction::Stop));
        requests.clear_startup_requests()?;
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn restart_request_round_trip_is_idempotent() -> Result<()> {
        let (directory, requests) = temporary_requests()?;
        assert!(!requests.complete_restart()?);

        requests.request_restart()?;
        assert_eq!(requests.watcher().poll(), Some(ControlAction::Restart));
        assert!(requests.complete_restart()?);
        assert!(!requests.complete_restart()?);

        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn status_probe_requires_a_matching_live_acknowledgement() -> Result<()> {
        let (directory, requests) = temporary_requests()?;
        let worker_requests = requests.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let mut watcher = worker_requests.watcher();
            ready_tx.send(()).expect("test readiness send should work");
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let _ = watcher.poll();
                if worker_requests.status_ack.exists() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        ready_rx.recv()?;

        assert!(requests.probe_running(Duration::from_secs(1))?);
        worker.join().expect("status worker should not panic");
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn stop_waits_until_the_running_instance_completes_cleanup() -> Result<()> {
        let (directory, requests) = temporary_requests()?;
        let worker_requests = requests.clone();
        let worker = thread::spawn(move || {
            let mut watcher = worker_requests.watcher();
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                if watcher.poll() == Some(ControlAction::Stop) {
                    worker_requests
                        .complete_stop()
                        .expect("test stop completion should work");
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            panic!("test worker did not receive stop request");
        });

        assert!(requests.stop_running(Duration::from_secs(1), Duration::from_secs(1))?);
        worker.join().expect("stop worker should not panic");
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
