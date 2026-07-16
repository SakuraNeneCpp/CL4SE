use std::{
    collections::{BTreeMap, HashSet},
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{bail, Result};
use evdev::{Device, EventType, InputEvent, KeyCode};

use crate::core::ObservedEvent;

use super::injector::is_own_virtual_device;

const RESCAN_INTERVAL: Duration = Duration::from_secs(1);
const MODIFIER_SLOTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceRole {
    Keyboard,
    Pointer,
    KeyboardAndPointer,
}

impl DeviceRole {
    const fn has_keyboard(self) -> bool {
        matches!(self, Self::Keyboard | Self::KeyboardAndPointer)
    }
}

pub(crate) struct PollBatch {
    pub(crate) events: Vec<ObservedEvent>,
    pub(crate) state_uncertain: bool,
}

pub(crate) struct LinuxInputMonitor {
    devices: BTreeMap<PathBuf, MonitoredDevice>,
    modifiers: ModifierCounts,
    last_scan: Instant,
}

impl LinuxInputMonitor {
    pub(crate) fn new() -> Result<Self> {
        let mut monitor = Self {
            devices: BTreeMap::new(),
            modifiers: ModifierCounts::default(),
            last_scan: Instant::now()
                .checked_sub(RESCAN_INTERVAL)
                .unwrap_or_else(Instant::now),
        };
        let _ = monitor.rescan();
        if !monitor
            .devices
            .values()
            .any(|device| device.role.has_keyboard())
        {
            bail!(
                "no readable evdev keyboard found; run `clime doctor` and grant /dev/input/event* access"
            );
        }
        Ok(monitor)
    }

    pub(crate) fn poll(&mut self) -> PollBatch {
        let mut state_uncertain = false;
        if self.last_scan.elapsed() >= RESCAN_INTERVAL {
            state_uncertain |= self.rescan();
        }

        let paths = self.devices.keys().cloned().collect::<Vec<_>>();
        let mut events = Vec::new();
        let mut failed_paths = Vec::new();

        for path in paths {
            let fetched = {
                let Some(device) = self.devices.get_mut(&path) else {
                    continue;
                };
                device
                    .device
                    .fetch_events()
                    .map(|events| events.collect::<Vec<_>>())
            };

            match fetched {
                Ok(input_events) => {
                    for input_event in input_events {
                        let Some(device) = self.devices.get_mut(&path) else {
                            break;
                        };
                        if let Some(event) =
                            classify_input_event(device, &mut self.modifiers, input_event)
                        {
                            events.push(event);
                        }
                    }
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => {
                    log::warn!("evdev read failed for {}: {error}", path.display());
                    failed_paths.push(path);
                    state_uncertain = true;
                }
            }
        }

        for path in failed_paths {
            self.remove_device(&path);
        }

        PollBatch {
            events,
            state_uncertain,
        }
    }

    /// Periodic enumeration provides hotplug support without taking EVIOCGRAB.
    /// `evdev::enumerate` opens every device read-only; this module never calls
    /// `Device::grab`, so CLIME cannot block other input consumers.
    fn rescan(&mut self) -> bool {
        self.last_scan = Instant::now();
        let mut seen = HashSet::new();
        let mut state_uncertain = false;

        for (path, device) in evdev::enumerate() {
            seen.insert(path.clone());
            if self.devices.contains_key(&path)
                || is_own_virtual_device(device.name(), &device.input_id())
            {
                continue;
            }

            let Some(role) = classify_device(&device) else {
                continue;
            };
            if let Err(error) = device.set_nonblocking(true) {
                log::warn!(
                    "failed to make evdev device nonblocking ({}): {error}",
                    path.display()
                );
                continue;
            }

            let key_state = match device.get_key_state() {
                Ok(key_state) => key_state,
                Err(error) => {
                    log::warn!(
                        "failed to read initial evdev key state ({}): {error}",
                        path.display()
                    );
                    continue;
                }
            };
            let held_keys = key_state.iter().map(|key| key.0).collect::<HashSet<_>>();
            let mut held_modifiers = [false; MODIFIER_SLOTS];
            for code in held_keys.iter().copied() {
                self.modifiers.update(&mut held_modifiers, code, 1);
            }

            log::info!("monitoring evdev device {} ({role:?})", path.display());
            self.devices.insert(
                path,
                MonitoredDevice {
                    device,
                    role,
                    held_keys,
                    held_modifiers,
                },
            );
            // Input events may have occurred before the new device was
            // enumerated. Reset the heuristic rather than trusting an
            // incomplete history.
            state_uncertain = true;
        }

        let removed = self
            .devices
            .keys()
            .filter(|path| !seen.contains(path.as_path()))
            .cloned()
            .collect::<Vec<_>>();
        state_uncertain |= !removed.is_empty();
        for path in removed {
            log::info!("evdev device removed: {}", path.display());
            self.remove_device(&path);
        }
        state_uncertain
    }

    fn remove_device(&mut self, path: &Path) {
        if let Some(device) = self.devices.remove(path) {
            self.modifiers.release_all(device.held_modifiers);
        }
    }
}

struct MonitoredDevice {
    device: Device,
    role: DeviceRole,
    held_keys: HashSet<u16>,
    held_modifiers: [bool; MODIFIER_SLOTS],
}

#[derive(Default)]
struct ModifierCounts {
    counts: [usize; MODIFIER_SLOTS],
}

impl ModifierCounts {
    fn update(&mut self, held: &mut [bool; MODIFIER_SLOTS], code: u16, value: i32) -> bool {
        let Some(slot) = modifier_slot(code) else {
            return false;
        };
        match value {
            1 if !held[slot] => {
                held[slot] = true;
                self.counts[slot] = self.counts[slot].saturating_add(1);
            }
            0 if held[slot] => {
                held[slot] = false;
                self.counts[slot] = self.counts[slot].saturating_sub(1);
            }
            _ => {}
        }
        true
    }

    fn release_all(&mut self, held: [bool; MODIFIER_SLOTS]) {
        for (slot, was_held) in held.into_iter().enumerate() {
            if was_held {
                self.counts[slot] = self.counts[slot].saturating_sub(1);
            }
        }
    }

    fn shift_down(&self) -> bool {
        self.counts[0] > 0 || self.counts[1] > 0
    }

    fn ctrl_down(&self) -> bool {
        self.counts[2] > 0 || self.counts[3] > 0
    }

    fn other_mods_down(&self) -> bool {
        self.ctrl_down() || self.counts[4..].iter().any(|count| *count > 0)
    }
}

fn classify_input_event(
    device: &mut MonitoredDevice,
    modifiers: &mut ModifierCounts,
    input_event: InputEvent,
) -> Option<ObservedEvent> {
    if input_event.event_type() != EventType::KEY {
        return None;
    }
    let code = input_event.code();
    let value = input_event.value();
    if !matches!(value, 0..=2) {
        return None;
    }

    let already_down = device.held_keys.contains(&code);
    match value {
        0 => {
            device.held_keys.remove(&code);
        }
        1 => {
            device.held_keys.insert(code);
        }
        _ => {}
    }
    let repeat = value == 2 || (value == 1 && already_down);

    if modifiers.update(&mut device.held_modifiers, code, value) || value == 0 {
        return None;
    }
    if is_mouse_button(code) {
        return (value == 1).then_some(ObservedEvent::MouseClick);
    }
    if code == KeyCode::KEY_CAPSLOCK.0 {
        return Some(ObservedEvent::TriggerKeyDown {
            shift: modifiers.shift_down(),
            other_mods: modifiers.other_mods_down(),
            repeat,
            self_injected: false,
        });
    }
    if is_commit_like(code, modifiers.ctrl_down()) {
        return Some(ObservedEvent::CommitLikeKeyDown {
            repeat,
            self_injected: false,
        });
    }
    is_printable_key(code).then_some(ObservedEvent::PrintableKeyDown {
        repeat,
        self_injected: false,
    })
}

fn classify_device(device: &Device) -> Option<DeviceRole> {
    if is_own_virtual_device(device.name(), &device.input_id()) {
        return None;
    }
    let codes = device
        .supported_keys()?
        .iter()
        .map(|key| key.0)
        .collect::<Vec<_>>();
    classify_supported_codes(&codes)
}

fn classify_supported_codes(codes: &[u16]) -> Option<DeviceRole> {
    let keyboard = codes.iter().copied().any(is_keyboard_code);
    let pointer = codes.iter().copied().any(is_mouse_button);
    match (keyboard, pointer) {
        (true, true) => Some(DeviceRole::KeyboardAndPointer),
        (true, false) => Some(DeviceRole::Keyboard),
        (false, true) => Some(DeviceRole::Pointer),
        (false, false) => None,
    }
}

const fn is_keyboard_code(code: u16) -> bool {
    code == KeyCode::KEY_CAPSLOCK.0
        || code == KeyCode::KEY_ENTER.0
        || code == KeyCode::KEY_ESC.0
        || code == KeyCode::KEY_LEFTCTRL.0
        || code == KeyCode::KEY_RIGHTCTRL.0
        || code == KeyCode::KEY_LEFTSHIFT.0
        || code == KeyCode::KEY_RIGHTSHIFT.0
        || is_printable_key(code)
}

const fn is_commit_like(code: u16, ctrl_down: bool) -> bool {
    code == KeyCode::KEY_ENTER.0
        || code == KeyCode::KEY_KPENTER.0
        || code == KeyCode::KEY_ESC.0
        || (code == KeyCode::KEY_M.0 && ctrl_down)
}

const fn is_printable_key(code: u16) -> bool {
    matches!(
        code,
        2..=13
            | 16..=27
            | 30..=41
            | 43..=53
            | 55
            | 57
            | 71..=83
            | 86
            | 89
            | 95
            | 98
            | 117
            | 121
            | 124
            | 179..=180
    )
}

const fn is_mouse_button(code: u16) -> bool {
    code >= 0x110 && code <= 0x117
}

const fn modifier_slot(code: u16) -> Option<usize> {
    match code {
        42 => Some(0),  // KEY_LEFTSHIFT
        54 => Some(1),  // KEY_RIGHTSHIFT
        29 => Some(2),  // KEY_LEFTCTRL
        97 => Some(3),  // KEY_RIGHTCTRL
        56 => Some(4),  // KEY_LEFTALT
        100 => Some(5), // KEY_RIGHTALT
        125 => Some(6), // KEY_LEFTMETA
        126 => Some(7), // KEY_RIGHTMETA
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> MonitoredDevice {
        let (_, device) = evdev::enumerate()
            .next()
            .expect("tests that use this helper require one evdev device");
        MonitoredDevice {
            device,
            role: DeviceRole::Keyboard,
            held_keys: HashSet::new(),
            held_modifiers: [false; MODIFIER_SLOTS],
        }
    }

    fn raw_key(code: KeyCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, code.0, value)
    }

    #[test]
    fn physical_caps_lock_identity_is_linux_keycode_58() {
        assert_eq!(KeyCode::KEY_CAPSLOCK.0, 58);
        assert!(is_keyboard_code(58));
        assert!(!is_keyboard_code(59));
    }

    #[test]
    fn device_selection_accepts_keyboards_and_mouse_buttons_only() {
        assert_eq!(
            classify_supported_codes(&[KeyCode::KEY_CAPSLOCK.0, KeyCode::KEY_A.0]),
            Some(DeviceRole::Keyboard)
        );
        assert_eq!(
            classify_supported_codes(&[KeyCode::BTN_LEFT.0]),
            Some(DeviceRole::Pointer)
        );
        assert_eq!(
            classify_supported_codes(&[KeyCode::KEY_A.0, KeyCode::BTN_RIGHT.0]),
            Some(DeviceRole::KeyboardAndPointer)
        );
        assert_eq!(classify_supported_codes(&[KeyCode::KEY_MUTE.0]), None);
    }

    #[test]
    fn pure_key_classification_covers_ctrl_m_and_modifiers() {
        let mut modifiers = ModifierCounts::default();
        let mut held = [false; MODIFIER_SLOTS];
        assert!(modifiers.update(&mut held, KeyCode::KEY_LEFTCTRL.0, 1));
        assert!(modifiers.ctrl_down());
        assert!(is_commit_like(KeyCode::KEY_M.0, modifiers.ctrl_down()));
        assert!(modifiers.update(&mut held, KeyCode::KEY_LEFTCTRL.0, 0));
        assert!(!modifiers.ctrl_down());
        assert!(!is_commit_like(KeyCode::KEY_M.0, modifiers.ctrl_down()));
    }

    #[test]
    fn printable_and_control_key_sets_do_not_overlap() {
        assert!(is_printable_key(KeyCode::KEY_A.0));
        assert!(is_printable_key(KeyCode::KEY_SPACE.0));
        assert!(!is_printable_key(KeyCode::KEY_ENTER.0));
        assert!(!is_printable_key(KeyCode::KEY_ESC.0));
        assert!(!is_printable_key(KeyCode::KEY_LEFTCTRL.0));
    }

    #[test]
    #[ignore = "requires a readable evdev device; pure classification is covered above"]
    fn end_to_end_event_classification_on_host_device() {
        let mut device = device();
        let mut modifiers = ModifierCounts::default();
        assert!(matches!(
            classify_input_event(
                &mut device,
                &mut modifiers,
                raw_key(KeyCode::KEY_CAPSLOCK, 1)
            ),
            Some(ObservedEvent::TriggerKeyDown { repeat: false, .. })
        ));
    }
}
