use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::BaseDirs;
use serde::{Deserialize, Serialize};

const APP_DIR: &str = "zenha";
const LEGACY_APP_DIR: &str = "voice-rooms";
const FILE_NAME: &str = "settings.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseMode {
    Off,
    Standard,
    #[default]
    #[serde(alias = "maximum")]
    Enhanced,
    Strong,
}

impl NoiseMode {
    pub const ALL: [NoiseMode; 4] = [Self::Off, Self::Standard, Self::Enhanced, Self::Strong];

    pub fn uses_own_capture(self) -> bool {
        matches!(self, Self::Enhanced | Self::Strong)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Desligada",
            Self::Standard => "Reduzida",
            Self::Enhanced => "Reforçada",
            Self::Strong => "Intensa",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceMode {
    #[default]
    VoiceActivity,
    PushToTalk,
}

impl VoiceMode {
    pub const ALL: [VoiceMode; 2] = [Self::VoiceActivity, Self::PushToTalk];

    pub fn label(self) -> &'static str {
        match self {
            Self::VoiceActivity => "Detecção de voz",
            Self::PushToTalk => "Push-to-talk",
        }
    }
}

/// Default noise gate: cuts what is not voice between words in the own capture modes.
pub const DEFAULT_NOISE_GATE_DB: i8 = -45;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub client_id: String,
    pub display_name: String,
    pub token_server_url: String,
    pub last_room: String,
    pub muted_on_join: bool,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub noise_suppression: NoiseMode,
    pub voice_mode: VoiceMode,
    /// Stream sound volume, 0 to 100; absent means 100.
    pub stream_volume: Option<u8>,
    /// Individual volume of each remote participant, 0 to 200; absent means 100.
    pub participant_volumes: BTreeMap<String, u16>,
    /// Noise gate threshold in dBFS (-70 to -20); absent means off. On by default; the key was
    /// renamed then, so the off that older versions saved for everyone is not read back.
    #[serde(rename = "noise_gate_threshold_db")]
    pub noise_gate_db: Option<i8>,
    /// Echo cancellation of the own capture modes; off by default because headphones need none.
    pub echo_cancellation: bool,
    /// Global hotkey in the `global-hotkey` format (e.g. "shift+control+KeyM"); absent = none.
    pub mute_hotkey: Option<String>,
    pub deafen_hotkey: Option<String>,
    pub ptt_hotkey: Option<String>,
    pub close_to_tray: bool,
    pub room_sounds: bool,
    pub dismissed_update_version: Option<String>,
    /// Version of the profile photo uploaded to the server; absent means no photo.
    pub avatar_version: Option<String>,
    /// Keys of already-unlocked password-protected rooms, by server and room.
    pub room_keys: RoomKeys,
}

/// Access keys for password-protected rooms, by server and room; never stores the password.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomKeys(BTreeMap<String, BTreeMap<String, String>>);

impl RoomKeys {
    pub fn for_server(&self, server: &str) -> BTreeMap<String, String> {
        self.0.get(server).cloned().unwrap_or_default()
    }

    pub fn insert(&mut self, server: &str, room: &str, key: &str) {
        self.0
            .entry(server.to_owned())
            .or_default()
            .insert(room.to_owned(), key.to_owned());
    }

    /// Removes the keys of the given rooms; returns `true` if any existed.
    pub fn remove(&mut self, server: &str, rooms: &[&str]) -> bool {
        let Some(keys) = self.0.get_mut(server) else {
            return false;
        };
        let before = keys.len();
        keys.retain(|room, _| !rooms.contains(&room.as_str()));
        let removed = keys.len() != before;
        if keys.is_empty() {
            self.0.remove(server);
        }
        removed
    }
}

impl fmt::Debug for RoomKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rooms: BTreeMap<&str, Vec<&str>> = self
            .0
            .iter()
            .map(|(server, keys)| (server.as_str(), keys.keys().map(String::as_str).collect()))
            .collect();
        f.debug_tuple("RoomKeys").field(&rooms).finish()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            display_name: String::new(),
            token_server_url: String::new(),
            last_room: String::new(),
            muted_on_join: false,
            input_device: None,
            output_device: None,
            noise_suppression: NoiseMode::default(),
            voice_mode: VoiceMode::default(),
            stream_volume: None,
            participant_volumes: BTreeMap::new(),
            noise_gate_db: Some(DEFAULT_NOISE_GATE_DB),
            echo_cancellation: false,
            mute_hotkey: None,
            deafen_hotkey: None,
            ptt_hotkey: None,
            close_to_tray: true,
            room_sounds: true,
            dismissed_update_version: None,
            avatar_version: None,
            room_keys: RoomKeys::default(),
        }
    }
}

