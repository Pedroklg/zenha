use std::path::{Path, PathBuf};

use directories::BaseDirs;
use serde::{Deserialize, Serialize};

const APP_DIR: &str = "voice-rooms";
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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub display_name: String,
    pub token_server_url: String,
    pub last_room: String,
    pub muted_on_join: bool,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub noise_suppression: NoiseMode,
    /// Volume do som das transmissões, de 0 a 100; ausente significa 100.
    pub stream_volume: Option<u8>,
    /// Limiar da porta de ruído em dBFS (-70 a -20); ausente significa desligada.
    pub noise_gate_db: Option<i8>,
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
    BaseDirs::new().map(|dirs| dirs.config_dir().join(APP_DIR).join(FILE_NAME))
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
        let dir = std::env::temp_dir().join(format!(
            "voice-rooms-test-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_through_a_file() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("nested").join(FILE_NAME);
        let settings = Settings {
            display_name: "Pedro".to_owned(),
            token_server_url: "http://localhost:8080".to_owned(),
            last_room: "geral".to_owned(),
            muted_on_join: true,
            input_device: Some("{0.0.1.00000000}.{abc}".to_owned()),
            output_device: None,
            noise_suppression: NoiseMode::Enhanced,
            stream_volume: Some(40),
            noise_gate_db: Some(-45),
        };
        save(&path, &settings).unwrap();
        assert_eq!(load(&path), settings);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
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
}
