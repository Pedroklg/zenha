//! Detecção pura de resumo para cards de lobby e partida da Gamers Club.

use reqwest::Url;
use zenha_protocol::{WebhookEmbed, WebhookPost};

use super::links::{ConnectCommand, connect_command};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Summary {
    Lobby {
        nick: Option<String>,
        url: String,
    },
    Match {
        teams: [String; 2],
        map: Option<String>,
        connect: ConnectCommand,
    },
}

pub fn summary(post: &WebhookPost) -> Option<Summary> {
    post.embeds
        .iter()
        .find_map(match_summary)
        .or_else(|| post.embeds.iter().find_map(lobby_summary))
}

fn match_summary(embed: &WebhookEmbed) -> Option<Summary> {
    let mut team_names = embed
        .fields
        .iter()
        .filter(|field| field.name.starts_with("Time "))
        .map(|field| field.name.as_str());
    let first = team_names.next()?;
    let second = team_names.next()?;
    if team_names.next().is_some() {
        return None;
    }
    let connect = embed
        .fields
        .iter()
        .find_map(|field| connect_command(&field.value))?;
    let map = embed
        .fields
        .iter()
        .find(|field| field_name_matches(&field.name, "Mapa"))
        .map(|field| field.value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Some(Summary::Match {
        teams: [first.to_owned(), second.to_owned()],
        map,
        connect,
    })
}

fn lobby_summary(embed: &WebhookEmbed) -> Option<Summary> {
    let url = embed.url.as_deref().filter(|url| is_gamersclub_url(url))?;
    if let Some(admin) = embed
        .fields
        .iter()
        .find(|field| field_name_matches(&field.name, "Admin da sala"))
    {
        return Some(Summary::Lobby {
            nick: extract_admin_nick(&admin.value),
            url: url.to_owned(),
        });
    }
    let has_lobby_title = embed
        .title
        .as_deref()
        .is_some_and(|title| title.to_lowercase().contains("lobby"));
    has_lobby_title.then(|| Summary::Lobby {
        nick: None,
        url: url.to_owned(),
    })
}

fn is_gamersclub_url(raw: &str) -> bool {
    let Ok(parsed) = Url::parse(raw) else {
        return false;
    };
    parsed.scheme() == "https"
        && parsed
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("gamersclub.com.br"))
}

fn field_name_matches(name: &str, expected: &str) -> bool {
    name.trim()
        .trim_end_matches(':')
        .trim()
        .eq_ignore_ascii_case(expected)
}

fn extract_admin_nick(value: &str) -> Option<String> {
    let nick = value.split(" | ").next().unwrap_or(value).trim();
    (!nick.is_empty()).then(|| nick.to_owned())
}

#[cfg(test)]
mod tests {
    use zenha_protocol::WebhookField;

    use super::*;

