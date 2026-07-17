//! Platform abstraction and OS-specific dispatch.

use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{Context, Result};

use crate::config::{config_directory, Config};
use crate::control::ControlRequests;
use crate::core::Engine;
pub use crate::core::{CommitKey, Decision, ImeGuess, ImeSnapshot, ObservedEvent, Platform};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use self::linux as backend;
#[cfg(target_os = "macos")]
use self::macos as backend;
#[cfg(target_os = "windows")]
use self::windows as backend;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("CL4SE supports only Windows, macOS, and Linux");

#[cfg(target_os = "windows")]
pub const CURRENT_PLATFORM: Platform = Platform::Windows;
#[cfg(target_os = "macos")]
pub const CURRENT_PLATFORM: Platform = Platform::MacOs;
#[cfg(target_os = "linux")]
pub const CURRENT_PLATFORM: Platform = Platform::Linux;

pub trait KeyInterceptor {
    fn run(&mut self, engine: &mut Engine, injector: &mut dyn KeyInjector) -> anyhow::Result<()>;
}

pub trait KeyInjector {
    fn inject_commit_key(&mut self, key: CommitKey) -> anyhow::Result<()>;
    fn inject_shift_enter(&mut self) -> anyhow::Result<()>;
    fn inject_capslock(&mut self) -> anyhow::Result<()>;
}

pub trait ImeStateProvider {
    fn snapshot(&mut self) -> ImeSnapshot;
}

pub trait Autostart {
    fn install(&self) -> anyhow::Result<()>;
    fn uninstall(&self) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Stopped,
    RestartRequested,
}

#[derive(Debug)]
pub struct BackgroundProcess {
    pub pid: u32,
    pub log_path: PathBuf,
}

pub fn run(config: &Config, control: &ControlRequests) -> Result<RunOutcome> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        backend::run(config, control)
    }))
    .map_err(|_| anyhow::anyhow!("platform backend panicked after cleanup"))?
}

pub fn start_background() -> Result<BackgroundProcess> {
    backend::start_background()
}

pub fn install_autostart() -> Result<()> {
    backend::install_autostart()
}

pub fn uninstall_autostart() -> Result<()> {
    backend::uninstall_autostart()
}

pub fn doctor() -> Result<()> {
    backend::doctor()
}

fn configure_background_command(command: &mut Command) -> Result<PathBuf> {
    let directory = config_directory()?;
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "failed to create CL4SE background log directory: {}",
            directory.display()
        )
    })?;
    let log_path = directory.join("cl4se.log");
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open background log: {}", log_path.display()))?;
    let stdout = stderr
        .try_clone()
        .with_context(|| format!("failed to clone background log: {}", log_path.display()))?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    Ok(log_path)
}
