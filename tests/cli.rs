use std::{error::Error, process::Command};

#[test]
fn version_is_available() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_cl4se"))
        .arg("--version")
        .output()?;

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout)?.trim(), "cl4se 1.0.3");
    Ok(())
}

#[test]
fn setting_help_lists_safe_idle_actions() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_cl4se"))
        .args(["setting", "idle-action", "--help"])
        .output()?;

    let stdout = String::from_utf8(output.stdout)?;
    assert!(output.status.success());
    assert!(stdout.contains("none"));
    assert!(stdout.contains("shift-enter"));
    assert!(stdout.contains("capslock"));
    Ok(())
}

#[test]
fn help_lists_background_lifecycle_commands() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_cl4se"))
        .arg("--help")
        .output()?;

    let stdout = String::from_utf8(output.stdout)?;
    assert!(output.status.success());
    assert!(stdout.contains("start"));
    assert!(stdout.contains("stop"));
    assert!(stdout.contains("update"));
    Ok(())
}

#[test]
#[cfg(target_os = "linux")]
fn doctor_runs_linux_diagnostics() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_cl4se"))
        .arg("doctor")
        .output()?;

    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("CL4SE doctor (Linux)"));
    assert!(stdout.contains("/dev/input:"));
    assert!(stdout.contains("/dev/uinput:"));
    assert!(stdout.contains("IME framework:"));
    assert!(stdout.contains("Caps Lock suppression:"));
    assert!(stdout.contains("Result:"));
    assert!(!stdout.contains("backend not implemented"));
    Ok(())
}

#[test]
#[cfg(target_os = "macos")]
fn doctor_runs_macos_diagnostics() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_cl4se"))
        .arg("doctor")
        .output()?;

    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("CL4SE doctor (macOS)"));
    assert!(stdout.contains("Input Monitoring:"));
    assert!(stdout.contains("Accessibility:"));
    assert!(stdout.contains("hidutil mapping:"));
    assert!(stdout.contains("Result:"));
    assert!(!stdout.contains("backend not implemented"));
    Ok(())
}

#[test]
#[cfg(target_os = "windows")]
fn doctor_runs_windows_diagnostics() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_cl4se"))
        .arg("doctor")
        .output()?;

    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("CL4SE doctor (Windows)"));
    assert!(stdout.contains("Windows hooks:"));
    assert!(stdout.contains("Result:"));
    assert!(!stdout.contains("backend not implemented"));
    Ok(())
}
