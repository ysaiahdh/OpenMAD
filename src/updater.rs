//! Téléchargement et installation sécurisés des mises à jour depuis GitHub Releases.
//!
//! Traduction directe de `src/mad68/updater.py`.
//!
//! Sécurité et intégrité :
//! - Validation stricte des hôtes autorisés (GitHub et son CDN `objects.githubusercontent.com`).
//! - Filtrage du nom de l'installeur (format `OpenMAD-Setup-*.exe`).
//! - Calcul et vérification de la somme de contrôle SHA-256 et de la taille en octets.
//! - Découplage de l'analyse JSON pour permettre les tests unitaires hors-ligne.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use serde::{Deserialize, Serialize};

/// Domaines et suffixes d'hôtes autorisés pour les téléchargements de releases GitHub.
pub const ALLOWED_HOST_SUFFIXES: &[&str] = &[".githubusercontent.com", ".github.com"];
pub const ALLOWED_HOSTS: &[&str] = &["github.com", "api.github.com", "objects.githubusercontent.com"];

/// Script batch intermédiaire pour fermer l'exécutable appelant et lancer l'installeur Inno Setup.
pub const LAUNCHER_NAME: &str = "install-update.cmd";
pub const LAUNCHER_MAX_TRIES: u32 = 120;
pub const LAUNCH_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
pub const RESTART_SETTLE_SECONDS: u32 = 4;

const CHUNK_SIZE: usize = 64 * 1024;

/// Modèle du script de lancement Windows.
///
/// Le script attend la disparition du PID de l'application avant d'exécuter l'installeur Inno Setup
/// avec `/SILENT /SUPPRESSMSGBOXES /NORESTART /LOG=...`, puis relance le nouveau binaire.
pub const LAUNCHER_CMD_TEMPLATE: &str = "@echo off\r\n\
rem OpenMAD update launcher, written by the app. Safe to delete.\r\n\
rem %1 installer  %2 installer log  %3 PID to wait for  %4 max tries\r\n\
rem %5 app to restart afterwards, or \"\" not to\r\n\
echo [%DATE% %TIME%] waiting for PID %~3 > \"%~dp0launcher.log\"\r\n\
set TRIES=0\r\n\
:wait\r\n\
tasklist /FI \"PID eq %~3\" /NH 2>nul | find \"%~3\" >nul\r\n\
if errorlevel 1 goto run\r\n\
set /a TRIES+=1\r\n\
if %TRIES% GEQ %~4 goto run\r\n\
ping -n 2 127.0.0.1 >nul\r\n\
goto wait\r\n\
:run\r\n\
echo [%DATE% %TIME%] app gone after %TRIES%s, starting installer >> \"%~dp0launcher.log\"\r\n\
%1 /SILENT /SUPPRESSMSGBOXES /NORESTART /LOG=%2\r\n\
echo [%DATE% %TIME%] installer exit code %ERRORLEVEL% >> \"%~dp0launcher.log\"\r\n\
if \"%~5\"==\"\" goto done\r\n\
ping -n 5 127.0.0.1 >nul\r\n\
echo [%DATE% %TIME%] restarting %~5 >> \"%~dp0launcher.log\"\r\n\
start \"\" %5\r\n\
echo [%DATE% %TIME%] restart issued >> \"%~dp0launcher.log\"\r\n\
:done\r\n";

/// Erreurs possibles du sous-système de mise à jour.
#[derive(Debug, thiserror::Error)]
pub enum UpdaterError {
    #[error("requête réseau échouée : {0}")]
    Network(String),

    #[error("analyse JSON échouée : {0}")]
    Json(#[from] serde_json::Error),

    #[error("hôte non autorisé pour le téléchargement : {0}")]
    HostNotAllowed(String),

    #[error("nom de fichier d'installeur inattendu : {0}")]
    InvalidInstallerName(String),

    #[error("taille de téléchargement incohérente : {got} octets reçus, {expected} attendus")]
    SizeMismatch { got: u64, expected: u64 },

    #[error("somme de contrôle SHA-256 non correspondante")]
    ChecksumMismatch,

    #[error("erreur E/S : {0}")]
    Io(#[from] std::io::Error),

    #[error("échec de l'installeur : {0}")]
    Execution(String),
}

/// Asset exécutable sélectionné pour la mise à jour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub digest: String,
}

/// Résultat d'une vérification de mise à jour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCheckResult {
    pub has_update: bool,
    pub current_version: String,
    pub latest_version: String,
    pub asset: Option<ReleaseAsset>,
    pub release_url: String,
}

