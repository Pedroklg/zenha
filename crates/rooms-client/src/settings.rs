use std::collections::BTreeMap;
use std::collections::hash_map::RandomState;
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
    #[default]
    Standard,
    Enhanced,
    Maximum,
}

impl NoiseMode {
    pub const ALL: [NoiseMode; 4] = [Self::Off, Self::Standard, Self::Enhanced, Self::Maximum];

    pub fn uses_own_capture(self) -> bool {
        matches!(self, Self::Enhanced | Self::Maximum)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Desligada",
            Self::Standard => "Padrão",
            Self::Enhanced => "Reforçada",
            Self::Maximum => "Máxima",
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
    /// Volume do som das transmissões, de 0 a 100; ausente significa 100.
    pub stream_volume: Option<u8>,
    /// Volume individual de cada participante remoto, de 0 a 200; ausente significa 100.
    pub participant_volumes: BTreeMap<String, u16>,
    /// Limiar da porta de ruído em dBFS (-70 a -20); ausente significa desligada.
    pub noise_gate_db: Option<i8>,
    /// Atalho global no formato do `global-hotkey` (ex.: "shift+control+KeyM"); ausente = nenhum.
    pub mute_hotkey: Option<String>,
    pub deafen_hotkey: Option<String>,
    pub ptt_hotkey: Option<String>,
    pub close_to_tray: bool,
    pub dismissed_update_version: Option<String>,
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
            noise_gate_db: None,
            mute_hotkey: None,
            deafen_hotkey: None,
            ptt_hotkey: None,
            close_to_tray: true,
            dismissed_update_version: None,
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

pub fn default_path() -> Option<PathBuf> {
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
    std::fs::write(&tmp, json)?;
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
            mute_hotkey: Some("shift+control+KeyM".to_owned()),
            deafen_hotkey: Some("alt+KeyD".to_owned()),
            ptt_hotkey: Some("F9".to_owned()),
            close_to_tray: false,
            dismissed_update_version: Some("0.2.0".to_owned()),
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
    fn maximum_mode_serializes_as_snake_case_and_uses_its_own_capture() {
        assert_eq!(
            serde_json::to_string(&NoiseMode::Maximum).unwrap(),
            "\"maximum\""
        );
        assert_eq!(
            serde_json::from_str::<NoiseMode>("\"maximum\"").unwrap(),
            NoiseMode::Maximum
        );
        assert_eq!(NoiseMode::Maximum.label(), "Máxima");
        assert!(NoiseMode::ALL.contains(&NoiseMode::Maximum));
        let own: Vec<_> = NoiseMode::ALL
            .into_iter()
            .filter(|mode| mode.uses_own_capture())
            .collect();
        assert_eq!(own, [NoiseMode::Enhanced, NoiseMode::Maximum]);
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
        assert!(rooms_protocol::validate_client_id(&settings.client_id).is_ok());

        let first = settings.client_id.clone();
        assert!(!ensure_client_id(&mut settings));
        assert_eq!(settings.client_id, first);

        assert!(!is_valid_client_id_hex("short"));
        assert!(!is_valid_client_id_hex("0123456789ABCDEF0123456789abcdef"));
        assert!(!is_valid_client_id_hex("0123456789abcdef0123456789abcdeg"));
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
}
