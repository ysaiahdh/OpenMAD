//! Types structurés des fonctions avancées du clavier.
//!
//! Traduction de `src/mad68/features.py`. Dispositions récupérées dans les
//! classes de paquets DuckBread du configurateur web. Les décalages d'octets
//! ci-dessous sont relatifs au rapport de 32 octets (`raw[...]`), canal vendeur
//! déjà compté ; voir `research/notes/protocol.md` pour la provenance.
//!
//! Deux habillages différents sont en jeu, et c'est facile de se tromper :
//!
//! * canal vendeur, `[0x02|0x03, 0x96, sub, ...]` — dead band, drapeaux de
//!   fonctionnalité, game mode, bottom optimise, box light, DKS et touches
//!   avancées ;
//! * valeur custom/lighting de VIA, `[0x08|0x07, sub, ...]` **sans** octet
//!   0x96 — RGB par touche (0x45/0x42) et effet global (0x41).
//!
//! Ce module ne construit pas de trame : les paquets partent de `device.rs`, qui
//! connaît la commande hôte. Ici on ne décrit que les charges utiles qui ont une
//! sérialisation propre dans le Python (DKS, macros), plus les structures de
//! données et leur forme JSON, pour que `profile.rs` et le HUD n'aient pas à
//! redéfinir les mêmes champs.

use serde_json::{json, Value};
use thiserror::Error;

use crate::protocol::{AdvancedKeyMode, MacroAction};

// ---------------------------------------------------------------------------
// Dynamic keystroke (DKS)
// ---------------------------------------------------------------------------

/// Une entrée DKS contient quatre actions de quatre octets chacune.
pub const DKS_ACTION_COUNT: usize = 4;
pub const DKS_ACTION_SIZE: usize = 4;

/// Nombre de bits de déclenchement par action (le statut est un uint16 dont les
/// bits 0..=9 sont utilisés).
pub const DKS_STATUS_BITS: usize = 10;

/// Une action DKS : un masque de déclenchement sur 10 bits plus le keycode
/// qu'elle émet.
///
/// Les bits de statut sélectionnent les événements de course qui déclenchent
/// cette action. La sémantique exacte des bits n'est pas documentée par
/// l'application web non plus — elle fait un aller-retour du masque tel quel —
/// donc le masque est préservé et non interprété.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DksAction {
    /// Masque de déclenchement, uint16 big-endian sur le fil.
    pub status: u16,
    /// Octet de poids fort du keycode (`keyClass`).
    pub key_class: u8,
    /// Octet de poids faible du keycode (`keyId`).
    pub key_id: u8,
}

impl DksAction {
    /// Action vide, telle que le firmware la renvoie pour un emplacement libre.
    pub const EMPTY: DksAction = DksAction {
        status: 0,
        key_class: 0,
        key_id: 0,
    };

    /// Keycode 16 bits reconstitué : `(key_class << 8) | key_id`.
    pub const fn keycode(self) -> u16 {
        ((self.key_class as u16) << 8) | self.key_id as u16
    }

    /// Les 10 bits de statut, du poids faible au poids fort.
    pub fn bits(self) -> [u8; DKS_STATUS_BITS] {
        let mut out = [0u8; DKS_STATUS_BITS];
        for (i, bit) in out.iter_mut().enumerate() {
            *bit = ((self.status >> i) & 1) as u8;
        }
        out
    }

    /// Vrai si l'action ne déclenche rien et n'émet rien.
    pub const fn is_empty(self) -> bool {
        self.status == 0 && self.keycode() == 0
    }

    /// Sérialise sur 4 octets : `status` big-endian, puis `key_class`, `key_id`.
    pub fn to_bytes(self) -> [u8; DKS_ACTION_SIZE] {
        let [hi, lo] = self.status.to_be_bytes();
        [hi, lo, self.key_class, self.key_id]
    }

    /// Relit une action depuis une tranche d'au moins 4 octets.
    ///
    /// Le Python indexe directement `data[0:2]`, `data[2]`, `data[3]` : une
    /// tranche plus courte lève `IndexError`. Ici c'est une erreur typée.
    pub fn from_bytes(data: &[u8]) -> Result<Self, FeatureError> {
        if data.len() < DKS_ACTION_SIZE {
            return Err(FeatureError::ShortBuffer {
                expected: DKS_ACTION_SIZE,
                got: data.len(),
            });
        }
        Ok(Self {
            status: u16::from_be_bytes([data[0], data[1]]),
            key_class: data[2],
            key_id: data[3],
        })
    }

    /// Construit une action depuis un keycode 16 bits complet.
    pub const fn from_keycode(keycode: u16, status: u16) -> Self {
        Self {
            status,
            key_class: (keycode >> 8) as u8,
            key_id: keycode as u8,
        }
    }

