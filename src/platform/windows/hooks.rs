use std::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, OnceLock,
    },
};

use anyhow::{anyhow, Context, Result};
use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
        Input::KeyboardAndMouse::{
            VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL,
            VK_RETURN, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
        },
        WindowsAndMessaging::{
            CallNextHookEx, SetWindowsHookExW, UnhookWindowsHookEx, EVENT_SYSTEM_FOREGROUND, HHOOK,
            KBDLLHOOKSTRUCT, LLKHF_INJECTED, WH_KEYBOARD_LL, WH_MOUSE_LL, WINEVENT_OUTOFCONTEXT,
            WINEVENT_SKIPOWNPROCESS, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
            WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_XBUTTONDOWN,
        },
    },
};

use crate::core::ObservedEvent;

pub(crate) const INJECTION_MARKER: usize = 0x434c_494d;
const CAPS_LOCK_SCAN_CODE: u32 = 0x3a;
const KEY_STATE_COUNT: usize = 256;

#[derive(Debug, Clone, Copy)]
pub(crate) struct QueuedEvent {
    pub(crate) event: ObservedEvent,
    pub(crate) generation: usize,
}

static EVENT_QUEUE: OnceLock<Arc<EventQueue>> = OnceLock::new();
static EVENT_GENERATION: AtomicUsize = AtomicUsize::new(0);
static CAPTURE_ENABLED: AtomicBool = AtomicBool::new(false);
static KEY_STATES: [AtomicBool; KEY_STATE_COUNT] =
    [const { AtomicBool::new(false) }; KEY_STATE_COUNT];

pub(crate) fn set_event_queue(queue: Arc<EventQueue>) -> Result<()> {
    EVENT_QUEUE
        .set(queue)
        .map_err(|_| anyhow!("Windows hook event queue was already initialized"))
}

pub(crate) fn current_generation() -> usize {
    EVENT_GENERATION.load(Ordering::Acquire)
}

/// Preallocated single-producer/single-consumer channel used by callbacks.
///
/// Windows dispatches all three installed callbacks on the hook thread: the
/// low-level hooks target that thread, and WINEVENT_OUTOFCONTEXT queues events
/// back to the SetWinEventHook caller's message loop. The worker is the sole
/// consumer. These constraints make the lock-free SPSC indexes sound.
pub(crate) struct EventQueue {
    slots: Box<[UnsafeCell<MaybeUninit<QueuedEvent>>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: Only the hook thread calls push and only the worker calls pop. Slot
// ownership is transferred with Release/Acquire ordering before either index is
// advanced, and a full queue is never overwritten.
unsafe impl Sync for EventQueue {}
// SAFETY: EventQueue owns its slots and all shared mutation is governed by the
// SPSC atomic-index protocol described above.
unsafe impl Send for EventQueue {}

impl EventQueue {
    pub(crate) fn new(capacity: usize) -> Self {
        let slots = (0..capacity.saturating_add(1))
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            slots,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    fn push(&self, event: QueuedEvent) -> std::result::Result<(), QueuedEvent> {
        let tail = self.tail.load(Ordering::Relaxed);
        let next = self.next(tail);
        if next == self.head.load(Ordering::Acquire) {
            return Err(event);
        }

        // SAFETY: In the SPSC protocol, tail denotes a slot exclusively owned
        // by the producer until tail is published with Release ordering.
        unsafe { (*self.slots[tail].get()).write(event) };
        self.tail.store(next, Ordering::Release);
        Ok(())
    }

    pub(crate) fn pop(&self) -> Option<QueuedEvent> {
        let head = self.head.load(Ordering::Relaxed);
        if head == self.tail.load(Ordering::Acquire) {
            return None;
        }

        // SAFETY: The Acquire load above observes the producer's initialized
        // slot. The consumer owns head until it publishes the next index.
        let event = unsafe { (*self.slots[head].get()).assume_init_read() };
        self.head.store(self.next(head), Ordering::Release);
        Some(event)
    }

    fn next(&self, index: usize) -> usize {
        if index + 1 == self.slots.len() {
            0
        } else {
            index + 1
        }
    }
}

pub(crate) struct CaptureGuard;

impl CaptureGuard {
    pub(crate) fn enable() -> Self {
        for state in &KEY_STATES {
            state.store(false, Ordering::Relaxed);
        }
        EVENT_GENERATION.store(0, Ordering::Release);
        CAPTURE_ENABLED.store(true, Ordering::Release);
        Self
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        CAPTURE_ENABLED.store(false, Ordering::Release);
    }
}

pub(crate) struct WindowsHooks {
    _keyboard: HookHandle,
    _mouse: HookHandle,
    _focus: WinEventHandle,
}

impl WindowsHooks {
    pub(crate) fn install() -> Result<Self> {
        // SAFETY: Passing None requests the module handle of this executable.
        // The returned handle remains valid for the process lifetime.
        let module = unsafe { GetModuleHandleW(None) }.context("failed to get module handle")?;
        let instance = HINSTANCE(module.0);

        // SAFETY: Both callback functions have the exact system ABI required by
        // WH_KEYBOARD_LL/WH_MOUSE_LL, live for the process lifetime, never unwind,
        // and perform only atomics plus a bounded lock-free enqueue.
        let keyboard =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(instance), 0) }
                .context("failed to install WH_KEYBOARD_LL")?;
        let keyboard = HookHandle(keyboard);

        // SAFETY: See the callback lifetime and ABI argument above. Partial
        // installation is cleaned up by HookHandle if this call fails.
        let mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(instance), 0) }
            .context("failed to install WH_MOUSE_LL")?;
        let mouse = HookHandle(mouse);

