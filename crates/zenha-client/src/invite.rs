//! Formatting and parsing of `zenha://entrar` invite links.

use std::fmt;

/// Data extracted from a Zenha invite link.
#[derive(Clone, PartialEq, Eq)]
pub struct Invite {
    pub server: String,
    pub password: String,
}

impl fmt::Debug for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invite")
            .field("server", &self.server)
            .field("password", &"<redacted>")
            .finish()
    }
}

const PREFIX: &str = "zenha://entrar?";

/// Formats a `zenha://entrar?servidor=<pct>&senha=<pct>` link.
pub fn format(server: &str, password: &str) -> String {
    format!(
        "{PREFIX}servidor={}&senha={}",
        percent_encode(server.trim()),
        percent_encode(password),
    )
}

/// Parses a `zenha://entrar?...` link, accepting surrounding whitespace and
/// parameters in any order. Rejects an empty server or one without `http://`/`https://`.
pub fn parse(text: &str) -> Option<Invite> {
    let query = text.trim().strip_prefix(PREFIX)?;
    let mut server: Option<String> = None;
    let mut password: Option<String> = None;

    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=')?;
        match key {
            "servidor" => server = Some(percent_decode(value)?.trim().to_owned()),
            "senha" => password = Some(percent_decode(value)?),
            _ => {}
        }
    }

    let server = server.filter(|url| has_http_authority(url))?;
    Some(Invite {
        server,
        password: password.unwrap_or_default(),
    })
}

fn has_http_authority(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    !rest.is_empty() && !rest.starts_with('/') && !url.chars().any(char::is_whitespace)
}

fn percent_encode(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for &byte in input.as_bytes() {
        if matches!(
            byte,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~'
        ) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0F) as usize] as char);
        }
    }
    out
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_nibble(bytes[i + 1])?;
            let lo = hex_nibble(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_special_chars_and_accents() {
        let server = "https://voz.amigos.example:8443/sub";
        let password = "cão & café = 100% + çãõ! #42?";
        let link = format(server, password);
        assert_eq!(
            link,
            "zenha://entrar?servidor=https%3A%2F%2Fvoz.amigos.example%3A8443%2Fsub&senha=c%C3%A3o%20%26%20caf%C3%A9%20%3D%20100%25%20%2B%20%C3%A7%C3%A3%C3%B5%21%20%2342%3F"
        );
        assert_eq!(
            parse(&format!("  {link}\n")),
            Some(Invite {
                server: server.to_owned(),
                password: password.to_owned(),
            })
        );
    }

    #[test]
    fn swapped_parameter_order_and_surrounding_spaces() {
        let link = "  zenha://entrar?senha=segredo%20123&servidor=http%3A%2F%2F10.0.0.5%3A3000  ";
        assert_eq!(
            parse(link),
            Some(Invite {
                server: "http://10.0.0.5:3000".to_owned(),
                password: "segredo 123".to_owned(),
            })
        );
    }

    #[test]
    fn link_without_password_parses_empty_password() {
        let missing_param = "zenha://entrar?servidor=https%3A%2F%2Fvoz.exemplo";
        assert_eq!(
            parse(missing_param),
            Some(Invite {
                server: "https://voz.exemplo".to_owned(),
                password: String::new(),
            })
        );

        let empty_param = "zenha://entrar?servidor=https%3A%2F%2Fvoz.exemplo&senha=";
        assert_eq!(
            parse(empty_param),
            Some(Invite {
                server: "https://voz.exemplo".to_owned(),
                password: String::new(),
            })
        );
    }

    #[test]
    fn rejects_garbage_empty_server_and_missing_scheme() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("texto qualquer"), None);
        assert_eq!(parse("https://voz.exemplo"), None);
        assert_eq!(parse("zenha://sair?servidor=https://voz.exemplo"), None);
        assert_eq!(parse("zenha://entrar?senha=123"), None);
        assert_eq!(parse("zenha://entrar?servidor=&senha=123"), None);
        assert_eq!(parse("zenha://entrar?servidor=voz.exemplo&senha=123"), None);
        assert_eq!(
            parse("zenha://entrar?servidor=ftp%3A%2F%2Fvoz.exemplo&senha=123"),
            None
        );
        assert_eq!(
            parse("zenha://entrar?servidor=https%3A%2F%2F&senha=123"),
            None
        );
        assert_eq!(
            parse("zenha://entrar?servidor=https%3A%2F%2Fvoz.exemplo&senha=%ZZ"),
            None
        );
    }

    #[test]
    fn invite_debug_redacts_password() {
        let invite = Invite {
            server: "https://voz.exemplo".to_owned(),
            password: "super-secret-password".to_owned(),
        };
        let dbg = format!("{invite:?}");
        assert!(!dbg.contains("super-secret-password"));
        assert!(dbg.contains("https://voz.exemplo"));
        assert!(dbg.contains("<redacted>"));
    }
}
