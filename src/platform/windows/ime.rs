use anyhow::{Context, Result};
use windows::{
    core::GUID,
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        },
        UI::{
            Input::Ime::ImmGetDefaultIMEWnd,
            TextServices::{
                CLSID_TF_InputProcessorProfiles, ITfInputProcessorProfileMgr,
                GUID_TFCAT_TIP_KEYBOARD, TF_INPUTPROCESSORPROFILE, TF_PROFILETYPE_INPUTPROCESSOR,
            },
            WindowsAndMessaging::{
                GetForegroundWindow, SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK,
                WM_IME_CONTROL,
            },
        },
    },
};

use crate::{
    core::{ImeGuess, ImeSnapshot},
    platform::ImeStateProvider,
};

const IMC_GETOPENSTATUS: usize = 0x0005;
const IME_QUERY_TIMEOUT_MS: u32 = 100;

// Microsoft Learn Windows 11 input method identifier:
// 0411:{03B5835F-F03C-411B-9CE2-AA23E1171E36}{A76C93D9-5523-4E90-AAFA-4DB112F9AC76}
// https://learn.microsoft.com/windows-hardware/manufacture/desktop/windows-language-pack-default-values
const MS_IME_CLSID: GUID = GUID::from_u128(0x03b5835f_f03c_411b_9ce2_aa23e1171e36);
const MS_IME_PROFILE: GUID = GUID::from_u128(0xa76c93d9_5523_4e90_aafa_4db112f9ac76);

// google/mozc src/win32/base/tsf_profile.cc under GOOGLE_JAPANESE_INPUT_BUILD.
// https://github.com/google/mozc/blob/master/src/win32/base/tsf_profile.cc
const GOOGLE_JAPANESE_INPUT_CLSID: GUID = GUID::from_u128(0xd5a86fd5_5308_47ea_ad16_9c4eb160ec3c);
const GOOGLE_JAPANESE_INPUT_PROFILE: GUID = GUID::from_u128(0x773eb24e_ca1d_4b1b_b420_fa985bb0b80d);

pub(crate) struct ComApartment;

impl ComApartment {
    pub(crate) fn initialize() -> Result<Self> {
        // SAFETY: The caller initializes COM once on its current worker/doctor
        // thread and ComApartment::drop balances every successful result,
        // including S_FALSE.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .context("failed to initialize COM apartment")?;
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: This balances the successful CoInitializeEx performed by this
        // guard on the same thread.
        unsafe { CoUninitialize() };
    }
}

pub(crate) struct WindowsImeStateProvider {
    profile_manager: Option<ITfInputProcessorProfileMgr>,
}

impl WindowsImeStateProvider {
    pub(crate) fn new() -> Self {
        // SAFETY: COM is initialized by ComApartment on this thread. The CLSID
        // and requested interface are defined by TSF and the returned smart
        // pointer owns its COM reference.
        let profile_manager = unsafe {
            CoCreateInstance::<_, ITfInputProcessorProfileMgr>(
                &CLSID_TF_InputProcessorProfiles,
                None,
                CLSCTX_INPROC_SERVER,
            )
        }
        .map_err(|error| {
            log::warn!("TSF profile manager unavailable; ime_id will be None: {error}");
            error
        })
        .ok();

        Self { profile_manager }
    }

    pub(crate) fn has_foreground_ime_window() -> bool {
        foreground_ime_window().is_some()
    }

    fn active_guess() -> ImeGuess {
        let Some(ime_window) = foreground_ime_window() else {
            return ImeGuess::Unknown;
        };

        let mut open_status = 0usize;
        // SAFETY: ime_window is returned by ImmGetDefaultIMEWnd. The output
        // pointer remains valid for this bounded synchronous call. SMTO flags
        // and the 100 ms timeout prevent a hung target from blocking the worker.
        let delivered = unsafe {
            SendMessageTimeoutW(
                ime_window,
                WM_IME_CONTROL,
                WPARAM(IMC_GETOPENSTATUS),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                IME_QUERY_TIMEOUT_MS,
                Some(&mut open_status),
            )
        };
        if delivered.0 == 0 {
            return ImeGuess::Unknown;
        }

        if open_status == 0 {
            ImeGuess::No
        } else {
            ImeGuess::Yes
        }
    }

    fn active_ime_id(&self) -> Option<String> {
        let manager = self.profile_manager.as_ref()?;
        let mut profile = TF_INPUTPROCESSORPROFILE::default();
        // SAFETY: manager is a valid COM interface on its initializing thread;
        // profile points to writable storage and the keyboard category is the
        // only category accepted by GetActiveProfile.
        unsafe { manager.GetActiveProfile(&GUID_TFCAT_TIP_KEYBOARD, &mut profile) }.ok()?;

        recognized_ime_id(&profile).map(str::to_owned)
    }
}

fn recognized_ime_id(profile: &TF_INPUTPROCESSORPROFILE) -> Option<&'static str> {
    if profile.dwProfileType != TF_PROFILETYPE_INPUTPROCESSOR {
        return None;
    }
    if profile.clsid == MS_IME_CLSID && profile.guidProfile == MS_IME_PROFILE {
        Some("ms-ime")
    } else if profile.clsid == GOOGLE_JAPANESE_INPUT_CLSID
        && profile.guidProfile == GOOGLE_JAPANESE_INPUT_PROFILE
    {
        Some("google-japanese-input")
    } else {
        None
    }
}

impl ImeStateProvider for WindowsImeStateProvider {
    fn snapshot(&mut self) -> ImeSnapshot {
        ImeSnapshot {
            active: Self::active_guess(),
            ime_id: self.active_ime_id(),
        }
    }
}

fn foreground_ime_window() -> Option<HWND> {
    // SAFETY: Both functions return borrowed HWND values managed by Windows;
    // CLIME only checks and sends a bounded message to them.
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0.is_null() {
        return None;
    }
    // SAFETY: foreground is a non-null HWND returned by GetForegroundWindow.
    let ime_window = unsafe { ImmGetDefaultIMEWnd(foreground) };
    (!ime_window.0.is_null()).then_some(ime_window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_verified_profiles_are_recognized() {
        let mut profile = TF_INPUTPROCESSORPROFILE {
            dwProfileType: TF_PROFILETYPE_INPUTPROCESSOR,
            clsid: MS_IME_CLSID,
            guidProfile: MS_IME_PROFILE,
            ..Default::default()
        };
        assert_eq!(recognized_ime_id(&profile), Some("ms-ime"));

        profile.clsid = GOOGLE_JAPANESE_INPUT_CLSID;
        profile.guidProfile = GOOGLE_JAPANESE_INPUT_PROFILE;
        assert_eq!(recognized_ime_id(&profile), Some("google-japanese-input"));

        profile.guidProfile = GUID::zeroed();
        assert_eq!(recognized_ime_id(&profile), None);

        profile.dwProfileType = 0;
        assert_eq!(recognized_ime_id(&profile), None);
    }
}
