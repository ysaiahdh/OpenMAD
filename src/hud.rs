//! Overlay HUD et serveur de télémétrie en temps réel pour claviers magnétiques.
//!
//! Ce module implémente :
//! 1. Le `Sampler` qui possède le handle USB et échantillonne la télémétrie en temps réel (~33 Hz).
//! 2. Un overlay natif Windows ultra-léger (`WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`)
//!    qui ne vole jamais le focus d'un jeu plein écran.
//! 3. Un serveur HTTP local embarqué (sur 127.0.0.1:8787) servant le dashboard interactif et l'API JSON.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::device::Mad68;
use crate::keycodes::label;
use crate::version::app_version;

pub const DEFAULT_PORT: u16 = 8787;
pub const DEFAULT_INTERVAL: f64 = 0.03;

/// État d'une touche pour la télémétrie et l'affichage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyTelemetry {
    pub row: usize,
    pub col: usize,
    pub label: String,
    pub travel_mm: f64,
    pub pressed: bool,
}

/// Instantané d'état de la télémétrie (snapshot).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub keys: Vec<KeyTelemetry>,
    pub calibrated: usize,
    pub total: usize,
    pub sample_hz: f64,
    pub samples: usize,
    pub active_profile: Option<String>,
    pub profiles: Vec<String>,
    pub error: Option<String>,
    pub sweep_ms: f64,
    pub connected: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            calibrated: 0,
            total: 68,
            sample_hz: 0.0,
            samples: 0,
            active_profile: None,
            profiles: Vec::new(),
            error: None,
            sweep_ms: 0.0,
            connected: false,
        }
    }
}

/// Échantillonneur de télémétrie en arrière-plan.
pub struct Sampler {
    pub profile_dir: PathBuf,
    pub interval: Duration,
    snapshot: Arc<RwLock<Snapshot>>,
    stop: Arc<AtomicBool>,
}

impl Sampler {
    pub fn new(profile_dir: PathBuf, interval_secs: f64) -> Self {
        Self {
            profile_dir,
            interval: Duration::from_secs_f64(interval_secs.max(0.01)),
            snapshot: Arc::new(RwLock::new(Snapshot::default())),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.read().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Démarre la boucle d'échantillonnage dans un thread détaché.
    pub fn start(&self) {
        let profile_dir = self.profile_dir.clone();
        let interval = self.interval;
        let snap_arc = self.snapshot.clone();
        let stop_arc = self.stop.clone();

        std::thread::spawn(move || {
            let mut kb_opt: Option<Mad68> = None;
            let mut sample_count = 0usize;
            let mut last_hz_calc = Instant::now();
            let mut hz_samples = 0usize;
            let mut current_hz = 0.0f64;

            while !stop_arc.load(Ordering::SeqCst) {
                let start_sweep = Instant::now();

                // Gestion de la connexion du clavier
                if kb_opt.is_none() {
                    let mut kb = Mad68::new(false, false, 500, None, false);
                    if kb.open().is_ok() {
                        kb_opt = Some(kb);
                    }
                }

                // Récupération des profils disponibles
                let mut profiles = Vec::new();
                if let Ok(entries) = fs::read_dir(&profile_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("json") {
                            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                                profiles.push(stem.to_string());
                            }
                        }
                    }
                }
                profiles.sort();

                let mut keys = Vec::new();
                let mut connected = false;
                let mut error_msg = None;

                if let Some(ref mut kb) = kb_opt {
                    connected = true;
                    let total = kb.total_keys();
                    let _rows = kb.matrix_rows();
                    let cols = kb.matrix_cols();

                    let keymap = kb.read_keymap().unwrap_or_default();

                    match kb.read_trip_raw(None) {
                        Ok(trip_values) => {
                            for (i, &raw) in trip_values.iter().take(total).enumerate() {
                                let r = i / cols;
                                let c = i % cols;
                                let off = (r * cols + c) * 2;
                                let kc = if off + 1 < keymap.len() {
                                    u16::from_be_bytes([keymap[off], keymap[off + 1]])
                                } else {
                                    0
                                };
                                let lbl = label(kc);
                                let mm = crate::protocol::travel_to_mm(raw);
                                let is_pressed = mm >= 0.25;
                                keys.push(KeyTelemetry {
                                    row: r,
                                    col: c,
                                    label: lbl,
                                    travel_mm: mm,
                                    pressed: is_pressed,
                                });
                            }
                        }
                        Err(e) => {
                            error_msg = Some(e.to_string());
                            kb_opt = None;
                        }
                    }
                }

                sample_count += 1;
                hz_samples += 1;
                let elapsed_hz = last_hz_calc.elapsed().as_secs_f64();
                if elapsed_hz >= 1.0 {
                    current_hz = (hz_samples as f64) / elapsed_hz;
                    hz_samples = 0;
                    last_hz_calc = Instant::now();
                }

                let sweep_ms = start_sweep.elapsed().as_secs_f64() * 1000.0;

                if let Ok(mut g) = snap_arc.write() {
                    g.keys = keys;
                    g.connected = connected;
                    g.profiles = profiles;
                    g.error = error_msg;
                    g.sample_hz = current_hz;
                    g.samples = sample_count;
                    g.sweep_ms = sweep_ms;
                }

                let elapsed = start_sweep.elapsed();
                if elapsed < interval {
                    std::thread::sleep(interval - elapsed);
                }
            }
        });
    }
}

