//! Profils de configuration par nom pour claviers magnétiques DuckBread / MAD68.
//!
//! Traduction directe de `src/mad68/profile.py`.
//!
//! Un profil est une description déclarative de l'état cible du clavier.
//! L'application d'un profil calcule un diff minimal par rapport à l'état réel
//! du périphérique et n'écrit que les champs qui diffèrent effectivement.
//!
//! Cette approche différentielle préserve la durée de vie des cellules flash
//! de la mémoire non-volatile et permet des bascules automatiques instantanées
//! lors des changements de fenêtre active (jeu ou application au premier plan).

use std::collections::BTreeMap;
use std::path::Path;
use serde::{Deserialize, Serialize};

use crate::device::{DeviceError, Mad68, RapidTrigger};
use crate::features::{
    AdvancedKey, DeadBand, Dks, DksAction, KeyboardFeature, LightInfo, TapDance,
};
use crate::protocol::{
    ANIMATED_EFFECTS, COLOUR_EFFECTS, DEFAULT_EFFECT_COLOUR, DEFAULT_EFFECT_SPEED,
    KEYCODE_SIZE, MAX_ACTUATION_WRITE, MAX_BUFFER_CHUNK, MAX_RT_WRITE, TOTAL_KEYS,
    AdvancedKeyMode, FlashOp,
};

/// Nombre de slots de touches avancées (SOCD, Rapid Snap, OKS) capturés par un profil.
pub const ADVANCED_KEY_SLOTS: usize = 16;

/// Effet d'éclairage confiant le contrôle de chaque LED à la table de couleurs par touche.
pub const PER_KEY_EFFECT: u8 = 1;

/// Tolérance par canal lors de la comparaison des couleurs des touches (la PWM quantifie).
pub const COLOR_TOLERANCE: i32 = 8;

/// Version courante du schéma de profil (v2 ajoute éclairage, RVB par touche et touches avancées).
pub const PROFILE_VERSION: u32 = 2;

/// Nom du profil de référence généré au premier lancement.
pub const DEFAULT_PROFILE_NAME: &str = "Default";

/// Erreurs liées à la manipulation et au chargement des profils.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("version de profil non supportée : {0}, attendue 1 ou 2")]
    UnsupportedVersion(u32),

    #[error("erreur de sérialisation JSON : {0}")]
    Json(#[from] serde_json::Error),

    #[error("erreur E/S : {0}")]
    Io(#[from] std::io::Error),

    #[error("erreur périphérique : {0}")]
    Device(#[from] DeviceError),

    #[error("erreur de validation : {0}")]
    Validation(String),
}

/// Clé textuelle stable pour les objets JSON représentant une position de matrice : "row,col".
pub fn key_string(row: u8, col: u8) -> String {
    format!("{row},{col}")
}

/// Analyse une chaîne "row,col" en coordonnées de matrice.
pub fn parse_key_string(text: &str) -> Option<(u8, u8)> {
    let (r, c) = text.split_once(',')?;
    let row = r.trim().parse::<u8>().ok()?;
    let col = c.trim().parse::<u8>().ok()?;
    Some((row, col))
}

/// Réagence un tableau linéaire de couleurs RVB par touche pour s'adapter à la largeur d'une autre carte.
///
/// Les couleurs par touche étant stockées dans l'ordre linéaire des touches, leur signification
/// dépend du nombre de colonnes de la carte source. Chaque carte de cette famille possède 5 lignes,
/// ce qui permet de réindexer la matrice (row, col) pour une carte de largeur différente
/// sans décaler d'une colonne chaque rangée suivante.
pub fn fit_key_colors(colors: &[[u8; 3]], rows: usize, cols: usize) -> Vec<[u8; 3]> {
    let total = rows * cols;
    let src_cols = if rows > 0 && colors.len() % rows == 0 {
        colors.len() / rows
    } else {
        cols
    };

    let mut out = Vec::with_capacity(total);
    if src_cols != cols {
        for r in 0..rows {
            for c in 0..cols {
                if c < src_cols && r * src_cols + c < colors.len() {
                    out.push(colors[r * src_cols + c]);
                } else {
                    out.push([0, 0, 0]);
                }
            }
        }
    } else {
        out.extend_from_slice(colors);
    }

    while out.len() < total {
        out.push([0, 0, 0]);
    }
    out.truncate(total);
    out
}

/// Réglages de Rapid Trigger sans liaison à une touche spécifique.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RapidTriggerSpec {
    pub enabled: bool,
    pub press_mm: f64,
    pub release_mm: f64,
}

impl RapidTriggerSpec {
    pub fn new(enabled: bool, press_mm: f64, release_mm: f64) -> Self {
        Self {
            enabled,
            press_mm,
            release_mm,
        }
    }

    pub fn from_device(rt: &RapidTrigger) -> Self {
        Self {
            enabled: rt.enabled,
            press_mm: rt.press_mm,
            release_mm: rt.release_mm,
        }
    }
}

/// Règle de déclenchement automatique pour associer une application au profil.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerSpec {
    #[serde(default)]
    pub exe: String,
    #[serde(default)]
    pub title: String,
}

