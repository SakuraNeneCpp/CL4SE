use std::{
    ffi::{c_char, c_void, CStr},
    ptr,
};

use anyhow::{anyhow, bail, Context, Result};

type IoObject = u32;
type IoConnect = u32;
type KernReturn = i32;
type GetModifierLockState = unsafe extern "C" fn(IoConnect, i32, *mut bool) -> KernReturn;
type SetModifierLockState = unsafe extern "C" fn(IoConnect, i32, bool) -> KernReturn;

const KERN_SUCCESS: KernReturn = 0;
const IO_HID_PARAM_CONNECT_TYPE: u32 = 1;
const IO_HID_CAPS_LOCK_STATE: i32 = 1;
const RTLD_LAZY: i32 = 0x1;
const IOKIT_PATH: &[u8] = b"/System/Library/Frameworks/IOKit.framework/IOKit\0";
const IO_HID_SYSTEM_CLASS: &[u8] = b"IOHIDSystem\0";
const GET_SYMBOL: &[u8] = b"IOHIDGetModifierLockState\0";
const SET_SYMBOL: &[u8] = b"IOHIDSetModifierLockState\0";

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> IoObject;
    fn IOServiceOpen(
        service: IoObject,
        owning_task: u32,
        connect_type: u32,
        connect: *mut IoConnect,
    ) -> KernReturn;
    fn IOObjectRelease(object: IoObject) -> KernReturn;
    fn IOServiceClose(connect: IoConnect) -> KernReturn;
}

extern "C" {
    static mach_task_self_: u32;

    fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    fn dlclose(handle: *mut c_void) -> i32;
}

pub(crate) struct ModifierLock {
    connection: IoConnect,
    functions: IokitFunctions,
}

impl ModifierLock {
    pub(crate) fn open() -> Result<Self> {
        let functions = IokitFunctions::load()?;

        // SAFETY: IO_HID_SYSTEM_CLASS is a static null-terminated class name.
        // IOKit owns the returned matching dictionary.
        let matching = unsafe { IOServiceMatching(IO_HID_SYSTEM_CLASS.as_ptr().cast()) };
        if matching.is_null() {
            bail!("IOServiceMatching(IOHIDSystem) returned null");
        }

        // SAFETY: matching is the dictionary returned immediately above and
        // ownership is consumed by IOServiceGetMatchingService.
        let service = unsafe { IOServiceGetMatchingService(0, matching) };
        if service == 0 {
            bail!("IOHIDSystem service was not found");
        }
        let service = Service(service);

        let mut connection = 0;
        // SAFETY: service is a live IOHIDSystem service, connection points to
        // writable storage, and mach_task_self_ is the current task port.
        let status = unsafe {
            IOServiceOpen(
                service.0,
                mach_task_self_,
                IO_HID_PARAM_CONNECT_TYPE,
                &mut connection,
            )
        };
        if status != KERN_SUCCESS {
            bail!("IOServiceOpen(IOHIDSystem) failed with kern_return_t {status}");
        }

        Ok(Self {
            connection,
            functions,
        })
    }

    pub(crate) fn current_caps_lock_state(&self) -> Result<bool> {
        let mut state = false;
        // SAFETY: connection is an open IOHIDSystem parameter connection and
        // state points to valid writable storage for the duration of the call.
        let status =
            unsafe { (self.functions.get)(self.connection, IO_HID_CAPS_LOCK_STATE, &mut state) };
        if status != KERN_SUCCESS {
            bail!("IOHIDGetModifierLockState failed with kern_return_t {status}");
        }
        Ok(state)
    }

    pub(crate) fn toggle_caps_lock(&self) -> Result<()> {
        let state = self.current_caps_lock_state()?;
        let target = toggled_state(state);
        // SAFETY: connection is an open IOHIDSystem parameter connection and
        // the selector/state pair follows IOHIDLib's modifier-lock contract.
        let status =
            unsafe { (self.functions.set)(self.connection, IO_HID_CAPS_LOCK_STATE, target) };
        if status != KERN_SUCCESS {
            bail!("IOHIDSetModifierLockState failed with kern_return_t {status}");
        }
        Ok(())
    }
}

