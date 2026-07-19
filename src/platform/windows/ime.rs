use std::mem::size_of;

use anyhow::{Context, Result};
use windows::{
    core::{BOOL, GUID},
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED,
        },
        UI::{
            Input::Ime::{
                ImmEnumInputContext, ImmGetContext, ImmGetConversionStatus, ImmGetDefaultIMEWnd,
                ImmGetOpenStatus, ImmReleaseContext, HIMC, IME_CMODE_NATIVE,
                IME_CMODE_NOCONVERSION, IME_CONVERSION_MODE,
            },
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

const IMC_GETCONVERSIONMODE: usize = 0x0001;
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
        let Some(input_windows) = foreground_input_windows() else {
            return ImeGuess::Unknown;
        };

        let context_guess = query_context_active_guess(
            &input_windows,
            query_window_context_guess,
            query_thread_context_guess,
        );
        let guess = if context_guess == ImeGuess::Unknown {
            query_active_guess(&input_windows, default_ime_window, query_ime_guess)
        } else {
            context_guess
        };

        // The worker may have waited for a foreign IME window to answer. A
        // changed foreground invalidates the result rather than risking an
        // action based on the previous application.
        if foreground_is_still(input_windows.foreground) {
            guess
        } else {
            ImeGuess::Unknown
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

fn query_ime_open_status(ime_window: HWND) -> Option<bool> {
    query_ime_control(ime_window, IMC_GETOPENSTATUS).map(|status| status != 0)
}

fn query_ime_conversion_mode(ime_window: HWND) -> Option<u32> {
    u32::try_from(query_ime_control(ime_window, IMC_GETCONVERSIONMODE)?).ok()
}

fn query_ime_control(ime_window: HWND, command: usize) -> Option<usize> {
    let mut result = 0usize;
    // SAFETY: ime_window is returned by ImmGetDefaultIMEWnd. The output
    // pointer remains valid for this bounded synchronous call. SMTO flags
    // and the 100 ms timeout prevent a hung target from blocking the worker.
    let delivered = unsafe {
        SendMessageTimeoutW(
            ime_window,
            WM_IME_CONTROL,
            WPARAM(command),
            LPARAM(0),
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            IME_QUERY_TIMEOUT_MS,
            Some(&mut result),
        )
    };
    (delivered.0 != 0).then_some(result)
}

fn query_ime_guess(ime_window: HWND) -> Option<ImeGuess> {
    let open = query_ime_open_status(ime_window)?;
    let conversion_mode = if open {
        query_ime_conversion_mode(ime_window)
    } else {
        None
    };
    guess_from_ime_status(open, conversion_mode)
}

fn guess_from_ime_status(open: bool, conversion_mode: Option<u32>) -> Option<ImeGuess> {
    if !open {
        return Some(ImeGuess::No);
    }

    let conversion_mode = conversion_mode?;
    let native = conversion_mode & IME_CMODE_NATIVE.0 != 0;
    let conversion_disabled = conversion_mode & IME_CMODE_NOCONVERSION.0 != 0;
    Some(if native && !conversion_disabled {
        ImeGuess::Yes
    } else {
        ImeGuess::No
    })
}

fn query_window_context_guess(input_window: HWND) -> Option<ImeGuess> {
    // SAFETY: input_window is a non-null HWND returned by Windows. Every
    // non-null input context acquired here is released before returning.
    let context = unsafe { ImmGetContext(input_window) };
    if context.0.is_null() {
        return None;
    }

    let guess = query_input_context_guess(context);
    // SAFETY: This balances the successful ImmGetContext call above for the
    // same window and context handle.
    if !unsafe { ImmReleaseContext(input_window, context) }.as_bool() {
        log::debug!("failed to release foreground IME input context");
    }
    guess
}

fn query_input_context_guess(context: HIMC) -> Option<ImeGuess> {
    // SAFETY: context is supplied by ImmGetContext or ImmEnumInputContext and
    // remains valid for the duration of this synchronous query.
    let open = unsafe { ImmGetOpenStatus(context) }.as_bool();
    if !open {
        return Some(ImeGuess::No);
    }

    let mut conversion_mode = IME_CONVERSION_MODE(0);
    // SAFETY: context is valid as described above and conversion_mode points
    // to writable storage. Sentence mode is not needed.
    let available =
        unsafe { ImmGetConversionStatus(context, Some(&mut conversion_mode), None).as_bool() };
    available
        .then_some(conversion_mode.0)
        .and_then(|mode| guess_from_ime_status(true, Some(mode)))
}

#[derive(Default)]
struct ContextGuessAccumulator {
    guess: Option<ImeGuess>,
    conflict: bool,
}

impl ContextGuessAccumulator {
    fn observe(&mut self, guess: ImeGuess) {
        if self.guess.is_some_and(|current| current != guess) {
            self.conflict = true;
        } else {
            self.guess = Some(guess);
        }
    }

    fn result(self) -> Option<ImeGuess> {
        if self.conflict {
            None
        } else {
            self.guess
        }
    }
}

// SAFETY: Windows invokes this callback synchronously from
// ImmEnumInputContext. lparam points to the live accumulator passed by
// query_thread_context_guess, and the callback never stores that pointer.
unsafe extern "system" fn collect_context_guess(context: HIMC, lparam: LPARAM) -> BOOL {
    // SAFETY: query_thread_context_guess passes a valid exclusive pointer to
    // ContextGuessAccumulator for the complete enumeration call.
    let accumulator = unsafe { &mut *(lparam.0 as *mut ContextGuessAccumulator) };
    if let Some(guess) = query_input_context_guess(context) {
        accumulator.observe(guess);
    }
    BOOL::from(true)
}

fn query_thread_context_guess(thread_id: u32) -> Option<ImeGuess> {
    let mut accumulator = ContextGuessAccumulator::default();
    // SAFETY: collect_context_guess has the required ABI and does not unwind;
    // lparam points to accumulator, which remains live and exclusively owned
    // for this synchronous call. Windows documents cross-process thread IDs.
    let enumerated = unsafe {
        ImmEnumInputContext(
            thread_id,
            Some(collect_context_guess),
            LPARAM((&raw mut accumulator) as isize),
        )
    }
    .as_bool();
    if enumerated {
        accumulator.result()
    } else {
        None
    }
}

fn query_context_active_guess(
    input_windows: &ForegroundInputWindows,
    mut window_guess_for: impl FnMut(HWND) -> Option<ImeGuess>,
    mut thread_guess_for: impl FnMut(u32) -> Option<ImeGuess>,
) -> ImeGuess {
    for input_window in input_windows.candidates().into_iter().flatten() {
        if let Some(guess) = window_guess_for(input_window) {
            return guess;
        }
    }

    thread_guess_for(input_windows.thread_id).unwrap_or(ImeGuess::Unknown)
}

fn query_active_guess(
    input_windows: &ForegroundInputWindows,
    mut ime_window_for: impl FnMut(HWND) -> Option<HWND>,
    mut guess_for: impl FnMut(HWND) -> Option<ImeGuess>,
) -> ImeGuess {
    for input_window in input_windows.candidates().into_iter().flatten() {
        let Some(ime_window) = ime_window_for(input_window) else {
            continue;
        };
        let Some(guess) = guess_for(ime_window) else {
            continue;
        };
        return guess;
    }

    ImeGuess::Unknown
}

#[derive(Debug, Clone, Copy)]
struct ForegroundInputWindows {
    foreground: HWND,
    focused: Option<HWND>,
    thread_id: u32,
}

impl ForegroundInputWindows {
    fn candidates(self) -> [Option<HWND>; 2] {
        let focused = self.focused.filter(|focused| *focused != self.foreground);
        [focused, Some(self.foreground)]
    }
}

fn default_ime_window(input_window: HWND) -> Option<HWND> {
    // SAFETY: input_window is a non-null HWND returned by Windows. The result
    // is a borrowed IME window handle that CL4SE only checks and queries.
    let ime_window = unsafe { ImmGetDefaultIMEWnd(input_window) };
    (!ime_window.0.is_null()).then_some(ime_window)
}

fn foreground_is_still(expected: HWND) -> bool {
    // SAFETY: GetForegroundWindow has no preconditions and returns a borrowed
    // handle managed by Windows.
    unsafe { GetForegroundWindow() == expected }
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
    let input_windows = foreground_input_windows()?;
    let ime_window = input_windows
        .candidates()
        .into_iter()
        .flatten()
        .find_map(default_ime_window)?;

    foreground_is_still(input_windows.foreground).then_some(ime_window)
}

fn foreground_input_windows() -> Option<ForegroundInputWindows> {
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
    if !foreground_is_still(foreground) {
        return None;
    }

    Some(ForegroundInputWindows {
        foreground,
        focused,
        thread_id: foreground_thread,
    })
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

    #[test]
    fn closed_ime_on_foreground_fallback_is_known_no() {
        let foreground = test_hwnd(1);
        let focused = test_hwnd(2);
        let foreground_ime = test_hwnd(3);
        let input_windows = ForegroundInputWindows {
            foreground,
            focused: Some(focused),
            thread_id: 10,
        };

        let guess = query_active_guess(
            &input_windows,
            |window| (window == foreground).then_some(foreground_ime),
            |window| (window == foreground_ime).then_some(ImeGuess::No),
        );

        assert_eq!(guess, ImeGuess::No);
    }

    #[test]
    fn failed_focused_ime_query_falls_back_to_foreground() {
        let foreground = test_hwnd(1);
        let focused = test_hwnd(2);
        let focused_ime = test_hwnd(3);
        let foreground_ime = test_hwnd(4);
        let input_windows = ForegroundInputWindows {
            foreground,
            focused: Some(focused),
            thread_id: 10,
        };

        let guess = query_active_guess(
            &input_windows,
            |window| {
                if window == focused {
                    Some(focused_ime)
                } else if window == foreground {
                    Some(foreground_ime)
                } else {
                    None
                }
            },
            |window| (window == foreground_ime).then_some(ImeGuess::Yes),
        );

        assert_eq!(guess, ImeGuess::Yes);
    }

    #[test]
    fn unavailable_ime_state_remains_unknown() {
        let input_windows = ForegroundInputWindows {
            foreground: test_hwnd(1),
            focused: Some(test_hwnd(2)),
            thread_id: 10,
        };

        assert_eq!(
            query_active_guess(&input_windows, |_| None, |_| Some(ImeGuess::No)),
            ImeGuess::Unknown
        );
    }

    #[test]
    fn open_alphanumeric_mode_shown_as_taskbar_a_is_known_no() {
        assert_eq!(guess_from_ime_status(true, Some(0)), Some(ImeGuess::No));
    }

    #[test]
    fn open_native_mode_is_known_yes() {
        assert_eq!(
            guess_from_ime_status(true, Some(IME_CMODE_NATIVE.0)),
            Some(ImeGuess::Yes)
        );
    }

    #[test]
    fn closed_ime_does_not_require_conversion_mode() {
        assert_eq!(guess_from_ime_status(false, None), Some(ImeGuess::No));
    }

    #[test]
    fn missing_conversion_mode_for_open_ime_remains_unknown() {
        assert_eq!(guess_from_ime_status(true, None), None);
    }

    #[test]
    fn no_conversion_mode_is_not_treated_as_native_input() {
        assert_eq!(
            guess_from_ime_status(true, Some(IME_CMODE_NATIVE.0 | IME_CMODE_NOCONVERSION.0)),
            Some(ImeGuess::No)
        );
    }

    #[test]
    fn focused_input_context_precedes_stale_ime_window_fallback() {
        let focused = test_hwnd(2);
        let input_windows = ForegroundInputWindows {
            foreground: test_hwnd(1),
            focused: Some(focused),
            thread_id: 10,
        };

        assert_eq!(
            query_context_active_guess(
                &input_windows,
                |window| (window == focused).then_some(ImeGuess::No),
                |_| Some(ImeGuess::Yes),
            ),
            ImeGuess::No
        );
    }

    #[test]
    fn conflicting_thread_contexts_remain_unknown() {
        let mut accumulator = ContextGuessAccumulator::default();
        accumulator.observe(ImeGuess::No);
        accumulator.observe(ImeGuess::Yes);

        assert_eq!(accumulator.result(), None);
    }
}