/// Section actionnement dans le JSON du profil.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActuationSection {
    pub default_mm: f64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides_mm: BTreeMap<String, f64>,
}

/// Section Rapid Trigger dans le JSON du profil.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RapidTriggerSection {
    pub default: RapidTriggerSpec,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, RapidTriggerSpec>,
}

/// Profil de configuration nommé pour claviers magnétiques.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub profile_version: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,

    pub actuation: ActuationSection,
    pub rapid_trigger: RapidTriggerSection,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keymap_hex: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_colors: Option<Vec<[u8; 3]>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<serde_json::Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advanced_keys: Option<Vec<serde_json::Value>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub performance: Option<serde_json::Value>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tap_dance: Option<Vec<serde_json::Value>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dks: Option<Vec<serde_json::Value>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triggers: Option<Vec<TriggerSpec>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_exe: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_title: Option<String>,
}

impl Profile {
    /// Crée un profil minimal à partir des valeurs par défaut.
    pub fn new(
        name: impl Into<String>,
        actuation_default: f64,
        rapid_trigger_default: RapidTriggerSpec,
    ) -> Self {
        Self {
            profile_version: PROFILE_VERSION,
            name: name.into(),
            description: String::new(),
            actuation: ActuationSection {
                default_mm: actuation_default,
                overrides_mm: BTreeMap::new(),
            },
            rapid_trigger: RapidTriggerSection {
                default: rapid_trigger_default,
                overrides: BTreeMap::new(),
            },
            keymap_hex: None,
            key_colors: None,
            light: None,
            advanced_keys: None,
            performance: None,
            tap_dance: None,
            dks: None,
            triggers: None,
            trigger_exe: None,
            trigger_title: None,
        }
    }

    /// Point d'actionnement pour une touche donnée ("row,col").
    pub fn actuation_for(&self, row: u8, col: u8) -> f64 {
        let key = key_string(row, col);
        self.actuation
            .overrides_mm
            .get(&key)
            .copied()
            .unwrap_or(self.actuation.default_mm)
    }

    /// Configuration Rapid Trigger pour une touche donnée ("row,col").
    pub fn rapid_trigger_for(&self, row: u8, col: u8) -> RapidTriggerSpec {
        let key = key_string(row, col);
        self.rapid_trigger
            .overrides
            .get(&key)
            .copied()
            .unwrap_or(self.rapid_trigger.default)
    }

    /// Liste des déclencheurs applicatifs, compatible avec l'ancien et le nouveau schéma.
    pub fn trigger_list(&self) -> Vec<TriggerSpec> {
        if let Some(ref trs) = self.triggers {
            let mut out = Vec::new();
            for t in trs {
                let exe = t.exe.trim().to_string();
                let title = t.title.trim().to_string();
                if !exe.is_empty() || !title.is_empty() {
                    out.push(TriggerSpec { exe, title });
                }
            }
            return out;
        }

        let exe = self.trigger_exe.as_deref().unwrap_or("").trim().to_string();
        let title = self.trigger_title.as_deref().unwrap_or("").trim().to_string();
        if !exe.is_empty() || !title.is_empty() {
            vec![TriggerSpec { exe, title }]
        } else {
            Vec::new()
        }
    }

    /// Normalise le profil avant sauvegarde (mise à jour des déclencheurs).
    pub fn prepare_for_save(&mut self) {
        self.profile_version = PROFILE_VERSION;
        let trs = self.trigger_list();
        if !trs.is_empty() {
            if trs.len() == 1 {
                self.trigger_exe = Some(trs[0].exe.clone());
                self.trigger_title = Some(trs[0].title.clone());
            } else {
                self.trigger_exe = None;
                self.trigger_title = None;
            }
            self.triggers = Some(trs);
        } else {
            self.triggers = None;
            self.trigger_exe = None;
            self.trigger_title = None;
        }
    }

    /// Charge un profil depuis un fichier JSON (gère le BOM UTF-8).
    pub fn load(path: &Path) -> Result<Self, ProfileError> {
        let bytes = std::fs::read(path)?;
        let slice = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            &bytes[3..]
        } else {
            &bytes[..]
        };

