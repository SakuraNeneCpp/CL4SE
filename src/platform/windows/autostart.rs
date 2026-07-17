use std::{env, ffi::OsStr, os::windows::ffi::OsStrExt, path::Path};

use anyhow::{Context, Result};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::ERROR_FILE_NOT_FOUND,
        System::Registry::{
            RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY,
            HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
        },
    },
};

use crate::platform::Autostart;

const RUN_KEY: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: windows::core::PCWSTR = w!("CL4SE");

pub(crate) struct WindowsAutostart;

impl Autostart for WindowsAutostart {
    fn install(&self) -> Result<()> {
        let executable = env::current_exe().context("failed to locate cl4se executable")?;
        let command = autostart_command(&executable);
        let utf16: Vec<u16> = OsStr::new(&command).encode_wide().chain(Some(0)).collect();
        let bytes: Vec<u8> = utf16.iter().flat_map(|word| word.to_le_bytes()).collect();

        let mut raw_key = HKEY::default();
        // SAFETY: All pointers are produced by windows-rs static wide strings or
        // point to valid output storage. No security descriptor is supplied.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut raw_key,
                None,
            )
        };
        status.ok().context("failed to open HKCU Run key")?;
        let key = RegistryKey(raw_key);

        // SAFETY: key is an open HKCU Run handle and bytes contains a
        // null-terminated UTF-16 REG_SZ value for the duration of this call.
        unsafe { RegSetValueExW(key.0, VALUE_NAME, None, REG_SZ, Some(&bytes)) }
            .ok()
            .context("failed to set HKCU Run value CL4SE")
    }

    fn uninstall(&self) -> Result<()> {
        let mut raw_key = HKEY::default();
        // SAFETY: RUN_KEY is a valid static wide string and raw_key is writable.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                None,
                KEY_SET_VALUE,
                &mut raw_key,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        status.ok().context("failed to open HKCU Run key")?;
        let key = RegistryKey(raw_key);

        // SAFETY: key is an open HKCU Run handle and VALUE_NAME is static.
        let status = unsafe { RegDeleteValueW(key.0, VALUE_NAME) };
        if status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            status.ok().context("failed to delete HKCU Run value CL4SE")
        }
    }
}

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        // SAFETY: RegistryKey owns a successfully opened registry handle and
        // closes it exactly once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn autostart_command(executable: &Path) -> String {
    format!("\"{}\" start", executable.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autostart_command_uses_background_start_and_quotes_executable_path() {
        assert_eq!(
            autostart_command(Path::new(r"C:\Program Files\CL4SE\cl4se.exe")),
            r#""C:\Program Files\CL4SE\cl4se.exe" start"#
        );
    }
}
