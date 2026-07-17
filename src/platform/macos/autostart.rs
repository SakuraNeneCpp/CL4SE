use std::{env, fs, path::PathBuf, process::Command};

use anyhow::{anyhow, bail, Context, Result};
use directories::BaseDirs;

use crate::platform::Autostart;

const LABEL: &str = "dev.cl4se.agent";
const PLIST_FILE: &str = "dev.cl4se.agent.plist";
const LAUNCHCTL: &str = "/bin/launchctl";
const ID: &str = "/usr/bin/id";

pub(crate) struct MacOsAutostart;

impl Autostart for MacOsAutostart {
    fn install(&self) -> Result<()> {
        let executable = env::current_exe().context("failed to locate cl4se executable")?;
        let plist_path = launch_agent_path()?;
        let parent = plist_path
            .parent()
            .ok_or_else(|| anyhow!("LaunchAgent path has no parent: {}", plist_path.display()))?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create LaunchAgents directory: {}",
                parent.display()
            )
        })?;
        fs::write(
            &plist_path,
            launch_agent_plist(&executable.to_string_lossy()),
        )
        .with_context(|| format!("failed to write LaunchAgent: {}", plist_path.display()))?;

        let domain = user_domain()?;
        let service = format!("{domain}/{LABEL}");
        if launchctl_service_is_loaded(&service)? {
            run_launchctl(["bootout", service.as_str()])
                .context("failed to replace the loaded CL4SE LaunchAgent")?;
        }
        let plist = plist_path.to_string_lossy();
        run_launchctl(["bootstrap", domain.as_str(), plist.as_ref()])
            .context("failed to bootstrap CL4SE LaunchAgent")
    }

    fn uninstall(&self) -> Result<()> {
        let plist_path = launch_agent_path()?;
        let domain = user_domain()?;
        let service = format!("{domain}/{LABEL}");
        if launchctl_service_is_loaded(&service)? {
            run_launchctl(["bootout", service.as_str()])
                .context("failed to boot out CL4SE LaunchAgent")?;
        }

        match fs::remove_file(&plist_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error)
                .with_context(|| format!("failed to remove LaunchAgent: {}", plist_path.display())),
        }
    }
}

fn launch_agent_path() -> Result<PathBuf> {
    let base = BaseDirs::new().ok_or_else(|| anyhow!("could not determine the home directory"))?;
    Ok(base
        .home_dir()
        .join("Library/LaunchAgents")
        .join(PLIST_FILE))
}

fn user_domain() -> Result<String> {
    let output = Command::new(ID)
        .arg("-u")
        .output()
        .context("failed to execute id -u")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("id -u failed with {}: {}", output.status, stderr.trim());
    }
    let uid = String::from_utf8(output.stdout)
        .context("id -u returned non-UTF-8 output")?
        .trim()
        .parse::<u32>()
        .context("id -u returned an invalid uid")?;
    Ok(format!("gui/{uid}"))
}

fn launchctl_service_is_loaded(service: &str) -> Result<bool> {
    let output = Command::new(LAUNCHCTL)
        .args(["print", service])
        .output()
        .context("failed to execute launchctl print")?;
    Ok(output.status.success())
}

fn run_launchctl<'a>(arguments: impl IntoIterator<Item = &'a str>) -> Result<()> {
    let output = Command::new(LAUNCHCTL)
        .args(arguments)
        .output()
        .context("failed to execute launchctl")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("launchctl failed with {}: {}", output.status, stderr.trim())
}

fn launch_agent_plist(executable: &str) -> String {
    let executable = xml_escape(executable);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
    <string>run</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
"#
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contains_required_launch_agent_fields() {
        let plist = launch_agent_plist("/Applications/CL4SE & Tools/cl4se");

        assert!(plist.contains("<string>dev.cl4se.agent</string>"));
        assert!(plist.contains("<string>/Applications/CL4SE &amp; Tools/cl4se</string>"));
        assert!(plist.contains("<string>run</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
    }

    #[test]
    fn xml_escape_covers_path_sensitive_characters() {
        assert_eq!(xml_escape("&<>\"'"), "&amp;&lt;&gt;&quot;&apos;");
    }
}
