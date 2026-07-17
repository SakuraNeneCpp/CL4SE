use std::process::Command;

use anyhow::{bail, Context, Result};

const HIDUTIL: &str = "/usr/bin/hidutil";
const CL4SE_MAPPING: &str = r#"{"UserKeyMapping":[{"HIDKeyboardModifierMappingSrc":0x700000039,"HIDKeyboardModifierMappingDst":0x70000006D}]}"#;
const EMPTY_MAPPING: &str = r#"{"UserKeyMapping":[]}"#;
const CAPS_LOCK_USAGE_HEX: &str = "0x700000039";
const F18_USAGE_HEX: &str = "0x70000006d";
const CAPS_LOCK_USAGE_DECIMAL: &str = "30064771129";
const F18_USAGE_DECIMAL: &str = "30064771181";

pub(crate) struct HidutilRemapGuard {
    active: bool,
}

impl HidutilRemapGuard {
    pub(crate) fn install() -> Result<Self> {
        // Mark the guard active before invoking hidutil. If the child reports a
        // failure after partially applying the property, Drop still attempts
        // the fail-safe empty mapping restoration.
        let guard = Self { active: true };
        set_mapping(CL4SE_MAPPING).context("failed to map Caps Lock to F18 with hidutil")?;
        Ok(guard)
    }

    pub(crate) fn restore(mut self) -> Result<()> {
        self.restore_inner()
    }

    fn restore_inner(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        set_mapping(EMPTY_MAPPING).context("failed to restore hidutil UserKeyMapping")?;
        self.active = false;
        Ok(())
    }
}

impl Drop for HidutilRemapGuard {
    fn drop(&mut self) {
        if let Err(error) = self.restore_inner() {
            log::warn!("hidutil cleanup failed; run `cl4se doctor`: {error:#}");
        }
    }
}

pub(crate) fn cl4se_mapping_is_active() -> Result<bool> {
    let output = Command::new(HIDUTIL)
        .args(["property", "--get", "UserKeyMapping"])
        .output()
        .context("failed to execute hidutil property --get")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "hidutil property --get failed with {}: {}",
            output.status,
            stderr.trim()
        );
    }

    Ok(output_contains_cl4se_mapping(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

pub(crate) fn restore_residual_mapping() -> Result<()> {
    set_mapping(EMPTY_MAPPING)
}

fn set_mapping(mapping: &str) -> Result<()> {
    let output = Command::new(HIDUTIL)
        .args(["property", "--set", mapping])
        .output()
        .context("failed to execute hidutil property --set")?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!(
        "hidutil property --set failed with {}: {}",
        output.status,
        stderr.trim()
    )
}

fn output_contains_cl4se_mapping(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    let has_source = lower.contains("hidkeyboardmodifiermappingsrc")
        && (lower.contains(CAPS_LOCK_USAGE_HEX) || lower.contains(CAPS_LOCK_USAGE_DECIMAL));
    let has_destination = lower.contains("hidkeyboardmodifiermappingdst")
        && (lower.contains(F18_USAGE_HEX) || lower.contains(F18_USAGE_DECIMAL));
    has_source && has_destination
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_cl4se_mapping_in_hex_or_decimal_output() {
        let hex = r#"HIDKeyboardModifierMappingSrc = 0x700000039; HIDKeyboardModifierMappingDst = 0x70000006D;"#;
        let decimal = r#"HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771181;"#;

        assert!(output_contains_cl4se_mapping(hex));
        assert!(output_contains_cl4se_mapping(decimal));
    }

    #[test]
    fn ignores_unrelated_user_mapping() {
        let output = r#"HIDKeyboardModifierMappingSrc = 0x700000039; HIDKeyboardModifierMappingDst = 0x700000029;"#;
        assert!(!output_contains_cl4se_mapping(output));
    }
}