// -----------------------------------------------------------------------------
// Serveur HTTP local (127.0.0.1)
// -----------------------------------------------------------------------------

/// Arguments CLI pour la commande `hud`.
#[derive(Debug, Clone, clap::Args)]
pub struct HudArgs {
    /// Port d'écoute du serveur HTTP de télémétrie HUD (défaut: 8787).
    #[arg(long, default_value_t = DEFAULT_PORT)]
    pub port: u16,

    /// Période d'échantillonnage en secondes (défaut: 0.03 = ~33 Hz).
    #[arg(long, default_value_t = DEFAULT_INTERVAL)]
    pub interval: f64,

    /// Ne pas ouvrir le navigateur web automatiquement.
    #[arg(long)]
    pub no_browser: bool,

    /// Afficher l'overlay transparent natif Windows en plus du serveur web.
    #[arg(long)]
    pub overlay: bool,
}

/// Point d'entrée principal pour la commande `hud`.
pub fn run(args: &HudArgs) -> anyhow::Result<i32> {
    let profile_dir = crate::paths::profile_dir();
    let sampler = Arc::new(Sampler::new(profile_dir, args.interval));
    sampler.start();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_srv = stop.clone();
    let sampler_srv = sampler.clone();
    let port = args.port;

    let srv_handle = std::thread::spawn(move || {
        let _ = serve_http(sampler_srv, "127.0.0.1", port, stop_srv);
    });

    let url = format!("http://127.0.0.1:{port}/");
    println!("HUD OpenMAD démarré sur {url}");
    println!("Appuyez sur des touches pour observer la télémétrie en direct. Ctrl-C pour quitter.");

    if !args.no_browser {
        #[cfg(windows)]
        {
            use std::process::Command;
            let _ = Command::new("cmd").args(["/C", "start", &url]).spawn();
        }
    }

    if args.overlay {
        #[cfg(windows)]
        show_overlay(sampler.clone(), stop.clone());
    } else {
        // Boucle d'attente d'interruption
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    stop.store(true, Ordering::SeqCst);
    sampler.stop();
    let _ = srv_handle.join();
    Ok(0)
}

/// Serveur HTTP minimaliste servant le dashboard et l'API JSON.
pub fn serve_http(
    sampler: Arc<Sampler>,
    host: &str,
    port: u16,
    stop: Arc<AtomicBool>,
) -> Result<(), std::io::Error> {
    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr)?;
    listener.set_nonblocking(true)?;

    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                handle_http_client(&mut stream, &sampler);
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn handle_http_client(stream: &mut TcpStream, sampler: &Sampler) {
    let mut buf = [0u8; 2048];
    let bytes_read = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let req = String::from_utf8_lossy(&buf[..bytes_read]);
    let first_line = req.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }

    let path = parts[1];

    if path == "/api/snapshot" || path == "/api/telemetry" {
        let snap = sampler.snapshot();
        let json = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_string());
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\n\r\n{}",
            json.len(),
            json
        );
        let _ = stream.write_all(resp.as_bytes());
    } else if path == "/api/version" {
        let v = serde_json::json!({
            "app": "openmad-hud",
            "version": app_version(),
            "build": format!("openmad-rs-{}", app_version()),
        });
        let json = v.to_string();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            json.len(),
            json
        );
        let _ = stream.write_all(resp.as_bytes());
    } else {
        // Dashboard HTML embarqué
        let html = include_str!("../assets/hud_dashboard.html");
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            html.len(),
            html
        );
        let _ = stream.write_all(resp.as_bytes());
    }
}

