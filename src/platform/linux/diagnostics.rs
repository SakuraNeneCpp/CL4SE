use std::{
    fs::{self, File, OpenOptions},
    io::ErrorKind,
    path::Path,
};

use crate::core::ImeGuess;

use super::{
    ime::{ImeFramework, ImeProbe},
    xkb::{XkbBackend, XkbStatus},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputAccess {
    Ready { readable: usize, total: usize },
    MissingDirectory,
    NoEventDevices,
    Denied { total: usize, detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UInputAccess {
    Ready,
    Missing,
    Denied(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DoctorReport {
    pub(crate) lines: Vec<String>,
    pub(crate) has_error: bool,
}

pub(crate) fn inspect_input_access() -> InputAccess {
    inspect_input_directory(Path::new("/dev/input"))
}

pub(crate) fn inspect_uinput_access() -> UInputAccess {
    match OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/uinput")
    {
        Ok(_) => UInputAccess::Ready,
        Err(error) if error.kind() == ErrorKind::NotFound => UInputAccess::Missing,
        Err(error) => UInputAccess::Denied(error.to_string()),
    }
}

pub(crate) fn build_report(
    input: InputAccess,
    uinput: UInputAccess,
    ime: &ImeProbe,
    xkb: &XkbStatus,
) -> DoctorReport {
    let mut lines = Vec::new();
    let mut has_error = false;

    match input {
        InputAccess::Ready { readable, total } => {
            lines.push(format!(
                "/dev/input: OK ({readable}/{total} event devices readable)"
            ));
        }
        InputAccess::MissingDirectory => {
            has_error = true;
            lines.push("/dev/input: ERROR directory is missing".to_owned());
            lines.push(input_permission_help());
        }
        InputAccess::NoEventDevices => {
            has_error = true;
            lines.push("/dev/input: ERROR no event devices found".to_owned());
            lines.push(input_permission_help());
        }
        InputAccess::Denied { total, detail } => {
            has_error = true;
            lines.push(format!(
                "/dev/input: ERROR none of {total} event devices are readable ({detail})"
            ));
            lines.push(input_permission_help());
        }
    }

    match uinput {
        UInputAccess::Ready => lines.push("/dev/uinput: OK (read/write)".to_owned()),
        UInputAccess::Missing => {
            has_error = true;
            lines.push(
                "/dev/uinput: ERROR device is missing; run `sudo modprobe uinput`".to_owned(),
            );
            lines.extend(uinput_permission_help());
        }
        UInputAccess::Denied(detail) => {
            has_error = true;
            lines.push(format!("/dev/uinput: ERROR not writable ({detail})"));
            lines.extend(uinput_permission_help());
        }
    }

    match ime.framework {
        ImeFramework::Fcitx5 => lines.push(format!(
            "IME framework: fcitx5 (active={:?}, ime_id={})",
            ime.snapshot.active,
            ime.snapshot.ime_id.as_deref().unwrap_or("none")
        )),
        ImeFramework::IBus => lines.push(format!(
            "IME framework: IBus (active={:?}, ime_id={})",
            ime.snapshot.active,
            ime.snapshot.ime_id.as_deref().unwrap_or("none")
        )),
        ImeFramework::Unavailable => {
            has_error = true;
            lines.push("IME framework: ERROR fcitx5 and IBus are unavailable".to_owned());
            lines.push(
                "Fix: install and start fcitx5 or IBus in this desktop session, then rerun `clime doctor`"
                    .to_owned(),
            );
        }
    }
    if let Some(error) = ime.error.as_deref() {
        has_error = true;
        lines.push(format!("IME query: ERROR {error}"));
        lines.push(
            "Fix: confirm the desktop D-Bus session and selected IME are running, then retry"
                .to_owned(),
        );
    } else if ime.snapshot.active == ImeGuess::Unknown {
        lines.push(
            "IME query: WARN state is Unknown; CLIME will not inject (focus a text field and retry)"
                .to_owned(),
        );
    }

    if xkb.caps_none {
        lines.push(format!(
            "Caps Lock suppression: OK ({:?}, {})",
            xkb.backend, xkb.detail
        ));
    } else {
        has_error = true;
        lines.push(format!(
            "Caps Lock suppression: ERROR caps:none is not active ({:?}, {})",
            xkb.backend, xkb.detail
        ));
        match xkb.backend {
            XkbBackend::Gnome => lines.push(
                "Fix: gsettings set org.gnome.desktop.input-sources xkb-options \"['caps:none']\""
                    .to_owned(),
            ),
            XkbBackend::X11 => {
                lines.push("Fix: setxkbmap -option caps:none".to_owned());
            }
            XkbBackend::Unsupported => lines.push(
                "Fix: disable Caps Lock in your Wayland compositor/desktop keyboard settings; automatic setup supports GNOME only"
                    .to_owned(),
            ),
        }
    }

    if has_error {
        lines.push(
            "Result: ERROR. Apply the fixes above, log out and back in if group membership changed, then rerun `clime doctor`."
                .to_owned(),
        );
    } else {
        lines.push("Result: OK. Next: run `clime install-autostart`.".to_owned());
    }

    DoctorReport { lines, has_error }
}

fn inspect_input_directory(path: &Path) -> InputAccess {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return InputAccess::MissingDirectory;
        }
        Err(error) => {
            return InputAccess::Denied {
                total: 0,
                detail: error.to_string(),
            };
        }
    };

    let mut total = 0;
    let mut readable = 0;
    let mut last_error = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("event") {
            continue;
        }
        total += 1;
        match File::open(entry.path()) {
            Ok(_) => readable += 1,
            Err(error) => last_error = Some(error.to_string()),
        }
    }

    if total == 0 {
        InputAccess::NoEventDevices
    } else if readable > 0 {
        InputAccess::Ready { readable, total }
    } else {
        InputAccess::Denied {
            total,
            detail: last_error.unwrap_or_else(|| "permission denied".to_owned()),
        }
    }
}

fn input_permission_help() -> String {
    "Fix: sudo usermod -aG input \"$USER\"  # then log out and back in".to_owned()
}

fn uinput_permission_help() -> Vec<String> {
    vec![
        "Fix: sudo usermod -aG input \"$USER\"  # then log out and back in".to_owned(),
        "Udev rule (/etc/udev/rules.d/99-clime-uinput.rules):".to_owned(),
        "KERNEL==\"uinput\", SUBSYSTEM==\"misc\", GROUP=\"input\", MODE=\"0660\", OPTIONS+=\"static_node=uinput\""
            .to_owned(),
        "Reload: sudo udevadm control --reload-rules && sudo udevadm trigger --name-match=uinput"
            .to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use crate::core::ImeSnapshot;

    use super::*;

    fn ime(active: ImeGuess) -> ImeProbe {
        ImeProbe {
            framework: ImeFramework::Fcitx5,
            snapshot: ImeSnapshot {
                active,
                ime_id: Some("mozc".to_owned()),
            },
            error: None,
        }
    }

    #[test]
    fn healthy_report_has_no_error() {
        let report = build_report(
            InputAccess::Ready {
                readable: 2,
                total: 2,
            },
            UInputAccess::Ready,
            &ime(ImeGuess::Yes),
            &XkbStatus {
                backend: XkbBackend::Gnome,
                caps_none: true,
                detail: "['caps:none']".to_owned(),
            },
        );
        assert!(!report.has_error);
        assert!(report.lines.iter().any(|line| line.contains("fcitx5")));
        assert!(report
            .lines
            .iter()
            .any(|line| line.contains("install-autostart")));
    }

    #[test]
    fn missing_permissions_and_xkb_include_actionable_commands() {
        let report = build_report(
            InputAccess::Denied {
                total: 4,
                detail: "permission denied".to_owned(),
            },
            UInputAccess::Denied("permission denied".to_owned()),
            &ImeProbe {
                framework: ImeFramework::Unavailable,
                snapshot: ImeSnapshot {
                    active: ImeGuess::Unknown,
                    ime_id: None,
                },
                error: Some("no D-Bus".to_owned()),
            },
            &XkbStatus {
                backend: XkbBackend::X11,
                caps_none: false,
                detail: "".to_owned(),
            },
        );
        let output = report.lines.join("\n");
        assert!(report.has_error);
        assert!(output.contains("usermod -aG input"));
        assert!(output.contains("99-clime-uinput.rules"));
        assert!(output.contains("setxkbmap -option caps:none"));
        assert!(output.contains("rerun `clime doctor`"));
    }

    #[test]
    fn unknown_ime_without_transport_error_is_warning_only() {
        let report = build_report(
            InputAccess::Ready {
                readable: 1,
                total: 1,
            },
            UInputAccess::Ready,
            &ime(ImeGuess::Unknown),
            &XkbStatus {
                backend: XkbBackend::X11,
                caps_none: true,
                detail: "caps:none".to_owned(),
            },
        );
        assert!(!report.has_error);
        assert!(report.lines.iter().any(|line| line.contains("WARN")));
    }
}
