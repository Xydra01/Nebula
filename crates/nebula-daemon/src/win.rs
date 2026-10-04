//! The Win32 calls: the single-instance mutex, the pipe's security descriptor and
//! Credential Manager reads.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::Credentials::{CRED_TYPE_GENERIC, CREDENTIALW, CredFree, CredReadW};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

fn io_err(e: &windows::core::Error) -> io::Error {
    io::Error::other(e.message())
}

/// Holds the named mutex; released on drop.
pub struct InstanceGuard(HANDLE);

// SAFETY: a mutex handle may be closed from any thread.
unsafe impl Send for InstanceGuard {}
// SAFETY: the handle is never used after creation except in Drop.
unsafe impl Sync for InstanceGuard {}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: closes the handle created in `acquire_instance`, once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Creates (and owns) the named mutex `name`. `Ok(None)` if another process holds it.
///
/// # Errors
/// If the mutex can't be created at all.
pub fn acquire_instance(name: &str) -> io::Result<Option<InstanceGuard>> {
    let wide = HSTRING::from(name);
    // SAFETY: default security, initially owned, `wide` outlives the call.
    let handle = unsafe { CreateMutexW(None, true, &wide) }.map_err(|e| io_err(&e))?;
    // SAFETY: reads the calling thread's last error, set by CreateMutexW just above.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // SAFETY: closing the handle we just received.
        let _ = unsafe { CloseHandle(handle) };
        return Ok(None);
    }
    Ok(Some(InstanceGuard(handle)))
}

/// The current user's SID as a string, e.g. `S-1-5-21-...`.
///
/// # Errors
/// If the process token can't be queried.
pub fn current_user_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: pseudo-handle for this process; out-pointer to a local.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }
        .map_err(|e| io_err(&e))?;
    let result = (|| {
        let mut len = 0u32;
        // SAFETY: size query; fails with ERROR_INSUFFICIENT_BUFFER and sets `len`.
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &raw mut len) };
        // u64 elements keep the buffer aligned for TOKEN_USER.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: `buf` holds at least `len` bytes.
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                len,
                &raw mut len,
            )
        }
        .map_err(|e| io_err(&e))?;
        // SAFETY: GetTokenInformation(TokenUser) wrote a TOKEN_USER at the start of `buf`.
        let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
        let mut sid = PWSTR::null();
        // SAFETY: the SID points into `buf`, which is alive; out-pointer to a local.
        unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut sid) }.map_err(|e| io_err(&e))?;
        // SAFETY: `sid` is a NUL-terminated string allocated by the call above.
        let text = unsafe { sid.to_string() }.map_err(io::Error::other);
        // SAFETY: frees the string allocated by ConvertSidToStringSidW.
        let _ = unsafe { LocalFree(Some(HLOCAL(sid.0.cast()))) };
        text
    })();
    // SAFETY: closes the token opened above.
    let _ = unsafe { CloseHandle(token) };
    result
}

/// A security descriptor built from SDDL, used for every pipe instance.
pub struct PipeSecurity {
    sd: PSECURITY_DESCRIPTOR,
}

// SAFETY: the descriptor is immutable after creation and only read by CreateNamedPipe.
unsafe impl Send for PipeSecurity {}
// SAFETY: as above.
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// Full access for the current user only; nobody else (not even administrators) can
    /// open the pipe.
    ///
    /// # Errors
    /// If the SID can't be read or the SDDL is rejected.
    pub fn current_user_only() -> io::Result<Self> {
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{})", current_user_sid()?));
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` outlives the call; out-pointer to a local.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &sddl,
                SDDL_REVISION_1,
                &raw mut sd,
                None,
            )
        }
        .map_err(|e| io_err(&e))?;
        Ok(Self { sd })
    }

    /// Creates a pipe instance with this descriptor, rejecting remote clients.
    ///
    /// # Errors
    /// If the pipe can't be created (e.g. `first` and another process owns the name).
    pub fn create(&self, path: &str, first: bool) -> io::Result<NamedPipeServer> {
        let mut attrs = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
            lpSecurityDescriptor: self.sd.0,
            bInheritHandle: false.into(),
        };
        // SAFETY: `attrs` is a valid SECURITY_ATTRIBUTES whose descriptor lives as long as
        // `self`, and it is only read during the call.
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(path, (&raw mut attrs).cast::<c_void>())
        }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: frees the descriptor allocated by the SDDL conversion, once.
        let _ = unsafe { LocalFree(Some(HLOCAL(self.sd.0))) };
    }
}

/// Reads a generic credential's secret (stored as UTF-16, as `store-bot-token.ps1` does).
/// `None` if it doesn't exist.
#[must_use]
pub fn read_credential(target: &str) -> Option<String> {
    let name = HSTRING::from(target);
    let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `name` outlives the call; out-pointer to a local.
    unsafe { CredReadW(&name, CRED_TYPE_GENERIC, None, &raw mut cred) }.ok()?;
    if cred.is_null() {
        return None;
    }
    // SAFETY: CredReadW succeeded, so `cred` points to a valid CREDENTIALW whose blob has
    // `CredentialBlobSize` bytes.
    let text = unsafe {
        let c = &*cred;
        let units = c.CredentialBlobSize as usize / 2;
        if c.CredentialBlob.is_null() || units == 0 {
            None
        } else {
            let wide = std::slice::from_raw_parts(c.CredentialBlob.cast::<u16>(), units);
            Some(String::from_utf16_lossy(wide))
        }
    };
    // SAFETY: frees the buffer returned by CredReadW, once.
    unsafe { CredFree(cred.cast_const().cast()) };
    text
}
