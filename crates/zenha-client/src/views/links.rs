//! Link detection for chat text and webhook cards.

const HTTPS: &str = "https://";
const STEAM_CONNECT: &str = "steam://connect/";
const TRAILING_PUNCTUATION: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '\'', '"'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(&'a str),
    Link { label: &'a str, url: &'a str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectCommand {
    pub command: String,
    pub url: String,
}

/// Splits `text` into plain text and links. Only `https://` and `steam://connect/` become links.
pub fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut plain_start = 0;
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        let found = if rest.starts_with('[') {
            markdown_link(rest)
        } else if has_link_prefix(rest) && boundary_before(text, index) {
            bare_link(rest)
        } else {
            None
        };
        match found {
            Some((segment, used)) => {
                if plain_start < index {
                    out.push(Segment::Text(&text[plain_start..index]));
                }
                out.push(segment);
                index += used;
                plain_start = index;
            }
            None => index += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    if plain_start < text.len() {
        out.push(Segment::Text(&text[plain_start..]));
    }
    out
}

/// Finds `connect <host:port>;password <password>` in a line of `text`.
pub fn connect_command(text: &str) -> Option<ConnectCommand> {
    text.lines().find_map(|line| {
        line.match_indices("connect ")
            .filter(|(index, _)| boundary_before(line, *index))
            .find_map(|(index, keyword)| parse_connect(&line[index + keyword.len()..]))
    })
}

fn parse_connect(rest: &str) -> Option<ConnectCommand> {
    let (host, rest) = rest.trim_start().split_once(';')?;
    let valid_host = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_' | '[' | ']'));
    if !valid_host {
        return None;
    }
    let password = rest
        .trim_start()
        .strip_prefix("password ")?
        .split_whitespace()
        .next()
        .filter(|password| !password.chars().any(char::is_control))?;
    Some(ConnectCommand {
        command: format!("connect {host};password {password}"),
        url: format!("{STEAM_CONNECT}{host}/{password}"),
    })
}

fn markdown_link(rest: &str) -> Option<(Segment<'_>, usize)> {
    let close = rest.find("](")?;
    let label = &rest[1..close];
    if label.is_empty() || label.contains(['[', ']', '\n']) {
        return None;
    }
    let after = &rest[close + 2..];
    let end = after.find(')')?;
    let url = &after[..end];
    is_link_url(url).then_some((Segment::Link { label, url }, close + 2 + end + 1))
}

fn bare_link(rest: &str) -> Option<(Segment<'_>, usize)> {
    let end = rest
        .find(|c: char| c.is_whitespace() || c.is_control())
        .unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches(TRAILING_PUNCTUATION);
    is_link_url(url).then_some((Segment::Link { label: url, url }, url.len()))
}

fn is_link_url(url: &str) -> bool {
    let body = url
        .strip_prefix(HTTPS)
        .or_else(|| url.strip_prefix(STEAM_CONNECT));
    body.is_some_and(|body| !body.is_empty())
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn has_link_prefix(rest: &str) -> bool {
    rest.starts_with(HTTPS) || rest.starts_with(STEAM_CONNECT)
}

fn boundary_before(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MATCH_VALUE: &str = "connect 1.2.3.4:27015;password xyz \n[Conectar ao servidor](steam://connect/1.2.3.4:27015/xyz)";

    #[test]
    fn extension_value_splits_into_text_and_steam_link() {
        assert_eq!(
            segments(MATCH_VALUE),
            vec![
                Segment::Text("connect 1.2.3.4:27015;password xyz \n"),
                Segment::Link {
                    label: "Conectar ao servidor",
                    url: "steam://connect/1.2.3.4:27015/xyz",
                },
            ]
        );
    }

    #[test]
    fn extension_value_yields_normalized_connect_command() {
        assert_eq!(
            connect_command(MATCH_VALUE),
            Some(ConnectCommand {
                command: "connect 1.2.3.4:27015;password xyz".to_owned(),
                url: "steam://connect/1.2.3.4:27015/xyz".to_owned(),
            })
        );
    }

    #[test]
    fn connect_command_accepts_no_space_after_semicolon() {
        let found = connect_command("connect 10.0.0.1:27016;password abc");
        assert_eq!(
            found.map(|c| c.url),
            Some("steam://connect/10.0.0.1:27016/abc".to_owned())
        );
    }

    #[test]
    fn connect_command_rejects_incomplete_or_odd_lines() {
        assert_eq!(connect_command("connect 1.2.3.4:27015"), None);
        assert_eq!(connect_command("connect 1.2.3.4:27015;password "), None);
        assert_eq!(connect_command("connect a b;password x"), None);
        assert_eq!(connect_command("reconnect 1.2.3.4:1;password x"), None);
        assert_eq!(connect_command("connect 1.2.3.4/x;password x"), None);
        assert_eq!(connect_command("de_mirage"), None);
    }

    #[test]
    fn javascript_markdown_link_stays_plain_text() {
        let text = "[clique](javascript:alert(1))";
        assert_eq!(segments(text), vec![Segment::Text(text)]);
        let other = "[abrir](file:///c:/windows)";
        assert_eq!(segments(other), vec![Segment::Text(other)]);
    }

    #[test]
    fn bare_url_in_the_middle_of_a_sentence_is_a_link() {
        assert_eq!(
            segments("veja https://gamersclub.com.br/j/1, depois entre"),
            vec![
                Segment::Text("veja "),
                Segment::Link {
                    label: "https://gamersclub.com.br/j/1",
                    url: "https://gamersclub.com.br/j/1",
                },
                Segment::Text(", depois entre"),
            ]
        );
    }

    #[test]
    fn bare_urls_with_other_schemes_or_no_body_stay_text() {
        for text in [
            "http://exemplo.com",
            "ftp://x",
            "https://",
            "xhttps://a.com",
        ] {
            assert_eq!(segments(text), vec![Segment::Text(text)], "{text}");
        }
    }

    #[test]
    fn text_without_links_is_a_single_segment() {
        assert_eq!(
            segments("sem links aqui"),
            vec![Segment::Text("sem links aqui")]
        );
        assert_eq!(segments(""), Vec::<Segment<'_>>::new());
        let bracket = "ação [x] é só texto";
        assert_eq!(segments(bracket), vec![Segment::Text(bracket)]);
    }

    #[test]
    fn two_links_keep_their_order() {
        assert_eq!(
            segments("[a](https://a.com) e https://b.com."),
            vec![
                Segment::Link {
                    label: "a",
                    url: "https://a.com",
                },
                Segment::Text(" e "),
                Segment::Link {
                    label: "https://b.com",
                    url: "https://b.com",
                },
                Segment::Text("."),
            ]
        );
    }
}