/// Structure minimale pour désérialiser la réponse de GitHub Releases API.
#[derive(Debug, Deserialize)]
struct GitHubReleaseDto {
    tag_name: String,
    html_url: Option<String>,
    #[serde(default)]
    assets: Vec<GitHubAssetDto>,
}

#[derive(Debug, Deserialize)]
struct GitHubAssetDto {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// Valide si l'URL provient bien d'une source GitHub de confiance.
pub fn is_host_allowed(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("https://") {
        return false;
    }
    // Extrait le host après "https://"
    let without_scheme = &lower[8..];
    let host = without_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");

    if host.is_empty() {
        return false;
    }

    if ALLOWED_HOSTS.contains(&host) {
        return true;
    }

    ALLOWED_HOST_SUFFIXES.iter().any(|&suffix| host.ends_with(suffix))
}

/// Valide le motif du nom d'installeur (ex: `OpenMAD-Setup-1.2.3.exe`).
pub fn is_installer_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with(".exe") {
        return false;
    }
    let without_ext = &lower[..lower.len() - 4];
    if let Some((prefix, ver)) = without_ext.rsplit_once("-setup-") {
        if prefix.is_empty() || ver.is_empty() {
            return false;
        }
        ver.chars().all(|c| c.is_ascii_digit() || c == '.')
    } else {
        false
    }
}

/// Découpe une chaîne sémantique de version en tableau d'entiers (`1.2.3` -> `[1, 2, 3]`).
pub fn parse_version(v: &str) -> Vec<u64> {
    let clean = v.trim().trim_start_matches(|c| c == 'v' || c == 'V');
    clean
        .split('.')
        .filter_map(|part| {
            // Ne conserve que les chiffres initiaux si suffixe (ex: 1.2.3-beta)
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u64>().ok()
        })
        .collect()
}

/// Compare deux versions : renvoie `true` si `candidate` est strictement supérieure à `current`.
pub fn is_newer_version(current: &str, candidate: &str) -> bool {
    let cur = parse_version(current);
    let cand = parse_version(candidate);

    let max_len = cur.len().max(cand.len());
    for i in 0..max_len {
        let c = cur.get(i).copied().unwrap_or(0);
        let n = cand.get(i).copied().unwrap_or(0);
        if n > c {
            return true;
        }
        if n < c {
            return false;
        }
    }
    false
}

/// Analyse le JSON de l'API GitHub Releases et détermine la disponibilité d'une mise à jour.
pub fn parse_release_json(json_str: &str, current_version: &str) -> Result<ReleaseCheckResult, UpdaterError> {
    let release: GitHubReleaseDto = serde_json::from_str(json_str)?;
    let latest_tag = release.tag_name.clone();
    let has_update = is_newer_version(current_version, &latest_tag);

    let mut chosen_asset: Option<ReleaseAsset> = None;
    for a in release.assets {
        if is_installer_name(&a.name) && is_host_allowed(&a.browser_download_url) {
            chosen_asset = Some(ReleaseAsset {
                name: a.name,
                url: a.browser_download_url,
                size: a.size,
                digest: a.digest.unwrap_or_default(),
            });
            break;
        }
    }

    Ok(ReleaseCheckResult {
        has_update,
        current_version: current_version.to_string(),
        latest_version: latest_tag,
        asset: chosen_asset,
        release_url: release.html_url.unwrap_or_default(),
    })
}

/// Interroge l'API GitHub pour vérifier s'il existe une release plus récente.
pub fn check_for_update(repo: &str, current_version: &str) -> Result<ReleaseCheckResult, UpdaterError> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(10))
        .set("User-Agent", "OpenMAD-updater")
        .set("Accept", "application/vnd.github.v3+json")
        .call()
        .map_err(|e| UpdaterError::Network(e.to_string()))?;

    let json_text = resp
        .into_string()
        .map_err(|e| UpdaterError::Network(e.to_string()))?;

    parse_release_json(&json_text, current_version)
}