pub fn is_valid_client_id_hex(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn needs_client_id(id: &str) -> bool {
    !is_valid_client_id_hex(id)
}

pub fn generate_client_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h1 = RandomState::new().build_hasher();
    h1.write_u128(nanos);
    let a = h1.finish();
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u128(nanos);
    h2.write_u64(a);
    let b = h2.finish();
    format!("{a:016x}{b:016x}")
}

pub fn ensure_client_id(settings: &mut Settings) -> bool {
    if needs_client_id(&settings.client_id) {
        settings.client_id = generate_client_id();
        true
    } else {
        false
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not write settings file: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not encode settings: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("settings path has no parent directory")]
    NoParent,
}

pub const CONFIG_DIR_ENV: &str = "ZENHA_CONFIG_DIR";

pub fn config_dir_override() -> Option<PathBuf> {
    std::env::var_os(CONFIG_DIR_ENV)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

pub fn default_path() -> Option<PathBuf> {
    if let Some(dir) = config_dir_override() {
        return Some(dir.join(FILE_NAME));
    }
    let config = BaseDirs::new()?.config_dir().to_path_buf();
    let path = config.join(APP_DIR).join(FILE_NAME);
    migrate_legacy(&config.join(LEGACY_APP_DIR).join(FILE_NAME), &path);
    Some(path)
}

fn migrate_legacy(legacy: &Path, path: &Path) {
    if path.exists() || !legacy.exists() {
        return;
    }
    let copied = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::copy(legacy, path));
    match copied {
        Ok(_) => tracing::info!(from = %legacy.display(), "migrated legacy settings"),
        Err(err) => tracing::warn!("could not migrate legacy settings: {err}"),
    }
}

pub fn load(path: &Path) -> Settings {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("could not read settings, using defaults: {err}");
            }
            return Settings::default();
        }
    };
    serde_json::from_str(&text).unwrap_or_else(|err| {
        tracing::warn!("settings file is corrupt, using defaults: {err}");
        Settings::default()
    })
}

