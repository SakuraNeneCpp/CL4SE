use anyhow::{bail, Result};

pub(super) fn run() -> Result<()> {
    bail!("backend not implemented")
}

pub(super) fn install_autostart() -> Result<()> {
    bail!("backend not implemented")
}

pub(super) fn uninstall_autostart() -> Result<()> {
    bail!("backend not implemented")
}

pub(super) fn doctor() -> Result<()> {
    println!("Windows backend: 未実装 (backend not implemented)");
    bail!("backend not implemented")
}