        let profile: Self = serde_json::from_slice(slice)?;
        if profile.profile_version != 1 && profile.profile_version != PROFILE_VERSION {
            return Err(ProfileError::UnsupportedVersion(profile.profile_version));
        }
        Ok(profile)
    }

    /// Enregistre le profil au format JSON indenté à 2 espaces.
    pub fn save(&mut self, path: &Path) -> Result<(), ProfileError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.prepare_for_save();
        let json_str = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json_str)?;
        Ok(())
    }

    // Calcul de l'état cible pour un clavier connecté

    pub fn target_actuation(&self, kb: &Mad68) -> Vec<f64> {
        kb.iter_keys()
            .into_iter()
            .map(|(r, c)| self.actuation_for(r, c))
            .collect()
    }

    pub fn target_rapid_trigger(&self, kb: &Mad68) -> Vec<RapidTrigger> {
        kb.iter_keys()
            .into_iter()
            .map(|(r, c)| {
                let spec = self.rapid_trigger_for(r, c);
                RapidTrigger {
                    row: r,
                    col: c,
                    enabled: spec.enabled,
                    release_mm: spec.release_mm,
                    press_mm: spec.press_mm,
                }
            })
            .collect()
    }

    pub fn target_key_colors(&self, kb: &Mad68) -> Option<Vec<[u8; 3]>> {
        self.key_colors.as_ref().map(|colors| {
            fit_key_colors(colors, kb.matrix_rows(), kb.matrix_cols())
        })
    }

    pub fn keymap_fits(&self, kb: &Mad68) -> bool {
        match self.keymap_hex {
            Some(ref hex) => hex.len() / 2 == kb.keymap_size(),
            None => true,
        }
    }

    /// Évalue l'état de la keymap entre le profil et le clavier.
    pub fn keymap_status(&self, kb: &mut Mad68) -> Result<KeymapStatus, DeviceError> {
        let hex = match self.keymap_hex {
            Some(ref h) => h,
            None => {
                return Ok(KeymapStatus {
                    has_keymap: false,
                    matches: true,
                    keys_differ: 0,
                    incompatible: false,
                    reason: String::new(),
                });
            }
        };

        let want = hex_to_bytes(hex);
        if want.len() != kb.keymap_size() {
            let reason = format!(
                "cette keymap a été capturée pour une autre taille de carte et ne peut être appliquée à la {}.",
                kb.spec.name
            );
            return Ok(KeymapStatus {
                has_keymap: true,
                matches: false,
                keys_differ: 0,
                incompatible: true,
                reason,
            });
        }

        let have = kb.read_keymap()?;
        if have == want {
            return Ok(KeymapStatus {
                has_keymap: true,
                matches: true,
                keys_differ: 0,
                incompatible: false,
                reason: String::new(),
            });
        }

        let mut differ = 0;
        let min_len = have.len().min(want.len());
        for i in (0..min_len).step_by(KEYCODE_SIZE) {
            if i + KEYCODE_SIZE <= min_len && have[i..i + KEYCODE_SIZE] != want[i..i + KEYCODE_SIZE] {
                differ += 1;
            }
        }

        Ok(KeymapStatus {
            has_keymap: true,
            matches: false,
            keys_differ: differ,
            incompatible: false,
            reason: String::new(),
        })
    }

    /// Écrit la keymap du profil dans le clavier (opération persistante VIA).
    pub fn apply_keymap(&self, kb: &mut Mad68) -> Result<usize, DeviceError> {
        let hex = match self.keymap_hex {
            Some(ref h) => h,
            None => return Ok(0),
        };

        let want = hex_to_bytes(hex);
        if want.len() != kb.keymap_size() {
            return Err(DeviceError::InvalidArgument(
                "keymap incompatible avec la taille de la matrice du clavier".to_string(),
            ));
        }

        let have = kb.read_keymap()?;
        if have == want {
            return Ok(0);
        }

        kb.write_keymap(&want)?;
        Ok(want.len().div_ceil(MAX_BUFFER_CHUNK))
    }

    /// Calcule le plan de mise à jour différentiel entre ce profil et l'état actuel du périphérique.
    pub fn plan(&self, kb: &mut Mad68, include_keymap: bool) -> Result<ApplyPlan, DeviceError> {
        let have_act = kb.read_actuation_bulk(0, None)?;
        let have_rt = kb.read_rapid_trigger_bulk(0, None)?;
        let want_act = self.target_actuation(kb);
        let want_rt = self.target_rapid_trigger(kb);

        let act_changes = have_act
            .iter()
            .zip(want_act.iter())
            .filter(|(&a, &b)| (a - b).abs() > 1e-4)
            .count();

        let rt_changes = have_rt
            .iter()
            .zip(want_rt.iter())
            .filter(|(a, b)| a != b)
            .count();

        let mut keymap = None;
        if include_keymap {
            if let Some(ref hex) = self.keymap_hex {
                let want_km = hex_to_bytes(hex);
                if want_km.len() == kb.keymap_size() {
                    let have_km = kb.read_keymap()?;
                    if have_km != want_km {
                        keymap = Some(want_km);
                    }
                }
            }
        }

        let mut key_colors = None;
        if let Some(want_kc) = self.target_key_colors(kb) {
            let effect = self
                .light
                .as_ref()
                .and_then(|l| l.get("effect"))
                .and_then(|e| e.as_u64())
                .unwrap_or(0) as u8;

            if effect == PER_KEY_EFFECT {
                let have = kb.read_all_key_colors()?;
                let live = kb.populated_mask();
                let differs = have.len() != want_kc.len()
                    || have
                        .iter()
                        .zip(want_kc.iter())
                        .enumerate()
                        .any(|(i, (hc, wc))| {
                            i < live.len()
                                && live[i]
                                && ((hc[0] as i32 - wc[0] as i32).abs() > COLOR_TOLERANCE
                                    || (hc[1] as i32 - wc[1] as i32).abs() > COLOR_TOLERANCE
                                    || (hc[2] as i32 - wc[2] as i32).abs() > COLOR_TOLERANCE)
                        });
                if differs {
                    key_colors = Some(want_kc);
                }
            }
        }

        let mut light = None;
        if let Some(ref l_val) = self.light {
            let effect = l_val.get("effect").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            let mut speed = l_val.get("speed").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            let mut r = l_val.get("r").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            let mut g = l_val.get("g").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            let mut b = l_val.get("b").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
            let brightness = l_val.get("brightness").and_then(|v| v.as_u64()).unwrap_or(0) as u8;

            if ANIMATED_EFFECTS.contains(&effect) && speed == 0 {
                speed = DEFAULT_EFFECT_SPEED;
            }
            if COLOUR_EFFECTS.contains(&effect) && r == 0 && g == 0 && b == 0 {
                r = DEFAULT_EFFECT_COLOUR.0;
                g = DEFAULT_EFFECT_COLOUR.1;
                b = DEFAULT_EFFECT_COLOUR.2;
            }
            let li = LightInfo {
                effect,
                speed,
                r,
                g,
                b,
                brightness,
            };
            if let Ok(have_li) = kb.read_light_info() {
                if (have_li.effect, have_li.speed, have_li.brightness)
                    != (li.effect, li.speed, li.brightness)
                {
                    light = Some(li);
                }
            } else {
                light = Some(li);
            }
        }

        let mut performance = None;
        if let Some(ref p_val) = self.performance {
            if let Ok(cur_f) = kb.read_feature() {
                let cur_bot = kb.read_bottom_optimize().unwrap_or(0);
                let cur_game = kb.read_game_mode().unwrap_or(0);

                let want_wasd = p_val.get("wasd_switch").and_then(|v| v.as_u64()).map(|v| v as u8);
                let want_bot = p_val.get("bottom_optimize").and_then(|v| v.as_u64()).map(|v| v as u8);
                let want_game = p_val.get("game_mode").and_then(|v| v.as_u64()).map(|v| v as u8);

                let mut differ = false;
                if let Some(w) = want_wasd {
                    if cur_f.wasd_switch != w { differ = true; }
                }
                if let Some(b) = want_bot {
                    if cur_bot != b { differ = true; }
                }
                if let Some(g) = want_game {
                    if cur_game != g { differ = true; }
                }

                if differ {
                    performance = Some(p_val.clone());
                }
            } else {
                performance = Some(p_val.clone());
            }
        }

        let mut advanced_keys = Vec::new();
        if let Some(ref aks) = self.advanced_keys {
            for entry in aks {
                if entry.get("error").is_some() {
                    continue;
                }
                let idx = entry.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let mode_val = entry.get("mode");
                let mode = match mode_val {
                    Some(serde_json::Value::String(s)) => match s.to_ascii_uppercase().as_str() {
                        "RS" => AdvancedKeyMode::Rs,
                        "SOCD" => AdvancedKeyMode::Socd,
                        "SOCDKEY1" | "SOCD_KEY1" => AdvancedKeyMode::SocdKey1,
                        "SOCDKEY2" | "SOCD_KEY2" => AdvancedKeyMode::SocdKey2,
                        "SOCDBALANCE" | "SOCD_BALANCE" => AdvancedKeyMode::SocdBalance,
                        "OKS" => AdvancedKeyMode::Oks,
                        _ => AdvancedKeyMode::None,
                    },
                    Some(serde_json::Value::Number(n)) => n
                        .as_u64()
                        .and_then(|v| AdvancedKeyMode::from_id(v as u8))
                        .unwrap_or(AdvancedKeyMode::None),
                    _ => AdvancedKeyMode::None,
                };
                let id = entry.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let rs_apc_lv = entry.get("rs_apc_lv").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                let gapc_sw = entry.get("gapc_sw").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let rt_sw = entry.get("rt_sw").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let key1_row = entry.get("key1_row").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let key1_col = entry.get("key1_col").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let key2_row = entry.get("key2_row").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let key2_col = entry.get("key2_col").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let layer = entry.get("layer").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let ak = AdvancedKey {
                    index: idx,
                    mode,
                    id,
                    rs_apc_lv,
                    gapc_sw,
                    rt_sw,
                    key1_row,
                    key1_col,
                    key2_row,
                    key2_col,
                    layer,
                };
                if let Ok(have_ak) = kb.read_advanced_key(ak.index) {
                    if have_ak != ak {
                        advanced_keys.push(ak);
                    }
                } else {
                    advanced_keys.push(ak);
                }
            }
        }

        let mut tap_dance = Vec::new();
        if let Some(ref tds) = self.tap_dance {
            for entry in tds {
                let idx = entry.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let tap = entry.get("tap").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                let hold = entry.get("hold").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                let timer_ms = entry.get("timer_ms").and_then(|v| v.as_u64()).unwrap_or(200) as u16;
                let td = TapDance {
                    index: idx,
                    tap,
                    hold,
                    timer_ms,
                };
                if let Ok(have_td) = kb.read_tap_dance(td.index) {
                    if have_td != td {
                        tap_dance.push(td);
                    }
                } else {
                    tap_dance.push(td);
                }
            }
        }

        let mut dks_writes = Vec::new();
        if let Some(ref dks_entries) = self.dks {
            for entry in dks_entries {
                let idx = entry.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
                let mut actions = [DksAction::EMPTY; crate::features::DKS_ACTION_COUNT];
                if let Some(acts) = entry.get("actions").and_then(|v| v.as_array()) {
                    for (i, a) in acts.iter().take(crate::features::DKS_ACTION_COUNT).enumerate() {
                        let kc = a.get("keycode").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        let st = a.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                        actions[i] = DksAction::from_keycode(kc, st);
                    }
                }
                let want_dks = Dks {
                    index: idx,
                    actions,
                };
                if let Ok(have_dks) = kb.read_dks(idx) {
                    if have_dks != want_dks {
                        dks_writes.push(want_dks);
                    }
                } else {
                    dks_writes.push(want_dks);
                }
            }
        }

        Ok(ApplyPlan {
            profile: self.clone(),
            actuation: if act_changes > 0 { Some(want_act) } else { None },
            rapid_trigger: if rt_changes > 0 { Some(want_rt) } else { None },
            keymap,
            actuation_changes: act_changes,
            rapid_trigger_changes: rt_changes,
            key_colors,
            light,
            advanced_keys,
            performance_patch: performance,
            tap_dance,
            dks: dks_writes,
        })
    }

    /// Capture l'état en direct du clavier pour créer un nouveau profil.
    pub fn from_device(
        kb: &mut Mad68,
        name: impl Into<String>,
        include_keymap: bool,
        include_lighting: bool,
        include_advanced: bool,
        description: impl Into<String>,
    ) -> Result<Self, DeviceError> {
        let keys = kb.iter_keys();
        let actuation = kb.read_actuation_bulk(0, None)?;
        let triggers = kb.read_rapid_trigger_bulk(0, None)?;

        // Détermine la valeur la plus fréquente pour servir de valeur par défaut
        let act_default = most_common_f64(&actuation).unwrap_or(1.50);
        let rt_specs: Vec<RapidTriggerSpec> = triggers.iter().map(RapidTriggerSpec::from_device).collect();
        let rt_default = most_common_rt(&rt_specs).unwrap_or(RapidTriggerSpec::new(false, 0.50, 0.50));

        let mut act_overrides = BTreeMap::new();
        for ((r, c), &val) in keys.iter().zip(actuation.iter()) {
            if (val - act_default).abs() > 1e-4 {
                act_overrides.insert(key_string(*r, *c), val);
            }
        }

        let mut rt_overrides = BTreeMap::new();
        for rt in &triggers {
            let spec = RapidTriggerSpec::from_device(rt);
            if spec != rt_default {
                rt_overrides.insert(key_string(rt.row, rt.col), spec);
            }
        }

        let mut key_colors = None;
        let mut light = None;
        if include_lighting {
            if let Ok(colors) = kb.read_all_key_colors() {
                key_colors = Some(colors);
            }
            if let Ok(li) = kb.read_light_info() {
                light = Some(li.to_json());
            }
        }

        let mut advanced_keys = None;
        if include_advanced {
            let mut list = Vec::new();
            for i in 0..ADVANCED_KEY_SLOTS as u8 {
                if let Ok(ak) = kb.read_advanced_key(i) {
                    list.push(ak.to_json());
                }
            }
            if !list.is_empty() {
                advanced_keys = Some(list);
            }
        }

        let keymap_hex = if include_keymap {
            kb.read_keymap().ok().map(|km| bytes_to_hex(&km))
        } else {
            None
        };

        Ok(Self {
            profile_version: PROFILE_VERSION,
            name: name.into(),
            description: description.into(),
            actuation: ActuationSection {
                default_mm: act_default,
                overrides_mm: act_overrides,
            },
            rapid_trigger: RapidTriggerSection {
                default: rt_default,
                overrides: rt_overrides,
            },
            keymap_hex,
            key_colors,
            light,
            advanced_keys,
            performance: None,
            tap_dance: None,
            dks: None,
            triggers: None,
            trigger_exe: None,
            trigger_title: None,
        })
    }
}

