//! Application System Tray (barre des tâches) pour claviers MAD68.
//!
//! Ce module implémente l'icône dans la zone de notification Windows :
//! - Surveillance et bascule automatique de profils via `Switcher`.
//! - Menu contextuel interactif (sélection manuelle, bascule auto, key bindings).
//! - Lancement du configurateur web / HUD local.
//! - Options système : "Démarrer avec Windows", "Vérifier les mises à jour".
//! - Icône dynamique avec pastille d'état (vert = actif, orange = pause, rouge = erreur).

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use image::{ImageBuffer, Rgba};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::paths::{assets_dir, data_dir, seed_user_data};
use crate::settings::{get_start_at_login, set_start_at_login};
use crate::switcher::{
    ForegroundApp, SwitchEvent, Switcher, SwitcherConfig, WindowsForegroundDetector,
};
use crate::updater::check_for_update;

/// Arguments CLI pour la commande `tray`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct TrayArgs {
    /// Mode simulation (dry-run) : observe et journalise sans écrire sur le matériel.
    #[arg(long)]
    pub dry_run: bool,
}

/// Point d'entrée principal pour la commande `tray`.
pub fn run(args: &TrayArgs) -> anyhow::Result<i32> {
    let mut app = TrayApp::new(args.dry_run)?;
    app.run()
}

pub struct TrayApp {
    pub dry_run: bool,
    config_path: PathBuf,
    profile_dir: PathBuf,
    log_path: PathBuf,
    config: SwitcherConfig,
    state: Arc<Mutex<String>>, // "active", "paused", "error"
    last_note: Arc<Mutex<String>>,
    active_profile: Arc<Mutex<Option<String>>>,
    current_app: Arc<Mutex<Option<ForegroundApp>>>,
    stop: Arc<AtomicBool>,
}

