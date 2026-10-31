//! Lecture de `version.ini`, unique source de vérité pour l'identité de l'application.
//!
//! Traduction directe de `src/mad68/version.py`.
//! Le script d'installation Inno Setup (`packaging/mad68.iss`) lit le même fichier via `ReadIni`,
//! garantissant une synchronisation parfaite entre l'exécutable, l'installeur et les mises à jour.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Valeurs de repli utilisées uniquement si `version.ini` est introuvable.
pub const DEFAULT_APP_NAME: &str = "OpenMAD";
pub const DEFAULT_APP_VERSION: &str = "0.0.0";
pub const DEFAULT_GITHUB_REPO: &str = "";
pub const DEFAULT_PUBLISHER: &str = "";
pub const DEFAULT_PROJECT_URL: &str = "";

/// Informations d'identité de l'application extraites de la section `[app]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub repo: String,
    pub publisher: String,
    pub url: String,
}

impl Default for AppInfo {
    fn default() -> Self {
        Self {
            name: DEFAULT_APP_NAME.to_string(),
            version: DEFAULT_APP_VERSION.to_string(),
            repo: DEFAULT_GITHUB_REPO.to_string(),
            publisher: DEFAULT_PUBLISHER.to_string(),
            url: DEFAULT_PROJECT_URL.to_string(),
        }
    }
}

/// Analyse le contenu textuel d'un fichier INI et extrait la section `[app]`.
///
/// Cette fonction gère :
/// - Les commentaires commençant par `;` ou `#`
/// - Les espaces superflus autour des clés et valeurs
/// - Les guillemets optionnels entourant les valeurs
pub fn parse_version_ini(content: &str) -> AppInfo {
    let mut in_app_section = false;
    let mut map: HashMap<String, String> = HashMap::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            let section_name = line[1..line.len() - 1].trim();
            in_app_section = section_name.eq_ignore_ascii_case("app");
            continue;
        }

        if in_app_section {
            if let Some((key, val)) = line.split_once('=') {
                let k = key.trim().to_ascii_lowercase();
                let mut v = val.trim();
                // Retire d'éventuels guillemets simples ou doubles
                if (v.starts_with('"') && v.ends_with('"'))
                    || (v.starts_with('\'') && v.ends_with('\''))
                {
                    if v.len() >= 2 {
                        v = &v[1..v.len() - 1];
                    }
                }
                map.insert(k, v.to_string());
            }
        }
    }

    AppInfo {
        name: map.remove("name").unwrap_or_else(|| DEFAULT_APP_NAME.to_string()),
        version: map.remove("version").unwrap_or_else(|| DEFAULT_APP_VERSION.to_string()),
        repo: map.remove("repo").unwrap_or_else(|| DEFAULT_GITHUB_REPO.to_string()),
        publisher: map.remove("publisher").unwrap_or_else(|| DEFAULT_PUBLISHER.to_string()),
        url: map.remove("url").unwrap_or_else(|| DEFAULT_PROJECT_URL.to_string()),
    }
}

/// Localise le fichier `version.ini` sur le disque selon les conventions de chemins :
/// 1. À côté de l'exécutable en cours d'exécution (cas d'un binaire déployé ou portable)
/// 2. Dans le répertoire racine du dépôt Cargo (développement / tests)
/// 3. Dans le répertoire de travail actuel
pub fn locate_version_ini() -> Option<PathBuf> {
    // 1. À côté de l'exécutable
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let candidate = exe_dir.join("version.ini");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // 2. Racine du projet Cargo (défini à la compilation)
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_candidate = manifest_dir.join("version.ini");
    if manifest_candidate.is_file() {
        return Some(manifest_candidate);
    }

    // 3. Répertoire courant
    let cur_candidate = Path::new("version.ini");
    if cur_candidate.is_file() {
        return Some(cur_candidate.to_path_buf());
    }

    None
}

/// Charge les informations d'application depuis `version.ini` ou retourne les valeurs par défaut.
pub fn load_app_info() -> AppInfo {
    if let Some(path) = locate_version_ini() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            return parse_version_ini(&content);
        }
    }
    AppInfo::default()
}

/// Accès statique mis en cache aux informations globales d'OpenMAD.
pub fn get_app_info() -> &'static AppInfo {
    static INSTANCE: OnceLock<AppInfo> = OnceLock::new();
    INSTANCE.get_or_init(load_app_info)
}

pub fn app_name() -> &'static str {
    &get_app_info().name
}

pub fn app_version() -> &'static str {
    &get_app_info().version
}

pub fn github_repo() -> &'static str {
    &get_app_info().repo
}

pub fn publisher() -> &'static str {
    &get_app_info().publisher
}

pub fn project_url() -> &'static str {
    &get_app_info().url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_ini_nominal() {
        let ini = r#"
; Commentaire
[app]
name = OpenMAD
version = 1.1.4
repo = Acciaw/OpenMAD
publisher = Acciaw
url = https://github.com/Acciaw/OpenMAD
"#;
        let info = parse_version_ini(ini);
        assert_eq!(info.name, "OpenMAD");
        assert_eq!(info.version, "1.1.4");
        assert_eq!(info.repo, "Acciaw/OpenMAD");
        assert_eq!(info.publisher, "Acciaw");
        assert_eq!(info.url, "https://github.com/Acciaw/OpenMAD");
    }

    #[test]
    fn parse_version_ini_empty_and_defaults() {
        let ini = "[other]\nfoo=bar";
        let info = parse_version_ini(ini);
        assert_eq!(info, AppInfo::default());
    }

    #[test]
    fn load_real_version_ini() {
        let info = get_app_info();
        assert_eq!(info.name, "OpenMAD");
        assert_eq!(info.version, "1.1.4");
        assert_eq!(info.repo, "Acciaw/OpenMAD");
    }
}

