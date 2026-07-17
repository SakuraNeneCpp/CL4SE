use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{anyhow, bail, Context, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

const STATE_FILE_NAME: &str = "linux-xkb-state.toml";
const CL4SE_CONFIG_DIR: &str = "cl4se";
const GNOME_SCHEMA: &str = "org.gnome.desktop.input-sources";
const GNOME_KEY: &str = "xkb-options";
const CAPS_NONE: &str = "caps:none";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum XkbBackend {
    Gnome,
    X11,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum XkbInstallOutcome {
    Applied(XkbBackend),
    AlreadyConfigured(XkbBackend),
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct XkbStatus {
    pub(crate) backend: XkbBackend,
    pub(crate) caps_none: bool,
    pub(crate) detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RestoreState {
    backend: XkbBackend,
    previous: String,
}

pub(crate) fn configure_for_install() -> Result<XkbInstallOutcome> {
    let path = state_path()?;
    if path.exists() {
        let state = read_state(&path)?;
        apply_managed_state(&state)?;
        return Ok(XkbInstallOutcome::Applied(state.backend));
    }

    match detect_backend() {
        XkbBackend::Gnome => configure_gnome(&path),
        XkbBackend::X11 => configure_x11(&path),
        XkbBackend::Unsupported => Ok(XkbInstallOutcome::Unsupported),
    }
}

/// Reapplies only settings previously managed by install-autostart. This is
/// needed because X11 keymaps can be reset at login; a manual `cl4se run`
/// without managed state never mutates the desktop configuration.
pub(crate) fn reapply_if_managed() -> Result<()> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(());
    }
    apply_managed_state(&read_state(&path)?)
}

pub(crate) fn restore_managed() -> Result<()> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(());
    }
    let state = read_state(&path)?;
    match state.backend {
        XkbBackend::Gnome => set_gnome_raw(&state.previous)?,
        XkbBackend::X11 => set_x11_options(&split_x11_options(&state.previous))?,
        XkbBackend::Unsupported => {}
    }
    fs::remove_file(&path)
        .with_context(|| format!("failed to remove XKB restore state: {}", path.display()))
}

pub(crate) fn inspect() -> XkbStatus {
    match detect_backend() {
        XkbBackend::Gnome => match get_gnome_raw().and_then(|raw| {
            let options = parse_gsettings_options(&raw)?;
            Ok((raw, options))
        }) {
            Ok((raw, options)) => XkbStatus {
                backend: XkbBackend::Gnome,
                caps_none: has_caps_none(&options),
                detail: raw,
            },
            Err(error) => XkbStatus {
                backend: XkbBackend::Gnome,
                caps_none: false,
                detail: format!("gsettings query failed: {error:#}"),
            },
        },
        XkbBackend::X11 => match get_x11_options() {
            Ok(options) => XkbStatus {
                backend: XkbBackend::X11,
                caps_none: has_caps_none(&options),
                detail: options.join(","),
            },
            Err(error) => XkbStatus {
                backend: XkbBackend::X11,
                caps_none: false,
                detail: format!("setxkbmap query failed: {error:#}"),
            },
        },
        XkbBackend::Unsupported => XkbStatus {
            backend: XkbBackend::Unsupported,
            caps_none: false,
            detail:
                "unsupported desktop/session; disable Caps Lock in the desktop keyboard settings"
                    .to_owned(),
        },
    }
}

