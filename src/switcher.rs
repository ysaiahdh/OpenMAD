//! Switcher automatique de profils en fonction de l'application au premier plan.
//!
//! Ce module surveille les fenêtres actives sous Windows et applique automatiquement
//! le profil dont les règles correspondent au processus ou au titre de la fenêtre.
//!
//! Contraintes de conception :
//! 1. Préservation de la mémoire flash : on évite les ré-écritures si le profil est déjà actif,
//!    et on ne modifie que les champs nécessaires.
//! 2. Filtrage des rebonds Alt-Tab : une application candidate doit rester stable pendant
//!    `debounce_ms` avant d'être appliquée, et un intervalle minimum `min_dwell_ms`
//!    limite la fréquence des bascules.
//! 3. Reconnexion USB à chaud : le périphérique USB est ouvert à la demande lors d'une
//!    bascule et libéré immédiatement après.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::device::{HidTransport, Mad68};
use crate::profile::Profile;

pub const CONFIG_VERSION: u32 = 1;
pub const BACKOFF_SECONDS: &[u64] = &[2, 5, 15, 60];

/// Description d'un processus propriétaire d'une fenêtre.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundApp {
    /// Nom de base de l'exécutable (ex: "cs2.exe")
    pub exe: String,
    /// Chemin complet vers l'exécutable sur disque
    pub path: String,
    /// Titre de la fenêtre
    pub title: String,
}

impl std::fmt::Display for ForegroundApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.title.is_empty() {
            write!(f, "{}", self.exe)
        } else {
            let short_title = if self.title.len() > 40 {
                &self.title[..40]
            } else {
                &self.title
            };
            write!(f, "{} ({})", self.exe, short_title)
        }
    }
}

/// Règle associant une application ou un titre de fenêtre à un profil.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rule {
    /// Nom du profil à charger
    pub profile: String,
    /// Masque de nom d'exécutable (ex: "cs2.exe", "*.exe")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    /// Sous-chaîne requise dans le titre de la fenêtre (insensible à la casse)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_contains: Option<String>,
}

impl Rule {
    /// Vérifie si cette règle correspond à l'application active.
    pub fn matches(&self, app: &ForegroundApp) -> bool {
        if let Some(ref target_exe) = self.exe {
            if !glob_match(target_exe, &app.exe) {
                return false;
            }
        }
        if let Some(ref sub) = self.title_contains {
            if !app.title.to_lowercase().contains(&sub.to_lowercase()) {
                return false;
            }
        }
        self.exe.is_some() || self.title_contains.is_some()
    }
}

/// Comparaison simple de type glob insensible à la casse avec support de `*` et `?`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p = pattern.to_lowercase();
    let t = text.to_lowercase();
    let p_chars: Vec<char> = p.chars().collect();
    let t_chars: Vec<char> = t.chars().collect();

    let mut i = 0;
    let mut j = 0;
    let mut star_idx = None;
    let mut match_idx = 0;

    while j < t_chars.len() {
        if i < p_chars.len() && (p_chars[i] == '?' || p_chars[i] == t_chars[j]) {
            i += 1;
            j += 1;
        } else if i < p_chars.len() && p_chars[i] == '*' {
            star_idx = Some(i);
            match_idx = j;
            i += 1;
        } else if let Some(si) = star_idx {
            i = si + 1;
            match_idx += 1;
            j = match_idx;
        } else {
            return false;
        }
    }

    while i < p_chars.len() && p_chars[i] == '*' {
        i += 1;
    }

    i == p_chars.len()
}

fn default_config_version() -> u32 {
    CONFIG_VERSION
}
fn default_true() -> bool {
    true
}
fn default_poll_interval() -> u64 {
    400
}
fn default_debounce() -> u64 {
    800
}
fn default_min_dwell() -> u64 {
    3000
}

/// Configuration globale du switcher automatique (`switcher.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwitcherConfig {
    #[serde(default = "default_config_version")]
    pub config_version: u32,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub default_profile: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
    #[serde(default = "default_debounce")]
    pub debounce_ms: u64,
    #[serde(default = "default_min_dwell")]
    pub min_dwell_ms: u64,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

impl Default for SwitcherConfig {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            enabled: true,
            default_profile: String::new(),
            poll_interval_ms: 400,
            debounce_ms: 800,
            min_dwell_ms: 3000,
            rules: Vec::new(),
        }
    }
}

