#[derive(Debug, Clone, Copy)]
pub(crate) struct TccStatus {
    pub(crate) input_monitoring: bool,
    pub(crate) event_posting: bool,
    pub(crate) accessibility: bool,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGPreflightPostEventAccess() -> bool;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

pub(crate) fn tcc_status() -> TccStatus {
    // SAFETY: These preflight functions have no arguments, do not prompt, and
    // return the current process's TCC authorization state.
    let input_monitoring = unsafe { CGPreflightListenEventAccess() };
    // SAFETY: See above; this separately preflights synthetic event posting.
    let event_posting = unsafe { CGPreflightPostEventAccess() };
    // SAFETY: AXIsProcessTrusted has no arguments and only reads the current
    // process's Accessibility trust state.
    let accessibility = unsafe { AXIsProcessTrusted() };
    TccStatus {
        input_monitoring,
        event_posting,
        accessibility,
    }
}
