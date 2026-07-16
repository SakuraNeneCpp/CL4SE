use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{anyhow, bail, Context, Result};
use directories::BaseDirs;

use crate::platform::Autostart;

use super::xkb::{self, XkbInstallOutcome};

const SYSTEMD_UNIT_NAME: &str = "clime.service";
const XDG_DESKTOP_NAME: &str = "clime.desktop";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutostartBackend {
    Systemd,
    Xdg,
}

pub(crate) struct LinuxAutostart;

impl Autostart for LinuxAutostart {
    fn install(&self) -> Result<()> {
        let executable = env::current_exe().context("failed to locate clime executable")?;
        let backend = choose_backend(systemd_user_available());
        match backend {
            AutostartBackend::Systemd => {
                install_systemd(&executable)?;
                remove_if_exists(&xdg_desktop_path()?)?;
                println!("Autostart: installed systemd user service {SYSTEMD_UNIT_NAME}");
            }
            AutostartBackend::Xdg => {
                install_xdg(&executable)?;
                println!("Autostart: installed XDG desktop entry {XDG_DESKTOP_NAME}");
            }
        }

        match xkb::configure_for_install() {
            Ok(XkbInstallOutcome::Applied(backend)) => {
                println!("Caps Lock suppression: applied caps:none using {backend:?}");
            }
            Ok(XkbInstallOutcome::AlreadyConfigured(backend)) => {
                println!("Caps Lock suppression: caps:none already configured via {backend:?}");
            }
            Ok(XkbInstallOutcome::Unsupported) => {
                println!(
                    "Caps Lock suppression: WARN unsupported desktop/session; configure Caps Lock as disabled in desktop keyboard settings"
                );
            }
            Err(error) => {
                println!(
                    "Caps Lock suppression: WARN automatic configuration failed: {error:#}; run `clime doctor`"
                );
            }
        }
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        let autostart_result = uninstall_autostart_files();
        let xkb_result = xkb::restore_managed();
        autostart_result.and(xkb_result)
    }
}

fn choose_backend(systemd_available: bool) -> AutostartBackend {
    if systemd_available {
        AutostartBackend::Systemd
    } else {
        AutostartBackend::Xdg
    }
}

fn install_systemd(executable: &Path) -> Result<()> {
    let path = systemd_unit_path()?;
    write_parented(&path, &systemd_unit(executable)?)?;
    run_systemctl(["daemon-reload"])?;
    run_systemctl(["enable", "--now", SYSTEMD_UNIT_NAME])
}

fn install_xdg(executable: &Path) -> Result<()> {
    let path = xdg_desktop_path()?;
    write_parented(&path, &xdg_desktop_entry(executable)?)
}

fn uninstall_autostart_files() -> Result<()> {
    let systemd_path = systemd_unit_path()?;
    let mut systemd_error = None;
    if systemd_path.exists() && systemd_user_available() {
        if let Err(error) = run_systemctl(["disable", "--now", SYSTEMD_UNIT_NAME]) {
            systemd_error = Some(error);
        }
    }
    remove_if_exists(&systemd_path)?;
    if systemd_user_available() {
        if let Err(error) = run_systemctl(["daemon-reload"]) {
            systemd_error.get_or_insert(error);
        }
    }
    remove_if_exists(&xdg_desktop_path()?)?;

    if let Some(error) = systemd_error {
        Err(error)
    } else {
        Ok(())
    }
}

fn systemd_user_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn run_systemctl<const N: usize>(arguments: [&str; N]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(arguments)
        .output()
        .context("failed to execute systemctl --user")?;
    require_success(output, "systemctl --user")
}

fn require_success(output: Output, command: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("{command} failed with {}: {}", output.status, stderr.trim())
}

fn systemd_unit(executable: &Path) -> Result<String> {
    let executable = quote_systemd(executable)?;
    Ok(format!(
        "[Unit]\nDescription=CLIME Caps Lock IME commit\nAfter=graphical-session.target\n\n[Service]\nType=simple\nExecStart={executable} run\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n"
    ))
}

fn xdg_desktop_entry(executable: &Path) -> Result<String> {
    let executable = quote_desktop_exec(executable)?;
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=CLIME\nComment=Assign IME commit to the physical Caps Lock key\nExec={executable} run\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    ))
}

fn quote_systemd(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("clime executable path is not UTF-8: {}", path.display()))?;
    reject_line_breaks(path)?;
    let escaped = path
        .replace('%', "%%")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    Ok(format!("\"{escaped}\""))
}

fn quote_desktop_exec(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow!("clime executable path is not UTF-8: {}", path.display()))?;
    reject_line_breaks(path)?;
    let escaped = path.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("\"{escaped}\""))
}

fn reject_line_breaks(value: &str) -> Result<()> {
    if value.contains(['\n', '\r']) {
        bail!("clime executable path contains a line break");
    }
    Ok(())
}

fn systemd_unit_path() -> Result<PathBuf> {
    let base = BaseDirs::new().ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(base
        .config_dir()
        .join("systemd/user")
        .join(SYSTEMD_UNIT_NAME))
}

fn xdg_desktop_path() -> Result<PathBuf> {
    let base = BaseDirs::new().ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(base.config_dir().join("autostart").join(XDG_DESKTOP_NAME))
}

fn write_parented(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("autostart path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create autostart directory: {}", parent.display()))?;
    fs::write(path, contents)
        .with_context(|| format!("failed to write autostart file: {}", path.display()))
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to remove autostart file: {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_is_preferred_and_xdg_is_the_fallback() {
        assert_eq!(choose_backend(true), AutostartBackend::Systemd);
        assert_eq!(choose_backend(false), AutostartBackend::Xdg);
    }

    #[test]
    fn systemd_unit_contains_required_service_fields() -> Result<()> {
        let unit = systemd_unit(Path::new("/opt/CLIME Tools/clime"))?;
        assert!(unit.contains("ExecStart=\"/opt/CLIME Tools/clime\" run"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("Restart=on-failure"));
        Ok(())
    }

    #[test]
    fn systemd_path_escapes_specifiers() -> Result<()> {
        let unit = systemd_unit(Path::new("/opt/100%/clime"))?;
        assert!(unit.contains("/opt/100%%/clime"));
        Ok(())
    }

    #[test]
    fn xdg_entry_quotes_paths_with_spaces() -> Result<()> {
        let entry = xdg_desktop_entry(Path::new("/opt/CLIME Tools/clime"))?;
        assert!(entry.contains("Exec=\"/opt/CLIME Tools/clime\" run"));
        assert!(entry.contains("X-GNOME-Autostart-enabled=true"));
        Ok(())
    }
}
