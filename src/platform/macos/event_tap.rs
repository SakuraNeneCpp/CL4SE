use std::{
    cell::UnsafeCell,
    ffi::c_void,
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicPtr, AtomicUsize, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Result};
use core_foundation::{
    base::TCFType,
    mach_port::CFMachPortRef,
    runloop::{kCFRunLoopCommonModes, CFRunLoop, CFRunLoopSource},
};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventType, CallbackResult, EventField, KeyCode,
};

use crate::{core::ObservedEvent, platform::macos::injector::INJECTION_MARKER};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct QueuedEvent {
    pub(crate) event: ObservedEvent,
    pub(crate) generation: usize,
}

/// Preallocated single-producer/single-consumer queue used by the event tap.
///
/// The current run-loop thread is the sole producer and the engine worker is
/// the sole consumer. The callback therefore needs only bounded atomic work.
pub(crate) struct EventQueue {
    slots: Box<[UnsafeCell<MaybeUninit<QueuedEvent>>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: Only the event-tap thread calls push and only the worker calls pop.
// Slot ownership is transferred with Release/Acquire ordering, and a full
// queue is never overwritten.
unsafe impl Sync for EventQueue {}
// SAFETY: EventQueue owns its slots and all shared mutation follows the SPSC
// protocol described above.
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

        // SAFETY: In this SPSC protocol, tail is exclusively owned by the
        // producer until the new tail is published with Release ordering.
        unsafe { (*self.slots[tail].get()).write(event) };
        self.tail.store(next, Ordering::Release);
        Ok(())
    }