// -----------------------------------------------------------------------------
// Overlay Win32 natif transparent et au premier plan (WS_EX_NOACTIVATE)
// -----------------------------------------------------------------------------

#[cfg(windows)]
pub fn show_overlay(sampler: Arc<Sampler>, stop: Arc<AtomicBool>) {
    use std::sync::Once;
    use windows::core::w;
    use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND};
    use windows::Win32::Graphics::Gdi::HBRUSH;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetSystemMetrics, PeekMessageW, RegisterClassW,
        SetLayeredWindowAttributes, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW, LWA_ALPHA,
        MSG, PM_REMOVE, SM_CXSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_SHOWNOACTIVATE,
        WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
        WS_VISIBLE,
    };

    static REGISTER: Once = Once::new();
    let class_name = w!("OpenMadOverlayHUD");

    REGISTER.call_once(|| {
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: HINSTANCE::default(),
            hIcon: windows::Win32::UI::WindowsAndMessaging::HICON::default(),
            hCursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR::default(),
            hbrBackground: HBRUSH::default(),
            lpszMenuName: windows::core::PCWSTR::null(),
            lpszClassName: class_name,
        };
        unsafe {
            let _ = RegisterClassW(&wc);
        }
    });

    let width = 360;
    let height = 240;
    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let x = (screen_w - width - 20).max(10);
    let y = 30;

    let ex_style = WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;

    let hwnd_res = unsafe {
        CreateWindowExW(
            ex_style,
            class_name,
            w!("OpenMAD HUD"),
            WS_POPUP | WS_VISIBLE,
            x,
            y,
            width,
            height,
            HWND::default(),
            windows::Win32::UI::WindowsAndMessaging::HMENU::default(),
            HINSTANCE::default(),
            None,
        )
    };

    let hwnd = match hwnd_res {
        Ok(h) => h,
        Err(_) => return,
    };

    unsafe {
        // Translucidité à 85%
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 220, LWA_ALPHA);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(
            hwnd,
            HWND::default(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        let _ = windows::Win32::UI::WindowsAndMessaging::SetTimer(hwnd, 1, 33, None);
    }

    // Sauvegarde globale pour le WNDPROC
    CURRENT_SAMPLER.with(|s| {
        *s.borrow_mut() = Some(sampler);
    });

    let mut msg = MSG::default();
    while !stop.load(Ordering::SeqCst) {
        unsafe {
            while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                if msg.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
                    stop.store(true, Ordering::SeqCst);
                    break;
                }
                let _ = windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(hwnd);
    }
}

#[cfg(windows)]
std::thread_local! {
    static CURRENT_SAMPLER: std::cell::RefCell<Option<Arc<Sampler>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(windows)]
