//! Funções puras para envio e recebimento de arquivos: nomes seguros, tipo e caminho de destino.

use std::path::{Path, PathBuf};

use rooms_protocol::MAX_FILE_NAME_LEN;

pub const FILE_TOPIC: &str = "file";
const FALLBACK_NAME: &str = "arquivo";
const MAX_ID_LEN: usize = 64;
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

fn is_reserved(stem: &str) -> bool {
    let upper = stem.trim_end().to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            upper
                .strip_prefix(prefix)
                .is_some_and(|rest| matches!(rest.as_bytes(), [b'1'..=b'9']))
        })
}

fn clean(part: &str) -> String {
    let filtered: String = part
        .chars()
        .filter(|c| !c.is_control() && !FORBIDDEN.contains(c))
        .collect();
    let mut collapsed = filtered;
    while collapsed.contains("..") {
        collapsed = collapsed.replace("..", ".");
    }
    collapsed
        .trim_matches(|c: char| c == '.' || c.is_whitespace())
        .to_owned()
}

fn safe_extension(ext: &str) -> bool {
    !ext.is_empty() && ext.chars().all(|c| c.is_alphanumeric()) && ext.chars().count() <= 16
}

pub fn sanitize_file_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned = clean(last);
    let (stem, ext) = match cleaned.rfind('.') {
        Some(idx) if idx > 0 => (&cleaned[..idx], &cleaned[idx + 1..]),
        _ => (cleaned.as_str(), ""),
    };
    let stem = stem.trim_matches(|c: char| c == '.' || c.is_whitespace());
    let ext = if safe_extension(ext) { ext } else { "" };
    let stem = if stem.is_empty() || is_reserved(stem) {
        FALLBACK_NAME
    } else {
        stem
    };
    let ext_len = if ext.is_empty() {
        0
    } else {
        ext.chars().count() + 1
    };
    let budget = MAX_FILE_NAME_LEN.saturating_sub(ext_len).max(1);
    let stem: String = stem.chars().take(budget).collect();
    let stem = stem.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    let stem = if stem.is_empty() { FALLBACK_NAME } else { stem };
    if ext.is_empty() {
        stem.to_owned()
    } else {
        format!("{stem}.{ext}")
    }
}

pub fn mime_for_name(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    }
}

pub fn is_image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp"
    )
}

pub fn received_path(dir: &Path, transfer_id: &str, name: &str) -> PathBuf {
    let id: String = transfer_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(MAX_ID_LEN)
        .collect();
    let id = if id.is_empty() { "x".to_owned() } else { id };
    dir.join(format!("{id}-{}", sanitize_file_name(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_safe(name: &str) {
        assert!(!name.contains(['/', '\\']), "{name}");
        assert!(!name.contains(".."), "{name}");
        assert!(!name.is_empty());
    }

    #[test]
    fn traversal_is_cut_to_last_component() {
        assert_eq!(sanitize_file_name("..\\..\\x.exe"), "x.exe");
        assert_eq!(sanitize_file_name("/etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("a/b\\c.txt"), "c.txt");
        assert_eq!(sanitize_file_name("C:\\dir\\foto.png"), "foto.png");
    }

    #[test]
    fn dotted_and_trailing_names_are_safe() {
        assert_eq!(sanitize_file_name("..."), "arquivo");
        assert_eq!(sanitize_file_name(".."), "arquivo");
        assert_eq!(sanitize_file_name("   "), "arquivo");
        assert_eq!(sanitize_file_name(""), "arquivo");
        assert_eq!(sanitize_file_name("/"), "arquivo");
        assert_eq!(sanitize_file_name("  nome.txt. . "), "nome.txt");
        assert_eq!(sanitize_file_name("a..b.txt"), "a.b.txt");
        for name in ["a..b", "..a", "x/..", "..\\..", "a...b.c"] {
            let out = sanitize_file_name(name);
            assert!(!out.contains(".."), "{name} -> {out}");
        }
    }

    #[test]
    fn forbidden_and_control_chars_are_removed() {
        assert_eq!(sanitize_file_name("a<b>c:d\"e|f?g*h.txt"), "abcdefgh.txt");
        assert_eq!(sanitize_file_name("a\u{0}b\nc\td.txt"), "abcd.txt");
    }

    #[test]
    fn reserved_names_fall_back() {
        for name in [
            "CON", "con.txt", "Nul.tar", "PRN", "aux.png", "COM1", "lpt9.bin", "Com5.x",
        ] {
            let out = sanitize_file_name(name);
            assert!(out.starts_with("arquivo"), "{name} -> {out}");
        }
        assert_eq!(sanitize_file_name("con.txt"), "arquivo.txt");
        assert_eq!(sanitize_file_name("COM0.txt"), "COM0.txt");
        assert_eq!(sanitize_file_name("COM10"), "COM10");
        assert_eq!(sanitize_file_name("console.txt"), "console.txt");
    }

    #[test]
    fn long_names_keep_extension() {
        let name = format!("{}.png", "ã".repeat(300));
        let out = sanitize_file_name(&name);
        assert_eq!(out.chars().count(), MAX_FILE_NAME_LEN);
        assert!(out.ends_with(".png"));
        let plain = sanitize_file_name(&"x".repeat(500));
        assert_eq!(plain.chars().count(), MAX_FILE_NAME_LEN);
    }

    #[test]
    fn unsafe_extension_is_dropped() {
        assert_eq!(sanitize_file_name("a.t xt"), "a");
        assert_eq!(sanitize_file_name(&format!("a.{}", "e".repeat(40))), "a");
        assert_eq!(sanitize_file_name(".bashrc"), "bashrc");
    }

    #[test]
    fn output_is_always_safe() {
        for name in [
            "../../..",
            "a/../b",
            "..\\",
            "....x....",
            "\\\\srv\\share\\f",
        ] {
            assert_safe(&sanitize_file_name(name));
        }
    }

    #[test]
    fn mimes_by_extension() {
        assert_eq!(mime_for_name("a.PNG"), "image/png");
        assert_eq!(mime_for_name("a.jpg"), "image/jpeg");
        assert_eq!(mime_for_name("a.JPEG"), "image/jpeg");
        assert_eq!(mime_for_name("a.gif"), "image/gif");
        assert_eq!(mime_for_name("a.webp"), "image/webp");
        assert_eq!(mime_for_name("a.bmp"), "image/bmp");
        assert_eq!(mime_for_name("a.svg"), "application/octet-stream");
        assert_eq!(mime_for_name("png"), "application/octet-stream");
        assert_eq!(mime_for_name("a.bin"), "application/octet-stream");
    }

    #[test]
    fn image_mimes_exclude_svg() {
        assert!(is_image_mime("image/png"));
        assert!(is_image_mime("image/bmp"));
        assert!(!is_image_mime("image/svg+xml"));
        assert!(!is_image_mime("application/octet-stream"));
    }

    #[test]
    fn received_path_sanitizes_id_and_name() {
        let dir = Path::new("recebidos");
        assert_eq!(
            received_path(dir, "abc-123", "foto.png"),
            dir.join("abc-123-foto.png")
        );
        assert_eq!(
            received_path(dir, "../../x", "..\\y.exe"),
            dir.join("x-y.exe")
        );
        assert_eq!(received_path(dir, "///", "a.txt"), dir.join("x-a.txt"));
        let long = "a".repeat(200);
        let path = received_path(dir, &long, "a.txt");
        let file = path.file_name().and_then(|f| f.to_str()).unwrap_or("");
        assert!(file.starts_with(&format!("{}-", "a".repeat(64))));
        assert_eq!(path.parent(), Some(dir));
    }
}
