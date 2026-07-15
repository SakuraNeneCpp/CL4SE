use std::{error::Error, process::Command};

#[test]
fn version_is_available() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_clime"))
        .arg("--version")
        .output()?;

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout)?.trim(), "clime 0.1.0");
    Ok(())
}

#[test]
#[cfg(not(target_os = "windows"))]
fn doctor_reports_the_stub_as_unimplemented() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_clime"))
        .arg("doctor")
        .output()?;

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("未実装"));
    assert!(stdout.contains("backend not implemented"));
    Ok(())
}

#[test]
#[cfg(target_os = "windows")]
fn doctor_runs_windows_diagnostics() -> Result<(), Box<dyn Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_clime"))
        .arg("doctor")
        .output()?;

    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("Windows hooks:"));
    assert!(!stdout.contains("backend not implemented"));
    Ok(())
}