impl TrayApp {
    pub fn new(dry_run: bool) -> anyhow::Result<Self> {
        let p_dir = seed_user_data()?;
        let d_dir = data_dir();
        let cfg_path = d_dir.join("switcher.json");
        let log_p = d_dir.join("switcher.log");

        // Chargement ou création de switcher.json
        let config = if cfg_path.exists() {
            SwitcherConfig::load(&cfg_path).unwrap_or_default()
        } else {
            let default_cfg = SwitcherConfig::default();
            let _ = default_cfg.save(&cfg_path);
            default_cfg
        };

        // Chargement des cartes déverrouillées
        let unlocked_path = d_dir.join("unlocked_boards.json");
        let _ = crate::devices::load_unlocked(&unlocked_path);

        let initial_state = if config.enabled { "active" } else { "paused" };

        Ok(Self {
            dry_run,
            config_path: cfg_path,
            profile_dir: p_dir,
            log_path: log_p,
            config,
            state: Arc::new(Mutex::new(initial_state.to_string())),
            last_note: Arc::new(Mutex::new(String::new())),
            active_profile: Arc::new(Mutex::new(None)),
            current_app: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Charge ou génère l'icône de la barre des tâches avec la pastille d'état.
    pub fn create_icon(&self, state: &str) -> Icon {
        let ico_path = assets_dir().join("icon.ico");
        let png_path = assets_dir().join("icon.png");

        let mut img: ImageBuffer<Rgba<u8>, Vec<u8>> = if ico_path.exists() {
            image::open(&ico_path)
                .map(|i| i.to_rgba8())
                .unwrap_or_else(|_| Self::fallback_icon())
        } else if png_path.exists() {
            image::open(&png_path)
                .map(|i| i.to_rgba8())
                .unwrap_or_else(|_| Self::fallback_icon())
        } else {
            // Fallback embarqué compilé en dur dans le binaire
            let embedded = include_bytes!("../assets/icon.ico");
            image::load_from_memory(embedded)
                .map(|i| i.to_rgba8())
                .unwrap_or_else(|_| Self::fallback_icon())
        };

        // Redimensionner à 64x64 si nécessaire
        if img.width() != 64 || img.height() != 64 {
            img = image::imageops::resize(&img, 64, 64, image::imageops::FilterType::Lanczos3);
        }

        // Dessiner la pastille de notification dans le coin inférieur droit si état non "active"
        if state != "active" {
            let dot_color = match state {
                "paused" => Rgba([245, 152, 32, 255]), // Orange ambré
                "error" => Rgba([220, 53, 69, 255]),   // Rouge erreur
                _ => Rgba([120, 120, 120, 255]),
            };

            // Dessin d'un cercle plein de rayon 6 pixels à (54, 54)
            let cx = 54;
            let cy = 54;
            let radius = 6;
            for x in (cx - radius - 1)..=(cx + radius + 1) {
                for y in (cy - radius - 1)..=(cy + radius + 1) {
                    let dist_sq = (x - cx) * (x - cx) + (y - cy) * (y - cy);
                    if dist_sq <= radius * radius {
                        if x < 64 && y < 64 {
                            img.put_pixel(x as u32, y as u32, dot_color);
                        }
                    } else if dist_sq <= (radius + 1) * (radius + 1) {
                        if x < 64 && y < 64 {
                            img.put_pixel(x as u32, y as u32, Rgba([0, 0, 0, 200]));
                        }
                    }
                }
            }
        }

        let width = img.width();
        let height = img.height();
        Icon::from_rgba(img.into_raw(), width, height).unwrap_or_else(|_| {
            let blank = vec![0u8; 64 * 64 * 4];
            Icon::from_rgba(blank, 64, 64).expect("blank icon")
        })
    }

    fn fallback_icon() -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        let mut buf = ImageBuffer::new(64, 64);
        for pixel in buf.pixels_mut() {
            *pixel = Rgba([56, 142, 60, 255]); // Vert par défaut
        }
        buf
    }

    /// Exécute l'application de barre des tâches avec sa boucle d'événements.
    pub fn run(&mut self) -> anyhow::Result<i32> {
        let initial_state = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let icon = self.create_icon(&initial_state);

        let tray_icon = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip("OpenMAD — Initialisation...")
            .build()?;

        // Démarrage du thread de surveillance Switcher
        #[cfg(windows)]
        let detector = Box::new(WindowsForegroundDetector);
        #[cfg(not(windows))]
        let detector = Box::new(MockForegroundDetector::new());

        let mut switcher = Switcher::new(
            self.config.clone(),
            self.profile_dir.clone(),
            detector,
            self.dry_run,
        );

        let state_arc = self.state.clone();
        let last_note_arc = self.last_note.clone();
        let active_prof_arc = self.active_profile.clone();
        let current_app_arc = self.current_app.clone();
        let log_path_clone = self.log_path.clone();

        switcher.on_event = Some(Box::new(move |event: &SwitchEvent| {
            if event.note.contains("failed")
                || event.note.contains("not found")
                || event.note.contains("unreadable")
            {
                if let Ok(mut s) = state_arc.lock() {
                    *s = "error".to_string();
                }
            }
            if let Ok(mut ln) = last_note_arc.lock() {
                *ln = event.note.clone();
            }
            if let Ok(mut ap) = active_prof_arc.lock() {
                *ap = Some(event.profile.clone());
            }
            if let Ok(mut ca) = current_app_arc.lock() {
                *ca = event.app.clone();
            }

            // Écriture du journal
            if event.writes > 0 || !event.note.is_empty() {
                let app_str = event.app.as_ref().map_or("-", |a| a.exe.as_str());
                let line = format!(
                    "{}  app={:<24} profile={:<12} writes={:<4} {}\n",
                    chrono_timestamp(),
                    app_str,
                    event.profile,
                    event.writes,
                    event.note
                );
                let _ = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path_clone)
                    .and_then(|mut f| f.write_all(line.as_bytes()));
            }
        }));

        let stop_switcher = switcher.stop.clone();
        let switcher_handle = std::thread::spawn(move || {
            switcher.run();
        });

        // Menu ids mapping
        let id_auto_switch = "toggle_auto_switch";
        let id_open_hud = "open_hud";
        let id_open_profiles = "open_profiles";
        let id_start_login = "start_at_login";
        let id_check_updates = "check_updates";
        let id_reread_cfg = "reread_config";
        let id_quit = "quit";

        let mut last_menu_build = Instant::now() - Duration::from_secs(10);
        let mut last_icon_state = String::new();

        println!("OpenMAD Tray démarré dans la zone de notification.");

        // Boucle principale d'événements
        while !self.stop.load(Ordering::SeqCst) {
            // Re-génération périodique du menu et du tooltip
            let current_state = self
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let active_p = self
                .active_profile
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_else(|| "aucun".to_string());
            let current_a = self
                .current_app
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let note = self
                .last_note
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();

            if last_menu_build.elapsed() > Duration::from_millis(1500) {
                last_menu_build = Instant::now();

                // Mise à jour de l'icône si l'état a changé
                if current_state != last_icon_state {
                    last_icon_state = current_state.clone();
                    let _ = tray_icon.set_icon(Some(self.create_icon(&current_state)));
                }

                // Tooltip
                let mut tooltip = format!("OpenMAD — {active_p}");
                if self.dry_run {
                    tooltip.push_str(" (simulation)");
                }
                if let Some(ref a) = current_a {
                    if !a.exe.is_empty() {
                        tooltip.push_str(&format!("\nApp: {}", a.exe));
                    }
                }
                if !self.config.enabled {
                    tooltip.push_str("\nAuto-switch en pause");
                }
                if !note.is_empty() {
                    tooltip.push_str(&format!("\n{note}"));
                }
                let _ = tray_icon.set_tooltip(Some(tooltip));

                // Construction dynamique du menu
                let menu = Menu::new();
                let _ = menu.append(&MenuItem::new(format!("Actif : {active_p}"), false, None));
                if let Some(ref a) = current_a {
                    if !a.exe.is_empty() {
                        let _ = menu.append(&MenuItem::new(format!("Premier plan : {}", a.exe), false, None));
                    }
                }
                let _ = menu.append(&PredefinedMenuItem::separator());

                let auto_item = CheckMenuItem::with_id(
                    id_auto_switch,
                    "Bascule automatique (Auto-switch)",
                    true,
                    self.config.enabled,
                    None,
                );
                let _ = menu.append(&auto_item);
                let _ = menu.append(&PredefinedMenuItem::separator());

                // Liste des profils
                let profiles = self.list_profiles();
                for p in &profiles {
                    let checked = p == &active_p;
                    let prof_item = CheckMenuItem::with_id(
                        format!("apply_profile_{p}"),
                        format!("Appliquer '{p}'"),
                        true,
                        checked,
                        None,
                    );
                    let _ = menu.append(&prof_item);
                }

                let _ = menu.append(&PredefinedMenuItem::separator());
                let _ = menu.append(&MenuItem::with_id(id_open_hud, "Ouvrir le HUD / Télémétrie", true, None));
                let _ = menu.append(&MenuItem::with_id(id_open_profiles, "Dossier des profils", true, None));

                let login_enabled = get_start_at_login();
                let _ = menu.append(&CheckMenuItem::with_id(
                    id_start_login,
                    "Démarrer avec Windows",
                    true,
                    login_enabled,
                    None,
                ));
                let _ = menu.append(&MenuItem::with_id(id_check_updates, "Vérifier les mises à jour", true, None));
                let _ = menu.append(&MenuItem::with_id(id_reread_cfg, "Recharger la configuration", true, None));
                let _ = menu.append(&PredefinedMenuItem::separator());
                let _ = menu.append(&MenuItem::with_id(id_quit, "Quitter OpenMAD", true, None));

                tray_icon.set_menu(Some(Box::new(menu)));
            }

            // Écoute des clics de menu
            if let Ok(event) = MenuEvent::receiver().try_recv() {
                let id_str = event.id.0.as_str();

                if id_str == id_quit {
                    self.stop.store(true, Ordering::SeqCst);
                    break;
                } else if id_str == id_auto_switch {
                    self.config.enabled = !self.config.enabled;
                    let _ = self.config.save(&self.config_path);
                    if let Ok(mut s) = self.state.lock() {
                        *s = if self.config.enabled { "active" } else { "paused" }.to_string();
                    }
                    last_menu_build = Instant::now() - Duration::from_secs(10);
                } else if id_str == id_open_hud {
                    #[cfg(windows)]
                    {
                        use std::process::Command;
                        let _ = Command::new("cmd").args(["/C", "start", "http://127.0.0.1:8787/"]).spawn();
                    }
                } else if id_str == id_open_profiles {
                    #[cfg(windows)]
                    {
                        use std::process::Command;
                        let _ = Command::new("explorer").arg(&self.profile_dir).spawn();
                    }
                } else if id_str == id_start_login {
                    let cur = get_start_at_login();
                    let _ = set_start_at_login(!cur);
                    last_menu_build = Instant::now() - Duration::from_secs(10);
                } else if id_str == id_check_updates {
                    let res = check_for_update(crate::version::github_repo(), &crate::version::app_version());
                    if let Ok(check) = res {
                        if check.has_update {
                            println!("Mise à jour disponible: v{}", check.latest_version);
                        } else {
                            println!("OpenMAD est à jour (v{}).", crate::version::app_version());
                        }
                    }
                } else if id_str == id_reread_cfg {
                    if let Ok(reloaded) = SwitcherConfig::load(&self.config_path) {
                        self.config = reloaded;
                    }
                    last_menu_build = Instant::now() - Duration::from_secs(10);
                } else if let Some(p_name) = id_str.strip_prefix("apply_profile_") {
                    // Sélection manuelle : met en pause l'auto-switch comme dans switcher.py
                    self.config.enabled = false;
                    let _ = self.config.save(&self.config_path);
                    if let Ok(mut s) = self.state.lock() {
                        *s = "paused".to_string();
                    }
                    if let Ok(mut ap) = self.active_profile.lock() {
                        *ap = Some(p_name.to_string());
                    }
                    last_menu_build = Instant::now() - Duration::from_secs(10);
                }
            }

            // Pompage minimal des messages Windows
            #[cfg(windows)]
            unsafe {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{
                    DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_QUIT,
                };
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        self.stop.store(true, Ordering::SeqCst);
                        break;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }

            std::thread::sleep(Duration::from_millis(50));
        }

        stop_switcher.store(true, Ordering::SeqCst);
        let _ = switcher_handle.join();
        Ok(0)
    }

    fn list_profiles(&self) -> Vec<String> {
        let mut list = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.profile_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                        list.push(stem.to_string());
                    }
                }
            }
        }
        list.sort();
        list
    }
}

fn chrono_timestamp() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now();
    let dur = now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let secs = dur.as_secs();
    format!("{secs}")
}