fn configure_gnome(path: &Path) -> Result<XkbInstallOutcome> {
    let previous = get_gnome_raw()?;
    let mut options = parse_gsettings_options(&previous)?;
    if has_caps_none(&options) {
        return Ok(XkbInstallOutcome::AlreadyConfigured(XkbBackend::Gnome));
    }
    options.push(CAPS_NONE.to_owned());
    let state = RestoreState {
        backend: XkbBackend::Gnome,
        previous,
    };
    write_state(path, &state)?;
    if let Err(error) = set_gnome_options(&options) {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(XkbInstallOutcome::Applied(XkbBackend::Gnome))
}

fn configure_x11(path: &Path) -> Result<XkbInstallOutcome> {
    let mut options = get_x11_options()?;
    if has_caps_none(&options) {
        return Ok(XkbInstallOutcome::AlreadyConfigured(XkbBackend::X11));
    }
    let state = RestoreState {
        backend: XkbBackend::X11,
        previous: options.join(","),
    };
    options.push(CAPS_NONE.to_owned());
    write_state(path, &state)?;
    if let Err(error) = set_x11_options(&options) {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(XkbInstallOutcome::Applied(XkbBackend::X11))
}

fn apply_managed_state(state: &RestoreState) -> Result<()> {
    match state.backend {
        XkbBackend::Gnome => {
            let mut options = parse_gsettings_options(&state.previous)?;
            if !has_caps_none(&options) {
                options.push(CAPS_NONE.to_owned());
            }
            set_gnome_options(&options)
        }
        XkbBackend::X11 => {
            let mut options = split_x11_options(&state.previous);
            if !has_caps_none(&options) {
                options.push(CAPS_NONE.to_owned());
            }
            set_x11_options(&options)
        }
        XkbBackend::Unsupported => Ok(()),
    }
}

fn detect_backend() -> XkbBackend {
    backend_from_environment(
        env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        env::var("DISPLAY").ok().as_deref(),
    )
}

fn backend_from_environment(desktop: Option<&str>, display: Option<&str>) -> XkbBackend {
    if desktop.is_some_and(|desktop| {
        desktop
            .split([':', ';'])
            .any(|part| part.eq_ignore_ascii_case("gnome"))
    }) {
        XkbBackend::Gnome
    } else if display.is_some_and(|display| !display.is_empty()) {
        XkbBackend::X11
    } else {
        XkbBackend::Unsupported
    }
}

fn get_gnome_raw() -> Result<String> {
    let output = Command::new("gsettings")
        .args(["get", GNOME_SCHEMA, GNOME_KEY])
        .output()
        .context("failed to execute gsettings")?;
    output_stdout(output, "gsettings get")
}

fn set_gnome_options(options: &[String]) -> Result<()> {
    set_gnome_raw(&format_gsettings_options(options))
}

fn set_gnome_raw(value: &str) -> Result<()> {
    let output = Command::new("gsettings")
        .args(["set", GNOME_SCHEMA, GNOME_KEY, value])
        .output()
        .context("failed to execute gsettings")?;
    require_success(output, "gsettings set")
}

fn get_x11_options() -> Result<Vec<String>> {
    let output = Command::new("setxkbmap")
        .arg("-query")
        .output()
        .context("failed to execute setxkbmap -query")?;
    let stdout = output_stdout(output, "setxkbmap -query")?;
    Ok(parse_setxkbmap_options(&stdout))
}

fn set_x11_options(options: &[String]) -> Result<()> {
    let mut command = Command::new("setxkbmap");
    // An empty -option clears the current list before the preserved options
    // and caps:none are restored, preventing duplicate/conflicting entries.
    command.arg("-option").arg("");
    for option in options {
        command.arg("-option").arg(option);
    }
    let output = command.output().context("failed to execute setxkbmap")?;
    require_success(output, "setxkbmap")
}

fn output_stdout(output: Output, command: &str) -> Result<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{command} failed with {}: {}", output.status, stderr.trim());
    }
    String::from_utf8(output.stdout)
        .with_context(|| format!("{command} returned non-UTF-8 output"))
        .map(|output| output.trim().to_owned())
}

fn require_success(output: Output, command: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("{command} failed with {}: {}", output.status, stderr.trim())
}

fn state_path() -> Result<PathBuf> {
    let base = BaseDirs::new().ok_or_else(|| anyhow!("could not determine config directory"))?;
    Ok(base
        .config_dir()
        .join(CL4SE_CONFIG_DIR)
        .join(STATE_FILE_NAME))
}