    /// Forme JSON du Python : `{"status", "keycode", "bits"}`.
    pub fn to_json(self) -> Value {
        json!({
            "status": self.status,
            "keycode": self.keycode(),
            "bits": self.bits(),
        })
    }
}

/// Une entrée dynamic-keystroke complète : index plus quatre actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Dks {
    pub index: u8,
    pub actions: [DksAction; DKS_ACTION_COUNT],
}

impl Dks {
    /// Vrai si les quatre actions sont vides.
    pub fn is_empty(&self) -> bool {
        self.actions.iter().all(|action| action.is_empty())
    }

    /// Sérialise sur 16 octets, quatre actions consécutives.
    ///
    /// Le Python complète les actions manquantes par des actions vides avant
    /// d'écrire ; ici le tableau a toujours exactement quatre entrées, donc les
    /// deux formes produisent les mêmes 16 octets.
    pub fn to_bytes(self) -> [u8; DKS_ACTION_COUNT * DKS_ACTION_SIZE] {
        let mut out = [0u8; DKS_ACTION_COUNT * DKS_ACTION_SIZE];
        for (i, action) in self.actions.iter().enumerate() {
            let start = i * DKS_ACTION_SIZE;
            out[start..start + DKS_ACTION_SIZE].copy_from_slice(&action.to_bytes());
        }
        out
    }

    /// Relit une entrée depuis les 16 octets renvoyés par `read_dks`.
    pub fn from_bytes(index: u8, data: &[u8]) -> Result<Self, FeatureError> {
        let expected = DKS_ACTION_COUNT * DKS_ACTION_SIZE;
        if data.len() < expected {
            return Err(FeatureError::ShortBuffer {
                expected,
                got: data.len(),
            });
        }
        let mut actions = [DksAction::EMPTY; DKS_ACTION_COUNT];
        for (i, chunk) in data[..expected].chunks_exact(DKS_ACTION_SIZE).enumerate() {
            actions[i] = DksAction::from_bytes(chunk)?;
        }
        Ok(Self { index, actions })
    }

    /// Forme JSON du Python : `{"index", "actions"}`.
    pub fn to_json(self) -> Value {
        json!({
            "index": self.index,
            "actions": self.actions.iter().map(|a| a.to_json()).collect::<Vec<_>>(),
        })
    }
}

// ---------------------------------------------------------------------------
// Touches avancées : rapid snap / SOCD / OKS (vendeur rs, 0x20)
// ---------------------------------------------------------------------------

/// Une liaison rapid-snap / SOCD / OKS.
///
/// Les décalages de champs sont relatifs au début du paquet :
/// ```text
/// raw[5]  subField      raw[6]  index      raw[7]  mode      raw[8]  id
/// raw[9:11] rsApcLv u16 raw[11] gapcSw     raw[12] rtSw
/// raw[13] key1Row  raw[14] key1Col  raw[15] key2Row  raw[16] key2Col
/// raw[17] layer
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AdvancedKey {
    pub index: u8,
    pub mode: AdvancedKeyMode,
    pub id: u8,
    /// `rsApcLv`, uint16 big-endian sur le fil.
    pub rs_apc_lv: u16,
    pub gapc_sw: u8,
    pub rt_sw: u8,
    pub key1_row: u8,
    pub key1_col: u8,
    pub key2_row: u8,
    pub key2_col: u8,
    pub layer: u8,
}

impl AdvancedKey {
    /// Vrai si la touche porte réellement une liaison.
    pub fn is_active(&self) -> bool {
        self.mode != AdvancedKeyMode::None
    }

    /// Forme JSON du Python, `mode` rendu par son nom (`"SOCD"`, `"OKS"`, ...).
    pub fn to_json(&self) -> Value {
        json!({
            "index": self.index,
            "mode": mode_name(self.mode),
            "id": self.id,
            "rs_apc_lv": self.rs_apc_lv,
            "gapc_sw": self.gapc_sw,
            "rt_sw": self.rt_sw,
            "key1_row": self.key1_row,
            "key1_col": self.key1_col,
            "key2_row": self.key2_row,
            "key2_col": self.key2_col,
            "layer": self.layer,
        })
    }
}

/// Nom du mode, identique à `AdvancedKeyMode.name` en Python.
///
/// Le match exhaustif est volontaire : ajouter un mode dans `protocol.rs` casse
/// la compilation ici plutôt que de produire silencieusement un nom faux.
const fn mode_name(mode: AdvancedKeyMode) -> &'static str {
    match mode {
        AdvancedKeyMode::None => "NONE",
        AdvancedKeyMode::Rs => "RS",
        AdvancedKeyMode::Socd => "SOCD",
        AdvancedKeyMode::SocdKey1 => "SOCD_KEY1",
        AdvancedKeyMode::SocdKey2 => "SOCD_KEY2",
        AdvancedKeyMode::SocdBalance => "SOCD_BALANCE",
        AdvancedKeyMode::Oks => "OKS",
    }
}