/// Statut de compatibilité d'une keymap stockée.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapStatus {
    pub has_keymap: bool,
    pub matches: bool,
    pub keys_differ: usize,
    pub incompatible: bool,
    pub reason: String,
}

/// Plan d'application calculé par diff entre un profil et le périphérique.
#[derive(Debug, Clone)]
pub struct ApplyPlan {
    pub profile: Profile,
    pub actuation: Option<Vec<f64>>,
    pub rapid_trigger: Option<Vec<RapidTrigger>>,
    pub keymap: Option<Vec<u8>>,
    pub actuation_changes: usize,
    pub rapid_trigger_changes: usize,
    pub key_colors: Option<Vec<[u8; 3]>>,
    pub light: Option<LightInfo>,
    pub advanced_keys: Vec<AdvancedKey>,
    pub performance_patch: Option<serde_json::Value>,
    pub tap_dance: Vec<TapDance>,
    pub dks: Vec<Dks>,
}

impl ApplyPlan {
    pub fn is_empty(&self) -> bool {
        self.actuation.is_none()
            && self.rapid_trigger.is_none()
            && self.keymap.is_none()
            && self.key_colors.is_none()
            && self.light.is_none()
            && self.advanced_keys.is_empty()
            && self.performance_patch.is_none()
            && self.tap_dance.is_empty()
            && self.dks.is_empty()
    }