pub fn save(path: &Path, settings: &Settings) -> Result<(), SettingsError> {
    let parent = path.parent().ok_or(SettingsError::NoParent)?;
    std::fs::create_dir_all(parent)?;
    let json = serde_json::to_string_pretty(settings)?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut file, json.as_bytes())?;
        // Flushed before the rename, or a crash can leave an empty settings file behind.
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn save_in_background(path: PathBuf, settings: Settings) {
    std::thread::spawn(move || {
        if let Err(err) = save(&path, &settings) {
            tracing::warn!("could not save settings: {err}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("zenha-test-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn migrates_legacy_file_once_without_overwriting() {
        let dir = temp_dir("migrate");
        let legacy = dir.join(LEGACY_APP_DIR).join(FILE_NAME);
        let path = dir.join(APP_DIR).join(FILE_NAME);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, "old").unwrap();

        migrate_legacy(&legacy, &path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");

        std::fs::write(&legacy, "newer legacy").unwrap();
        migrate_legacy(&legacy, &path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        assert!(legacy.exists());
    }

    #[test]
    fn round_trips_through_a_file() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("nested").join(FILE_NAME);
        let settings = Settings {
            client_id: "0123456789abcdef0123456789abcdef".to_owned(),
            display_name: "Pedro".to_owned(),
            token_server_url: "http://localhost:8080".to_owned(),
            last_room: "geral".to_owned(),
            muted_on_join: true,
            input_device: Some("{0.0.1.00000000}.{abc}".to_owned()),
            output_device: None,
            noise_suppression: NoiseMode::Enhanced,
            voice_mode: VoiceMode::PushToTalk,
            stream_volume: Some(40),
            participant_volumes: BTreeMap::from([
                ("alice-id".to_owned(), 70),
                ("bob-id".to_owned(), 160),
            ]),
            noise_gate_db: Some(-45),
            echo_cancellation: true,
            mute_hotkey: Some("shift+control+KeyM".to_owned()),
            deafen_hotkey: Some("alt+KeyD".to_owned()),
            ptt_hotkey: Some("F9".to_owned()),
            close_to_tray: false,
            room_sounds: false,
            dismissed_update_version: Some("0.2.0".to_owned()),
            avatar_version: Some("0123456789abcdef".to_owned()),
            room_keys: RoomKeys::default(),
        };
        save(&path, &settings).unwrap();
        assert_eq!(load(&path), settings);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_participant_volumes_defaults_to_empty_map() {
        let parsed: Settings = serde_json::from_str(r#"{"display_name":"Ana"}"#).unwrap();
        assert!(parsed.participant_volumes.is_empty());
    }

    #[test]
    fn noise_mode_defaults_to_enhanced_and_keeps_serde_compatibility() {
        assert_eq!(NoiseMode::default(), NoiseMode::Enhanced);
        assert_eq!(Settings::default().noise_suppression, NoiseMode::Enhanced);
        assert_eq!(
            serde_json::from_str::<NoiseMode>("\"maximum\"").unwrap(),
            NoiseMode::Enhanced
        );
        assert_eq!(
            serde_json::from_str::<Settings>(r#"{"noise_suppression":"maximum"}"#)
                .unwrap()
                .noise_suppression,
            NoiseMode::Enhanced
        );
        assert_eq!(
            serde_json::to_string(&NoiseMode::Enhanced).unwrap(),
            "\"enhanced\""
        );
        assert_eq!(
            serde_json::from_str::<NoiseMode>("\"standard\"").unwrap(),
            NoiseMode::Standard
        );
        assert_eq!(
            serde_json::to_string(&NoiseMode::Standard).unwrap(),
            "\"standard\""
        );
        assert_eq!(NoiseMode::Off.label(), "Desligada");
        assert_eq!(NoiseMode::Standard.label(), "Reduzida");
        assert_eq!(NoiseMode::Enhanced.label(), "Reforçada");
        assert_eq!(NoiseMode::Strong.label(), "Intensa");
        assert_eq!(
            serde_json::from_str::<NoiseMode>("\"strong\"").unwrap(),
            NoiseMode::Strong
        );
        assert_eq!(
            NoiseMode::ALL,
            [
                NoiseMode::Off,
                NoiseMode::Standard,
                NoiseMode::Enhanced,
                NoiseMode::Strong
            ]
        );
        let own: Vec<_> = NoiseMode::ALL
            .into_iter()
            .filter(|mode| mode.uses_own_capture())
            .collect();
        assert_eq!(own, [NoiseMode::Enhanced, NoiseMode::Strong]);
    }

    #[test]
    fn save_overwrites_existing_file() {
        let dir = temp_dir("overwrite");
        let path = dir.join(FILE_NAME);
        save(&path, &Settings::default()).unwrap();
        let updated = Settings {
            last_room: "jogos".to_owned(),
            ..Settings::default()
        };
        save(&path, &updated).unwrap();
        assert_eq!(load(&path), updated);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = temp_dir("missing");
        assert_eq!(load(&dir.join(FILE_NAME)), Settings::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_file_yields_defaults() {
        let dir = temp_dir("corrupt");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), Settings::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn noise_gate_is_on_even_where_older_versions_saved_it_off() {
        assert_eq!(
            Settings::default().noise_gate_db,
            Some(DEFAULT_NOISE_GATE_DB)
        );
        let dir = temp_dir("gate");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{"display_name":"Ana","noise_gate_db":null}"#).unwrap();
        assert_eq!(load(&path).noise_gate_db, Some(DEFAULT_NOISE_GATE_DB));
        std::fs::write(
            &path,
            r#"{"display_name":"Ana","noise_gate_threshold_db":null}"#,
        )
        .unwrap();
        assert_eq!(load(&path).noise_gate_db, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_from_before_device_selection_still_loads() {
        let dir = temp_dir("old");
        let path = dir.join(FILE_NAME);
        std::fs::write(
            &path,
            r#"{"display_name":"Ana","token_server_url":"http://x","last_room":"geral","muted_on_join":true}"#,
        )
        .unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.display_name, "Ana");
        assert!(loaded.muted_on_join);
        assert_eq!(loaded.input_device, None);
        assert_eq!(loaded.output_device, None);
        assert_eq!(loaded.voice_mode, VoiceMode::VoiceActivity);
        assert_eq!(loaded.ptt_hotkey, None);
        assert!(loaded.close_to_tray);
        assert!(loaded.room_sounds);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn room_sounds_defaults_to_true_and_round_trips_false() {
        let dir = temp_dir("room_sounds");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{"display_name":"Ana"}"#).unwrap();
        assert!(load(&path).room_sounds);

        let settings = Settings {
            room_sounds: false,
            ..Settings::default()
        };
        save(&path, &settings).unwrap();
        assert!(!load(&path).room_sounds);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn close_to_tray_defaults_to_true_and_round_trips_false() {
        let dir = temp_dir("tray");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{"display_name":"Ana"}"#).unwrap();
        assert!(load(&path).close_to_tray);

        let settings = Settings {
            close_to_tray: false,
            ..Settings::default()
        };
        save(&path, &settings).unwrap();
        assert!(!load(&path).close_to_tray);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn partial_file_fills_missing_fields() {
        let dir = temp_dir("partial");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{"display_name":"Ana"}"#).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.display_name, "Ana");
        assert_eq!(loaded.last_room, "");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn client_id_is_generated_once_when_empty_and_has_32_hex_chars() {
        let mut settings = Settings::default();
        assert!(needs_client_id(&settings.client_id));
        assert!(ensure_client_id(&mut settings));
        assert!(!needs_client_id(&settings.client_id));
        assert!(is_valid_client_id_hex(&settings.client_id));
        assert!(zenha_protocol::validate_client_id(&settings.client_id).is_ok());

        let first = settings.client_id.clone();
        assert!(!ensure_client_id(&mut settings));
        assert_eq!(settings.client_id, first);

        assert!(!is_valid_client_id_hex("short"));
        assert!(!is_valid_client_id_hex("0123456789ABCDEF0123456789abcdef"));
        assert!(!is_valid_client_id_hex("0123456789abcdef0123456789abcdeg"));
    }

    #[test]
    fn avatar_version_defaults_to_none_and_round_trips() {
        let dir = temp_dir("avatar-version");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{"display_name":"Ana"}"#).unwrap();
        assert_eq!(load(&path).avatar_version, None);

        let settings = Settings {
            avatar_version: Some("0123456789abcdef".to_owned()),
            ..Settings::default()
        };
        save(&path, &settings).unwrap();
        assert_eq!(
            load(&path).avatar_version.as_deref(),
            Some("0123456789abcdef")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dismissed_update_version_defaults_to_none_and_round_trips() {
        let dir = temp_dir("dismissed-update");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, r#"{"display_name":"Ana"}"#).unwrap();
        assert_eq!(load(&path).dismissed_update_version, None);

        let settings = Settings {
            dismissed_update_version: Some("0.3.1".to_owned()),
            ..Settings::default()
        };
        save(&path, &settings).unwrap();
        assert_eq!(
            load(&path).dismissed_update_version.as_deref(),
            Some("0.3.1")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn room_keys_round_trip_per_server_and_stay_out_of_debug() {
        let mut settings = Settings::default();
        settings
            .room_keys
            .insert("https://voz.exemplo", "vip", "chave-secreta");
        settings
            .room_keys
            .insert("https://outro.exemplo", "geral", "outra-chave");
        assert!(!format!("{settings:?}").contains("chave-secreta"));
        assert!(format!("{settings:?}").contains("vip"));

        let json = serde_json::to_string(&settings).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, settings);
        assert_eq!(
            back.room_keys.for_server("https://voz.exemplo"),
            BTreeMap::from([("vip".to_owned(), "chave-secreta".to_owned())])
        );

        let old: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.room_keys, RoomKeys::default());

        assert!(
            settings
                .room_keys
                .remove("https://voz.exemplo", &["vip", "x"])
        );
        assert!(!settings.room_keys.remove("https://voz.exemplo", &["vip"]));
        assert!(
            settings
                .room_keys
                .for_server("https://voz.exemplo")
                .is_empty()
        );
    }
}
