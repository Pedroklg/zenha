//! Caminhos temporários, detecção da instalação e execução do instalador.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use directories::BaseDirs;

use super::INSTALLER_ARGS;
use super::version::parse_semver;

const UPDATE_DIR_NAME: &str = "zenha-update";

pub fn normalize_windows_path(raw: &str) -> Option<String> {
    let replaced = raw.trim().replace('/', "\\");
    if replaced.is_empty() {
        return None;
    }
    let without_verbatim = if let Some(unc_rest) = replaced.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc_rest}")
    } else if let Some(rest) = replaced.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else if let Some(rest) = replaced.strip_prefix(r"\\.\") {
        rest.to_owned()
    } else {
        replaced
    };

    let (root, rest) = if let Some(unc) = without_verbatim.strip_prefix(r"\\") {
        let mut parts = unc.split('\\').filter(|s| !s.is_empty());
        let server = parts.next()?;
        let share = parts.next()?;
        let prefix = format!(r"\\{}\{}", server.to_lowercase(), share.to_lowercase());
        let remaining: Vec<&str> = parts.collect();
        (prefix, remaining)
    } else {
        let mut parts = without_verbatim.split('\\');
        let first = parts.next()?;
        if first.len() == 2 && first.as_bytes()[0].is_ascii_alphabetic() && first.ends_with(':') {
            (
                first.to_ascii_lowercase(),
                parts.filter(|s| !s.is_empty()).collect(),
            )
        } else {
            return None;
        }
    };

    let mut stack: Vec<String> = Vec::new();
    for segment in rest {
        match segment {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other.to_lowercase()),
        }
    }

    if stack.is_empty() {
        Some(format!(r"{root}\"))
    } else {
        Some(format!(r"{root}\{}", stack.join(r"\")))
    }
}

pub fn is_installed_exe_path(current_exe: &Path, local_app_data: &Path) -> bool {
    let Some(exe_str) = current_exe.to_str() else {
        return false;
    };
    let Some(local_str) = local_app_data.to_str() else {
        return false;
    };
    let Some(normalized_exe) = normalize_windows_path(exe_str) else {
        return false;
    };
    let expected_raw = format!(r"{local_str}\Programs\Zenha\zenha.exe");
    let Some(normalized_expected) = normalize_windows_path(&expected_raw) else {
        return false;
    };
    normalized_exe == normalized_expected
}

pub fn is_installed_exe() -> bool {
    #[cfg(not(windows))]
    {
        false
    }
    #[cfg(windows)]
    {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let Some(local_app_data) = local_app_data_dir() else {
            return false;
        };
        if is_installed_exe_path(&exe, &local_app_data) {
            return true;
        }
        let canon_exe = fs::canonicalize(&exe).unwrap_or(exe);
        let canon_local = fs::canonicalize(&local_app_data).unwrap_or(local_app_data);
        is_installed_exe_path(&canon_exe, &canon_local)
    }
}

#[cfg(windows)]
fn local_app_data_dir() -> Option<PathBuf> {
    BaseDirs::new()
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .filter(|val| !val.is_empty())
                .map(PathBuf::from)
        })
}

pub fn update_temp_dir() -> PathBuf {
    std::env::temp_dir().join(UPDATE_DIR_NAME)
}

pub fn installer_paths(dir: &Path, version: &str) -> Option<(PathBuf, PathBuf)> {
    let parsed = parse_semver(version)?;
    let exe_name = format!("Zenha-{parsed}-setup.exe");
    let part_name = format!("{exe_name}.part");
    Some((dir.join(part_name), dir.join(exe_name)))
}

pub fn cleanup_update_dir(dir: &Path) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::debug!(%err, path = %dir.display(), "could not list update temp directory");
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && let Err(err) = fs::remove_file(&path)
        {
            tracing::debug!(%err, path = %path.display(), "could not remove stale update file");
        }
    }

    let _ = fs::remove_dir(dir);
}

pub fn launch_installer(path: &Path) -> std::io::Result<()> {
    let meta = fs::metadata(path)?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "installer path is not a file",
        ));
    }
    #[cfg(not(windows))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "installer execution is only supported on windows",
        ))
    }
    #[cfg(windows)]
    {
        let target = std::path::absolute(path)?;
        Command::new(&target)
            .args(INSTALLER_ARGS)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(())
    }
}