// ---------------------------------------------------------------------------
// Éclairage
// ---------------------------------------------------------------------------

/// Éclairage global : effet, vitesse, couleur et luminosité.
///
/// Habillage `id_lighting_get/set_value` avec le sous-ID à l'octet 1 ; il n'y a
/// **pas** d'octet de canal 0x96 ici :
/// ```text
/// raw[1] 0x41   raw[2] effect   raw[4] speed
/// raw[5:8] r,g,b               raw[8] brightness
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LightInfo {
    pub effect: u8,
    pub speed: u8,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub brightness: u8,
}

impl LightInfo {
    /// Couleur au format `#rrggbb`, comme `LightInfo.color_hex` en Python.
    pub fn color_hex(&self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Forme JSON du Python : les six champs plus `color_hex`.
    pub fn to_json(&self) -> Value {
        json!({
            "effect": self.effect,
            "speed": self.speed,
            "r": self.r,
            "g": self.g,
            "b": self.b,
            "brightness": self.brightness,
            "color_hex": self.color_hex(),
        })
    }
}

/// Éclairage latéral / caisson :
/// `raw[3] mode`, `raw[4] colorful`, `raw[5] brightness`, `raw[6] speed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BoxLight {
    pub mode: u8,
    pub colorful: u8,
    pub brightness: u8,
    pub speed: u8,
}

impl BoxLight {
    pub fn to_json(&self) -> Value {
        json!({
            "mode": self.mode,
            "colorful": self.colorful,
            "brightness": self.brightness,
            "speed": self.speed,
        })
    }
}

// ---------------------------------------------------------------------------
// Réglages scalaires
// ---------------------------------------------------------------------------

/// Hold/Click (MT) : un keycode au tap, un autre au maintien.
///
/// Habillé sur le préfixe vial de VIA plutôt que sur le canal 0x96 :
/// ```text
/// raw[1] entryOp 0x0D   raw[2] get 0x01 / set 0x02   raw[3] index
/// raw[4:6] tap keycode  raw[6:8] hold keycode        raw[8:10] timer ms
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TapDance {
    pub index: u8,
    pub tap: u16,
    pub hold: u16,
    pub timer_ms: u16,
}

impl Default for TapDance {
    /// La dataclass Python déclare `timer_ms: int = 200` : un `Default` dérivé
    /// mettrait 0 et changerait le comportement d'un TapDance créé par défaut.
    fn default() -> Self {
        Self {
            index: 0,
            tap: 0,
            hold: 0,
            timer_ms: 200,
        }
    }
}

impl TapDance {
    pub fn is_active(&self) -> bool {
        self.tap != 0 || self.hold != 0
    }

    pub fn to_json(&self) -> Value {
        json!({
            "index": self.index,
            "tap": self.tap,
            "hold": self.hold,
            "timer_ms": self.timer_ms,
        })
    }
}

/// Dead band du capteur en millimètres : `raw[3:5]` haut, `raw[5:7]` bas
/// (uint16 en centièmes de millimètre).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DeadBand {
    pub top_mm: f64,
    pub bottom_mm: f64,
}

impl DeadBand {
    pub fn to_json(&self) -> Value {
        json!({
            "top_mm": self.top_mm,
            "bottom_mm": self.bottom_mm,
        })
    }
}

/// Drapeaux de fonctionnalité :
/// `raw[3] rgbArea`, `[4] wasd`, `[5] mac`, `[6] winLock`, `[7] nkro`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyboardFeature {
    pub rgb_area: u8,
    pub wasd_switch: u8,
    pub mac_switch: u8,
    pub win_lock: u8,
    pub nkro_switch: u8,
}

impl KeyboardFeature {
    pub fn to_json(&self) -> Value {
        json!({
            "rgb_area": self.rgb_area,
            "wasd_switch": self.wasd_switch,
            "mac_switch": self.mac_switch,
            "win_lock": self.win_lock,
            "nkro_switch": self.nkro_switch,
        })
    }
}

// ---------------------------------------------------------------------------
// Macros
// ---------------------------------------------------------------------------

/// VIA marque une action non littérale avec cet octet d'échappement dans le
/// tampon de macros.
pub const MACRO_ESCAPE: u8 = 0x01;

/// Les macros sont séparées par un NUL dans le tampon partagé.
pub const MACRO_TERMINATOR: u8 = 0x00;

/// Délai maximal représentable par l'encodage VIA sur deux octets.
///
/// `_encode_delay` en Python borne à `255 * 255 + 254` = 65279, mais l'octet de
/// poids fort vaut alors `(ms // 255) + 1` = 256, hors d'un octet : `bytes()`
/// lève `ValueError`. La vraie borne est donc 65024, et au-delà on renvoie une
/// erreur explicite plutôt que de reproduire la panique.
pub const MACRO_DELAY_MAX: u16 = 255 * 255 - 1; // 65024

