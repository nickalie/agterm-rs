//! Owner-only files and directories on Windows, the counterpart of Unix's 0700 and 0600 for what carries an agent's
//! posts (the HUD body): the current user as the owner and a protected DACL, nothing inherited from the parent, that
//! grants full access to that user and SYSTEM alone, the pair the session endpoint's pipe grants
//! (agterm-session's `endpoint/windows.rs`). Windows has no mode bits, and a directory outside the user's profile
//! (a state directory override, `C:\tmp`) inherits whatever its parent grants, `BUILTIN\Users` among them.

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
#[cfg(test)]
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetTokenInformation,
    OBJECT_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    PSID, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// What is set on a path: its DACL, protected from the parent's inheritable grants (and its owner, when the
/// descriptor names one).
const APPLIED: OBJECT_SECURITY_INFORMATION = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;

/// Gives `path` to the current user alone: owner, full access for that user and SYSTEM, no other grant, none
/// inherited. A `directory` passes the same grants on to what is created in it.
///
/// # Errors
///
/// The current user is unknown, or the path's security cannot be set (another user's path, for one).
pub(crate) fn restrict(path: &Path, directory: bool) -> io::Result<()> {
    apply(path, &owner_only_sddl(&current_user_sid()?, directory))
}

/// The owner-only descriptor in SDDL: `user`'s and SYSTEM's full access, inheritable by files and subdirectories for
/// a `directory`.
fn owner_only_sddl(user: &str, directory: bool) -> String {
    let inherit = if directory { "OICI" } else { "" };
    format!("O:{user}D:P(A;{inherit};FA;;;SY)(A;{inherit};FA;;;{user})")
}

/// Sets the protected DACL, and the owner when it names one, of the descriptor `sddl` spells on `path`.
fn apply(path: &Path, sddl: &str) -> io::Result<()> {
    let descriptor = Descriptor::from_sddl(sddl)?;
    let (owner, dacl) = descriptor.owner_and_dacl()?;
    let applied = if owner.is_null() {
        APPLIED
    } else {
        APPLIED | OWNER_SECURITY_INFORMATION
    };
    let name = wide(path.as_os_str())?;
    // SAFETY: `name` is a NUL-terminated UTF-16 path; `owner` (null when not set) and `dacl` point into `descriptor`,
    // which outlives the call; the group and the SACL are not set.
    let status =
        unsafe { SetNamedSecurityInfoW(name.as_ptr(), SE_FILE_OBJECT, applied, owner, null_mut(), dacl, null()) };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    Ok(())
}

/// A self-relative security descriptor the system allocated, freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text = wide(std::ffi::OsStr::new(sddl))?;
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `text` is a NUL-terminated UTF-16 string and `descriptor` outlives the call; the size is not asked.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    /// The owner SID (null when the descriptor names none) and the DACL, both pointing into the descriptor.
    fn owner_and_dacl(&self) -> io::Result<(PSID, *mut ACL)> {
        let mut owner: PSID = null_mut();
        let mut defaulted = 0;
        // SAFETY: the descriptor is a valid one the system built; `owner` and `defaulted` outlive the call.
        if unsafe { GetSecurityDescriptorOwner(self.0, &raw mut owner, &raw mut defaulted) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut present = 0;
        let mut dacl: *mut ACL = null_mut();
        // SAFETY: as above; `present`, `dacl` and `defaulted` outlive the call.
        if unsafe { GetSecurityDescriptorDacl(self.0, &raw mut present, &raw mut dacl, &raw mut defaulted) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if present == 0 || dacl.is_null() {
            return Err(io::Error::other("the descriptor has no DACL"));
        }
        Ok((owner, dacl))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the system allocated the descriptor with LocalAlloc, and nothing else frees it.
        unsafe { LocalFree(self.0) };
    }
}

/// The current process's user SID in its string form (`S-1-5-21-…`).
fn current_user_sid() -> io::Result<String> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing and is always valid.
    let process = unsafe { GetCurrentProcess() };
    let mut raw: HANDLE = null_mut();
    // SAFETY: `process` is valid for the call and `raw` outlives it.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken returned a fresh handle that nothing else owns.
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    let user = token_user(&token)?;
    // SAFETY: GetTokenInformation(TokenUser) filled the aligned buffer with a TOKEN_USER at its start.
    let sid = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() }.User.Sid;
    let mut text: *mut u16 = null_mut();
    // SAFETY: `sid` points into `user`, alive across the call, and `text` outlives it.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ConvertSidToStringSidW returned a NUL-terminated string allocated with LocalAlloc, now ours.
    Ok(unsafe { take_local_string(text) })
}

