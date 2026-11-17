//! Per-room password: access key (HMAC-SHA256) and parent room inheritance rule.

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::store::RoomRecord;

const KEY_LABEL: &[u8] = b"zenha.room_key.v1";

/// Access key of `room` while its password has hash `password_hash`.
pub fn room_key(secret: &str, room: &str, password_hash: &str) -> String {
    let message = [
        KEY_LABEL,
        b"\0",
        room.as_bytes(),
        b"\0",
        password_hash.as_bytes(),
    ]
    .concat();
    hmac_sha256(secret.as_bytes(), &message)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn key_matches(secret: &str, room: &str, password_hash: &str, provided: &str) -> bool {
    room_key(secret, room, password_hash)
        .as_bytes()
        .ct_eq(provided.as_bytes())
        .into()
}

/// Rooms whose password applies to `room`: itself and the parent room, when they have a password.
pub fn lock_owners<'a>(rooms: &'a [RoomRecord], room: &'a RoomRecord) -> Vec<(&'a str, &'a str)> {
    let parent = room
        .parent
        .as_deref()
        .and_then(|id| rooms.iter().find(|r| r.id == id));
    [Some(room), parent]
        .into_iter()
        .flatten()
        .filter_map(|r| r.password_hash.as_deref().map(|hash| (r.id.as_str(), hash)))
        .collect()
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.map(|b| b ^ byte);
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, parent: Option<&str>, hash: Option<&str>) -> RoomRecord {
        RoomRecord {
            id: id.to_owned(),
            title: id.to_owned(),
            position: 0,
            parent: parent.map(str::to_owned),
            password_hash: hash.map(str::to_owned),
        }
    }

    #[test]
    fn hmac_matches_rfc_4231_vector() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn keys_depend_on_room_hash_and_secret() {
        let key = room_key("secret", "vip", "hash-1");
        assert!(key_matches("secret", "vip", "hash-1", &key));
        assert!(!key_matches("secret", "outra", "hash-1", &key));
        assert!(!key_matches("secret", "vip", "hash-2", &key));
        assert!(!key_matches("other", "vip", "hash-1", &key));
        assert!(!key_matches("secret", "vip", "hash-1", ""));
    }

    #[test]
    fn lock_owners_include_own_and_parent_passwords() {
        let rooms = vec![
            record("mae", None, Some("h-mae")),
            record("filha", Some("mae"), None),
            record("trancada", Some("mae"), Some("h-filha")),
            record("livre", None, None),
        ];
        assert_eq!(lock_owners(&rooms, &rooms[0]), vec![("mae", "h-mae")]);
        assert_eq!(lock_owners(&rooms, &rooms[1]), vec![("mae", "h-mae")]);
        assert_eq!(
            lock_owners(&rooms, &rooms[2]),
            vec![("trancada", "h-filha"), ("mae", "h-mae")]
        );
        assert!(lock_owners(&rooms, &rooms[3]).is_empty());
    }
}