    pub fn packet_estimate(&self) -> usize {
        let mut n = 0;
        if let Some(ref act) = self.actuation {
            n += act.len().div_ceil(MAX_ACTUATION_WRITE);
        }
        if let Some(ref rt) = self.rapid_trigger {
            n += rt.len().div_ceil(MAX_RT_WRITE);
        }
        if let Some(ref km) = self.keymap {
            n += km.len().div_ceil(MAX_BUFFER_CHUNK);
        }
        n += self.advanced_keys.len();
        n += self.tap_dance.len();
        n += self.dks.len();
        if self.light.is_some() {
            n += 1;
        }
        if self.key_colors.is_some() {
            n += 9;
        }
        n
    }

    /// Exécute le plan en écrivant les modifications nécessaires sur le clavier.
    ///
    /// Par défaut, les écritures sont volatiles (`FlashOp::Normal`) afin de ne pas
    /// consommer l'endurance de la flash lors des bascules automatiques d'applications.
    /// Passer `persist = true` uniquement lors d'une sauvegarde explicite initiée par l'utilisateur.
    ///
    /// IMPORTANT : L'ordre d'écriture est critique. L'écriture d'une touche avancée (SOCD)
    /// réapplique son niveau d'actionnement embarqué (rs_apc_lv) sur ses deux touches liées.
    /// Elle DOIT donc impérativement être effectuée AVANT l'écriture globale de l'actionnement,
    /// sous peine de voir les touches A et D écrasées silencieusement par les valeurs flash.
    pub fn execute(&self, kb: &mut Mad68, persist: bool) -> Result<usize, DeviceError> {
        let flash_op = if persist {
            FlashOp::EraseAndWrite
        } else {
            FlashOp::Normal
        };
        let mut packets = 0;

        // 1. Touches avancées en premier (SOCD / RS / OKS)
        for ak in &self.advanced_keys {
            kb.write_advanced_key(ak)?;
            packets += 1;
        }

        // 2. Tap Dance
        for td in &self.tap_dance {
            kb.write_tap_dance(td)?;
            packets += 1;
        }

        // 3. Dynamic Keystrokes (DKS)
        for d in &self.dks {
            kb.write_dks(d)?;
            packets += 1;
        }

        // 4. Paramètres de performance globaux
        if let Some(ref p) = self.performance_patch {
            let cur = kb.read_feature()?;
            let new_f = KeyboardFeature {
                rgb_area: p.get("rgb_area").and_then(|v| v.as_u64()).map(|v| v as u8).unwrap_or(cur.rgb_area),
                wasd_switch: p.get("wasd_switch").and_then(|v| v.as_u64()).map(|v| v as u8).unwrap_or(cur.wasd_switch),
                mac_switch: p.get("mac_switch").and_then(|v| v.as_u64()).map(|v| v as u8).unwrap_or(cur.mac_switch),
                win_lock: p.get("win_lock").and_then(|v| v.as_u64()).map(|v| v as u8).unwrap_or(cur.win_lock),
                nkro_switch: p.get("nkro_switch").and_then(|v| v.as_u64()).map(|v| v as u8).unwrap_or(cur.nkro_switch),
            };
            kb.write_feature(&new_f)?;
            if let Some(bot) = p.get("bottom_optimize").and_then(|v| v.as_u64()) {
                kb.write_bottom_optimize(bot as u8)?;
            }
            if let Some(game) = p.get("game_mode").and_then(|v| v.as_u64()) {
                kb.write_game_mode(game as u8)?;
            }
            if p.get("dead_band_top").is_some() || p.get("dead_band_bottom").is_some() {
                let top = p.get("dead_band_top").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let bot = p.get("dead_band_bottom").and_then(|v| v.as_f64()).unwrap_or(0.0);
                kb.write_dead_band(&DeadBand { top_mm: top, bottom_mm: bot })?;
            }
            packets += 4;
        }

        // 5. Actionnement par bloc (Bulk)
        if let Some(ref act) = self.actuation {
            packets += kb.write_actuation_bulk(act, 0, flash_op)?;
        }

        // 6. Rapid Trigger par bloc (Bulk)
        if let Some(ref rt) = self.rapid_trigger {
            packets += kb.write_rapid_trigger_bulk(rt, 0, flash_op)?;
        }

        // 7. Keymap dynamique (persiste toujours en EEPROM via VIA)
        if let Some(ref km) = self.keymap {
            kb.write_keymap(km)?;
            packets += km.len().div_ceil(MAX_BUFFER_CHUNK);
        }

        // 8. Couleurs par touche
        if let Some(ref kc) = self.key_colors {
            packets += kb.write_all_key_colors(kc)?;
        }

        // 9. Éclairage global
        if let Some(ref li) = self.light {
            kb.write_light_info(li)?;
            packets += 1;
        }

        Ok(packets)
    }

