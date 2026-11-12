//! OS keychain access used to persist the session token securely.

use crate::error::{AppError, AppResult};

/// Service name under which every credential is stored.
pub const KEYCHAIN_SERVICE: &str = "org.qestudio.agent";

fn entry(account: &str) -> AppResult<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, account).map_err(|error| AppError::msg(error.to_string()))
}

#[tauri::command]
pub fn keychain_get(account: String) -> AppResult<Option<String>> {
    match entry(&account)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(AppError::msg(error.to_string())),
    }
}

#[tauri::command]
pub fn keychain_set(account: String, secret: String) -> AppResult<()> {
    entry(&account)?
        .set_password(&secret)
        .map_err(|error| AppError::msg(error.to_string()))
}

#[tauri::command]
pub fn keychain_delete(account: String) -> AppResult<()> {
    match entry(&account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(AppError::msg(error.to_string())),
    }
}
