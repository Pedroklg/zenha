//! Persistência da senha do servidor no Gerenciador de Credenciais do Windows.

const SERVICE: &str = "zenha";

/// Carrega a senha salva para `server` no Credential Manager do Windows.
/// Retorna `None` se não houver entrada salva, se `server` estiver vazio, ou em
/// plataformas não-Windows; falhas de acesso são apenas registradas em log.
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

/// Salva a senha de `server` no Credential Manager do Windows de forma síncrona.
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

/// Remove a senha salva para `server` no Credential Manager do Windows de forma síncrona.
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

/// Salva a senha em uma thread curta para não bloquear a interface gráfica.
pub fn store_in_background(server: String, password: String) {
    if server.trim().is_empty() || password.is_empty() {
        return;
    }
    std::thread::spawn(move || store(&server, &password));
}

/// Remove a senha em uma thread curta para não bloquear a interface gráfica.
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