unsafe extern "system" fn overlay_wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{COLORREF, LRESULT, RECT};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC,
        DeleteObject, EndPaint, FillRect, InvalidateRect, SelectObject, SetBkMode, SetTextColor,
        TextOutW, PAINTSTRUCT, SRCCOPY, TRANSPARENT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetClientRect, PostQuitMessage, WM_DESTROY, WM_PAINT, WM_TIMER,
    };

    match msg {
        WM_TIMER => {
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            let w = rect.right - rect.left;
            let h = rect.bottom - rect.top;

            // Double buffering GDI
            let mem_dc = CreateCompatibleDC(hdc);
            let mem_bmp = CreateCompatibleBitmap(hdc, w, h);
            let old_bmp = SelectObject(mem_dc, mem_bmp);

            // Fond sombre
            let bg_brush = CreateSolidBrush(COLORREF(0x181212));
            let _ = FillRect(mem_dc, &rect, bg_brush);
            let _ = DeleteObject(bg_brush);

            let _ = SetBkMode(mem_dc, TRANSPARENT);

            // Titre
            let _ = SetTextColor(mem_dc, COLORREF(0x00FF88));
            let title = windows::core::w!("OpenMAD Overlay HUD");
            let _ = TextOutW(mem_dc, 15, 12, title.as_wide());

            // Récupération de la télémétrie actuelle
            let snap = CURRENT_SAMPLER.with(|s| {
                s.borrow().as_ref().map(|samp| samp.snapshot()).unwrap_or_default()
            });

            // Ligne de statut connexion & Hz
            let _ = SetTextColor(mem_dc, COLORREF(0xFFFFFF));
            let conn_str = if snap.connected {
                format!("Connecté  |  {:.0} Hz  |  {:.1} ms", snap.sample_hz, snap.sweep_ms)
            } else {
                "Recherche clavier MAD68 ...".to_string()
            };
            let conn_wide: Vec<u16> = conn_str.encode_utf16().collect();
            let _ = TextOutW(mem_dc, 15, 38, &conn_wide);

            // Affichage des touches actuellement enfoncées avec jauge de millimètres
            let mut y = 68;

            let active_keys: Vec<_> = snap.keys.iter().filter(|k| k.travel_mm > 0.20).collect();

            if active_keys.is_empty() {
                let _ = SetTextColor(mem_dc, COLORREF(0x888888));
                let idle_str = windows::core::w!("En attente d'appui touches (WASD, ...) ...");
                let _ = TextOutW(mem_dc, 15, y, idle_str.as_wide());
            } else {
                for k in active_keys.iter().take(5) {
                    let _ = SetTextColor(mem_dc, COLORREF(0x55FFFF));
                    let line = format!("{: <6} : {:.2} mm", k.label, k.travel_mm);
                    let line_wide: Vec<u16> = line.encode_utf16().collect();
                    let _ = TextOutW(mem_dc, 15, y, &line_wide);

                    // Jauge graphique (barre horizontale)
                    let gauge_max = 160;
                    let gauge_w = ((k.travel_mm / 3.5).clamp(0.0, 1.0) * (gauge_max as f64)) as i32;
                    let gauge_rect = RECT {
                        left: 170,
                        top: y + 2,
                        right: 170 + gauge_w,
                        bottom: y + 16,
                    };
                    let bar_brush = CreateSolidBrush(COLORREF(0x00A5FF));
                    let _ = FillRect(mem_dc, &gauge_rect, bar_brush);
                    let _ = DeleteObject(bar_brush);

                    y += 24;
                }
            }

            let _ = windows::Win32::Graphics::Gdi::BitBlt(
                hdc,
                0,
                0,
                w,
                h,
                mem_dc,
                0,
                0,
                SRCCOPY,
            );

            let _ = SelectObject(mem_dc, old_bmp);
            let _ = DeleteObject(mem_bmp);
            let _ = DeleteDC(mem_dc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