fn write_state(path: &Path, state: &RestoreState) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("XKB state path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create XKB state directory: {}", parent.display()))?;
    let contents = toml::to_string(state).context("failed to serialize XKB restore state")?;
    fs::write(path, contents)
        .with_context(|| format!("failed to write XKB restore state: {}", path.display()))
}

fn read_state(path: &Path) -> Result<RestoreState> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read XKB restore state: {}", path.display()))?;
    toml::from_str(&contents)
        .with_context(|| format!("failed to parse XKB restore state: {}", path.display()))
}

fn parse_setxkbmap_options(output: &str) -> Vec<String> {
    output
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case("options")
                .then(|| split_x11_options(value))
        })
        .unwrap_or_default()
}

fn split_x11_options(options: &str) -> Vec<String> {
    options
        .split(',')
        .map(str::trim)
        .filter(|option| !option.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_gsettings_options(value: &str) -> Result<Vec<String>> {
    let value = value.trim();
    let start = value
        .find('[')
        .ok_or_else(|| anyhow!("gsettings value is not a string array: {value}"))?;
    let end = value
        .rfind(']')
        .filter(|end| *end >= start)
        .ok_or_else(|| anyhow!("gsettings value is not a string array: {value}"))?;
    let chars = value[start + 1..end].chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut options = Vec::new();

    while index < chars.len() {
        while index < chars.len() && (chars[index].is_whitespace() || chars[index] == ',') {
            index += 1;
        }
        if index == chars.len() {
            break;
        }
        if chars[index] != '\'' {
            bail!("unexpected gsettings array syntax near character {index}");
        }
        index += 1;
        let mut option = String::new();
        let mut closed = false;
        while index < chars.len() {
            match chars[index] {
                '\\' => {
                    index += 1;
                    let escaped = chars
                        .get(index)
                        .ok_or_else(|| anyhow!("unterminated gsettings escape"))?;
                    option.push(*escaped);
                    index += 1;
                }
                '\'' => {
                    index += 1;
                    closed = true;
                    break;
                }
                character => {
                    option.push(character);
                    index += 1;
                }
            }
        }
        if !closed {
            bail!("unterminated string in gsettings array");
        }
        options.push(option);
    }
    Ok(options)
}

fn format_gsettings_options(options: &[String]) -> String {
    let options = options
        .iter()
        .map(|option| {
            let escaped = option.replace('\\', "\\\\").replace('\'', "\\'");
            format!("'{escaped}'")
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{options}]")
}

fn has_caps_none(options: &[String]) -> bool {
    options.iter().any(|option| option == CAPS_NONE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_detection_prefers_gnome_and_supports_x11() {
        assert_eq!(
            backend_from_environment(Some("ubuntu:GNOME"), Some(":0")),
            XkbBackend::Gnome
        );
        assert_eq!(
            backend_from_environment(Some("KDE"), Some(":0")),
            XkbBackend::X11
        );
        assert_eq!(
            backend_from_environment(Some("KDE"), None),
            XkbBackend::Unsupported
        );
    }

    #[test]
    fn setxkbmap_query_parser_preserves_existing_options() {
        let output = "rules: evdev\nmodel: pc105\noptions: grp:alt_shift_toggle,caps:none\n";
        assert_eq!(
            parse_setxkbmap_options(output),
            vec!["grp:alt_shift_toggle", "caps:none"]
        );
    }

    #[test]
    fn gsettings_parser_handles_empty_and_escaped_arrays() -> Result<()> {
        assert!(parse_gsettings_options("@as []")?.is_empty());
        assert_eq!(
            parse_gsettings_options("['grp:alt_shift_toggle', 'custom\\'option']")?,
            vec!["grp:alt_shift_toggle", "custom'option"]
        );
        Ok(())
    }

    #[test]
    fn gsettings_formatter_round_trips() -> Result<()> {
        let options = vec![
            "grp:alt_shift_toggle".to_owned(),
            "custom'option".to_owned(),
            CAPS_NONE.to_owned(),
        ];
        let formatted = format_gsettings_options(&options);
        assert_eq!(parse_gsettings_options(&formatted)?, options);
        Ok(())
    }
}