impl Drop for ModifierLock {
    fn drop(&mut self) {
        // SAFETY: ModifierLock owns the successful IOServiceOpen connection
        // and closes it exactly once before the dynamic library is unloaded.
        let _ = unsafe { IOServiceClose(self.connection) };
    }
}

struct Service(IoObject);

impl Drop for Service {
    fn drop(&mut self) {
        // SAFETY: Service owns the object returned by
        // IOServiceGetMatchingService and releases it exactly once.
        let _ = unsafe { IOObjectRelease(self.0) };
    }
}

struct IokitFunctions {
    get: GetModifierLockState,
    set: SetModifierLockState,
    _library: DynamicLibrary,
}

impl IokitFunctions {
    fn load() -> Result<Self> {
        let library = DynamicLibrary::open(IOKIT_PATH)?;
        let get = library.symbol(GET_SYMBOL)?;
        let set = library.symbol(SET_SYMBOL)?;

        // SAFETY: dlsym resolved the exact IOHIDGetModifierLockState symbol
        // from IOKit; Apple documents this function with this C signature.
        let get = unsafe { std::mem::transmute::<*mut c_void, GetModifierLockState>(get) };
        // SAFETY: dlsym resolved the exact IOHIDSetModifierLockState symbol
        // from IOKit; Apple documents this function with this C signature.
        let set = unsafe { std::mem::transmute::<*mut c_void, SetModifierLockState>(set) };

        Ok(Self {
            get,
            set,
            _library: library,
        })
    }
}

struct DynamicLibrary(*mut c_void);

impl DynamicLibrary {
    fn open(path: &'static [u8]) -> Result<Self> {
        // SAFETY: path is a static null-terminated byte string and RTLD_LAZY is
        // a valid dlopen mode. The returned handle is owned by this wrapper.
        let handle = unsafe { dlopen(path.as_ptr().cast(), RTLD_LAZY) };
        if handle.is_null() {
            bail!("could not load IOKit: {}", dynamic_link_error());
        }
        Ok(Self(handle))
    }

    fn symbol(&self, symbol: &'static [u8]) -> Result<*mut c_void> {
        // SAFETY: Calling dlerror clears any prior loader error before dlsym.
        unsafe { dlerror() };
        // SAFETY: self.0 is an open dlopen handle and symbol is a static
        // null-terminated symbol name.
        let address = unsafe { dlsym(self.0, symbol.as_ptr().cast()) };
        if address.is_null() {
            let name = CStr::from_bytes_with_nul(symbol)
                .context("invalid static IOKit symbol name")?
                .to_string_lossy();
            return Err(anyhow!(
                "IOKit symbol {name} is unavailable: {}",
                dynamic_link_error()
            ));
        }
        Ok(address)
    }
}

impl Drop for DynamicLibrary {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: DynamicLibrary owns this successful dlopen handle and
            // closes it exactly once after all copied function pointers cease
            // to be used.
            let _ = unsafe { dlclose(self.0) };
            self.0 = ptr::null_mut();
        }
    }
}

fn dynamic_link_error() -> String {
    // SAFETY: dlerror returns either null or a pointer to a thread-local,
    // null-terminated error string valid until the next loader call.
    let error = unsafe { dlerror() };
    if error.is_null() {
        "unknown dynamic loader error".to_owned()
    } else {
        // SAFETY: The non-null pointer returned by dlerror refers to a valid C
        // string for this immediate conversion.
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    }
}

const fn toggled_state(state: bool) -> bool {
    !state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_lock_toggle_inverts_the_observed_state() {
        assert!(toggled_state(false));
        assert!(!toggled_state(true));
    }
}