    pub(crate) fn pop(&self) -> Option<QueuedEvent> {
        let head = self.head.load(Ordering::Relaxed);
        if head == self.tail.load(Ordering::Acquire) {
            return None;
        }

        // SAFETY: The Acquire load observes the producer's initialized slot;
        // the consumer owns head until publishing its successor.
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

struct CallbackContext {
    queue: Arc<EventQueue>,
    generation: AtomicUsize,
    tap: AtomicPtr<c_void>,
}

pub(crate) struct MacOsEventTap {
    event_tap: CGEventTap<'static>,
    run_loop_source: CFRunLoopSource,
    run_loop: CFRunLoop,
}

impl MacOsEventTap {
    pub(crate) fn install(queue: Arc<EventQueue>) -> Result<Self> {
        let context = Arc::new(CallbackContext {
            queue,
            generation: AtomicUsize::new(0),
            tap: AtomicPtr::new(std::ptr::null_mut()),
        });
        let callback_context = Arc::clone(&context);
        let event_tap = CGEventTap::new(
            CGEventTapLocation::Session,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            vec![
                CGEventType::KeyDown,
                CGEventType::KeyUp,
                CGEventType::LeftMouseDown,
                CGEventType::RightMouseDown,
                CGEventType::OtherMouseDown,
            ],
            move |_proxy, event_type, event| {
                event_tap_callback(&callback_context, event_type, event)
            },
        )
        .map_err(|()| {
            anyhow!(
                "failed to create CGEventTap; grant Input Monitoring and Accessibility, then retry"
            )
        })?;

        let tap_ref = event_tap.mach_port().as_concrete_TypeRef();
        context.tap.store(tap_ref.cast(), Ordering::Release);
        let run_loop_source = event_tap
            .mach_port()
            .create_runloop_source(0)
            .map_err(|()| anyhow!("failed to create CGEventTap run-loop source"))?;
        let run_loop = CFRunLoop::get_current();
        // SAFETY: kCFRunLoopCommonModes is a framework-owned static mode and
        // both objects remain owned by this guard until removal in Drop.
        run_loop.add_source(&run_loop_source, unsafe { kCFRunLoopCommonModes });
        event_tap.enable();

        Ok(Self {
            event_tap,
            run_loop_source,
            run_loop,
        })
    }

    pub(crate) fn run_loop(&self) -> CFRunLoop {
        self.run_loop.clone()
    }

    pub(crate) fn run(&self) {
        CFRunLoop::run_current();
    }
}

impl Drop for MacOsEventTap {
    fn drop(&mut self) {
        // Disable and detach before the callback closure/context is destroyed.
        // SAFETY: event_tap owns a valid CFMachPort until its field is dropped.
        unsafe { CGEventTapEnable(self.event_tap.mach_port().as_concrete_TypeRef(), false) };
        // SAFETY: This removes the same source/mode pair added during install;
        // both wrappers remain alive for the call.
        self.run_loop
            .remove_source(&self.run_loop_source, unsafe { kCFRunLoopCommonModes });
    }
}

/// Event-tap callback body. It performs no allocation, locking, blocking, or
/// logging. Every event except the managed F18 trigger is returned unchanged.
fn event_tap_callback(
    context: &CallbackContext,
    event_type: CGEventType,
    event: &CGEvent,
) -> CallbackResult {
    if matches!(
        event_type,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        context.generation.fetch_add(1, Ordering::AcqRel);
        let tap = context.tap.load(Ordering::Acquire);
        if !tap.is_null() {
            // SAFETY: The pointer is published from the live CGEventTap's
            // CFMachPort and cleared only by destroying the callback itself.
            unsafe { CGEventTapEnable(tap.cast(), true) };
        }
        return CallbackResult::Keep;
    }

    if matches!(
        event_type,
        CGEventType::LeftMouseDown | CGEventType::RightMouseDown | CGEventType::OtherMouseDown
    ) {
        enqueue(context, ObservedEvent::MouseClick);
        return CallbackResult::Keep;
    }

    if !matches!(event_type, CGEventType::KeyDown | CGEventType::KeyUp) {
        return CallbackResult::Keep;
    }

    let raw_keycode = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
    let Ok(keycode) = u16::try_from(raw_keycode) else {
        return CallbackResult::Keep;
    };

    // Both boundaries of the CapsLock->F18 mapping are suppressed. Only the
    // initial key-down is sent to Engine; autorepeat is classified explicitly.
    if should_suppress_mapped_trigger(event_type, keycode) {
        if matches!(event_type, CGEventType::KeyDown) {
            let repeat = event.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT) != 0;
            let self_injected = event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA)
                == INJECTION_MARKER;
            let classified = classify_key_down(keycode, event.get_flags(), repeat, self_injected);
            if let Some(classified) = classified {
                enqueue(context, classified);
            }
        }
        return CallbackResult::Drop;
    }

    if matches!(event_type, CGEventType::KeyDown) {
        let repeat = event.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT) != 0;
        let self_injected =
            event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == INJECTION_MARKER;
        if let Some(classified) =
            classify_key_down(keycode, event.get_flags(), repeat, self_injected)
        {
            enqueue(context, classified);
        }
    }

    CallbackResult::Keep
}

const fn should_suppress_mapped_trigger(event_type: CGEventType, keycode: u16) -> bool {
    keycode == KeyCode::F18 && matches!(event_type, CGEventType::KeyDown | CGEventType::KeyUp)
}

fn enqueue(context: &CallbackContext, event: ObservedEvent) {
    let generation = context.generation.load(Ordering::Acquire);
    if context
        .queue
        .push(QueuedEvent { event, generation })
        .is_err()
    {
        context.generation.fetch_add(1, Ordering::AcqRel);
    }
}

