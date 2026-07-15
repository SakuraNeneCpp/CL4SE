//! Platform abstraction and OS-specific dispatch.

use anyhow::Result;

use crate::core::Engine;

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
compile_error!("CLIME supports only Windows, macOS, and Linux");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImeGuess {
    Yes,
    No,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitKey {
    Enter,
    CtrlM,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImeSnapshot {
    pub active: ImeGuess,
    pub ime_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedEvent {
    TriggerKeyDown { shift: bool, other_mods: bool },
    PrintableKeyDown,
    CommitLikeKeyDown,
    MouseClick,
    FocusChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    InjectCommitKey(CommitKey),
    PassThroughCapsLock,
    Suppress,
    Ignore,
}

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

pub(crate) fn run() -> Result<()> {
    backend::run()
}

pub(crate) fn install_autostart() -> Result<()> {
    backend::install_autostart()
}

pub(crate) fn uninstall_autostart() -> Result<()> {
    backend::uninstall_autostart()
}

pub(crate) fn doctor() -> Result<()> {
    backend::doctor()
}
