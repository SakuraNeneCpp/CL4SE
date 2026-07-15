use anyhow::{anyhow, Result};
use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation, EventField, KeyCode},
    event_source::{CGEventSource, CGEventSourceStateID},
};

use crate::{
    core::CommitKey,
    platform::{macos::modifier_lock::ModifierLock, KeyInjector},
};

pub(crate) const INJECTION_MARKER: i64 = 0x434c_494d;

pub(crate) struct MacOsKeyInjector {
    source: CGEventSource,
    modifier_lock: Option<ModifierLock>,
}

impl MacOsKeyInjector {
    pub(crate) fn new() -> Result<Self> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|()| anyhow!("failed to create CGEventSource"))?;
        let modifier_lock = ModifierLock::open()
            .map_err(|error| {
                log::warn!(
                    "macOS Shift+CapsLock pass-through is unavailable; Caps Lock remains suppressed: {error:#}"
                );
                error
            })
            .ok();

        Ok(Self {
            source,
            modifier_lock,
        })
    }

    fn post_sequence(&self, sequence: &[KeyStroke]) -> Result<()> {
        // Construct every event before posting any of them. A construction
        // failure therefore cannot leave Ctrl, M, or Enter logically held.
        let events = sequence
            .iter()
            .map(|stroke| self.create_event(*stroke))
            .collect::<Result<Vec<_>>>()?;
        for event in &events {
            event.post(CGEventTapLocation::HID);
        }
        Ok(())
    }

    fn create_event(&self, stroke: KeyStroke) -> Result<CGEvent> {
        let event =
            CGEvent::new_keyboard_event(self.source.clone(), stroke.keycode, stroke.key_down)
                .map_err(|()| anyhow!("failed to create marked CG keyboard event"))?;
        event.set_flags(stroke.flags);
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, INJECTION_MARKER);
        Ok(event)
    }
}

impl KeyInjector for MacOsKeyInjector {
    fn inject_commit_key(&mut self, key: CommitKey) -> Result<()> {
        match key {
            CommitKey::Enter => self.post_sequence(&enter_sequence()),
            CommitKey::CtrlM => self.post_sequence(&ctrl_m_sequence()),
        }
    }

    fn inject_capslock(&mut self) -> Result<()> {
        let Some(modifier_lock) = self.modifier_lock.as_ref() else {
            return Ok(());
        };
        if let Err(error) = modifier_lock.toggle_caps_lock() {
            log::warn!("macOS Shift+CapsLock pass-through failed and is now disabled: {error:#}");
            self.modifier_lock = None;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyStroke {
    keycode: u16,
    key_down: bool,
    flags: CGEventFlags,
}

fn enter_sequence() -> [KeyStroke; 2] {
    [
        KeyStroke {
            keycode: KeyCode::RETURN,
            key_down: true,
            flags: CGEventFlags::empty(),
        },
        KeyStroke {
            keycode: KeyCode::RETURN,
            key_down: false,
            flags: CGEventFlags::empty(),
        },
    ]
}

fn ctrl_m_sequence() -> [KeyStroke; 4] {
    [
        KeyStroke {
            keycode: KeyCode::CONTROL,
            key_down: true,
            flags: CGEventFlags::CGEventFlagControl,
        },
        KeyStroke {
            keycode: KeyCode::ANSI_M,
            key_down: true,
            flags: CGEventFlags::CGEventFlagControl,
        },
        KeyStroke {
            keycode: KeyCode::ANSI_M,
            key_down: false,
            flags: CGEventFlags::CGEventFlagControl,
        },
        KeyStroke {
            keycode: KeyCode::CONTROL,
            key_down: false,
            flags: CGEventFlags::empty(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_sequence_is_balanced() {
        let sequence = enter_sequence();
        assert_eq!(sequence.len(), 2);
        assert_eq!(sequence[0].keycode, KeyCode::RETURN);
        assert!(sequence[0].key_down);
        assert!(!sequence[1].key_down);
    }

    #[test]
    fn ctrl_m_sequence_is_balanced_and_ordered() {
        let sequence = ctrl_m_sequence();
        assert_eq!(
            sequence.map(|stroke| (stroke.keycode, stroke.key_down)),
            [
                (KeyCode::CONTROL, true),
                (KeyCode::ANSI_M, true),
                (KeyCode::ANSI_M, false),
                (KeyCode::CONTROL, false),
            ]
        );
        assert!(sequence[0].flags.contains(CGEventFlags::CGEventFlagControl));
        assert!(sequence[1].flags.contains(CGEventFlags::CGEventFlagControl));
        assert!(sequence[2].flags.contains(CGEventFlags::CGEventFlagControl));
        assert!(sequence[3].flags.is_empty());
    }
}
