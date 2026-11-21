//! Windows Credential Manager (`CredReadW` / `CredWriteW` / `CredDeleteW`) wrapper.
#![expect(
    unsafe_code,
    reason = "Windows Credential Manager is a Win32 C API without safe wrappers in the windows crate"
)]

use std::ptr;

use windows::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME};
use windows::Win32::Security::Credentials::{
    CRED_FLAGS, CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_MAX_GENERIC_TARGET_NAME_LENGTH,
    CRED_MAX_USERNAME_LENGTH, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW,
    CredDeleteW, CredFree, CredReadW, CredWriteW,
};
use windows::core::{HRESULT, PCWSTR, PWSTR};

use super::to_wide_null;

/// Reads a generic credential password from Windows Credential Manager.
///
/// Uses `{user}.{service}` as the primary target name (matching `keyring` 3.6.3's
/// Windows format) and falls back to `{service}:{user}` if not found.
pub fn read_password(service: &str, user: &str) -> Result<Option<String>, String> {
    let primary = primary_target(service, user);
    if let Some(password) = read_target(&primary)? {
        return Ok(Some(password));
    }
    let fallback = fallback_target(service, user);
    read_target(&fallback)
}

/// Writes a generic credential password to Windows Credential Manager as UTF-16LE.
pub fn write_password(service: &str, user: &str, password: &str) -> Result<(), String> {
    let target_name = primary_target(service, user);
    if target_name.encode_utf16().count() > CRED_MAX_GENERIC_TARGET_NAME_LENGTH as usize {
        return Err("credential target name exceeds Windows limit".to_owned());
    }
    if user.encode_utf16().count() > CRED_MAX_USERNAME_LENGTH as usize {
        return Err("credential username exceeds Windows limit".to_owned());
    }

    let mut blob: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
    if blob.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
        zeroize_bytes(&mut blob);
        return Err("credential password exceeds Windows limit".to_owned());
    }

    let mut target_wide = to_wide_null(&target_name);
    let mut user_wide = to_wide_null(user);

    let cred = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target_wide.as_mut_ptr()),
        Comment: PWSTR(ptr::null_mut()),
        LastWritten: FILETIME::default(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: ptr::null_mut(),
        TargetAlias: PWSTR(ptr::null_mut()),
        UserName: PWSTR(user_wide.as_mut_ptr()),
    };

    // SAFETY: `cred` and all buffers it points to (`target_wide`, `user_wide`, `blob`)
    // remain alive and valid for the duration of `CredWriteW`.
    let result = unsafe { CredWriteW(&cred, 0) }.map_err(|err| err.to_string());
    zeroize_bytes(&mut blob);
    result
}

/// Deletes a generic credential from Windows Credential Manager.
///
/// Missing entries (`ERROR_NOT_FOUND`) are treated as success.
pub fn delete_password(service: &str, user: &str) -> Result<(), String> {
    let primary = primary_target(service, user);
    let deleted_primary = delete_target(&primary)?;
    if !deleted_primary {
        let fallback = fallback_target(service, user);
        let _ = delete_target(&fallback)?;
    }
    Ok(())
}

fn primary_target(service: &str, user: &str) -> String {
    format!("{user}.{service}")
}

fn fallback_target(service: &str, user: &str) -> String {
    format!("{service}:{user}")
}

