//! Persisting the server password in Windows Credential Manager.

const SERVICE: &str = "zenha";

/// Loads the saved password for `server` from Windows Credential Manager.
/// Returns `None` if there is no saved entry, if `server` is empty, or on
/// non-Windows platforms; access failures are only logged.
#[cfg(windows)]
pub fn load(server: &str) -> Option<String> {
    let (user, entry) = entry_for(server)?;
    match entry.get_password() {
        Ok(password) if !password.is_empty() => Some(password),
        Ok(_) | Err(keyring::Error::NoEntry) => None,
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
    if password.is_empty() {
        return;
    }
    let Some((user, entry)) = entry_for(server) else {
        return;
    };
    if let Err(err) = entry.set_password(password) {
        tracing::warn!(%err, server = %user, "failed to save server password");
    }
}

#[cfg(not(windows))]
pub fn store(_server: &str, _password: &str) {}

/// Removes the saved password for `server` from Windows Credential Manager synchronously.
#[cfg(windows)]
pub fn forget(server: &str) {
    let Some((user, entry)) = entry_for(server) else {
        return;
    };
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(err) => {
            tracing::warn!(%err, server = %user, "failed to delete server password");
        }
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

#[cfg(windows)]
fn entry_for(server: &str) -> Option<(&str, keyring::Entry)> {
    let user = server.trim();
    if user.is_empty() {
        return None;
    }
    match keyring::Entry::new(SERVICE, user) {
        Ok(entry) => Some((user, entry)),
        Err(err) => {
            tracing::warn!(%err, server = %user, "failed to open credential entry");
            None
        }
    }
}

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
    }
}