/// Répertoire temporaire dédié au téléchargement des mises à jour.
pub fn download_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("OpenMAD-update");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Télécharge l'installeur sélectionné en vérifiant sa conformité et son intégrité.
pub fn download_asset(
    asset: &ReleaseAsset,
    stop_flag: Option<Arc<AtomicBool>>,
    progress: Option<Arc<AtomicU64>>,
) -> Result<PathBuf, UpdaterError> {
    if !is_host_allowed(&asset.url) {
        return Err(UpdaterError::HostNotAllowed(asset.url.clone()));
    }
    if !is_installer_name(&asset.name) {
        return Err(UpdaterError::InvalidInstallerName(asset.name.clone()));
    }

    let dest = download_dir().join(&asset.name);
    let part = dest.with_extension("exe.part");

    let resp = ureq::get(&asset.url)
        .timeout(Duration::from_secs(30))
        .set("User-Agent", "OpenMAD-updater")
        .call()
        .map_err(|e| UpdaterError::Network(e.to_string()))?;

    // Vérifie que l'URL finale après redirection reste sur un hôte autorisé
    let final_url = resp.get_url();
    if !is_host_allowed(final_url) {
        return Err(UpdaterError::HostNotAllowed(final_url.to_string()));
    }

    let mut reader = resp.into_reader();
    let mut file = File::create(&part)?;
    let mut buf = [0u8; CHUNK_SIZE];
    let mut done: u64 = 0;

    let mut hasher = Sha256Hasher::new();

    loop {
        if let Some(ref flag) = stop_flag {
            if flag.load(Ordering::Relaxed) {
                let _ = std::fs::remove_file(&part);
                return Err(UpdaterError::Execution("téléchargement interrompu".to_string()));
            }
        }

        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;

        if let Some(ref p) = progress {
            p.store(done, Ordering::Relaxed);
        }
    }

    file.flush()?;
    drop(file);

    // Validation de la taille
    if asset.size > 0 && done != asset.size {
        let _ = std::fs::remove_file(&part);
        return Err(UpdaterError::SizeMismatch {
            got: done,
            expected: asset.size,
        });
    }

    // Validation du digest SHA-256 si fourni par l'API
    let want_digest = asset.digest.trim().to_ascii_lowercase();
    if let Some(hex) = want_digest.strip_prefix("sha256:") {
        let computed = hasher.finalize_hex();
        if computed != hex {
            let _ = std::fs::remove_file(&part);
            return Err(UpdaterError::ChecksumMismatch);
        }
    }

    // Renommage atomique vers le fichier définitif
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}

/// Lance le script batch autonome pour installer la mise à jour et redémarrer l'application.
pub fn launch_installer(installer: &Path, restart_exe: Option<&Path>) -> Result<(), UpdaterError> {
    let dir = download_dir();
    let launcher = dir.join(LAUNCHER_NAME);
    let log = dir.join("install.log");
    let launcher_log = dir.join("launcher.log");

    std::fs::write(&launcher, LAUNCHER_CMD_TEMPLATE)?;
    let _ = std::fs::remove_file(&launcher_log);

    let current_pid = std::process::id();
    let restart_str = restart_exe
        .and_then(|p| p.to_str())
        .unwrap_or("");

    // Arguments : %1 installer  %2 log  %3 PID  %4 max_tries  %5 restart
    let mut cmd = std::process::Command::new("cmd.exe");
    cmd.arg("/C")
        .arg(&launcher)
        .arg(installer)
        .arg(&log)
        .arg(current_pid.to_string())
        .arg(LAUNCHER_MAX_TRIES.to_string())
        .arg(restart_str);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW (0x08000000)
        cmd.creation_flags(0x08000000);
    }

    cmd.spawn().map_err(|e| UpdaterError::Execution(format!("échec création processus : {e}")))?;

    // Vérifie l'apparition du fichier launcher.log pour confirmer l'exécution du script
    let deadline = std::time::Instant::now() + LAUNCH_CONFIRM_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if launcher_log.is_file() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if launcher_log.is_file() {
        Ok(())
    } else {
        Err(UpdaterError::Execution(
            "le launcher de mise à jour n'a pas répondu à temps".to_string(),
        ))
    }
}

/// Calcule la somme de contrôle SHA-256 d'un tampon d'octets au format hexadécimal.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256Hasher::new();
    hasher.update(data);
    hasher.finalize_hex()
}

// Implémentation SHA-256 sans dépendance externe lourde

