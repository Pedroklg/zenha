//! Diálogos de arquivo do sistema (só no Windows; nas demais plataformas não abrem nada).

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
