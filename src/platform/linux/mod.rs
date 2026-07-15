use anyhow::{bail, Result};

use crate::config::Config;

pub(super) fn run(_config: &Config) -> Result<()> {
    bail!("backend not implemented")
}

pub(super) fn install_autostart() -> Result<()> {
    bail!("backend not implemented")
}

pub(super) fn uninstall_autostart() -> Result<()> {
    bail!("backend not implemented")
}

pub(super) fn doctor() -> Result<()> {
    println!("Linux backend: 未実装 (backend not implemented)");
    bail!("backend not implemented")
}
