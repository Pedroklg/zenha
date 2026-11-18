//! `--diagnostico`: a text report for when the app does not open, saved on the Desktop.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use directories::UserDirs;

use super::logging;
use super::settings::Settings;

const LOG_FILES: usize = 3;
const LOG_TAIL_LINES: usize = 400;

/// System facts gathered with PowerShell; read-only and without network access.
const SYSTEM_SCRIPT: &str = r"
Get-CimInstance Win32_OperatingSystem | Format-List Caption, Version, BuildNumber, OSArchitecture
Get-CimInstance Win32_VideoController | Format-List Name, DriverVersion, DriverDate, CurrentHorizontalResolution, CurrentVerticalResolution
Get-PnpDevice -Class AudioEndpoint -Status OK -ErrorAction SilentlyContinue | Format-Table FriendlyName -AutoSize
'--- zenha processes ---'
Get-Process zenha -ErrorAction SilentlyContinue | Format-Table Id, Path, StartTime, Responding -AutoSize
'--- application errors (7 days) ---'
Get-WinEvent -FilterHashtable @{LogName='Application'; Id=1000,1001,1002; StartTime=(Get-Date).AddDays(-7)} -ErrorAction SilentlyContinue |
  Where-Object Message -match 'zenha' | Select-Object -First 10 | Format-List TimeCreated, Id, Message
";

/// Writes the report and returns its path.
pub fn write_report(settings: &Settings, current_log: Option<&Path>) -> std::io::Result<PathBuf> {
    let mut report = String::new();
    let _ = writeln!(report, "Zenha {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(report, "servidor: {}", settings.token_server_url);
    let _ = writeln!(report, "entrada: {:?}", settings.input_device);
    let _ = writeln!(report, "saida: {:?}", settings.output_device);
    let _ = writeln!(
        report,
        "\n===== sistema =====\n{}",
        run_powershell(SYSTEM_SCRIPT)
    );

    if let Some(dir) = logging::log_dir() {
        let logs = logging::previous_logs(&dir)
            .into_iter()
            .filter(|path| Some(path.as_path()) != current_log)
            .take(LOG_FILES);
        for log in logs {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let _ = writeln!(
                report,
                "\n===== {} =====\n{}",
                log.display(),
                tail(&text, LOG_TAIL_LINES)
            );
        }
    }

    let dir = UserDirs::new()
        .and_then(|dirs| dirs.desktop_dir().map(Path::to_path_buf))
        .or_else(logging::log_dir)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join("zenha-diagnostico.txt");
    std::fs::write(&path, report)?;
    Ok(path)
}

fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(windows)]
fn run_powershell(script: &str) -> String {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(out) => format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(err) => format!("powershell failed: {err}"),
    }
}

#[cfg(not(windows))]
fn run_powershell(_script: &str) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_only_the_last_lines() {
        assert_eq!(tail("a\nb\nc", 2), "b\nc");
        assert_eq!(tail("a", 5), "a");
        assert_eq!(tail("", 5), "");
    }
}
