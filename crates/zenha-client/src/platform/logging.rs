//! Log files under `%LOCALAPPDATA%\zenha\logs`, one per run, plus the panic hook.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use directories::BaseDirs;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

const KEEP_FILES: usize = 10;
const DEBUG_FILTER: &str = "info,zenha_client=debug,zenha_core=debug,zenha_protocol=debug";

pub fn log_dir() -> Option<PathBuf> {
    Some(BaseDirs::new()?.data_local_dir().join("zenha").join("logs"))
}

/// Starts logging to stdout and to a new file for this run; returns the file path when it could be created.
pub fn init(debug: bool) -> Option<PathBuf> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(if debug { DEBUG_FILTER } else { "info" }));
    let file = log_dir().and_then(|dir| open_run_file(&dir));
    let (path, file_layer) = match file {
        Some((path, file)) => (
            Some(path),
            Some(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(Mutex::new(file)),
            ),
        ),
        None => (None, None),
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(file_layer)
        .init();
    install_panic_hook();
    path
}

fn open_run_file(dir: &Path) -> Option<(PathBuf, File)> {
    std::fs::create_dir_all(dir).ok()?;
    prune(dir);
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("zenha-{secs:010}-{}.log", std::process::id()));
    let file = File::create(&path).ok()?;
    Some((path, file))
}

/// Log files of earlier runs, newest first.
pub fn previous_logs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| is_log_name(name))
        .collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    names.into_iter().map(|name| dir.join(name)).collect()
}

fn prune(dir: &Path) {
    for old in previous_logs(dir).into_iter().skip(KEEP_FILES - 1) {
        let _ = std::fs::remove_file(old);
    }
}

fn is_log_name(name: &str) -> bool {
    name.starts_with("zenha-") && name.ends_with(".log")
}

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(%info, %backtrace, "panic");
        default_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_runs_are_pruned_newest_first() {
        let dir = std::env::temp_dir().join(format!("zenha-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..12 {
            std::fs::write(dir.join(format!("zenha-{i:010}-1.log")), "x").unwrap();
        }
        std::fs::write(dir.join("other.txt"), "x").unwrap();

        prune(&dir);
        let left = previous_logs(&dir);
        assert_eq!(left.len(), KEEP_FILES - 1);
        assert!(left[0].ends_with("zenha-0000000011-1.log"));
        assert!(dir.join("other.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
