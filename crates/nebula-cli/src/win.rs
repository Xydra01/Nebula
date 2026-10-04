//! Win32 calls the CLI needs.
#![allow(unsafe_code)]

use windows::Win32::Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation};
use windows::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

/// Makes this process's stdin/stdout/stderr non-inheritable.
///
/// `std::process::Command` passes every inheritable handle to the child, even with null stdio.
/// A detached daemon would otherwise hold the caller's pipes open (e.g. an SSH session's or a
/// `| tail`), and the caller would wait for end-of-file until the daemon exits.
pub(crate) fn stop_inheriting_std_handles() {
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle has no preconditions; SetHandleInformation only changes a flag on
        // a handle this process owns, and fails harmlessly on null or console pseudo-handles.
        unsafe {
            if let Ok(h) = GetStdHandle(id)
                && !h.is_invalid()
                && !h.0.is_null()
            {
                let _ = SetHandleInformation(h, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
            }
        }
    }
}
