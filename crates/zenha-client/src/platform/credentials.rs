//! Persisting the server password in Windows Credential Manager.

#[cfg(windows)]
const SERVICE: &str = "zenha";
#[cfg(windows)]
const IDENTITY_SERVICE: &str = "zenha-identity";
#[cfg(windows)]
const IDENTITY_USER: &str = "client_id";

/// Loads the saved password for `server` from Windows Credential Manager.
/// Returns `None` if there is no saved entry, if `server` is empty, or on
/// non-Windows platforms; access failures are only logged.
#[cfg(windows)]
pub fn load(server: &str) -> Option<String> {
    let user = server.trim();
    if user.is_empty() {
        return None;
    }
    match zenha_core::win_platform::credentials::read_password(SERVICE, user) {
        Ok(Some(password)) if !password.is_empty() => Some(password),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!(%err, server = %user, "failed to load server password");
            None
        }
    }
}

#[cfg(not(windows))]
pub fn load(_server: &str) -> Option<String> {
    None
}

/// Saves the password for `server` in Windows Credential Manager synchronously.
#[cfg(windows)]
pub fn store(server: &str, password: &str) {
    let user = server.trim();
    if user.is_empty() || password.is_empty() {
        return;
    }
    if let Err(err) = zenha_core::win_platform::credentials::write_password(SERVICE, user, password)
    {
        tracing::warn!(%err, server = %user, "failed to save server password");
    }
}

#[cfg(not(windows))]
pub fn store(_server: &str, _password: &str) {}

/// Removes the saved password for `server` from Windows Credential Manager synchronously.
#[cfg(windows)]
pub fn forget(server: &str) {
    let user = server.trim();
    if user.is_empty() {
        return;
    }
    if let Err(err) = zenha_core::win_platform::credentials::delete_password(SERVICE, user) {
        tracing::warn!(%err, server = %user, "failed to delete server password");
    }
}

#[cfg(not(windows))]
pub fn forget(_server: &str) {}

/// Saves the password on a short-lived thread so the GUI is not blocked.
pub fn store_in_background(server: String, password: String) {
    if server.trim().is_empty() || password.is_empty() {
        return;
    }
    std::thread::spawn(move || store(&server, &password));
}

/// Removes the password on a short-lived thread so the GUI is not blocked.
pub fn forget_in_background(server: String) {
    if server.trim().is_empty() {
        return;
    }
    std::thread::spawn(move || forget(&server));
}

/// Loads the saved `client_id` from Windows Credential Manager.
#[cfg(windows)]
pub fn load_identity() -> Option<String> {
    match zenha_core::win_platform::credentials::read_password(IDENTITY_SERVICE, IDENTITY_USER) {
        Ok(Some(id)) if !id.is_empty() => Some(id),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!(%err, "failed to load client id");
            None
        }
    }
}

#[cfg(not(windows))]
pub fn load_identity() -> Option<String> {
    None
}

/// Saves the `client_id` in Windows Credential Manager synchronously.
#[cfg(windows)]
pub fn store_identity(client_id: &str) {
    let id = client_id.trim();
    if id.is_empty() {
        return;
    }
    if let Err(err) =
        zenha_core::win_platform::credentials::write_password(IDENTITY_SERVICE, IDENTITY_USER, id)
    {
        tracing::warn!(%err, "failed to save client id");
    }
}

#[cfg(not(windows))]
pub fn store_identity(_client_id: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_server_does_not_hit_keyring() {
        assert_eq!(load(""), None);
        assert_eq!(load("   "), None);
        store("", "segredo");
        store("   ", "segredo");
        store("https://voz.exemplo", "");
        forget("");
        forget("   ");
        store_identity("");
        store_identity("   ");
    }
}
