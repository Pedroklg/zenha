//! The fifteen scenarios, their registry and the tools they share.

mod audio;
mod chat;
mod ctx;
mod session;
mod share;

pub use ctx::{Ctx, Setup};

pub struct ScenarioDef {
    pub name: &'static str,
    pub title: &'static str,
    pub needs_media: bool,
    pub run: fn(&mut Ctx<'_>) -> anyhow::Result<()>,
}

pub const ALL: &[ScenarioDef] = &[
    ScenarioDef {
        name: "token",
        title: "Convite correto entra, convite errado é recusado",
        needs_media: false,
        run: session::token,
    },
    ScenarioDef {
        name: "presence",
        title: "Entrada e saída de participantes aparecem para os outros",
        needs_media: true,
        run: session::presence,
    },
    ScenarioDef {
        name: "playout-listener-first",
        title: "Ouvinte entra primeiro, o tom chega à saída de áudio",
        needs_media: true,
        run: audio::playout_listener_first,
    },
    ScenarioDef {
        name: "playout-tone-first",
        title: "Tom entra primeiro, o tom chega à saída de áudio",
        needs_media: true,
        run: audio::playout_tone_first,
    },
    ScenarioDef {
        name: "playout-explicit-device",
        title: "Como o anterior, com a saída padrão escolhida explicitamente",
        needs_media: true,
        run: audio::playout_explicit_device,
    },
    ScenarioDef {
        name: "enhanced-mic",
        title: "Microfone reforçado publica só o próprio áudio e troca de modo com a sala aberta",
        needs_media: true,
        run: audio::enhanced_mic,
    },
    ScenarioDef {
        name: "echo-cancel",
        title: "Cancelamento de eco tira do microfone o som que o próprio app toca",
        needs_media: true,
        run: audio::echo_cancel,
    },
    ScenarioDef {
        name: "mute",
        title: "Mute de quem fala some e volta na saída de áudio",
        needs_media: true,
        run: audio::mute,
    },
    ScenarioDef {
        name: "deafen",
        title: "Deafen de quem ouve silencia e restaura a saída de áudio",
        needs_media: true,
        run: audio::deafen,
    },
    ScenarioDef {
        name: "chat",
        title: "Mensagem de chat chega com o remetente certo",
        needs_media: true,
        run: chat::chat,
    },
    ScenarioDef {
        name: "files",
        title: "Arquivo enviado pelo chat chega íntegro, e o limite de 50 MB é respeitado",
        needs_media: true,
        run: chat::files,
    },
    ScenarioDef {
        name: "share",
        title: "Transmissão de tela chega em quadros e o áudio continua",
        needs_media: true,
        run: share::share,
    },
    ScenarioDef {
        name: "share-audio",
        title: "Som da transmissão chega a quem assiste e obedece ao volume próprio",
        needs_media: true,
        run: share::share_audio,
    },
    ScenarioDef {
        name: "participant-volume",
        title: "Volume por pessoa abaixa, zera e passa de 100%",
        needs_media: true,
        run: audio::participant_volume,
    },
    ScenarioDef {
        name: "volume-sticks",
        title: "Volume por pessoa se mantém depois de ensurdecer, mute, reentrada e alguém novo",
        needs_media: true,
        run: audio::volume_sticks,
    },
    ScenarioDef {
        name: "room-switch",
        title: "Troca de sala e volta com presença e áudio",
        needs_media: true,
        run: session::room_switch,
    },
    ScenarioDef {
        name: "cue",
        title: "Som de confirmação de mute e deafen toca na saída escolhida",
        needs_media: true,
        run: audio::cue,
    },
    ScenarioDef {
        name: "av1-decode",
        title: "Custo de CPU para decodificar AV1 1080p comparado a H.264 e VP9",
        needs_media: true,
        run: share::av1_decode,
    },
];

pub fn room_name(scenario: &str) -> String {
    format!("e2e-{scenario}")
}

pub fn second_room_name(scenario: &str) -> String {
    format!("e2e-{scenario}-b")
}

pub fn all_room_names() -> Vec<String> {
    ALL.iter()
        .flat_map(|def| [room_name(def.name), second_room_name(def.name)])
        .collect()
}

#[cfg(test)]
mod tests {
    use zenha_protocol::validate_room_name;

    use super::*;

    #[test]
    fn scenario_names_are_unique_and_rooms_are_valid_and_distinct() {
        let mut names: Vec<_> = ALL.iter().map(|d| d.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL.len());
        let rooms = all_room_names();
        let mut unique = rooms.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), rooms.len());
        for room in rooms {
            assert!(validate_room_name(&room).is_ok(), "{room}");
        }
    }

    #[test]
    fn registry_matches_the_brief() {
        let names: Vec<_> = ALL.iter().map(|d| d.name).collect();
        assert_eq!(
            names,
            [
                "token",
                "presence",
                "playout-listener-first",
                "playout-tone-first",
                "playout-explicit-device",
                "enhanced-mic",
                "echo-cancel",
                "mute",
                "deafen",
                "chat",
                "files",
                "share",
                "share-audio",
                "participant-volume",
                "volume-sticks",
                "room-switch",
                "cue",
                "av1-decode",
            ]
        );
        assert!(!ALL[0].needs_media);
    }
}