    fn field(name: &str, value: &str) -> WebhookField {
        WebhookField {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    fn post_with_embed(name: &str, embed: WebhookEmbed) -> WebhookPost {
        WebhookPost {
            id: "p1".to_owned(),
            name: name.to_owned(),
            sent_at_unix_ms: 1,
            content: None,
            embeds: vec![embed],
        }
    }

    fn real_lobby_post() -> WebhookPost {
        post_with_embed(
            "Outro nome",
            WebhookEmbed {
                title: Some("Clique aqui para abrir a lobby".to_owned()),
                url: Some("https://gamersclub.com.br/j/89323704/bc3d".to_owned()),
                description: None,
                fields: vec![
                    field("Tipo da sala:", "FECHADA"),
                    field("Admin da sala:", "KLG | 20"),
                    field("Lobby:", "Sequencia de vitória do admin: 0"),
                    field("Pré vetos", "de_nuke, de_anubis"),
                    field("Membros:", "KLG | 20 | KDR: 1.07"),
                ],
            },
        )
    }

    fn real_match_post() -> WebhookPost {
        post_with_embed(
            "Outro nome",
            WebhookEmbed {
                title: None,
                url: None,
                description: None,
                fields: vec![
                    field(
                        "Time .m. - 18",
                        "19 - .m. \n15 - avontZ \n20 - RanyMoney \n20 - lmx \n18 - Tesourow",
                    ),
                    field(
                        "Time .... - 19",
                        "19 - .... \n20 - Manchaa * \n20 - KLG \n19 - katchaw \n18 - zek1",
                    ),
                    field(
                        "IP da partida:",
                        "connect 203.159.80.65:27059;password GC5053 \n[Conectar ao servidor](steam://connect/203.159.80.65:27059/GC5053)",
                    ),
                    field("Mapa:", "de_inferno"),
                    field(
                        "Warmup",
                        "Até: 23:54:37 GMT-0300 (Horário Padrão de Brasília)",
                    ),
                    field(
                        "Link da partida",
                        "https://gamersclub.com.br/lobby/partida/28333771",
                    ),
                ],
            },
        )
    }

    #[test]
    fn summarizes_real_lobby_post_regardless_of_webhook_name() {
        assert_eq!(
            summary(&real_lobby_post()),
            Some(Summary::Lobby {
                nick: Some("KLG".to_owned()),
                url: "https://gamersclub.com.br/j/89323704/bc3d".to_owned(),
            })
        );
    }

    #[test]
    fn summarizes_lobby_without_admin_when_title_mentions_lobby() {
        let mut post = real_lobby_post();
        post.embeds[0].fields.remove(1);
        post.embeds[0].title = Some("Abrir LOBBY".to_owned());
        assert_eq!(
            summary(&post),
            Some(Summary::Lobby {
                nick: None,
                url: "https://gamersclub.com.br/j/89323704/bc3d".to_owned(),
            })
        );

        post.embeds[0].title = None;
        post.embeds[0]
            .fields
            .push(field("admin da sala", "  ana  | 15 "));
        assert_eq!(
            summary(&post),
            Some(Summary::Lobby {
                nick: Some("ana".to_owned()),
                url: "https://gamersclub.com.br/j/89323704/bc3d".to_owned(),
            })
        );
    }

    #[test]
    fn summarizes_real_match_post_regardless_of_webhook_name() {
        assert_eq!(
            summary(&real_match_post()),
            Some(Summary::Match {
                teams: ["Time .m. - 18".to_owned(), "Time .... - 19".to_owned()],
                map: Some("de_inferno".to_owned()),
                connect: ConnectCommand {
                    command: "connect 203.159.80.65:27059;password GC5053".to_owned(),
                    url: "steam://connect/203.159.80.65:27059/GC5053".to_owned(),
                },
            })
        );
    }

    #[test]
    fn match_map_is_optional_and_case_insensitive() {
        let mut post = real_match_post();
        post.embeds[0].fields[3].name = "mapa".to_owned();
        assert!(matches!(
            summary(&post),
            Some(Summary::Match { map: Some(ref m), .. }) if m == "de_inferno"
        ));

        post.embeds[0].fields.remove(3);
        assert!(matches!(
            summary(&post),
            Some(Summary::Match { map: None, .. })
        ));
    }

    #[test]
    fn unrelated_or_incomplete_posts_return_none() {
        let generic = post_with_embed(
            "Gamers Club",
            WebhookEmbed {
                title: Some("Gamers Club Booster".to_owned()),
                url: None,
                description: None,
                fields: vec![field("Status", "OK")],
            },
        );
        assert_eq!(summary(&generic), None);

        let text_only = WebhookPost {
            id: "p2".to_owned(),
            name: "Gamers Club".to_owned(),
            sent_at_unix_ms: 1,
            content: Some("olá".to_owned()),
            embeds: Vec::new(),
        };
        assert_eq!(summary(&text_only), None);

        let mut lookalike_lobby = real_lobby_post();
        lookalike_lobby.embeds[0].url = Some("https://evilgamersclub.com.br/j/1".to_owned());
        assert_eq!(summary(&lookalike_lobby), None);

        lookalike_lobby.embeds[0].url = Some("https://gamersclub.com.br.evil.com/j/1".to_owned());
        assert_eq!(summary(&lookalike_lobby), None);

        let mut gc_non_lobby = real_lobby_post();
        gc_non_lobby.embeds[0].fields.remove(1);
        gc_non_lobby.embeds[0].title = Some("Partida pronta".to_owned());
        assert_eq!(summary(&gc_non_lobby), None);

        let mut one_team = real_match_post();
        one_team.embeds[0].fields.remove(0);
        assert_eq!(summary(&one_team), None);

        let mut three_teams = real_match_post();
        three_teams.embeds[0]
            .fields
            .push(field("Time extra - 10", "x"));
        assert_eq!(summary(&three_teams), None);

        let mut no_connect = real_match_post();
        no_connect.embeds[0].fields.remove(2);
        assert_eq!(summary(&no_connect), None);
    }
}