struct Sha256Hasher {
    state: [u32; 8],
    buffer: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256Hasher {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
            ],
            buffer: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.total_len += data.len() as u64;
        while !data.is_empty() {
            if self.buf_len == 0 && data.len() >= 64 {
                self.transform(&data[..64]);
                data = &data[64..];
            } else {
                let to_copy = (64 - self.buf_len).min(data.len());
                self.buffer[self.buf_len..self.buf_len + to_copy].copy_from_slice(&data[..to_copy]);
                self.buf_len += to_copy;
                data = &data[to_copy..];
                if self.buf_len == 64 {
                    self.transform(&self.buffer.clone());
                    self.buf_len = 0;
                }
            }
        }
    }

    fn transform(&mut self, chunk: &[u8]) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
            0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
            0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
            0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
            0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
            0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
            0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
        ];

        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }

    fn finalize_hex(mut self) -> String {
        let bit_len = self.total_len * 8;
        self.buffer[self.buf_len] = 0x80;
        self.buf_len += 1;

        if self.buf_len > 56 {
            for i in self.buf_len..64 {
                self.buffer[i] = 0;
            }
            self.transform(&self.buffer.clone());
            self.buf_len = 0;
        }

        for i in self.buf_len..56 {
            self.buffer[i] = 0;
        }
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        self.transform(&self.buffer.clone());

        let mut hex = String::with_capacity(64);
        for word in self.state {
            use std::fmt::Write;
            let _ = write!(&mut hex, "{:08x}", word);
        }
        hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowed_validation() {
        assert!(is_host_allowed("https://github.com/Acciaw/OpenMAD/releases/download/v1.1.4/OpenMAD-Setup-1.1.4.exe"));
        assert!(is_host_allowed("https://objects.githubusercontent.com/github-production-release-asset-2e65be/12345"));
        assert!(is_host_allowed("https://api.github.com/repos/Acciaw/OpenMAD"));
        assert!(!is_host_allowed("http://github.com/test")); // Non-HTTPS
        assert!(!is_host_allowed("https://malicious.com/OpenMAD-Setup.exe"));
    }

    #[test]
    fn installer_name_validation() {
        assert!(is_installer_name("OpenMAD-Setup-1.1.4.exe"));
        assert!(is_installer_name("openmad-setup-2.0.0.exe"));
        assert!(!is_installer_name("OpenMAD-Portable.exe"));
        assert!(!is_installer_name("source.tar.gz"));
    }

    #[test]
    fn version_parsing_and_comparison() {
        assert_eq!(parse_version("1.1.4"), vec![1, 1, 4]);
        assert_eq!(parse_version("v2.0.1"), vec![2, 0, 1]);

        assert!(is_newer_version("1.1.4", "1.1.5"));
        assert!(is_newer_version("1.1.4", "1.2.0"));
        assert!(is_newer_version("1.1.4", "2.0.0"));
        assert!(!is_newer_version("1.1.4", "1.1.4"));
        assert!(!is_newer_version("1.1.4", "1.1.3"));
        assert!(!is_newer_version("1.2.0", "1.1.9"));
    }

    #[test]
    fn parse_release_json_nominal() {
        let json = r#"{
  "tag_name": "v1.2.0",
  "html_url": "https://github.com/Acciaw/OpenMAD/releases/tag/v1.2.0",
  "assets": [
    {
      "name": "source.zip",
      "browser_download_url": "https://github.com/Acciaw/OpenMAD/archive/refs/tags/v1.2.0.zip",
      "size": 1024
    },
    {
      "name": "OpenMAD-Setup-1.2.0.exe",
      "browser_download_url": "https://github.com/Acciaw/OpenMAD/releases/download/v1.2.0/OpenMAD-Setup-1.2.0.exe",
      "size": 2048576,
      "digest": "sha256:abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234"
    }
  ]
}"#;
        let res = parse_release_json(json, "1.1.4").expect("parse release");
        assert!(res.has_update);
        assert_eq!(res.latest_version, "v1.2.0");
        let asset = res.asset.expect("installer asset");
        assert_eq!(asset.name, "OpenMAD-Setup-1.2.0.exe");
        assert_eq!(asset.size, 2048576);
    }

    #[test]
    fn parse_release_json_no_update() {
        let json = r#"{
  "tag_name": "v1.1.4",
  "assets": [
    {
      "name": "OpenMAD-Setup-1.1.4.exe",
      "browser_download_url": "https://github.com/Acciaw/OpenMAD/releases/download/v1.1.4/OpenMAD-Setup-1.1.4.exe",
      "size": 2048576
    }
  ]
}"#;
        let res = parse_release_json(json, "1.1.4").expect("parse release");
        assert!(!res.has_update);
    }

    #[test]
    fn sha256_self_test() {
        let mut h = Sha256Hasher::new();
        h.update(b"hello world");
        assert_eq!(
            h.finalize_hex(),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