    pub fn describe(&self) -> String {
        if self.is_empty() {
            return format!("'{}' : déjà appliqué, rien à écrire", self.profile.name);
        }
        let mut lines = vec![format!(
            "'{}' : {} paquet(s) à écrire",
            self.profile.name,
            self.packet_estimate()
        )];
        if let Some(ref act) = self.actuation {
            lines.push(format!(
                "  actionnement     {} touche(s) diffèrent -> écriture bulk des {}",
                self.actuation_changes,
                act.len()
            ));
        }
        if let Some(ref rt) = self.rapid_trigger {
            lines.push(format!(
                "  rapid trigger    {} touche(s) diffèrent -> écriture bulk des {}",
                self.rapid_trigger_changes,
                rt.len()
            ));
        }
        if let Some(ref km) = self.keymap {
            lines.push(format!("  keymap           {} octets", km.len()));
        }
        if let Some(ref kc) = self.key_colors {
            lines.push(format!("  couleurs touches {} touches", kc.len()));
        }
        if let Some(ref li) = self.light {
            lines.push(format!(
                "  éclairage        effet {} vitesse {} luminosité {}",
                li.effect, li.speed, li.brightness
            ));
        }
        lines.join("\n")
    }
}

/// Profil par défaut servant de base saine (1.50 mm partout, RT désactivé, pas de LED).
pub fn default_profile(name: &str) -> Profile {
    let mut advanced_keys = Vec::with_capacity(ADVANCED_KEY_SLOTS);
    for i in 0..ADVANCED_KEY_SLOTS as u8 {
        advanced_keys.push(serde_json::json!({
            "index": i,
            "mode": "None",
            "id": 0,
            "rs_apc_lv": 0,
            "gapc_sw": 0,
            "rt_sw": 0,
            "key1_row": 0,
            "key1_col": 0,
            "key2_row": 0,
            "key2_col": 0,
            "layer": 0
        }));
    }

    Profile {
        profile_version: PROFILE_VERSION,
        name: name.to_string(),
        description: "Base saine : actionnement à 1,50 mm, pas d'éclairage, pas de rapid trigger, pas de touches avancées.".to_string(),
        actuation: ActuationSection {
            default_mm: 1.50,
            overrides_mm: BTreeMap::new(),
        },
        rapid_trigger: RapidTriggerSection {
            default: RapidTriggerSpec::new(false, 0.50, 0.50),
            overrides: BTreeMap::new(),
        },
        keymap_hex: None,
        key_colors: Some(vec![[0, 0, 0]; TOTAL_KEYS]),
        light: Some(serde_json::json!({
            "effect": 0,
            "speed": 0,
            "r": 0,
            "g": 0,
            "b": 0,
            "brightness": 0
        })),
        advanced_keys: Some(advanced_keys),
        performance: Some(serde_json::json!({
            "wasd_switch": 0,
            "mac_switch": 0,
            "win_lock": 0,
            "nkro_switch": 0,
            "rgb_area": 0,
            "bottom_optimize": 0,
            "game_mode": 0
        })),
        tap_dance: None,
        dks: None,
        triggers: None,
        trigger_exe: None,
        trigger_title: None,
    }
}

