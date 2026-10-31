//! Gestion des chemins de l'application : ressources embarquées vs données utilisateur.
//!
//! Traduction directe de `src/mad68/paths.py`.
//!
//! En mode développement (depuis l'arbre source Cargo) :
//! - `resource_dir()` et `data_dir()` pointent tous deux vers la racine du dépôt.
//!
//! En mode déployé / packagé (`is_frozen()` == true) :
//! - `resource_dir()` pointe vers le dossier de l'exécutable (fichiers en lecture seule).
//! - `data_dir()` pointe vers `%APPDATA%\OpenMAD` (ou `%LOCALAPPDATA%\OpenMAD`) pour
//!   conserver les profils, réglages et configurations utilisateur entre les redémarrages.

use std::path::PathBuf;

use crate::version::app_name;

/// Indique si l'application s'exécute en binaire autonome packagé ou depuis une arborescence de développement.
///
/// La détection suit les règles suivantes :
/// 1. Variable d'environnement `OPENMAD_FROZEN` ("1" ou "true" = forcé packagé, "0" ou "false" = forcé dev).
/// 2. Présence de l'environnement Cargo (`CARGO_MANIFEST_DIR`) ou existence d'un `Cargo.toml`
///    dans les dossiers parents de l'exécutable.
pub fn is_frozen() -> bool {
    if let Ok(val) = std::env::var("OPENMAD_FROZEN") {
        let v = val.trim().to_ascii_lowercase();
        if v == "1" || v == "true" {
            return true;
        }
        if v == "0" || v == "false" {
            return false;
        }
    }

    // Si la variable Cargo est définie (cargo run, cargo test), nous sommes en mode dev.
    if std::env::var("CARGO_MANIFEST_DIR").is_ok() {
        return false;
    }

    // Vérifie si l'exécutable réside dans un dossier de build cargo (ex: target/debug ou target/release)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            // target/debug -> parent: target/debug, grandparent: target, great-grandparent: root
            if let Some(target_dir) = parent.parent() {
                if target_dir.file_name().and_then(|n| n.to_str()) == Some("target") {
                    if let Some(repo_root) = target_dir.parent() {
                        if repo_root.join("Cargo.toml").is_file() {
                            return false;
                        }
                    }
                }
            }
        }
    }

    true
}

/// Répertoire racine du projet source lors du développement.
fn dev_repo_dir() -> PathBuf {
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        return PathBuf::from(manifest);
    }
    // Fallback à la constante de compilation
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Répertoire des ressources en lecture seule fournies avec l'application
/// (icônes, profils par défaut, etc.).
pub fn resource_dir() -> PathBuf {
    if is_frozen() {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                return parent.to_path_buf();
            }
        }
        PathBuf::from(".")
    } else {
        dev_repo_dir()
    }
}

/// Répertoire des fichiers modifiables par l'utilisateur (paramètres, profils, backups).
///
/// En mode packagé, utilise `%APPDATA%\OpenMAD` (ou `%LOCALAPPDATA%\OpenMAD`) afin
/// de ne pas nécessiter d'élévation de privilèges administrateur.
pub fn data_dir() -> PathBuf {
    if !is_frozen() {
        return dev_repo_dir();
    }

    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("LOCALAPPDATA"))
        .map(PathBuf::from);

    let root = match base {
        Some(dir) => dir,
        None => dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")),
    };

    root.join(app_name())
}

/// Répertoire de stockage des fichiers profils JSON.
pub fn profile_dir() -> PathBuf {
    data_dir().join("profiles")
}

/// Répertoire des ressources visuelles (assets/branding).
pub fn assets_dir() -> PathBuf {
    resource_dir().join("assets")
}

/// Initialise l'environnement utilisateur en créant le dossier de profils
/// et en y copiant les profils initiaux s'il est vide.
///
/// Ne remplace jamais un profil existant de l'utilisateur.
pub fn seed_user_data() -> std::io::Result<PathBuf> {
    let dest = profile_dir();
    std::fs::create_dir_all(&dest)?;

    if !is_frozen() {
        return Ok(dest);
    }

    let src = resource_dir().join("profiles");
    if src.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&src) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Some(file_name) = path.file_name() {
                        let target = dest.join(file_name);
                        if !target.exists() {
                            let _ = std::fs::copy(&path, &target);
                        }
                    }
                }
            }
        }
    }

    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_in_dev_mode() {
        // En mode test Cargo, is_frozen() doit retourner false
        assert!(!is_frozen());
        let res = resource_dir();
        let dat = data_dir();
        assert_eq!(res, dat);
        assert!(res.join("Cargo.toml").is_file());
    }

    #[test]
    fn profile_dir_matches_subpath() {
        assert_eq!(profile_dir(), data_dir().join("profiles"));
    }

    #[test]
    fn assets_dir_matches_subpath() {
        assert_eq!(assets_dir(), resource_dir().join("assets"));
    }
}
