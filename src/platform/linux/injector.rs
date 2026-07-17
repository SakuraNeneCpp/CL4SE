use anyhow::{Context, Result};
use evdev::{
    event_variants::KeyEvent, uinput::VirtualDevice, AttributeSet, BusType, InputEvent, InputId,
    KeyCode,
};

use crate::{core::CommitKey, platform::KeyInjector};

pub(crate) const VIRTUAL_DEVICE_NAME: &str = "CL4SE Virtual Keyboard";
const VIRTUAL_VENDOR: u16 = 0x434c;
const VIRTUAL_PRODUCT: u16 = 0x494d;
const VIRTUAL_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyStroke {
    key: KeyCode,
    value: i32,
}

const ENTER_SEQUENCE: [KeyStroke; 2] = [
    KeyStroke {
        key: KeyCode::KEY_ENTER,
        value: 1,
    },
    KeyStroke {
        key: KeyCode::KEY_ENTER,
        value: 0,
    },
];

const CTRL_M_SEQUENCE: [KeyStroke; 4] = [
    KeyStroke {
        key: KeyCode::KEY_LEFTCTRL,
        value: 1,
    },
    KeyStroke {
        key: KeyCode::KEY_M,
        value: 1,
    },
    KeyStroke {
        key: KeyCode::KEY_M,
        value: 0,
    },
    KeyStroke {
        key: KeyCode::KEY_LEFTCTRL,
        value: 0,
    },
];

const CAPS_LOCK_SEQUENCE: [KeyStroke; 2] = [
    KeyStroke {
        key: KeyCode::KEY_CAPSLOCK,
        value: 1,
    },
    KeyStroke {
        key: KeyCode::KEY_CAPSLOCK,
        value: 0,
    },
];

pub(crate) struct LinuxKeyInjector {
    device: VirtualDevice,
}

impl LinuxKeyInjector {
    pub(crate) fn new() -> Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for key in [
            KeyCode::KEY_ENTER,
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_M,
            KeyCode::KEY_CAPSLOCK,
        ] {
            keys.insert(key);
        }

        let device = VirtualDevice::builder()
            .context("failed to open /dev/uinput")?
            .name(VIRTUAL_DEVICE_NAME)
            .input_id(virtual_input_id())
            .with_keys(&keys)
            .context("failed to configure CL4SE uinput keys")?
            .build()
            .context("failed to create CL4SE uinput virtual keyboard")?;
        Ok(Self { device })
    }

    fn emit(&mut self, sequence: &[KeyStroke]) -> Result<()> {
        let events = sequence
            .iter()
            .map(|stroke| InputEvent::from(KeyEvent::new(stroke.key, stroke.value)))
            .collect::<Vec<_>>();
        self.device
            .emit(&events)
            .context("failed to emit uinput key sequence")
    }
}

impl KeyInjector for LinuxKeyInjector {
    fn inject_commit_key(&mut self, key: CommitKey) -> Result<()> {
        match key {
            CommitKey::Enter => self.emit(&ENTER_SEQUENCE),
            CommitKey::CtrlM => self.emit(&CTRL_M_SEQUENCE),
        }
    }

    fn inject_capslock(&mut self) -> Result<()> {
        self.emit(&CAPS_LOCK_SEQUENCE)
    }
}

pub(crate) fn is_own_virtual_device(name: Option<&str>, id: &InputId) -> bool {
    name == Some(VIRTUAL_DEVICE_NAME)
        && id.bus_type() == BusType::BUS_VIRTUAL
        && id.vendor() == VIRTUAL_VENDOR
        && id.product() == VIRTUAL_PRODUCT
        && id.version() == VIRTUAL_VERSION
}

fn virtual_input_id() -> InputId {
    InputId::new(
        BusType::BUS_VIRTUAL,
        VIRTUAL_VENDOR,
        VIRTUAL_PRODUCT,
        VIRTUAL_VERSION,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_sequence_is_balanced() {
        assert_eq!(
            ENTER_SEQUENCE,
            [
                KeyStroke {
                    key: KeyCode::KEY_ENTER,
                    value: 1,
                },
                KeyStroke {
                    key: KeyCode::KEY_ENTER,
                    value: 0,
                },
            ]
        );
    }

    #[test]
    fn ctrl_m_sequence_is_balanced_and_ordered() {
        assert_eq!(
            CTRL_M_SEQUENCE.map(|stroke| (stroke.key.0, stroke.value)),
            [
                (KeyCode::KEY_LEFTCTRL.0, 1),
                (KeyCode::KEY_M.0, 1),
                (KeyCode::KEY_M.0, 0),
                (KeyCode::KEY_LEFTCTRL.0, 0),
            ]
        );
    }

    #[test]
    fn virtual_device_marker_requires_name_and_full_input_id() {
        let id = virtual_input_id();
        assert!(is_own_virtual_device(Some(VIRTUAL_DEVICE_NAME), &id));
        assert!(!is_own_virtual_device(Some("other"), &id));
        assert!(!is_own_virtual_device(
            Some(VIRTUAL_DEVICE_NAME),
            &InputId::new(BusType::BUS_VIRTUAL, VIRTUAL_VENDOR, 0, VIRTUAL_VERSION)
        ));
    }
}