impl SwitcherConfig {
    /// Détermine le profil cible pour une application donnée.
    ///
    /// La première règle qui correspond l'emporte.
    /// Si aucune règle ne correspond, le profil par défaut (`default_profile`) est renvoyé.
    pub fn resolve(&self, app: Option<&ForegroundApp>) -> Option<&str> {
        if let Some(app) = app {
            for rule in &self.rules {
                if rule.matches(app) {
                    return Some(&rule.profile);
                }
            }
        }
        if self.default_profile.is_empty() {
            None
        } else {
            Some(&self.default_profile)
        }
    }

    /// Sauvegarde atomique de la configuration vers le fichier JSON spécifié.
    pub fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(path, json)
    }

    /// Charge la configuration depuis un fichier JSON.
    pub fn load(path: &Path) -> Result<Self, std::io::Error> {
        let content = fs::read_to_string(path)?;
        let cfg: Self = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(cfg)
    }
}

/// Événement émis lors d'une décision ou action du switcher.
#[derive(Debug, Clone)]
pub struct SwitchEvent {
    pub when: Instant,
    pub app: Option<ForegroundApp>,
    pub profile: String,
    pub writes: usize,
    pub note: String,
}

/// Trait abstrayant la détection de l'application active pour permettre les tests unitaires.
pub trait ForegroundDetector: Send + Sync {
    fn foreground_app(&self) -> Option<ForegroundApp>;
    fn list_windowed_processes(&self) -> Vec<ForegroundApp>;
}

/// Détecteur natif Windows utilisant les API Win32 User32 et Kernel32.
#[cfg(windows)]
pub struct WindowsForegroundDetector;

#[cfg(windows)]
impl ForegroundDetector for WindowsForegroundDetector {
    fn foreground_app(&self) -> Option<ForegroundApp> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        };

        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0.is_null() {
                return None;
            }

            // Récupération du titre de la fenêtre
            let len = GetWindowTextLengthW(hwnd);
            let mut title_buf = vec![0u16; (len + 1) as usize];
            if len > 0 {
                let _ = GetWindowTextW(hwnd, &mut title_buf);
            }
            let title = String::from_utf16_lossy(&title_buf)
                .trim_matches(char::from(0))
                .to_string();

            // Récupération du PID
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                return Some(ForegroundApp {
                    exe: String::new(),
                    path: String::new(),
                    title,
                });
            }

            // Récupération du chemin de l'exécutable
            let mut path = String::new();
            if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut buf = [0u16; 1024];
                let mut size = buf.len() as u32;
                if QueryFullProcessImageNameW(
                    handle,
                    PROCESS_NAME_FORMAT(0),
                    windows::core::PWSTR(buf.as_mut_ptr()),
                    &mut size,
                )
                .is_ok()
                {
                    path = String::from_utf16_lossy(&buf[..size as usize]);
                }
                let _ = CloseHandle(handle);
            }

            let exe = Path::new(&path)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("")
                .to_string();

            Some(ForegroundApp { exe, path, title })
        }
    }

    fn list_windowed_processes(&self) -> Vec<ForegroundApp> {
        use std::sync::Mutex;
        use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM};
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
            IsWindowVisible,
        };

        static FOUND: Mutex<Option<Vec<ForegroundApp>>> = Mutex::new(None);

        unsafe extern "system" fn enum_proc(hwnd: HWND, _lparam: LPARAM) -> BOOL {
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return BOOL(1);
            }
            let mut title_buf = vec![0u16; (len + 1) as usize];
            let _ = GetWindowTextW(hwnd, &mut title_buf);
            let title = String::from_utf16_lossy(&title_buf)
                .trim_matches(char::from(0))
                .trim()
                .to_string();
            if title.is_empty() {
                return BOOL(1);
            }

            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                return BOOL(1);
            }

            let mut path = String::new();
            if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut buf = [0u16; 1024];
                let mut size = buf.len() as u32;
                if QueryFullProcessImageNameW(
                    handle,
                    PROCESS_NAME_FORMAT(0),
                    windows::core::PWSTR(buf.as_mut_ptr()),
                    &mut size,
                )
                .is_ok()
                {
                    path = String::from_utf16_lossy(&buf[..size as usize]);
                }
                let _ = CloseHandle(handle);
            }

            let exe = Path::new(&path)
                .file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("")
                .to_string();

            if !exe.is_empty() {
                if let Ok(mut guard) = FOUND.lock() {
                    if let Some(ref mut list) = *guard {
                        list.push(ForegroundApp { exe, path, title });
                    }
                }
            }

            BOOL(1)
        }

        if let Ok(mut guard) = FOUND.lock() {
            *guard = Some(Vec::new());
        }

        unsafe {
            let _ = EnumWindows(Some(enum_proc), LPARAM(0));
        }

        let mut raw = Vec::new();
        if let Ok(mut guard) = FOUND.lock() {
            if let Some(list) = guard.take() {
                raw = list;
            }
        }

        // Déduplication par exe
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for item in raw {
            let key = item.exe.to_lowercase();
            if !seen.contains(&key) {
                seen.insert(key);
                out.push(item);
            }
        }
        out.sort_by(|a, b| a.exe.to_lowercase().cmp(&b.exe.to_lowercase()));
        out
    }
}

