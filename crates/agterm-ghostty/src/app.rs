//! Process-wide libghostty initialisation.

use std::ffi::{CString, c_char};
use std::os::unix::ffi::OsStringExt;
use std::sync::OnceLock;

use crate::Error;

static INIT: OnceLock<Result<(), Error>> = OnceLock::new();

/// Initialise libghostty once for the process; later calls return the first call's outcome.
///
/// Every other entry point of this crate calls it first. Only `argv[0]` is handed over: libghostty scans its argv
/// for `+action` CLI verbs, and agterm's own arguments are not ghostty's.
///
/// # Errors
///
/// [`Error::Init`] when `ghostty_init` fails, now or on the first call.
pub fn init() -> Result<(), Error> {
    *INIT.get_or_init(init_once)
}

fn init_once() -> Result<(), Error> {
    let argv0 = std::env::args_os()
        .next()
        .and_then(|arg| CString::new(arg.into_vec()).ok())
        .unwrap_or_else(|| c"agterm".to_owned());
    // Leaked on purpose: libghostty keeps the vector as its process argv for the rest of the process.
    let argv: &'static mut [*mut c_char; 2] = Box::leak(Box::new([argv0.into_raw(), std::ptr::null_mut()]));
    // SAFETY: argv is a null-terminated vector of one NUL-terminated string, alive for the whole process, and the
    // OnceLock guarantees ghostty_init runs once.
    let status = unsafe { agterm_ghostty_sys::ghostty_init(1, argv.as_mut_ptr()) };
    if status == 0 { Ok(()) } else { Err(Error::Init(status)) }
}