/// Nature d'une étape de macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MacroStepKind {
    /// Texte littéral, un octet ASCII par caractère.
    #[default]
    Text,
    /// Appui bref d'un keycode.
    Tap,
    /// Enfoncement d'un keycode.
    Down,
    /// Relâchement d'un keycode.
    Up,
    /// Pause en millisecondes.
    Delay,
}

impl MacroStepKind {
    /// Identifiant textuel, identique aux chaînes du Python.
    pub const fn as_str(self) -> &'static str {
        match self {
            MacroStepKind::Text => "text",
            MacroStepKind::Tap => "tap",
            MacroStepKind::Down => "down",
            MacroStepKind::Up => "up",
            MacroStepKind::Delay => "delay",
        }
    }
}

/// Une étape de macro décodée.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MacroStep {
    pub kind: MacroStepKind,
    /// Rempli pour `Text` uniquement.
    pub text: String,
    /// Rempli pour `Tap`/`Down`/`Up` uniquement.
    pub keycode: u16,
    /// Rempli pour `Delay` uniquement.
    pub delay_ms: u16,
}

impl MacroStep {
    /// Étape de texte littéral.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: MacroStepKind::Text,
            text: text.into(),
            ..Self::default()
        }
    }

    /// Étape de pause.
    pub fn delay(delay_ms: u16) -> Self {
        Self {
            kind: MacroStepKind::Delay,
            delay_ms,
            ..Self::default()
        }
    }

    /// Étape d'appui, d'enfoncement ou de relâchement d'un keycode.
    pub fn press(kind: MacroStepKind, keycode: u16) -> Self {
        Self {
            kind,
            keycode,
            ..Self::default()
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind.as_str(),
            "text": self.text,
            "keycode": self.keycode,
            "delay_ms": self.delay_ms,
        })
    }
}

/// Erreurs de codec des charges utiles de `features.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FeatureError {
    #[error("buffer too short: {got} bytes, expected {expected}")]
    ShortBuffer { expected: usize, got: usize },

    #[error("macro text cannot contain byte {codepoint:#04x}; use a tap step")]
    MacroTextByte { codepoint: u32 },

    #[error("macros need {needed} bytes, buffer is {size}")]
    MacroBufferOverflow { needed: usize, size: usize },

    #[error("unknown macro step kind {kind:?}")]
    UnknownMacroStep { kind: MacroStepKind },

    #[error("delay {ms} ms does not fit VIA's two-byte encoding (max {max})")]
    DelayOutOfRange { ms: u16, max: u16 },
}

/// Décode un délai VIA : chaque octet vaut 1..=255 pour ne jamais produire de
/// NUL ni d'échappement. `(b1 - 1) + (b2 - 1) * 255`.
///
/// Un couple corrompu `(0, 0)` donnerait -1 en Python ; ici il est ramené à 0
/// pour rester dans le type, ce cas ne pouvant pas sortir d'un tampon valide.
pub fn decode_delay(b1: u8, b2: u8) -> u16 {
    let value = i32::from(b1) - 1 + (i32::from(b2) - 1) * 255;
    value.clamp(0, i32::from(u16::MAX)) as u16
}

/// Encode un délai en deux octets 1..=255.
///
/// `(ms % 255) + 1` puis `(ms // 255) + 1` : le modulo plutôt qu'un découpage en
/// base 256 parce que 0 est réservé au terminateur et 1 à l'échappement.
pub fn encode_delay(ms: u16) -> Result<[u8; 2], FeatureError> {
    if ms > MACRO_DELAY_MAX {
        return Err(FeatureError::DelayOutOfRange {
            ms,
            max: MACRO_DELAY_MAX,
        });
    }
    Ok([(ms % 255) as u8 + 1, (ms / 255) as u8 + 1])
}