/// Détecteur fictif pour tests ou plateformes non-Windows.
#[derive(Default)]
pub struct MockForegroundDetector {
    pub current: std::sync::RwLock<Option<ForegroundApp>>,
    pub windows: std::sync::RwLock<Vec<ForegroundApp>>,
}

impl MockForegroundDetector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_current(&self, app: Option<ForegroundApp>) {
        if let Ok(mut g) = self.current.write() {
            *g = app;
        }
    }
}

impl ForegroundDetector for MockForegroundDetector {
    fn foreground_app(&self) -> Option<ForegroundApp> {
        self.current.read().ok().and_then(|g| g.clone())
    }

    fn list_windowed_processes(&self) -> Vec<ForegroundApp> {
        self.windows.read().ok().map(|g| g.clone()).unwrap_or_default()
    }
}

/// Moteur de changement automatique de profil.
pub struct Switcher {
    pub config: SwitcherConfig,
    pub profile_dir: PathBuf,
    pub detector: Box<dyn ForegroundDetector>,
    pub on_event: Option<Box<dyn Fn(&SwitchEvent) + Send + Sync>>,
    pub transport_factory: Option<Box<dyn Fn() -> Box<dyn HidTransport> + Send + Sync>>,
    pub dry_run: bool,

    pub active_profile: Option<String>,
    pub current_app: Option<ForegroundApp>,
    pub last_switch_at: Option<Instant>,
    pub candidate: Option<String>,
    pub candidate_since: Option<Instant>,
    pub stop: Arc<AtomicBool>,

    retry_after: Option<Instant>,
    consecutive_failures: usize,
}

impl Switcher {
    pub fn new(
        config: SwitcherConfig,
        profile_dir: PathBuf,
        detector: Box<dyn ForegroundDetector>,
        dry_run: bool,
    ) -> Self {
        Self {
            config,
            profile_dir,
            detector,
            on_event: None,
            transport_factory: None,
            dry_run,
            active_profile: None,
            current_app: None,
            last_switch_at: None,
            candidate: None,
            candidate_since: None,
            stop: Arc::new(AtomicBool::new(false)),
            retry_after: None,
            consecutive_failures: 0,
        }
    }