/// The token's `TokenUser` information, in `usize` storage so the structure at its start is aligned and the SID it
/// points to lives with it.
fn token_user(token: &OwnedHandle) -> io::Result<Vec<usize>> {
    let mut needed = 0_u32;
    // SAFETY: a null buffer of length zero only asks for the size, written to `needed`, which outlives the call.
    unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &raw mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let words = usize::try_from(needed)
        .map_err(io::Error::other)?
        .div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    // SAFETY: the buffer holds at least `needed` bytes, and `needed` outlives the call.
    let filled = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            needed,
            &raw mut needed,
        )
    };
    if filled == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

/// Copies out and frees a string the system allocated.
///
/// # Safety
///
/// `text` is a NUL-terminated UTF-16 string allocated with `LocalAlloc` that the caller owns.
unsafe fn take_local_string(text: *mut u16) -> String {
    let mut len = 0;
    // SAFETY: the caller guarantees a NUL terminator, so every unit up to it is in bounds.
    while unsafe { *text.wrapping_add(len) } != 0 {
        len += 1;
    }
    // SAFETY: the `len` units before the terminator were just read.
    let units = unsafe { std::slice::from_raw_parts(text, len) };
    let string = String::from_utf16_lossy(units);
    // SAFETY: the caller hands the allocation over, and nothing reads it after this.
    unsafe { LocalFree(text.cast::<c_void>()) };
    string
}

/// `text` as a NUL-terminated UTF-16 string; one with a NUL of its own is refused.
fn wide(text: &std::ffi::OsStr) -> io::Result<Vec<u16>> {
    let mut units: Vec<u16> = text.encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL in it"));
    }
    units.push(0);
    Ok(units)
}

/// The owner and DACL `path` has, in SDDL (with the system's own abbreviations of well-known SIDs), less the
/// auto-inherited flag (`AI`) the system sets on every DACL `SetNamedSecurityInfoW` writes: the grants and their
/// protection are what is compared, and an inherited grant still shows on its ACE (`ID`).
#[cfg(test)]
pub(crate) fn sddl_of(path: &Path) -> io::Result<String> {
    let name = wide(path.as_os_str())?;
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `name` is a NUL-terminated UTF-16 path and `descriptor` outlives the call; only the descriptor itself is
    // asked for, the owner and DACL pointers into it are not.
    let status = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status.cast_signed()));
    }
    let sddl = Descriptor(descriptor).to_sddl()?;
    Ok(sddl.replacen("D:PAI(", "D:P(", 1).replacen("D:AI(", "D:(", 1))
}

/// The owner-only descriptor `restrict` sets, in the same SDDL spelling [`sddl_of`] reads back.
#[cfg(test)]
pub(crate) fn owner_only_sddl_of_current_user(directory: bool) -> io::Result<String> {
    Descriptor::from_sddl(&owner_only_sddl(&current_user_sid()?, directory))?.to_sddl()
}

/// Sets the DACL (and the owner, if named) of `sddl` on `path`: a test's way to leave a path open to others.
#[cfg(test)]
pub(crate) fn apply_sddl(path: &Path, sddl: &str) -> io::Result<()> {
    apply(path, sddl)
}

#[cfg(test)]
impl Descriptor {
    fn to_sddl(&self) -> io::Result<String> {
        let mut text: *mut u16 = null_mut();
        // SAFETY: the descriptor is valid and `text` outlives the call; the length is not asked.
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                self.0,
                SDDL_REVISION_1,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &raw mut text,
                null_mut(),
            )
        };
        if converted == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the conversion returned a NUL-terminated string allocated with LocalAlloc, now ours.
        Ok(unsafe { take_local_string(text) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_grants_the_user_and_system_alone_and_inherits_nothing() {
        let user = current_user_sid().unwrap();
        assert!(user.starts_with("S-1-5-"), "{user}");
        assert_eq!(
            owner_only_sddl(&user, true),
            format!("O:{user}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{user})")
        );
        assert_eq!(
            owner_only_sddl(&user, false),
            format!("O:{user}D:P(A;;FA;;;SY)(A;;FA;;;{user})")
        );
    }

    #[test]
    fn a_path_shared_with_everyone_is_given_back_to_its_owner() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        apply_sddl(&shared, "D:P(A;OICI;FA;;;WD)").unwrap();
        assert!(sddl_of(&shared).unwrap().contains(";WD)"), "everyone is granted first");

        restrict(&shared, true).unwrap();
        assert_eq!(
            sddl_of(&shared).unwrap(),
            owner_only_sddl_of_current_user(true).unwrap()
        );
        let file = shared.join("body.txt");
        std::fs::write(&file, "secret").unwrap();
        restrict(&file, false).unwrap();
        assert_eq!(sddl_of(&file).unwrap(), owner_only_sddl_of_current_user(false).unwrap());
    }
}