/// Décode les octets d'une macro (sans terminateur) en étapes.
///
/// Le parcours reproduit exactement celui du Python, y compris ses subtilités :
/// * un octet différent de `MACRO_ESCAPE` est du texte, un caractère par octet
///   (correspondance Latin-1 de `chr()`, pas UTF-8) ;
/// * un échappement suivi d'un opcode d'appui à un octet exige `i + 2 < len` ;
/// * les extensions Vial portent un keycode 16 bits **little-endian**
///   (`data[i+2] | data[i+3] << 8`), contrairement au reste du protocole ;
/// * un opcode inconnu, ou tronqué en fin de tampon, avance de 2 octets.
pub fn decode_macro(data: &[u8]) -> Vec<MacroStep> {
    let mut steps: Vec<MacroStep> = Vec::new();
    let mut text = String::new();
    let mut i = 0usize;

    while i < data.len() {
        let byte = data[i];
        if byte != MACRO_ESCAPE {
            // `chr(b)` sur un octet : Latin-1, jamais d'erreur d'encodage.
            text.push(byte as char);
            i += 1;
            continue;
        }

        // Le texte en cours se termine dès qu'une action commence.
        if !text.is_empty() {
            steps.push(MacroStep::text(std::mem::take(&mut text)));
        }

        let Some(op) = data.get(i + 1).copied() else {
            // Échappement orphelin en fin de tampon : rien à décoder.
            break;
        };

        let Some(action) = MacroAction::from_id(op) else {
            i += 2;
            continue;
        };

        match action {
            MacroAction::SsTapCode if i + 2 < data.len() => {
                steps.push(MacroStep::press(MacroStepKind::Tap, u16::from(data[i + 2])));
                i += 3;
            }
            MacroAction::SsDownCode if i + 2 < data.len() => {
                steps.push(MacroStep::press(MacroStepKind::Down, u16::from(data[i + 2])));
                i += 3;
            }
            MacroAction::SsUpCode if i + 2 < data.len() => {
                steps.push(MacroStep::press(MacroStepKind::Up, u16::from(data[i + 2])));
                i += 3;
            }
            MacroAction::VialMacroExtTap if i + 3 < data.len() => {
                steps.push(MacroStep::press(MacroStepKind::Tap, ext_keycode(data, i)));
                i += 4;
            }
            MacroAction::VialMacroExtDown if i + 3 < data.len() => {
                steps.push(MacroStep::press(MacroStepKind::Down, ext_keycode(data, i)));
                i += 4;
            }
            MacroAction::VialMacroExtUp if i + 3 < data.len() => {
                steps.push(MacroStep::press(MacroStepKind::Up, ext_keycode(data, i)));
                i += 4;
            }
            MacroAction::SsDelayCode if i + 3 < data.len() => {
                steps.push(MacroStep::delay(decode_delay(data[i + 2], data[i + 3])));
                i += 4;
            }
            // Opcode tronqué : on saute l'échappement et l'opcode, comme Python.
            _ => i += 2,
        }
    }

    if !text.is_empty() {
        steps.push(MacroStep::text(text));
    }
    steps
}

/// Keycode 16 bits d'une extension Vial : octet de poids faible d'abord.
fn ext_keycode(data: &[u8], i: usize) -> u16 {
    u16::from(data[i + 2]) | (u16::from(data[i + 3]) << 8)
}

/// Encode des étapes en les octets d'une macro (sans terminateur).
///
/// Les keycodes au-delà de 0xFF passent obligatoirement par les opcodes
/// d'extension Vial (deux octets, little-endian) ; en dessous, l'opcode court
/// historique suffit, exactement comme le choisit le Python.
pub fn encode_macro(steps: &[MacroStep]) -> Result<Vec<u8>, FeatureError> {
    let mut out = Vec::new();
    for step in steps {
        match step.kind {
            MacroStepKind::Text => {
                for ch in step.text.chars() {
                    let codepoint = ch as u32;
                    // L'échappement et le NUL casserait le flux, et au-delà de
                    // l'ASCII on ne sait pas quel octet le firmware attendrait.
                    if codepoint == u32::from(MACRO_ESCAPE)
                        || codepoint == u32::from(MACRO_TERMINATOR)
                        || codepoint > 0x7F
                    {
                        return Err(FeatureError::MacroTextByte { codepoint });
                    }
                    out.push(codepoint as u8);
                }
            }
            MacroStepKind::Delay => {
                out.push(MACRO_ESCAPE);
                out.push(MacroAction::SsDelayCode.id());
                out.extend_from_slice(&encode_delay(step.delay_ms)?);
            }
            MacroStepKind::Tap | MacroStepKind::Down | MacroStepKind::Up => {
                if step.keycode > 0xFF {
                    let op = match step.kind {
                        MacroStepKind::Tap => MacroAction::VialMacroExtTap,
                        MacroStepKind::Down => MacroAction::VialMacroExtDown,
                        _ => MacroAction::VialMacroExtUp,
                    };
                    out.push(MACRO_ESCAPE);
                    out.push(op.id());
                    out.push(step.keycode as u8);
                    out.push((step.keycode >> 8) as u8);
                } else {
                    let op = match step.kind {
                        MacroStepKind::Tap => MacroAction::SsTapCode,
                        MacroStepKind::Down => MacroAction::SsDownCode,
                        _ => MacroAction::SsUpCode,
                    };
                    out.push(MACRO_ESCAPE);
                    out.push(op.id());
                    out.push(step.keycode as u8);
                }
            }
        }
    }
    Ok(out)
}

