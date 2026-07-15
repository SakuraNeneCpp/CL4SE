use std::mem::size_of;

use anyhow::{bail, Result};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VK_CAPITAL, VK_CONTROL, VK_RETURN,
};

use crate::{
    core::CommitKey,
    platform::{windows::hooks::INJECTION_MARKER, KeyInjector},
};

const VK_M: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY =
    windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(b'M' as u16);

pub(crate) struct WindowsKeyInjector;

impl WindowsKeyInjector {
    fn send(inputs: &[INPUT], recovery_key_ups: &[INPUT]) -> Result<()> {
        // SAFETY: INPUT values are fully initialized keyboard events, the slice
        // remains valid for the call, and cbSize exactly matches INPUT.
        let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
        if sent != inputs.len() as u32 {
            let recovered = if sent == 0 {
                0
            } else {
                // SAFETY: Recovery contains only marked key-up events for keys
                // already allowed in this sequence. It prevents a partial
                // SendInput result from leaving Enter, M, Ctrl, or Caps held.
                unsafe { SendInput(recovery_key_ups, size_of::<INPUT>() as i32) }
            };
            bail!(
                "SendInput inserted {sent} of {} requested events; recovery inserted {recovered} of {} key-ups",
                inputs.len(),
                recovery_key_ups.len()
            );
        }
        Ok(())
    }
}

impl KeyInjector for WindowsKeyInjector {
    fn inject_commit_key(&mut self, key: CommitKey) -> Result<()> {
        match key {
            CommitKey::Enter => Self::send(&enter_inputs(), &enter_recovery()),
            CommitKey::CtrlM => Self::send(&ctrl_m_inputs(), &ctrl_m_recovery()),
        }
    }

    fn inject_capslock(&mut self) -> Result<()> {
        Self::send(&capslock_inputs(), &capslock_recovery())
    }
}

fn enter_inputs() -> [INPUT; 2] {
    [
        keyboard_input(VK_RETURN, KEYBD_EVENT_FLAGS(0)),
        keyboard_input(VK_RETURN, KEYEVENTF_KEYUP),
    ]
}

fn ctrl_m_inputs() -> [INPUT; 4] {
    [
        keyboard_input(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        keyboard_input(VK_M, KEYBD_EVENT_FLAGS(0)),
        keyboard_input(VK_M, KEYEVENTF_KEYUP),
        keyboard_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ]
}

fn capslock_inputs() -> [INPUT; 2] {
    [
        keyboard_input(VK_CAPITAL, KEYBD_EVENT_FLAGS(0)),
        keyboard_input(VK_CAPITAL, KEYEVENTF_KEYUP),
    ]
}

fn enter_recovery() -> [INPUT; 1] {
    [keyboard_input(VK_RETURN, KEYEVENTF_KEYUP)]
}

fn ctrl_m_recovery() -> [INPUT; 2] {
    [
        keyboard_input(VK_M, KEYEVENTF_KEYUP),
        keyboard_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ]
}

fn capslock_recovery() -> [INPUT; 1] {
    [keyboard_input(VK_CAPITAL, KEYEVENTF_KEYUP)]
}

fn keyboard_input(
    vk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    flags: KEYBD_EVENT_FLAGS,
) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTION_MARKER,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_generated_event_has_marker() {
        for input in enter_inputs()
            .into_iter()
            .chain(ctrl_m_inputs())
            .chain(capslock_inputs())
            .chain(enter_recovery())
            .chain(ctrl_m_recovery())
            .chain(capslock_recovery())
        {
            // SAFETY: keyboard_input initializes the active INPUT union member
            // as KEYBDINPUT because type is INPUT_KEYBOARD.
            assert_eq!(unsafe { input.Anonymous.ki.dwExtraInfo }, INJECTION_MARKER);
        }
    }

    #[test]
    fn ctrl_m_sequence_is_balanced_and_ordered() {
        let inputs = ctrl_m_inputs();
        let expected = [
            (VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
            (VK_M, KEYBD_EVENT_FLAGS(0)),
            (VK_M, KEYEVENTF_KEYUP),
            (VK_CONTROL, KEYEVENTF_KEYUP),
        ];

        for (input, (expected_key, expected_flags)) in inputs.into_iter().zip(expected) {
            // SAFETY: ctrl_m_inputs initializes every active union member as
            // KEYBDINPUT because each INPUT type is INPUT_KEYBOARD.
            let keyboard = unsafe { input.Anonymous.ki };
            assert_eq!(keyboard.wVk, expected_key);
            assert_eq!(keyboard.dwFlags, expected_flags);
        }
    }
}