fn read_target(target_name: &str) -> Result<Option<String>, String> {
    let target_wide = to_wide_null(target_name);
    let mut cred_ptr: *mut CREDENTIALW = ptr::null_mut();

    // SAFETY: `target_wide` is null-terminated UTF-16 and `cred_ptr` is a valid out-pointer.
    let read = unsafe {
        CredReadW(
            PCWSTR(target_wide.as_ptr()),
            CRED_TYPE_GENERIC,
            None,
            &mut cred_ptr,
        )
    };

    if let Err(err) = read {
        if err.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) {
            return Ok(None);
        }
        return Err(err.to_string());
    }

    if cred_ptr.is_null() {
        return Ok(None);
    }

    let _guard = CredFreeGuard(cred_ptr);

    // SAFETY: `CredReadW` succeeded and returned a non-null `CREDENTIALW` pointer valid
    // until `_guard` calls `CredFree`.
    let cred = unsafe { &*cred_ptr };
    let blob_len = cred.CredentialBlobSize as usize;
    if blob_len == 0 || cred.CredentialBlob.is_null() {
        return Ok(Some(String::new()));
    }

    // SAFETY: `CredentialBlob` is valid for `CredentialBlobSize` writable bytes allocated
    // by `CredReadW` and owned until `CredFree`.
    let mut bytes = unsafe {
        let slice = std::slice::from_raw_parts_mut(cred.CredentialBlob, blob_len);
        let copy = slice.to_vec();
        zeroize_bytes(slice);
        copy
    };

    let decoded = decode_utf16le_password(&bytes);
    zeroize_bytes(&mut bytes);
    decoded.map(Some)
}

/// Returns `Ok(true)` if an entry existed and was deleted, `Ok(false)` if not found.
fn delete_target(target_name: &str) -> Result<bool, String> {
    let target_wide = to_wide_null(target_name);
    // SAFETY: `target_wide` is null-terminated UTF-16 valid for the duration of `CredDeleteW`.
    match unsafe { CredDeleteW(PCWSTR(target_wide.as_ptr()), CRED_TYPE_GENERIC, None) } {
        Ok(()) => Ok(true),
        Err(err) if err.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0) => Ok(false),
        Err(err) => Err(err.to_string()),
    }
}

fn decode_utf16le_password(bytes: &[u8]) -> Result<String, String> {
    if !bytes.len().is_multiple_of(2) {
        return Err("stored credential has invalid UTF-16 byte length".to_owned());
    }
    let mut units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    let result = String::from_utf16(&units)
        .map_err(|err| format!("stored credential is not valid UTF-16: {err}"));
    zeroize_u16(&mut units);
    result
}

fn zeroize_bytes(buf: &mut [u8]) {
    for byte in buf {
        // SAFETY: `byte` is a valid, aligned mutable reference to a `u8`.
        unsafe { ptr::write_volatile(byte, 0) };
    }
}

fn zeroize_u16(buf: &mut [u16]) {
    for unit in buf {
        // SAFETY: `unit` is a valid, aligned mutable reference to a `u16`.
        unsafe { ptr::write_volatile(unit, 0) };
    }
}

struct CredFreeGuard(*mut CREDENTIALW);

impl Drop for CredFreeGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` was allocated by `CredReadW` and is freed exactly once here.
            unsafe { CredFree(self.0.cast()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_names_match_keyring_format_and_fallback() {
        assert_eq!(
            primary_target("zenha", "https://voz.exemplo"),
            "https://voz.exemplo.zenha"
        );
        assert_eq!(
            fallback_target("zenha", "https://voz.exemplo"),
            "zenha:https://voz.exemplo"
        );
    }

    #[test]
    fn utf16le_round_trip_decodes_ascii_and_unicode() {
        for password in ["segredo-123", "senha-com-ação-🚀", ""] {
            let encoded: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
            assert_eq!(decode_utf16le_password(&encoded).unwrap(), password);
        }
        assert!(decode_utf16le_password(&[0x41]).is_err());
    }

    #[test]
    fn credential_manager_round_trip_ephemeral_entry() {
        let service = format!("zenha-selftest-{}", std::process::id());
        let user = "https://selftest.invalid";
        let secret = "segredo-ação-🚀";

        let _ = delete_password(&service, user);
        assert_eq!(read_password(&service, user).unwrap(), None);

        write_password(&service, user, secret).unwrap();
        let read_back = read_password(&service, user);
        let deleted = delete_password(&service, user);

        assert_eq!(read_back.unwrap().as_deref(), Some(secret));
        assert!(deleted.is_ok());
        assert_eq!(read_password(&service, user).unwrap(), None);
        // Deleting a missing entry succeeds idempotently.
        assert!(delete_password(&service, user).is_ok());
    }
}
