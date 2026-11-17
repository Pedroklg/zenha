//! Testes dos helpers de texto da sala.

use crate::ui_model::ParticipantView;

use super::center::people_count_label;
use super::local_display_name;

fn participant(identity: &str, name: &str) -> ParticipantView {
    ParticipantView {
        identity: identity.to_owned(),
        name: name.to_owned(),
        muted: false,
        speaking: false,
        avatar_version: None,
    }
}

#[test]
fn formats_people_count_in_portuguese() {
    assert_eq!(people_count_label(0), "0 pessoas na sala");
    assert_eq!(people_count_label(1), "1 pessoa na sala");
    assert_eq!(people_count_label(4), "4 pessoas na sala");
}

#[test]
fn resolves_local_display_name_from_participants_or_fallback() {
    let list = vec![participant("id-1", "Lucas")];
    assert_eq!(local_display_name(&list, "id-1"), "Lucas");
    assert_eq!(local_display_name(&list, "id-2"), "id-2");
    assert_eq!(local_display_name(&list, ""), "Você");
}