/// Crée le profil par défaut si le dossier des profils est complètement vide.
pub fn ensure_default_profile(profile_dir: &Path) -> Result<bool, ProfileError> {
    std::fs::create_dir_all(profile_dir)?;
    if let Ok(entries) = std::fs::read_dir(profile_dir) {
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
                return Ok(false);
            }
        }
    }
    let mut prof = default_profile(DEFAULT_PROFILE_NAME);
    prof.save(&profile_dir.join(format!("{DEFAULT_PROFILE_NAME}.json")))?;
    Ok(true)
}

// Helpers d'encodage hexadécimal et statistiques

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut chars = hex.chars().filter(|c| !c.is_whitespace());
    while let (Some(h), Some(l)) = (chars.next(), chars.next()) {
        if let (Some(hi), Some(lo)) = (h.to_digit(16), l.to_digit(16)) {
            bytes.push(((hi << 4) | lo) as u8);
        }
    }
    bytes
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}

fn most_common_f64(vals: &[f64]) -> Option<f64> {
    if vals.is_empty() { return None; }
    let mut counts: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
    for &v in vals {
        let key = (v * 1000.0).round() as i64;
        let entry = counts.entry(key).or_insert((v, 0));
        entry.1 += 1;
    }
    counts.into_values().max_by_key(|&(_, count)| count).map(|(v, _)| v)
}

