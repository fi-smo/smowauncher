//! Secrets in Windows Credential Manager (per user, encrypted by Windows): the Anthropic API
//! key lives here, never in config.toml.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::Security::Credentials::{
    CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW, CredWriteW,
};
use windows::core::{HSTRING, PWSTR};

pub fn read(target: &str) -> Option<String> {
    unsafe {
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        CredReadW(&HSTRING::from(target), CRED_TYPE_GENERIC, None, &mut cred).ok()?;
        let c = &*cred;
        let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let value = String::from_utf8(blob.to_vec()).ok();
        CredFree(cred as *const _);
        value.filter(|v| !v.is_empty())
    }
}

pub fn write(target: &str, value: &str) -> Result<(), String> {
    let mut target_w: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let mut user: Vec<u16> = "Smowauncher".encode_utf16().chain(std::iter::once(0)).collect();
    let mut blob = value.as_bytes().to_vec();
    let cred = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target_w.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user.as_mut_ptr()),
        LastWritten: FILETIME::default(),
        ..Default::default()
    };
    unsafe { CredWriteW(&cred, 0) }.map_err(|e| e.to_string())
}

pub fn delete(target: &str) {
    unsafe {
        let _ = CredDeleteW(&HSTRING::from(target), CRED_TYPE_GENERIC, None);
    }
}