    /// Liste les profils disponibles dans le dossier des profils.
    pub fn available_profiles(&self) -> Vec<String> {
        if !self.profile_dir.exists() {
            return Vec::new();
        }
        let mut list = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.profile_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        list.push(stem.to_string());
                    }
                }
            }
        }
        list.sort();
        list
    }

    fn note_failure(&mut self, now: Instant) {
        let idx = self.consecutive_failures.min(BACKOFF_SECONDS.len() - 1);
        self.retry_after = Some(now + Duration::from_secs(BACKOFF_SECONDS[idx]));
        self.consecutive_failures += 1;
    }

    fn note_success(&mut self, now: Instant, name: &str) {
        self.active_profile = Some(name.to_string());
        self.last_switch_at = Some(now);
        self.retry_after = None;
        self.consecutive_failures = 0;
    }

    fn emit(&self, event: SwitchEvent) -> SwitchEvent {
        if let Some(ref cb) = self.on_event {
            cb(&event);
        }
        event
    }

    /// Applique un profil sur le clavier physique (ou en dry-run).
    pub fn apply_profile(&mut self, name: &str, force: bool) -> SwitchEvent {
        let now = Instant::now();

        if !force && self.active_profile.as_deref() == Some(name) {
            return self.emit(SwitchEvent {
                when: now,
                app: self.current_app.clone(),
                profile: name.to_string(),
                writes: 0,
                note: "already active".to_string(),
            });
        }

        let prof_path = self.profile_dir.join(format!("{name}.json"));
        let profile = match Profile::load(&prof_path) {
            Ok(p) => p,
            Err(e) => {
                self.note_failure(now);
                return self.emit(SwitchEvent {
                    when: now,
                    app: self.current_app.clone(),
                    profile: name.to_string(),
                    writes: 0,
                    note: format!("profile '{name}' unreadable: {e}"),
                });
            }
        };

        // Ouverture USB à la demande
        let mut kb = Mad68::new(false, false, 500, None, !self.dry_run);
        let open_res = if let Some(ref factory) = self.transport_factory {
            kb.open_with_transport(factory(), crate::devices::default_board().clone())
        } else {
            kb.open()
        };
        if let Err(e) = open_res {
            self.note_failure(now);
            return self.emit(SwitchEvent {
                when: now,
                app: self.current_app.clone(),
                profile: name.to_string(),
                writes: 0,
                note: format!("apply failed: {e}"),
            });
        }

        let plan = match profile.plan(&mut kb, false) {
            Ok(p) => p,
            Err(e) => {
                self.note_failure(now);
                return self.emit(SwitchEvent {
                    when: now,
                    app: self.current_app.clone(),
                    profile: name.to_string(),
                    writes: 0,
                    note: format!("plan failed: {e}"),
                });
            }
        };

        if plan.is_empty() {
            self.note_success(now, name);
            return self.emit(SwitchEvent {
                when: now,
                app: self.current_app.clone(),
                profile: name.to_string(),
                writes: 0,
                note: "device already matched".to_string(),
            });
        }

        if self.dry_run {
            self.note_success(now, name);
            let est = plan.packet_estimate();
            return self.emit(SwitchEvent {
                when: now,
                app: self.current_app.clone(),
                profile: name.to_string(),
                writes: 0,
                note: format!("dry run: would send {est} packet(s)"),
            });
        }

        match plan.execute(&mut kb, false) {
            Ok(writes) => {
                self.note_success(now, name);
                self.emit(SwitchEvent {
                    when: now,
                    app: self.current_app.clone(),
                    profile: name.to_string(),
                    writes,
                    note: String::new(),
                })
            }
            Err(e) => {
                self.note_failure(now);
                self.emit(SwitchEvent {
                    when: now,
                    app: self.current_app.clone(),
                    profile: name.to_string(),
                    writes: 0,
                    note: format!("apply failed: {e}"),
                })
            }
        }
    }

    /// Exécute une itération de surveillance du processus actif.
    pub fn poll_once(&mut self) -> Option<SwitchEvent> {
        if !self.config.enabled {
            return None;
        }

        let app = self.detector.foreground_app();
        self.current_app = app.clone();

        let want = match self.config.resolve(app.as_ref()) {
            Some(w) => w.to_string(),
            None => {
                self.candidate = None;
                return None;
            }
        };

        let now = Instant::now();

        if self.candidate.as_deref() != Some(&want) {
            self.candidate = Some(want);
            self.candidate_since = Some(now);
            self.retry_after = None;
            self.consecutive_failures = 0;
            return None;
        }

        if self.active_profile.as_deref() == Some(&want) {
            return None;
        }

        if let Some(retry) = self.retry_after {
            if now < retry {
                return None;
            }
        }

        if let Some(since) = self.candidate_since {
            if now.duration_since(since).as_millis() < self.config.debounce_ms as u128 {
                return None;
            }
        }

        if let Some(last) = self.last_switch_at {
            if now.duration_since(last).as_millis() < self.config.min_dwell_ms as u128 {
                return None;
            }
        }

        Some(self.apply_profile(&want, false))
    }

    /// Boucle bloquante de surveillance. Arrêter avec `stop()`.
    pub fn run(&mut self) {
        self.stop.store(false, Ordering::SeqCst);
        while !self.stop.load(Ordering::SeqCst) {
            self.poll_once();
            std::thread::sleep(Duration::from_millis(self.config.poll_interval_ms));
        }
    }

    /// Demande l'arrêt de la boucle de surveillance.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_match() {
        assert!(glob_match("cs2.exe", "cs2.exe"));
        assert!(glob_match("*.exe", "valorant.exe"));
        assert!(glob_match("game_?_test.exe", "game_1_test.exe"));
        assert!(!glob_match("*.exe", "game.bin"));
        assert!(glob_match("VALORANT.EXE", "valorant.exe"));
    }

    #[test]
    fn test_rule_matching() {
        let rule = Rule {
            profile: "fps".to_string(),
            exe: Some("cs2.exe".to_string()),
            title_contains: Some("counter-strike".to_string()),
        };

        let app_ok = ForegroundApp {
            exe: "cs2.exe".to_string(),
            path: "C:\\cs2.exe".to_string(),
            title: "Counter-Strike 2".to_string(),
        };
        assert!(rule.matches(&app_ok));

        let app_diff_title = ForegroundApp {
            exe: "cs2.exe".to_string(),
            path: "C:\\cs2.exe".to_string(),
            title: "Settings".to_string(),
        };
        assert!(!rule.matches(&app_diff_title));
    }

    #[test]
    fn test_config_resolve_priority_and_fallback() {
        let cfg = SwitcherConfig {
            config_version: 1,
            enabled: true,
            default_profile: "desktop".to_string(),
            poll_interval_ms: 400,
            debounce_ms: 800,
            min_dwell_ms: 3000,
            rules: vec![
                Rule {
                    profile: "apex".to_string(),
                    exe: Some("r5apex.exe".to_string()),
                    title_contains: None,
                },
                Rule {
                    profile: "generic_game".to_string(),
                    exe: Some("*.exe".to_string()),
                    title_contains: None,
                },
            ],
        };

        let app_apex = ForegroundApp {
            exe: "r5apex.exe".to_string(),
            path: "".to_string(),
            title: "".to_string(),
        };
        assert_eq!(cfg.resolve(Some(&app_apex)), Some("apex"));

        let app_other = ForegroundApp {
            exe: "notepad.exe".to_string(),
            path: "".to_string(),
            title: "".to_string(),
        };
        assert_eq!(cfg.resolve(Some(&app_other)), Some("generic_game"));

        assert_eq!(cfg.resolve(None), Some("desktop"));
    }

    #[test]
    fn test_switcher_state_machine_with_mock_detector() {
        let tmp = std::env::temp_dir().join(format!("openmad_switcher_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        let mut prof = crate::profile::default_profile("default");
        let _ = prof.save(&tmp.join("game.json"));
        let _ = prof.save(&tmp.join("desktop.json"));

        let detector = Box::new(MockForegroundDetector::new());
        let mock_det = detector.as_ref() as *const MockForegroundDetector;

        let cfg = SwitcherConfig {
            config_version: 1,
            enabled: true,
            default_profile: "desktop".to_string(),
            poll_interval_ms: 50,
            debounce_ms: 100,
            min_dwell_ms: 100,
            rules: vec![Rule {
                profile: "game".to_string(),
                exe: Some("game.exe".to_string()),
                title_contains: None,
            }],
        };

        let mut switcher = Switcher::new(cfg, tmp.clone(), detector, true);
        switcher.transport_factory = Some(Box::new(|| Box::new(crate::device::mock::MockHidTransport::with_auto_reply())));

        unsafe {
            (*mock_det).set_current(Some(ForegroundApp {
                exe: "game.exe".to_string(),
                path: "".to_string(),
                title: "Game".to_string(),
            }));
        }

        // 1er poll: détection candidate
        let ev1 = switcher.poll_once();
        assert!(ev1.is_none());
        assert_eq!(switcher.candidate.as_deref(), Some("game"));

        // Attente debounce
        std::thread::sleep(Duration::from_millis(150));

        // 2e poll: applique (en dry-run)
        let ev2 = switcher.poll_once();
        assert!(ev2.is_some());
        let ev2 = ev2.unwrap();
        assert_eq!(ev2.profile, "game");
        assert_eq!(switcher.active_profile.as_deref(), Some("game"));

        // Fermeture du jeu -> retour au default
        unsafe {
            (*mock_det).set_current(None);
        }

        // Attente min_dwell
        std::thread::sleep(Duration::from_millis(150));

        let ev3 = switcher.poll_once();
        assert!(ev3.is_none());
        assert_eq!(switcher.candidate.as_deref(), Some("desktop"));

        std::thread::sleep(Duration::from_millis(150));
        let ev4 = switcher.poll_once().unwrap();
        assert_eq!(ev4.profile, "desktop");
        assert_eq!(switcher.active_profile.as_deref(), Some("desktop"));

        let _ = fs::remove_dir_all(&tmp);
    }
}
