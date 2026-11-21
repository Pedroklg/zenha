//! System file dialogs (Windows only; on other platforms they open nothing).

use std::path::PathBuf;

#[cfg(windows)]
pub fn pick_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Enviar arquivo")
        .pick_file()
}

#[cfg(windows)]
pub fn pick_avatar() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Escolher foto")
        .add_filter("Imagens", &["png", "jpg", "jpeg", "webp"])
        .pick_file()
}

#[cfg(windows)]
pub fn pick_save_path(name: &str) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Salvar arquivo")
        .set_file_name(name)
        .save_file()
}

#[cfg(windows)]
pub fn show_error(text: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(crate::APP_NAME)
        .set_description(text)
        .show();
}

#[cfg(windows)]
pub fn show_info(text: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Info)
        .set_title(crate::APP_NAME)
        .set_description(text)
        .show();
}

/// Opens Explorer with the file selected.
#[cfg(windows)]
pub fn reveal_file(path: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    if let Err(err) = std::process::Command::new("explorer")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn()
    {
        tracing::debug!(%err, "could not open explorer");
    }
}

/// Opens a non-web link (like `steam://`) with the app registered for its scheme.
#[cfg(windows)]
pub fn open_with_shell(url: &str) {
    if let Err(err) = std::process::Command::new("explorer").arg(url).spawn() {
        tracing::debug!(%err, "could not open link");
    }
}

#[cfg(not(windows))]
pub fn show_error(_text: &str) {}

#[cfg(not(windows))]
pub fn show_info(_text: &str) {}

#[cfg(not(windows))]
pub fn reveal_file(_path: &std::path::Path) {}

#[cfg(not(windows))]
pub fn open_with_shell(_url: &str) {}

#[cfg(not(windows))]
pub fn pick_file() -> Option<PathBuf> {
    None
}

#[cfg(not(windows))]
pub fn pick_avatar() -> Option<PathBuf> {
    None
}

#[cfg(not(windows))]
pub fn pick_save_path(_name: &str) -> Option<PathBuf> {
    None
}
