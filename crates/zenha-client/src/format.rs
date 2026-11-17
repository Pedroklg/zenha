pub const DISPLAY_UTC_OFFSET_MINUTES: i32 = -180;

pub fn format_hhmm(unix_ms: u64, utc_offset_minutes: i32) -> String {
    let minutes = i64::try_from(unix_ms / 60_000).unwrap_or(0) + i64::from(utc_offset_minutes);
    let in_day = minutes.rem_euclid(24 * 60);
    format!("{:02}:{:02}", in_day / 60, in_day % 60)
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit]).replace('.', ",")
}

pub fn server_host(url: &str) -> &str {
    let trimmed = url.trim();
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_server_host_from_urls() {
        assert_eq!(
            server_host("https://voz.exemplo.com.br"),
            "voz.exemplo.com.br"
        );
        assert_eq!(
            server_host("https://voz.exemplo.com.br/token/"),
            "voz.exemplo.com.br"
        );
        assert_eq!(server_host("http://localhost:8080/api"), "localhost:8080");
        assert_eq!(
            server_host("  https://zenha.servidor.local/  "),
            "zenha.servidor.local"
        );
        assert_eq!(server_host("voz.exemplo.com.br/sala"), "voz.exemplo.com.br");
        assert_eq!(server_host(""), "");
    }

    #[test]
    fn formats_utc_and_offsets() {
        let ms = (13 * 60 + 5) * 60_000;
        assert_eq!(format_hhmm(ms, 0), "13:05");
        assert_eq!(format_hhmm(ms, -180), "10:05");
    }

    #[test]
    fn formats_sizes_in_pt_br() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1,5 KB");
        assert_eq!(format_size(2 * 1024 * 1024), "2,0 MB");
        assert_eq!(format_size(50 * 1024 * 1024), "50,0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3,0 GB");
    }

    #[test]
    fn wraps_around_midnight() {
        let ms = 60 * 60_000;
        assert_eq!(format_hhmm(ms, -180), "22:00");
        assert_eq!(format_hhmm(23 * 60 * 60_000, 120), "01:00");
    }
}
