//! Platform abstraction and OS-specific dispatch.

use anyhow::Result;

use crate::config::Config;
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
    fn inject_capslock(&mut self) -> anyhow::Result<()>;
}

pub trait ImeStateProvider {
    fn snapshot(&mut self) -> ImeSnapshot;
}

pub trait Autostart {
    fn install(&self) -> anyhow::Result<()>;
    fn uninstall(&self) -> anyhow::Result<()>;
}

pub fn run(config: &Config) -> Result<()> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| backend::run(config)))
        .map_err(|_| anyhow::anyhow!("platform backend panicked after cleanup"))?
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