        // SAFETY: focus_hook has the WINEVENTPROC ABI and static lifetime. The
        // OUTOFCONTEXT callback only performs a bounded nonblocking enqueue.
        let focus = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(focus_hook),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            )
        };
        if focus.0.is_null() {
            return Err(anyhow!("failed to install EVENT_SYSTEM_FOREGROUND hook"));
        }

        Ok(Self {
            _keyboard: keyboard,
            _mouse: mouse,
            _focus: WinEventHandle(focus),
        })
    }
}

struct HookHandle(HHOOK);

impl Drop for HookHandle {
    fn drop(&mut self) {
        // SAFETY: This object owns the successful SetWindowsHookExW handle and
        // calls UnhookWindowsHookEx exactly once during cleanup.
        let _ = unsafe { UnhookWindowsHookEx(self.0) };
    }
}

struct WinEventHandle(HWINEVENTHOOK);

impl Drop for WinEventHandle {
    fn drop(&mut self) {
        // SAFETY: This object owns the successful SetWinEventHook handle and
        // calls UnhookWinEvent exactly once during cleanup.
        let _ = unsafe { UnhookWinEvent(self.0) };
    }
}

// SAFETY: This function is called only by Windows with the WH_KEYBOARD_LL ABI.
// Every branch is panic-free and performs no allocation, locking, or waiting.
unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 || !CAPTURE_ENABLED.load(Ordering::Acquire) {
        return call_next(code, wparam, lparam);
    }

    let message = wparam.0 as u32;
    let is_down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
    let is_up = message == WM_KEYUP || message == WM_SYSKEYUP;
    if !is_down && !is_up {
        return call_next(code, wparam, lparam);
    }

    // SAFETY: For a nonnegative WH_KEYBOARD_LL callback code, Windows specifies
    // that lParam points to a valid KBDLLHOOKSTRUCT for the duration of this call.
    let key = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    let injected = key.flags.contains(LLKHF_INJECTED);
    let self_injected = is_own_injection(injected, key.dwExtraInfo);

    // Events synthesized by other software are neither physical Caps Lock nor
    // trustworthy composition evidence, so they are passed through unobserved.
    if injected && !self_injected {
        return call_next(code, wparam, lparam);
    }

    if self_injected {
        if is_down {
            enqueue_self_injected(key);
        }
        return call_next(code, wparam, lparam);
    }

    let repeat = update_physical_key_state(key.vkCode, is_down, is_up);

    // Physical Caps Lock is identified solely by scan code 0x3A. Both down and
    // up are suppressed synchronously; any requested pass-through is later
    // re-injected with CLIME's marker by the worker.
    if is_caps_lock_scan_code(key.scanCode) {
        if is_down {
            enqueue(ObservedEvent::TriggerKeyDown {
                shift: shift_down(),
                other_mods: other_modifier_down(),
                repeat,
                self_injected: false,
            });
        }
        return LRESULT(1);
    }

    if is_down {
        if is_commit_like(key.vkCode) {
            enqueue(ObservedEvent::CommitLikeKeyDown {
                repeat,
                self_injected: false,
            });
        } else if is_printable_vk(key.vkCode) {
            enqueue(ObservedEvent::PrintableKeyDown {
                repeat,
                self_injected: false,
            });
        }
    }

    call_next(code, wparam, lparam)
}

// SAFETY: This function is called only by Windows with the WH_MOUSE_LL ABI.
// It performs only message classification and a lock-free enqueue.
unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && CAPTURE_ENABLED.load(Ordering::Acquire) {
        let message = wparam.0 as u32;
        if matches!(
            message,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        ) {
            enqueue(ObservedEvent::MouseClick);
        }
    }
    call_next(code, wparam, lparam)
}

// SAFETY: This function is called only by Windows with the WINEVENTPROC ABI.
// It performs only an event comparison and a lock-free enqueue.
unsafe extern "system" fn focus_hook(
    _hook: HWINEVENTHOOK,
    event: u32,
    _window: windows::Win32::Foundation::HWND,
    _object_id: i32,
    _child_id: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND && CAPTURE_ENABLED.load(Ordering::Acquire) {
        enqueue(ObservedEvent::FocusChanged);
    }
}