fn classify_key_down(
    keycode: u16,
    flags: CGEventFlags,
    repeat: bool,
    self_injected: bool,
) -> Option<ObservedEvent> {
    if keycode == KeyCode::F18 {
        return Some(ObservedEvent::TriggerKeyDown {
            shift: flags.contains(CGEventFlags::CGEventFlagShift),
            other_mods: flags.intersects(
                CGEventFlags::CGEventFlagControl
                    | CGEventFlags::CGEventFlagAlternate
                    | CGEventFlags::CGEventFlagCommand,
            ),
            repeat,
            self_injected,
        });
    }

    if is_commit_like(keycode, flags) {
        Some(ObservedEvent::CommitLikeKeyDown {
            repeat,
            self_injected,
        })
    } else if is_printable_keycode(keycode) {
        Some(ObservedEvent::PrintableKeyDown {
            repeat,
            self_injected,
        })
    } else {
        None
    }
}

fn is_commit_like(keycode: u16, flags: CGEventFlags) -> bool {
    keycode == KeyCode::RETURN
        || keycode == KeyCode::ANSI_KEYPAD_ENTER
        || keycode == KeyCode::ESCAPE
        || (keycode == KeyCode::ANSI_M && flags.contains(CGEventFlags::CGEventFlagControl))
}

const fn is_printable_keycode(keycode: u16) -> bool {
    matches!(
        keycode,
        0x00..=0x23
            | 0x25..=0x2f
            | 0x31..=0x32
            | 0x41
            | 0x43
            | 0x45
            | 0x4b
            | 0x4e
            | 0x51..=0x59
            | 0x5b..=0x5f
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f18_is_the_only_trigger_keycode() {
        let event = classify_key_down(KeyCode::F18, CGEventFlags::empty(), false, false);
        assert!(matches!(event, Some(ObservedEvent::TriggerKeyDown { .. })));
        assert!(!matches!(
            classify_key_down(KeyCode::F17, CGEventFlags::empty(), false, false),
            Some(ObservedEvent::TriggerKeyDown { .. })
        ));
    }

    #[test]
    fn only_mapped_f18_key_boundaries_are_suppressed() {
        assert!(should_suppress_mapped_trigger(
            CGEventType::KeyDown,
            KeyCode::F18
        ));
        assert!(should_suppress_mapped_trigger(
            CGEventType::KeyUp,
            KeyCode::F18
        ));
        assert!(!should_suppress_mapped_trigger(
            CGEventType::KeyDown,
            KeyCode::RETURN
        ));
        assert!(!should_suppress_mapped_trigger(
            CGEventType::LeftMouseDown,
            KeyCode::F18
        ));
    }

    #[test]
    fn trigger_modifier_classification_matches_safety_rules() {
        let flags = CGEventFlags::CGEventFlagShift | CGEventFlags::CGEventFlagCommand;
        assert_eq!(
            classify_key_down(KeyCode::F18, flags, true, false),
            Some(ObservedEvent::TriggerKeyDown {
                shift: true,
                other_mods: true,
                repeat: true,
                self_injected: false,
            })
        );
    }

    #[test]
    fn physical_ctrl_m_is_commit_like_not_printable() {
        assert!(matches!(
            classify_key_down(
                KeyCode::ANSI_M,
                CGEventFlags::CGEventFlagControl,
                false,
                false,
            ),
            Some(ObservedEvent::CommitLikeKeyDown { .. })
        ));
        assert!(matches!(
            classify_key_down(KeyCode::ANSI_M, CGEventFlags::empty(), false, false),
            Some(ObservedEvent::PrintableKeyDown { .. })
        ));
        assert_eq!(
            classify_key_down(
                KeyCode::ANSI_M,
                CGEventFlags::CGEventFlagControl,
                false,
                true,
            ),
            Some(ObservedEvent::CommitLikeKeyDown {
                repeat: false,
                self_injected: true,
            })
        );
    }

    #[test]
    fn control_keys_are_not_printable() {
        assert!(!is_printable_keycode(KeyCode::RETURN));
        assert!(!is_printable_keycode(KeyCode::ESCAPE));
        assert!(!is_printable_keycode(KeyCode::CONTROL));
        assert!(is_printable_keycode(KeyCode::ANSI_A));
        assert!(is_printable_keycode(KeyCode::SPACE));
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
