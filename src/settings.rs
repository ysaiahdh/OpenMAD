//! Paramètres généraux de l'application (mises à jour, lancement au démarrage).
//!
//! Traduction directe de `src/mad68/settings.py`.
//! Ces paramètres concernent le comportement de l'outil plutôt que la mémoire du clavier.
//! Le fichier `settings.json` est stocké à côté du dossier des profils.

use std::path::Path;
use serde::{Deserialize, Serialize};

pub const RUN_KEY_NAME: &str = "OpenMAD";
pub const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const INSTALLER_KEY_PATH: &str = r"Software\OpenMAD";

fn default_check_updates() -> bool {
    true
}

/// Paramètres généraux persistés dans `settings.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    /// Vérifier la présence d'une nouvelle version sur GitHub au démarrage.
    #[serde(default = "default_check_updates")]
    pub check_updates: bool,

    /// Reflète l'entrée de registre `Run` de Windows pour lancer OpenMAD à la connexion.
    /// Le registre Windows fait foi en cas de divergence.
    #[serde(default)]
    pub start_at_login: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            check_updates: true,
            start_at_login: false,
        }
    }
}

impl AppSettings {
    /// Charge les paramètres depuis un fichier JSON.
    ///
    /// Si le fichier est manquant ou illisible, interroge la clé de registre écrite
    /// par l'installeur Inno Setup (`Software\OpenMAD\CheckUpdates`) et l'état
    /// actuel du démarrage automatique sous Windows.
    pub fn load(path: &Path) -> Self {
        if let Ok(bytes) = std::fs::read(path) {
            // Élimine le BOM UTF-8 (utf-8-sig) si présent
            let slice = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                &bytes[3..]
            } else {
                &bytes[..]
            };

            if let Ok(settings) = serde_json::from_slice::<AppSettings>(slice) {
                return settings;
            }
        }

        // Repli : honore ce que l'assistant d'installation a consigné dans le registre
        Self {
            check_updates: installer_check_updates(true),
            start_at_login: get_start_at_login(),
        }
    }

    /// Enregistre les paramètres dans le fichier JSON spécifié.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json_str = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json_str)
    }
}

/// Commande à exécuter pour le démarrage automatique Windows.
pub fn launch_command() -> String {
    if let Ok(exe) = std::env::current_exe() {
        format!("\"{}\"", exe.display())
    } else {
        format!("\"{}.exe\"", crate::version::app_name())
    }
}

#[cfg(windows)]
mod win_reg {
    use super::*;
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, REG_SZ,
    };

    pub fn get_start_at_login() -> bool {
        unsafe {
            let subkey = HSTRING::from(RUN_KEY_PATH);
            let mut hkey = HKEY::default();
            if RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, 0, KEY_READ, &mut hkey).is_err() {
                return false;
            }

            let value_name = HSTRING::from(RUN_KEY_NAME);
            let res = RegQueryValueExW(hkey, &value_name, None, None, None, None);
            let _ = RegCloseKey(hkey);
            res.is_ok()
        }
    }

    pub fn set_start_at_login(enabled: bool) -> bool {
        unsafe {
            let subkey = HSTRING::from(RUN_KEY_PATH);
            let mut hkey = HKEY::default();
            if RegCreateKeyW(HKEY_CURRENT_USER, &subkey, &mut hkey).is_err() {
                return get_start_at_login();
            }

            let value_name = HSTRING::from(RUN_KEY_NAME);
            if enabled {
                let cmd = launch_command();
                let cmd_wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes = std::slice::from_raw_parts(
                    cmd_wide.as_ptr() as *const u8,
                    cmd_wide.len() * std::mem::size_of::<u16>(),
                );
                let _ = RegSetValueExW(hkey, &value_name, 0, REG_SZ, Some(bytes));
            } else {
                let _ = RegDeleteValueW(hkey, &value_name);
            }

            let _ = RegCloseKey(hkey);
            get_start_at_login()
        }
    }

    pub fn installer_check_updates(default: bool) -> bool {
        unsafe {
            let subkey = HSTRING::from(INSTALLER_KEY_PATH);
            let mut hkey = HKEY::default();
            if RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, 0, KEY_READ, &mut hkey).is_err() {
                return default;
            }

            let value_name = HSTRING::from("CheckUpdates");
            let mut val: u32 = 0;
            let mut val_len: u32 = std::mem::size_of::<u32>() as u32;
            let res = RegQueryValueExW(
                hkey,
                &value_name,
                None,
                None,
                Some(&mut val as *mut u32 as *mut u8),
                Some(&mut val_len),
            );
            let _ = RegCloseKey(hkey);

            if res.is_ok() {
                val != 0
            } else {
                default
            }
        }
    }
}

#[cfg(not(windows))]
mod win_reg {
    pub fn get_start_at_login() -> bool {
        false
    }
    pub fn set_start_at_login(_enabled: bool) -> bool {
        false
    }
    pub fn installer_check_updates(default: bool) -> bool {
        default
    }
}

pub use win_reg::{get_start_at_login, installer_check_updates, set_start_at_login};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_json_roundtrip_nominal() {
        let original = r#"{
  "check_updates": true,
  "start_at_login": false
}"#;
        let parsed: AppSettings = serde_json::from_str(original).expect("parse json");
        assert!(parsed.check_updates);
        assert!(!parsed.start_at_login);

        let serialized = serde_json::to_string_pretty(&parsed).expect("serialize json");
        // Normalise les fins de ligne pour comparer
        assert_eq!(
            serialized.replace("\r\n", "\n"),
            original.replace("\r\n", "\n")
        );
    }

    #[test]
    fn settings_json_load_real_file() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let settings_path = manifest_dir.join("settings.json");
        assert!(settings_path.is_file(), "settings.json must exist in repo root");

        let loaded = AppSettings::load(&settings_path);
        assert!(loaded.check_updates);
        assert!(!loaded.start_at_login);
    }

    #[test]
    fn settings_json_defaults_when_empty() {
        let empty_json = "{}";
        let parsed: AppSettings = serde_json::from_str(empty_json).expect("parse empty json");
        assert!(parsed.check_updates);
        assert!(!parsed.start_at_login);
    }
}
