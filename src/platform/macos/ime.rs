use std::ffi::c_void;

use core_foundation::{
    base::{CFRelease, CFTypeRef, TCFType},
    string::{CFString, CFStringRef},
};

use crate::{
    core::{ImeGuess, ImeSnapshot},
    platform::ImeStateProvider,
};

type TisInputSourceRef = *const c_void;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    static kTISPropertyInputSourceID: CFStringRef;

    fn TISCopyCurrentKeyboardInputSource() -> TisInputSourceRef;
    fn TISGetInputSourceProperty(
        input_source: TisInputSourceRef,
        property_key: CFStringRef,
    ) -> CFTypeRef;
}

pub(crate) struct MacOsImeStateProvider;

impl MacOsImeStateProvider {
    pub(crate) fn current_source_id() -> Option<String> {
        // SAFETY: TISCopyCurrentKeyboardInputSource has no arguments and
        // returns a retained TISInputSourceRef, or null on failure.
        let input_source = unsafe { TISCopyCurrentKeyboardInputSource() };
        if input_source.is_null() {
            return None;
        }
        let input_source = InputSource(input_source);

        // SAFETY: input_source owns a valid TIS reference and the property key
        // is a framework-provided static CFStringRef. The returned property is
        // borrowed for at least the lifetime of input_source.
        let property =
            unsafe { TISGetInputSourceProperty(input_source.0, kTISPropertyInputSourceID) };
        if property.is_null() {
            return None;
        }

        // SAFETY: kTISPropertyInputSourceID is documented to return a
        // CFStringRef. The get-rule wrapper retains it before input_source is
        // released and balances that retain when the wrapper is dropped.
        let source_id = unsafe { CFString::wrap_under_get_rule(property.cast()) };
        Some(source_id.to_string())
    }
}

impl ImeStateProvider for MacOsImeStateProvider {
    fn snapshot(&mut self) -> ImeSnapshot {
        let Some(source_id) = Self::current_source_id() else {
            return ImeSnapshot {
                active: ImeGuess::Unknown,
                ime_id: None,
            };
        };
        let active = if source_id.contains("Japanese") {
            ImeGuess::Yes
        } else {
            ImeGuess::No
        };

        ImeSnapshot {
            active,
            ime_id: Some(source_id),
        }
    }
}

struct InputSource(TisInputSourceRef);

impl Drop for InputSource {
    fn drop(&mut self) {
        // SAFETY: InputSource is constructed only from the retained result of
        // TISCopyCurrentKeyboardInputSource and releases that ownership once.
        unsafe { CFRelease(self.0.cast()) };
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn japanese_source_id_check_is_deliberately_conservative() {
        assert!("com.apple.inputmethod.Kotoeri.Japanese".contains("Japanese"));
        assert!(!"com.apple.keylayout.US".contains("Japanese"));
        assert!(!"japanese-but-unverified-case".contains("Japanese"));
    }
}