fn most_common_rt(specs: &[RapidTriggerSpec]) -> Option<RapidTriggerSpec> {
    if specs.is_empty() { return None; }
    let mut counts: BTreeMap<(bool, i64, i64), (RapidTriggerSpec, usize)> = BTreeMap::new();
    for &s in specs {
        let key = (s.enabled, (s.press_mm * 1000.0).round() as i64, (s.release_mm * 1000.0).round() as i64);
        let entry = counts.entry(key).or_insert((s, 0));
        entry.1 += 1;
    }
    counts.into_values().max_by_key(|&(_, count)| count).map(|(s, _)| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_key_colors_nominal() {
        let colors = vec![[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];
        // Ajuste un 2x2 vers un 2x3
        let fitted = fit_key_colors(&colors, 2, 3);
        assert_eq!(fitted.len(), 6);
        assert_eq!(fitted[0], [255, 0, 0]);
        assert_eq!(fitted[1], [0, 255, 0]);
        assert_eq!(fitted[2], [0, 0, 0]); // colonne ajoutée = noir
        assert_eq!(fitted[3], [0, 0, 255]);
        assert_eq!(fitted[4], [255, 255, 255]);
        assert_eq!(fitted[5], [0, 0, 0]); // colonne ajoutée = noir
    }

    #[test]
    fn profile_json_roundtrip() {
        let mut prof = default_profile("Gamer");
        prof.actuation.overrides_mm.insert("1,2".to_string(), 0.80);
        prof.rapid_trigger.overrides.insert(
            "1,2".to_string(),
            RapidTriggerSpec::new(true, 0.30, 0.30),
        );

        let json_str = serde_json::to_string_pretty(&prof).unwrap();
        let parsed: Profile = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed.name, "Gamer");
        assert_eq!(parsed.actuation_for(1, 2), 0.80);
        assert_eq!(parsed.actuation_for(0, 0), 1.50);
        assert_eq!(
            parsed.rapid_trigger_for(1, 2),
            RapidTriggerSpec::new(true, 0.30, 0.30)
        );
        assert_eq!(
            parsed.rapid_trigger_for(0, 0),
            RapidTriggerSpec::new(false, 0.50, 0.50)
        );
    }

    #[test]
    fn trigger_list_single_mirrors_fields() {
        let mut prof = default_profile("Apex");
        prof.triggers = Some(vec![TriggerSpec {
            exe: "r5apex.exe".to_string(),
            title: "".to_string(),
        }]);
        prof.prepare_for_save();

        assert_eq!(prof.trigger_exe.as_deref(), Some("r5apex.exe"));
        assert_eq!(prof.trigger_title.as_deref(), Some(""));
    }

    #[test]
    fn profile_v1_compatibility() {
        let v1_json = r#"{
  "profile_version": 1,
  "name": "LegacyProfile",
  "description": "Ancien format",
  "actuation": {
    "default_mm": 1.20,
    "overrides_mm": {
      "0,1": 0.50
    }
  },
  "rapid_trigger": {
    "default": {
      "enabled": false,
      "press_mm": 0.40,
      "release_mm": 0.40
    },
    "overrides": {}
  },
  "trigger_exe": "cs2.exe"
}"#;
        let prof: Profile = serde_json::from_str(v1_json).expect("parse v1 profile");
        assert_eq!(prof.profile_version, 1);
        assert_eq!(prof.name, "LegacyProfile");
        assert_eq!(prof.actuation_for(0, 1), 0.50);
        assert_eq!(prof.actuation_for(0, 0), 1.20);
        let triggers = prof.trigger_list();
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].exe, "cs2.exe");
    }

    #[test]
    fn trigger_list_multiple_entries() {
        let mut prof = default_profile("MultiGame");
        prof.triggers = Some(vec![
            TriggerSpec {
                exe: "game1.exe".to_string(),
                title: "Game 1".to_string(),
            },
            TriggerSpec {
                exe: "game2.exe".to_string(),
                title: "".to_string(),
            },
        ]);
        prof.prepare_for_save();

        assert_eq!(prof.trigger_exe, None);
        assert_eq!(prof.trigger_title, None);
        assert_eq!(prof.trigger_list().len(), 2);
    }
}
