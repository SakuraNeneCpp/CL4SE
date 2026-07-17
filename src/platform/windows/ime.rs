use std::mem::size_of;

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
                GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId,
                SendMessageTimeoutW, GUITHREADINFO, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_IME_CONTROL,
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
    let input_window = foreground_input_window()?;
    // SAFETY: input_window is a non-null HWND returned by Windows. The result
    // is a borrowed IME window handle that CL4SE only checks and queries.
    let ime_window = unsafe { ImmGetDefaultIMEWnd(input_window) };
    (!ime_window.0.is_null()).then_some(ime_window)
}

fn foreground_input_window() -> Option<HWND> {
    // SAFETY: GetForegroundWindow has no preconditions and returns a borrowed
    // handle managed by Windows.
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.0.is_null() {
        return None;
    }

    // SAFETY: foreground is a valid borrowed HWND. No process-id output is
    // requested, so Windows only returns the owning GUI thread identifier.
    let foreground_thread = unsafe { GetWindowThreadProcessId(foreground, None) };
    let mut gui = GUITHREADINFO {
        cbSize: size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let focused = if foreground_thread == 0 {
        None
    } else {
        // SAFETY: gui points to writable storage with cbSize initialized as
        // required. foreground_thread came from the foreground HWND above.
        unsafe { GetGUIThreadInfo(foreground_thread, &mut gui) }
            .ok()
            .and_then(|()| focus_window_for_foreground(foreground, &gui))
    };

    // A focus switch during resolution could pair an old HWND with a new IME
    // state. Treat that race as uncertainty instead of risking a false commit.
    // SAFETY: GetForegroundWindow has no preconditions.
    let foreground_after_resolution = unsafe { GetForegroundWindow() };
    if foreground_after_resolution != foreground {
        return None;
    }

    Some(focused.unwrap_or(foreground))
}

fn focus_window_for_foreground(foreground: HWND, gui: &GUITHREADINFO) -> Option<HWND> {
    (gui.hwndActive == foreground && !gui.hwndFocus.0.is_null()).then_some(gui.hwndFocus)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hwnd(value: usize) -> HWND {
        HWND(std::ptr::without_provenance_mut(value))
    }

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

    #[test]
    fn focused_child_window_is_preferred_for_the_active_foreground() {
        let foreground = test_hwnd(1);
        let focus = test_hwnd(2);
        let gui = GUITHREADINFO {
            hwndActive: foreground,
            hwndFocus: focus,
            ..Default::default()
        };

        assert_eq!(focus_window_for_foreground(foreground, &gui), Some(focus));
    }

    #[test]
    fn missing_or_stale_focus_falls_back_to_the_foreground_window() {
        let foreground = test_hwnd(1);
        let stale = GUITHREADINFO {
            hwndActive: test_hwnd(2),
            hwndFocus: test_hwnd(3),
            ..Default::default()
        };
        let missing = GUITHREADINFO {
            hwndActive: foreground,
            hwndFocus: HWND::default(),
            ..Default::default()
        };

        assert_eq!(focus_window_for_foreground(foreground, &stale), None);
        assert_eq!(focus_window_for_foreground(foreground, &missing), None);
    }
}
