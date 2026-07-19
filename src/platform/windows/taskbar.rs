use windows::{
    core::w,
    Win32::{
        System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
        UI::{
            Accessibility::{
                CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Descendants,
            },
            WindowsAndMessaging::FindWindowW,
        },
    },
};

use crate::core::ImeGuess;

const SYSTEM_TRAY_ICON_AUTOMATION_ID: &str = "SystemTrayIcon";
const MAX_TASKBAR_ELEMENTS: i32 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskbarModeProbe {
    Known(ImeGuess),
    Unavailable,
    Ambiguous,
}

pub(crate) struct TaskbarModeProvider {
    automation: Option<IUIAutomation>,
    cached_indicator: Option<IUIAutomationElement>,
}

impl TaskbarModeProvider {
    pub(crate) fn new() -> Self {
        // SAFETY: The Windows worker/doctor thread has initialized COM. The
        // system UI Automation class returns an owned smart interface pointer.
        let automation = unsafe {
            CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
        }
        .map_err(|error| {
            log::warn!("UI Automation unavailable; using bounded IMM fallback: {error}");
            error
        })
        .ok();

        Self {
            automation,
            cached_indicator: None,
        }
    }

    pub(crate) fn probe(&mut self) -> TaskbarModeProbe {
        if let Some(indicator) = self.cached_indicator.as_ref() {
            match probe_element(indicator) {
                ElementProbe::Known(guess) => return TaskbarModeProbe::Known(guess),
                ElementProbe::Ambiguous => return TaskbarModeProbe::Ambiguous,
                ElementProbe::NotIndicator => self.cached_indicator = None,
            }
        }

        self.scan_taskbar()
    }

    fn scan_taskbar(&mut self) -> TaskbarModeProbe {
        let Some(automation) = self.automation.as_ref() else {
            return TaskbarModeProbe::Unavailable;
        };

        // SAFETY: The class name is a static nul-terminated UTF-16 string.
        // FindWindowW returns a borrowed handle owned by Explorer.
        let Ok(taskbar_window) = (unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }) else {
            return TaskbarModeProbe::Unavailable;
        };
        // SAFETY: taskbar_window is a live borrowed Explorer window. UIA owns
        // the returned element interface independently of the HWND lifetime.
        let Ok(taskbar) = (unsafe { automation.ElementFromHandle(taskbar_window) }) else {
            return TaskbarModeProbe::Unavailable;
        };
        // SAFETY: automation is a valid COM interface. The condition is owned
        // by its returned smart pointer.
        let Ok(condition) = (unsafe { automation.CreateTrueCondition() }) else {
            return TaskbarModeProbe::Unavailable;
        };
        // SAFETY: The taskbar element and condition are valid UIA interfaces.
        // This runs only on the worker, never inside a keyboard hook callback.
        let Ok(elements) = (unsafe { taskbar.FindAll(TreeScope_Descendants, &condition) }) else {
            return TaskbarModeProbe::Unavailable;
        };
        // SAFETY: elements is a valid UIA array interface.
        let Ok(length) = (unsafe { elements.Length() }) else {
            return TaskbarModeProbe::Unavailable;
        };
        if length > MAX_TASKBAR_ELEMENTS {
            log::debug!("taskbar UIA tree exceeded bounded scan limit: {length}");
            return TaskbarModeProbe::Ambiguous;
        }

        let mut match_found: Option<(ImeGuess, IUIAutomationElement)> = None;
        for index in 0..length {
            // SAFETY: index is within the array length obtained immediately
            // above. The returned element is an owned COM smart pointer.
            let Ok(element) = (unsafe { elements.GetElement(index) }) else {
                continue;
            };
            match probe_element(&element) {
                ElementProbe::NotIndicator => {}
                ElementProbe::Ambiguous => return TaskbarModeProbe::Ambiguous,
                ElementProbe::Known(guess) => {
                    if match_found.is_some() {
                        // More than one plausible tray element is not enough
                        // evidence to authorize a key injection.
                        return TaskbarModeProbe::Ambiguous;
                    }
                    match_found = Some((guess, element));
                }
            }
        }