fn enqueue_self_injected(key: &KBDLLHOOKSTRUCT) {
    let event = if is_caps_lock_scan_code(key.scanCode) {
        Some(ObservedEvent::TriggerKeyDown {
            shift: false,
            other_mods: false,
            repeat: false,
            self_injected: true,
        })
    } else if is_commit_like(key.vkCode) {
        Some(ObservedEvent::CommitLikeKeyDown {
            repeat: false,
            self_injected: true,
        })
    } else if is_printable_vk(key.vkCode) {
        Some(ObservedEvent::PrintableKeyDown {
            repeat: false,
            self_injected: true,
        })
    } else {
        None
    };

    if let Some(event) = event {
        enqueue(event);
    }
}

fn enqueue(event: ObservedEvent) {
    let Some(queue) = EVENT_QUEUE.get() else {
        return;
    };
    let generation = EVENT_GENERATION.load(Ordering::Acquire);
    if queue.push(QueuedEvent { event, generation }).is_err() {
        EVENT_GENERATION.fetch_add(1, Ordering::AcqRel);
    }
}

const fn is_caps_lock_scan_code(scan_code: u32) -> bool {
    scan_code == CAPS_LOCK_SCAN_CODE
}

const fn is_own_injection(injected: bool, marker: usize) -> bool {
    injected && marker == INJECTION_MARKER
}

fn update_physical_key_state(vk_code: u32, is_down: bool, is_up: bool) -> bool {
    let Ok(index) = usize::try_from(vk_code) else {
        return false;
    };
    let Some(state) = KEY_STATES.get(index) else {
        return false;
    };

    if is_down {
        state.swap(true, Ordering::AcqRel)
    } else {
        if is_up {
            state.store(false, Ordering::Release);
        }
        false
    }
}

fn key_down(vk: u16) -> bool {
    KEY_STATES
        .get(usize::from(vk))
        .is_some_and(|state| state.load(Ordering::Acquire))
}

fn shift_down() -> bool {
    [VK_SHIFT, VK_LSHIFT, VK_RSHIFT]
        .iter()
        .any(|key| key_down(key.0))
}

fn ctrl_down() -> bool {
    [VK_CONTROL, VK_LCONTROL, VK_RCONTROL]
        .iter()
        .any(|key| key_down(key.0))
}

fn other_modifier_down() -> bool {
    ctrl_down()
        || [VK_MENU, VK_LMENU, VK_RMENU, VK_LWIN, VK_RWIN]
            .iter()
            .any(|key| key_down(key.0))
}

fn is_commit_like(vk_code: u32) -> bool {
    vk_code == u32::from(VK_RETURN.0)
        || vk_code == u32::from(VK_ESCAPE.0)
        || (vk_code == u32::from(b'M') && ctrl_down())
}

fn is_printable_vk(vk_code: u32) -> bool {
    matches!(
        vk_code,
        0x20 | 0x30..=0x39 | 0x41..=0x5a | 0x60..=0x6f | 0xba..=0xc0 | 0xdb..=0xdf | 0xe2
    )
}

fn call_next(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: Forwarding the callback parameters unchanged is required by the
    // hook contract for every event CLIME does not suppress.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_classification_excludes_control_keys() {
        assert!(is_printable_vk(u32::from(b'A')));
        assert!(is_printable_vk(0x20));
        assert!(!is_printable_vk(u32::from(VK_RETURN.0)));
        assert!(!is_printable_vk(u32::from(VK_ESCAPE.0)));
    }

    #[test]
    fn physical_capslock_identity_depends_only_on_scan_code() {
        assert!(is_caps_lock_scan_code(0x3a));
        assert!(!is_caps_lock_scan_code(0x14));
        assert!(!is_caps_lock_scan_code(0xf0));
    }

    #[test]
    fn only_injected_events_with_our_marker_are_ours() {
        assert!(is_own_injection(true, INJECTION_MARKER));
        assert!(!is_own_injection(false, INJECTION_MARKER));
        assert!(!is_own_injection(true, INJECTION_MARKER + 1));
    }

    #[test]
    fn event_queue_is_fifo_and_bounded() {
        let queue = EventQueue::new(2);
        let first = QueuedEvent {
            event: ObservedEvent::MouseClick,
            generation: 1,
        };
        let second = QueuedEvent {
            event: ObservedEvent::FocusChanged,
            generation: 2,
        };
        assert!(queue.push(first).is_ok());
        assert!(queue.push(second).is_ok());
        assert!(queue.push(first).is_err());
        assert!(matches!(queue.pop(), Some(event) if event.generation == 1));
        assert!(matches!(queue.pop(), Some(event) if event.generation == 2));
        assert!(queue.pop().is_none());
    }
}
