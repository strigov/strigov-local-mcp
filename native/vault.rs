#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::Result;
const SERVICE: &str = "app.localfiles.runtime-key";

pub fn validate(secret: &str) -> Result<()> {
    anyhow::ensure!(
        !secret.trim().is_empty()
            && secret.len() <= 16_384
            && !secret.chars().any(char::is_control),
        "Введите действующий runtime API key из OpenAI Platform"
    );
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn set(account: &str, secret: &str) -> Result<()> {
    validate(secret)?;
    security_framework::passwords::set_generic_password(SERVICE, account, secret.as_bytes())
        .context("Не удалось сохранить ключ в macOS Keychain")
}
#[cfg(target_os = "macos")]
pub fn get(account: &str) -> Result<Vec<u8>> {
    Ok(security_framework::passwords::generic_password(
        security_framework::passwords::PasswordOptions::new_generic_password(SERVICE, account),
    )
    .context("Ключ не найден в macOS Keychain. Настройте подключение заново.")?
    .to_vec())
}
#[cfg(target_os = "macos")]
pub fn delete(account: &str) -> Result<()> {
    security_framework::passwords::delete_generic_password(SERVICE, account)
        .context("Не удалось удалить ключ из macOS Keychain")
}

#[cfg(all(target_os = "windows", feature = "desktop"))]
mod windows {
    use super::*;
    use windows_sys::Win32::{Foundation::GetLastError, Security::Credentials::*};
    fn target(account: &str) -> Vec<u16> {
        format!("{SERVICE}/{account}")
            .encode_utf16()
            .chain(Some(0))
            .collect()
    }
    pub fn set(account: &str, secret: &str) -> Result<()> {
        validate(secret)?;
        let mut name = target(account);
        let mut bytes = secret.as_bytes().to_vec();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: name.as_mut_ptr(),
            CredentialBlobSize: bytes.len() as u32,
            CredentialBlob: bytes.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..unsafe { std::mem::zeroed() }
        };
        let ok = unsafe { CredWriteW(&credential, 0) };
        anyhow::ensure!(
            ok != 0,
            "Credential Manager could not save the key ({})",
            unsafe { GetLastError() }
        );
        Ok(())
    }
    pub fn get(account: &str) -> Result<Vec<u8>> {
        let mut credential = std::ptr::null_mut();
        let ok = unsafe {
            CredReadW(
                target(account).as_ptr(),
                CRED_TYPE_GENERIC,
                0,
                &mut credential,
            )
        };
        anyhow::ensure!(ok != 0, "Ключ не найден в Windows Credential Manager");
        let value = unsafe {
            std::slice::from_raw_parts(
                (*credential).CredentialBlob,
                (*credential).CredentialBlobSize as usize,
            )
            .to_vec()
        };
        unsafe { CredFree(credential.cast()) };
        Ok(value)
    }
    pub fn delete(account: &str) -> Result<()> {
        anyhow::ensure!(
            unsafe { CredDeleteW(target(account).as_ptr(), CRED_TYPE_GENERIC, 0) } != 0,
            "Не удалось удалить ключ из Windows Credential Manager"
        );
        Ok(())
    }
}
#[cfg(all(target_os = "windows", feature = "desktop"))]
pub use windows::{delete, get, set};

#[cfg(not(any(target_os = "macos", all(target_os = "windows", feature = "desktop"))))]
pub fn get(_account: &str) -> Result<Vec<u8>> {
    anyhow::bail!("System credential storage requires macOS or the Windows desktop build")
}