        let Some((guess, indicator)) = match_found else {
            return TaskbarModeProbe::Unavailable;
        };
        self.cached_indicator = Some(indicator);
        TaskbarModeProbe::Known(guess)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementProbe {
    Known(ImeGuess),
    NotIndicator,
    Ambiguous,
}

fn probe_element(element: &IUIAutomationElement) -> ElementProbe {
    // SAFETY: element is a valid UIA interface. Property BSTR values are owned
    // by windows-rs and released after conversion.
    let Ok(automation_id) = (unsafe { element.CurrentAutomationId() }) else {
        return ElementProbe::NotIndicator;
    };
    if !automation_id
        .to_string()
        .eq_ignore_ascii_case(SYSTEM_TRAY_ICON_AUTOMATION_ID)
    {
        return ElementProbe::NotIndicator;
    }

    let mut guess = None;
    for value in [
        // SAFETY: See the UIA property-read safety argument above.
        unsafe { element.CurrentName() }.ok(),
        // SAFETY: See the UIA property-read safety argument above.
        unsafe { element.CurrentItemStatus() }.ok(),
        // SAFETY: See the UIA property-read safety argument above.
        unsafe { element.CurrentHelpText() }.ok(),
    ]
    .into_iter()
    .flatten()
    {
        let Some(observed) = parse_mode_text(&value.to_string()) else {
            continue;
        };
        if guess.is_some_and(|current| current != observed) {
            return ElementProbe::Ambiguous;
        }
        guess = Some(observed);
    }

    guess.map_or(ElementProbe::NotIndicator, ElementProbe::Known)
}

fn parse_mode_text(text: &str) -> Option<ImeGuess> {
    let normalized = text.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }

    // Exact glyphs cover the visible Windows/Mozc tray icons. The UIA element
    // must also have the system-tray automation id and be the sole plausible
    // match, so an arbitrary application label cannot authorize injection.
    match normalized.as_str() {
        "a" | "_a" => return Some(ImeGuess::No),
        "あ" | "ア" | "ｱ" | "_ｱ" | "ａ" | "Ａ" => return Some(ImeGuess::Yes),
        _ => {}
    }

    let native_markers = [
        "hiragana",
        "katakana",
        "full-width alphanumeric",
        "full width alphanumeric",
        "fullwidth alphanumeric",
        "ひらがな",
        "カタカナ",
        "全角英数",
        "全角英数字",
    ];
    let alphanumeric_markers = [
        "half-width alphanumeric",
        "half width alphanumeric",
        "halfwidth alphanumeric",
        "direct input",
        "ime off",
        "ime is off",
        "半角英数",
        "半角英数字",
        "直接入力",
    ];
    let native = native_markers
        .iter()
        .any(|marker| normalized.contains(marker));
    let alphanumeric = alphanumeric_markers
        .iter()
        .any(|marker| normalized.contains(marker));

    match (native, alphanumeric) {
        (true, false) => Some(ImeGuess::Yes),
        (false, true) => Some(ImeGuess::No),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_taskbar_a_is_known_no() {
        for label in [
            "A",
            "_A",
            "Half-width Alphanumeric",
            "Halfwidth Alphanumeric / Direct Input",
            "半角英数",
            "半角英数字 / 直接入力",
        ] {
            assert_eq!(parse_mode_text(label), Some(ImeGuess::No), "{label}");
        }
    }

    #[test]
    fn native_taskbar_modes_are_known_yes() {
        for label in [
            "あ",
            "ア",
            "ｱ",
            "Ａ",
            "Hiragana",
            "Full-width Katakana",
            "全角カタカナ",
            "全角英数",
        ] {
            assert_eq!(parse_mode_text(label), Some(ImeGuess::Yes), "{label}");
        }
    }

    #[test]
    fn unrelated_or_self_conflicting_labels_are_not_guessed() {
        for label in [
            "",
            "Input Indicator",
            "Japanese Microsoft IME",
            "Hiragana / Half-width Alphanumeric",
        ] {
            assert_eq!(parse_mode_text(label), None, "{label}");
        }
    }
}