/// Découpe le tampon de macros partagé en `count` macros séparées par des NUL.
///
/// Comme le slicing Python : si le tampon contient moins de `count`
/// terminateurs, le dernier morceau va jusqu'au bout puis les emplacements
/// restants sont vides.
pub fn split_macro_buffer(buffer: &[u8], count: usize) -> Vec<&[u8]> {
    let mut macros: Vec<&[u8]> = Vec::with_capacity(count);
    let mut start = 0usize;

    for _ in 0..count {
        let tail = buffer.get(start..).unwrap_or(&[]);
        match tail.iter().position(|&b| b == MACRO_TERMINATOR) {
            Some(position) => {
                let end = start + position;
                macros.push(&buffer[start..end]);
                start = end + 1;
            }
            None => {
                macros.push(tail);
                break;
            }
        }
    }

    while macros.len() < count {
        macros.push(&[]);
    }
    macros
}

/// Reconditionne des macros dans un tampon de taille fixe séparé par des NUL.
///
/// Chaque macro est suivie de son terminateur, y compris la dernière, puis le
/// reste du tampon est rempli de zéros — c'est ce que fait le Python, et donc ce
/// que le firmware relit.
pub fn join_macro_buffer<M: AsRef<[u8]>>(
    macros: &[M],
    size: usize,
) -> Result<Vec<u8>, FeatureError> {
    let mut out = Vec::new();
    for macro_bytes in macros {
        out.extend_from_slice(macro_bytes.as_ref());
        out.push(MACRO_TERMINATOR);
    }
    if out.len() > size {
        return Err(FeatureError::MacroBufferOverflow {
            needed: out.len(),
            size,
        });
    }
    out.resize(size, 0);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// Les valeurs de référence ont été produites par `src/mad68/features.py` :
// `DksAction.to_bytes`, `Dks.to_bytes`, `encode_macro`, `decode_macro`,
// `split_macro_buffer`, `join_macro_buffer` et `_encode_delay`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dks_action_serialises_status_big_endian() {
        // f.DksAction(status=0b1010000001, key_class=0x52, key_id=0x21).to_bytes()
        let action = DksAction {
            status: 0b1010_0000_01,
            key_class: 0x52,
            key_id: 0x21,
        };
        assert_eq!(action.status, 641);
        assert_eq!(action.keycode(), 0x5221);
        assert_eq!(action.to_bytes(), [0x02, 0x81, 0x52, 0x21]);
        assert_eq!(
            DksAction::from_bytes(&action.to_bytes()),
            Ok(action)
        );
    }

    #[test]
    fn dks_entry_pads_missing_actions_with_zeroes() {
        // f.Dks(index=7, actions=(DksAction.from_keycode(0x5221, 1),
        //                         DksAction.from_keycode(0x04, 0x200))).to_bytes()
        let dks = Dks {
            index: 7,
            actions: [
                DksAction::from_keycode(0x5221, 1),
                DksAction::from_keycode(0x04, 0x200),
                DksAction::EMPTY,
                DksAction::EMPTY,
            ],
        };
        assert_eq!(
            dks.to_bytes(),
            [0x00, 0x01, 0x52, 0x21, 0x02, 0x00, 0x00, 0x04, 0, 0, 0, 0, 0, 0, 0, 0]
        );

        let parsed = Dks::from_bytes(3, &dks.to_bytes()).expect("16 octets");
        assert_eq!(parsed.actions, dks.actions);
        assert!(!parsed.is_empty());
        assert!(Dks::default().is_empty());
    }

    #[test]
    fn dks_action_bits_and_json_match_python() {
        // [a.to_json() for a in f.Dks.from_bytes(3, bytes.fromhex(...)).actions]
        let action = DksAction::from_keycode(0x5221, 1);
        assert_eq!(action.bits(), [1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            action.to_json(),
            json!({"status": 1, "keycode": 21025, "bits": [1, 0, 0, 0, 0, 0, 0, 0, 0, 0]})
        );

        // Statut 0x200 -> bit 9, le dernier bit utilisé.
        let high = DksAction::from_keycode(0x0004, 0x200);
        assert_eq!(high.bits(), [0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(high.to_json()["keycode"], json!(4));
    }

    #[test]
    fn dks_short_buffers_are_errors_not_panics() {
        assert_eq!(
            DksAction::from_bytes(&[0x00, 0x01, 0x52]),
            Err(FeatureError::ShortBuffer {
                expected: 4,
                got: 3
            })
        );
        assert!(matches!(
            Dks::from_bytes(0, &[0u8; 15]),
            Err(FeatureError::ShortBuffer {
                expected: 16,
                got: 15
            })
        ));
        // Les octets excédentaires sont ignorés, comme le slicing Python.
        assert!(Dks::from_bytes(0, &[0u8; 20]).is_ok());
    }

    #[test]
    fn macro_encoding_matches_python_byte_for_byte() {
        // f.encode_macro([...]).hex() avec :
        //   text "hello", delay 1234, tap 0x04, down 0x5221, up 0xFF
        let steps = [
            MacroStep::text("hello"),
            MacroStep::delay(1234),
            MacroStep::press(MacroStepKind::Tap, 0x04),
            MacroStep::press(MacroStepKind::Down, 0x5221),
            MacroStep::press(MacroStepKind::Up, 0xFF),
        ];
        let encoded = encode_macro(&steps).expect("encodage");
        assert_eq!(
            encoded,
            [
                0x68, 0x65, 0x6c, 0x6c, 0x6f, // "hello", littéral
                0x01, 0x04, 0xd7, 0x05, // échappement + delay 1234
                0x01, 0x01, 0x04, // tap court sur 0x04
                0x01, 0x06, 0x21, 0x52, // extension Vial, keycode little-endian
                0x01, 0x03, 0xff, // up court sur 0xFF
            ]
        );

        // Le décodage doit rendre exactement les mêmes étapes.
        assert_eq!(decode_macro(&encoded), steps);
    }

    #[test]
    fn macro_decoding_matches_python_json() {
        // [s.to_json() for s in f.decode_macro(bytes.fromhex(...))]
        let encoded = [
            0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x01, 0x04, 0xd7, 0x05, 0x01, 0x01, 0x04, 0x01, 0x06,
            0x21, 0x52, 0x01, 0x03, 0xff,
        ];
        let decoded = decode_macro(&encoded);
        let json: Vec<Value> = decoded.iter().map(|s| s.to_json()).collect();
        assert_eq!(
            json,
            vec![
                json!({"kind": "text", "text": "hello", "keycode": 0, "delay_ms": 0}),
                json!({"kind": "delay", "text": "", "keycode": 0, "delay_ms": 1234}),
                json!({"kind": "tap", "text": "", "keycode": 4, "delay_ms": 0}),
                json!({"kind": "down", "text": "", "keycode": 21025, "delay_ms": 0}),
                json!({"kind": "up", "text": "", "keycode": 255, "delay_ms": 0}),
            ]
        );
    }

    #[test]
    fn macro_delay_encoding_matches_python() {
        // {ms: f._encode_delay(ms).hex() for ms in [...]}
        for (ms, bytes) in [
            (0u16, [0x01, 0x01]),
            (1, [0x02, 0x01]),
            (254, [0xff, 0x01]),
            (255, [0x01, 0x02]),
            (256, [0x02, 0x02]),
            (1234, [0xd7, 0x05]),
            (MACRO_DELAY_MAX, [0xff, 0xff]),
        ] {
            assert_eq!(encode_delay(ms).expect("délai valide"), bytes, "{ms} ms");
            assert_eq!(decode_delay(bytes[0], bytes[1]), ms, "{ms} ms");
        }

        // Au-delà de 65024 le Python lève ValueError (son clamp à 65279 laisse
        // passer un octet de poids fort à 256) : ici c'est une erreur typée.
        for ms in [65025u16, 65279, u16::MAX] {
            assert_eq!(
                encode_delay(ms),
                Err(FeatureError::DelayOutOfRange {
                    ms,
                    max: MACRO_DELAY_MAX
                })
            );
        }
    }

    #[test]
    fn macro_text_rejects_escape_nul_and_non_ascii() {
        // Python : code in (0x01, 0x00) or code > 0x7F -> ValueError.
        assert_eq!(
            encode_macro(&[MacroStep::text("a\u{1}b")]),
            Err(FeatureError::MacroTextByte { codepoint: 1 })
        );
        assert_eq!(
            encode_macro(&[MacroStep::text("a\u{0}b")]),
            Err(FeatureError::MacroTextByte { codepoint: 0 })
        );
        assert_eq!(
            encode_macro(&[MacroStep::text("é")]),
            Err(FeatureError::MacroTextByte { codepoint: 0xE9 })
        );
        // ASCII imprimable : accepté tel quel.
        assert_eq!(
            encode_macro(&[MacroStep::text("AB")]).expect("texte ASCII"),
            vec![0x41, 0x42]
        );
    }

    #[test]
    fn macro_decoding_skips_unknown_and_truncated_opcodes() {
        // Opcode inconnu (0x7F) : échappement + opcode sautés, le reste suit.
        assert_eq!(
            decode_macro(&[b'x', 0x01, 0x7F, b'y', 0x01, 0x01]),
            vec![
                MacroStep::text("x"),
                MacroStep::text("y"),
                // Tap sans son octet de keycode : rien n'est émis pour ce tap.
            ]
        );
        // Échappement orphelin en fin de tampon : le texte déjà lu est rendu.
        assert_eq!(
            decode_macro(&[b'z', 0x01]),
            vec![MacroStep::text("z")]
        );
        // Tampon vide.
        assert!(decode_macro(&[]).is_empty());
    }

    #[test]
    fn macro_buffer_split_matches_python() {
        // [m.hex() for m in f.split_macro_buffer(b"ab\x00cd\x00", 4)]
        let buffer = b"ab\x00cd\x00";
        let parts: Vec<&[u8]> = split_macro_buffer(buffer, 4);
        assert_eq!(parts, vec![&b"ab"[..], &b"cd"[..], &b""[..], &b""[..]]);

        // Tampon sans assez de terminateurs : le dernier morceau va au bout.
        assert_eq!(
            split_macro_buffer(b"ab\x00cd", 3),
            vec![&b"ab"[..], &b"cd"[..], &b""[..]]
        );
        assert_eq!(split_macro_buffer(b"", 2), vec![&b""[..], &b""[..]]);
    }

    #[test]
    fn macro_buffer_join_matches_python() {
        // f.join_macro_buffer([b"ab", b"cd"], 8).hex()
        let joined = join_macro_buffer(&[b"ab".as_slice(), b"cd".as_slice()], 8).expect("jointure");
        assert_eq!(joined, vec![0x61, 0x62, 0x00, 0x63, 0x64, 0x00, 0x00, 0x00]);

        // Dépassement : Python lève ValueError(f"macros need {n} bytes, ...").
        let error = join_macro_buffer(&[b"abc".as_slice()], 3).expect_err("trop long");
        assert_eq!(
            error,
            FeatureError::MacroBufferOverflow {
                needed: 4,
                size: 3
            }
        );
        assert_eq!(error.to_string(), "macros need 4 bytes, buffer is 3");

        // Aller-retour avec split : le terminateur final est inclus.
        let parts = split_macro_buffer(&joined, 2);
        assert_eq!(parts, vec![&b"ab"[..], &b"cd"[..]]);
    }

    #[test]
    fn light_and_box_light_json_match_python_asdict() {
        let light = LightInfo {
            effect: 6,
            speed: 128,
            r: 255,
            g: 128,
            b: 0,
            brightness: 210,
        };
        assert_eq!(light.color_hex(), "#ff8000");
        assert_eq!(light.to_json()["color_hex"], json!("#ff8000"));
        assert_eq!(light.to_json()["brightness"], json!(210));
        assert_eq!(LightInfo::default().color_hex(), "#000000");

        let box_light = BoxLight {
            mode: 3,
            colorful: 1,
            brightness: 200,
            speed: 42,
        };
        assert_eq!(
            box_light.to_json(),
            json!({"mode": 3, "colorful": 1, "brightness": 200, "speed": 42})
        );
    }

    #[test]
    fn advanced_key_json_uses_the_mode_name() {
        let key = AdvancedKey {
            index: 4,
            mode: AdvancedKeyMode::Socd,
            id: 0x1A,
            rs_apc_lv: 120,
            gapc_sw: 1,
            rt_sw: 0,
            key1_row: 4,
            key1_col: 10,
            key2_row: 4,
            key2_col: 11,
            layer: 1,
        };
        assert!(key.is_active());
        assert_eq!(key.to_json()["mode"], json!("SOCD"));
        assert_eq!(AdvancedKey::default().to_json()["mode"], json!("NONE"));
        assert!(!AdvancedKey::default().is_active());

        // Tous les modes ont un nom, et il correspond à l'énumération Python.
        for (mode, name) in [
            (AdvancedKeyMode::None, "NONE"),
            (AdvancedKeyMode::Rs, "RS"),
            (AdvancedKeyMode::Socd, "SOCD"),
            (AdvancedKeyMode::SocdKey1, "SOCD_KEY1"),
            (AdvancedKeyMode::SocdKey2, "SOCD_KEY2"),
            (AdvancedKeyMode::SocdBalance, "SOCD_BALANCE"),
            (AdvancedKeyMode::Oks, "OKS"),
        ] {
            assert_eq!(mode_name(mode), name);
        }
    }

    #[test]
    fn tap_dance_default_timer_is_200ms() {
        let tap_dance = TapDance::default();
        assert_eq!(tap_dance.timer_ms, 200);
        assert!(!tap_dance.is_active());
        assert!(TapDance {
            tap: 0x04,
            ..TapDance::default()
        }
        .is_active());
        assert_eq!(tap_dance.to_json()["timer_ms"], json!(200));
    }

    #[test]
    fn scalar_settings_json_match_python_asdict() {
        let band = DeadBand {
            top_mm: 0.5,
            bottom_mm: 0.25,
        };
        assert_eq!(band.to_json(), json!({"top_mm": 0.5, "bottom_mm": 0.25}));

        let feature = KeyboardFeature {
            rgb_area: 1,
            wasd_switch: 0,
            mac_switch: 1,
            win_lock: 0,
            nkro_switch: 1,
        };
        assert_eq!(
            feature.to_json(),
            json!({"rgb_area": 1, "wasd_switch": 0, "mac_switch": 1,
                   "win_lock": 0, "nkro_switch": 1})
        );
    }
}
