//! Tous les claviers du contrôleur DuckBread, et ce qui les distingue.
//!
//! Traduction de `src/mad68/devices.py`. Le pilote a été écrit contre une carte,
//! la MAD68 HE, mais le protocole n'est pas celui de cette carte : c'est celui du
//! contrôleur. Le configurateur web du vendeur associe chaque clavier à une
//! classe de contrôleur et à un petit objet de configuration par carte, et 25 de
//! ses entrées nomment le même contrôleur `DuckBread` que ce pilote a
//! rétro-ingénieré. Les dispositions de paquets, les sous-commandes vendeur et la
//! sémantique de flash sont communes ; seule la géométrie de matrice change.
//!
//! C'est ce que contient cette table, issue du registre du vendeur (voir
//! `research/notes/protocol.md`), pas d'une supposition :
//!
//! ```text
//! { controller: DuckBread, digital: Qc, filters: [ { vendorId: 0x373b,
//!   productId: 0x1058, custom: { layer: 4, row: 5, col: 15, ... } } ] }
//! ```
//!
//! Deux choses sont à noter. Toutes les cartes font cinq lignes et quatre
//! couches sans exception, donc seul le nombre de colonnes varie — 14 pour les
//! 60 %, 15 pour les 65 %/68, et 6 pour le seul macropad. Et une carte listée ici
//! signifie que le vendeur la livre sur ce contrôleur, pas que ce pilote a été
//! exécuté dessus : `verified` est le relevé honnête de celles réellement
//! exercées sur matériel.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::Value;
use thiserror::Error;

/// Le VID du contrôleur, partagé par toutes les cartes de la table.
pub use crate::protocol::VENDOR_ID;

/// Classe physique approximative, utilisée pour choisir la disposition que le
/// HUD dessine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormFactor {
    /// Jusqu'à 8 colonnes : le macropad.
    Macropad,
    /// 9 à 14 colonnes : les 60 %.
    Sixty,
    /// Au-delà : les 65 % et 68 touches.
    SixtyFive,
}

impl FormFactor {
    /// Identifiant textuel, identique aux chaînes renvoyées par le Python.
    pub const fn as_str(self) -> &'static str {
        match self {
            FormFactor::Macropad => "macropad",
            FormFactor::Sixty => "60",
            FormFactor::SixtyFive => "65",
        }
    }
}

/// Un clavier : comment le trouver, et quelle est la taille de sa matrice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardSpec {
    pub product_id: u16,
    pub name: Cow<'static, str>,
    /// Lignes de matrice : 5 sur toutes les cartes connues.
    pub rows: u8,
    /// Colonnes de matrice : 14, 15 ou 6. C'est **ce** nombre qui sert de pas
    /// pour tous les tampons row-major sur le fil.
    pub cols: u8,
    pub layers: u8,
    /// Product ID du bootloader (IAP), différent de celui de l'application.
    pub bootloader_id: u16,
    /// Vrai uniquement pour le matériel sur lequel ce pilote a réellement été
    /// exécuté. Tout `false` est supporté au titre d'un contrôleur partagé,
    /// ce qui est un bon pari mais pas un fait testé : l'interface le dit, et
    /// les écritures restent bloquées tant que la version de protocole de la
    /// carte ne le confirme pas.
    pub verified: bool,
}

/// Constructeur `const` pour la table : garde la déclaration de chaque carte sur
/// une ligne lisible et rend `BOARDS` utilisable en `static`.
const fn board(
    product_id: u16,
    name: &'static str,
    rows: u8,
    cols: u8,
    layers: u8,
    bootloader_id: u16,
    verified: bool,
) -> BoardSpec {
    BoardSpec {
        product_id,
        name: Cow::Borrowed(name),
        rows,
        cols,
        layers,
        bootloader_id,
        verified,
    }
}

impl BoardSpec {
    /// Nombre de touches physiques de la matrice.
    pub const fn total_keys(&self) -> u16 {
        self.rows as u16 * self.cols as u16
    }

    /// Classe physique, déduite du nombre de colonnes.
    pub const fn form_factor(&self) -> FormFactor {
        if self.cols <= 8 {
            FormFactor::Macropad
        } else if self.cols <= 14 {
            FormFactor::Sixty
        } else {
            FormFactor::SixtyFive
        }
    }
}

/// Ordonnée grossièrement comme le vendeur les liste. Les noms viennent des
/// classes de configuration de son bundle (`_DuckBreadMad68HERgb` et consorts),
/// donc ils correspondent à ce que l'outil officiel appelle chaque carte.
pub static BOARDS: &[BoardSpec] = &[
    // -- famille 60 %, 5x14 ------------------------------------------------
    // 0x1053 est la deuxième carte confirmée sur matériel, et la première 5x14.
    // Deux rapports de diagnostic d'un propriétaire, tous deux en protocole 9 :
    // le premier a relu 70 entrées actionnement/RT/couleur et un masque de
    // calibration à 61 switchs — d'où la disposition 60 % de `hud.rs` — et le
    // second a lancé la sonde d'écriture, qui a réglé la touche 0 à 1,50 mm,
    // relu 1,50 mm et restauré les 1,00 mm d'origine. Chemins de lecture et
    // d'écriture exercés tous les deux, donc vérifiée au sens du drapeau.
    board(0x1053, "MAD60 HE", 5, 14, 4, 0x201F, true),
    board(0x1054, "MAD60 HE RGB", 5, 14, 4, 0x2020, false),
    board(0x1055, "MAD60 HE Ultra", 5, 14, 4, 0x2021, false),
    board(0x1056, "MAD60 HE Limited", 5, 14, 4, 0x2022, false),
    board(0x10AD, "MAD60 Pro", 5, 14, 4, 0x2053, false),
    board(0x1120, "MAD60 HE V2", 5, 14, 4, 0x210B, false),
    board(0x10F6, "MAD Light 60 HE", 5, 14, 4, 0x2085, false),
    board(0x110B, "MAD63 HE", 5, 14, 4, 0x2102, false),
    board(0x112A, "Z60", 5, 14, 4, 0x2115, false),
    // -- famille 65 % / 68, 5x15 -------------------------------------------
    // 0x1057 ne porte aucun nom dans le bundle vendeur. Il précède immédiatement
    // le modèle RGB avec une géométrie identique, donc c'est presque sûrement la
    // MAD68 HE de base ; étiquetée comme telle mais laissée non vérifiée.
    board(0x1057, "MAD68 HE", 5, 15, 4, 0x2023, false),
    board(0x1058, "MAD68 HE RGB", 5, 15, 4, 0x2024, true),
    board(0x1059, "MAD68 HE Ultra", 5, 15, 4, 0x2025, false),
    board(0x105A, "MAD68 HE Limited", 5, 15, 4, 0x2026, false),
    board(0x10A7, "MAD68 R", 5, 15, 4, 0x204E, false),
    board(0x10D6, "MAD68 Pro", 5, 15, 4, 0x2075, false),
    board(0x1123, "MAD68 HE V2", 5, 15, 4, 0x210E, false),
    board(0x1144, "MAD Light 68 HE", 5, 15, 4, 0x212D, false),
    board(0x1136, "Z68", 5, 15, 4, 0x2121, false),
    board(0x1139, "MAG68", 5, 15, 4, 0x2124, false),
    board(0x10A3, "FIRE68", 5, 15, 4, 0x204A, false),
    board(0x10A9, "FIRE68", 5, 15, 4, 0x2050, false),
    board(0x10AA, "FIRE68 Pro", 5, 15, 4, 0x2051, false),
    board(0x10A4, "FIRE68 Ultra", 5, 15, 4, 0x204B, false),
    board(0x10AB, "FIRE68 Ultra", 5, 15, 4, 0x2052, false),
    // -- macropad, 5x6 -----------------------------------------------------
    board(0x107D, "MAD Smart", 5, 6, 4, 0x2035, false),
];

/// Product ID de la carte de développement et de référence.
///
/// Identique à `protocol::PRODUCT_ID` : c'est la même carte vue depuis deux
/// modules, et la faire pointer sur la constante du protocole évite que les deux
/// valeurs divergent un jour.
pub const DEFAULT_BOARD_PRODUCT_ID: u16 = crate::protocol::PRODUCT_ID;

/// La carte de développement, utilisée comme géométrie de repli quand rien n'est
/// connecté et qu'il faut quand même dessiner quelque chose.
///
/// Le Python indexe `BY_PRODUCT_ID[0x1058]` ; ici on cherche par product ID pour
/// que la table reste l'unique source de vérité. Le repli sur la première entrée
/// n'est jamais atteint (un test le vérifie), il évite juste un panique
/// théorique si la table était vidée par erreur.
pub fn default_board() -> &'static BoardSpec {
    lookup(DEFAULT_BOARD_PRODUCT_ID).unwrap_or(&BOARDS[0])
}

/// La spec d'un product ID, ou `None` si ce n'est pas une carte DuckBread.
pub fn lookup(product_id: u16) -> Option<&'static BoardSpec> {
    BOARDS.iter().find(|b| b.product_id == product_id)
}

/// Comme [`lookup`], mais jamais `None`.
///
/// Un product ID inconnu sur le VID du vendeur qui répond sur l'interface de
/// configuration mérite quand même qu'on lui parle : c'est bien plus
/// vraisemblablement une carte plus récente qu'un périphérique hostile. Elle
/// reçoit la géométrie commune et est déclarée non vérifiée, ce qui maintient
/// les écritures bloquées tant que sa version de protocole ne dit pas le
/// contraire.
pub fn describe(product_id: u16) -> BoardSpec {
    if let Some(spec) = lookup(product_id) {
        return spec.clone();
    }
    let fallback = default_board();
    BoardSpec {
        product_id,
        // Le Python formate `{product_id:#06x}` : « 0x » + au moins quatre
        // chiffres hexadécimaux.
        name: Cow::Owned(format!("Unrecognised board ({product_id:#06x})")),
        rows: fallback.rows,
        cols: fallback.cols,
        layers: fallback.layers,
        bootloader_id: 0,
        verified: false,
    }
}

/// Les product IDs de bootloader de toutes les cartes connues, triés et
/// dédoublonnés (le Python en fait un `frozenset`).
pub fn bootloader_ids() -> Vec<u16> {
    let mut ids: Vec<u16> = BOARDS.iter().map(|b| b.bootloader_id).collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Vrai si ce product ID est un bootloader connu : le clavier est alors en mode
/// flash et ne parle plus l'interface de configuration.
pub fn is_bootloader_id(product_id: u16) -> bool {
    BOARDS.iter().any(|b| b.bootloader_id == product_id)
}

// ---------------------------------------------------------------------------
// Cartes explicitement déverrouillées pour l'écriture
// ---------------------------------------------------------------------------
//
// Cartes que l'utilisateur a explicitement autorisées à l'écriture malgré un
// firmware non vérifié.
//
// L'état vit ici plutôt que dans `settings.rs` pour que la couche transport
// puisse le consulter sans dépendre des réglages de l'application ni du module
// de chemins — `device.rs` n'a aucune raison de savoir où est le répertoire de
// données. Le propriétaire du répertoire de données appelle `load_unlocked()`
// une fois au démarrage.

/// Ensemble global des cartes déverrouillées.
///
/// Le Python utilise un `set` de module ; en Rust il faut une synchronisation
/// car plusieurs threads (HUD, tray, transport) peuvent lire et écrire. Un
/// `BTreeSet` donne au passage l'ordre trié que le Python doit reconstruire avec
/// `sorted()` à chaque appel.
static UNLOCKED: Mutex<BTreeSet<u16>> = Mutex::new(BTreeSet::new());

/// Verrouille l'ensemble, en ignorant un empoisonnement.
///
/// Si un thread a paniqué en tenant le verrou, l'ensemble reste cohérent : il n'y
/// a pas d'invariant à moitié appliqué, seulement un ajout ou un retrait. Refuser
/// de rendre la main transformerait une panique en blocage définitif.
fn unlocked_guard() -> std::sync::MutexGuard<'static, BTreeSet<u16>> {
    UNLOCKED.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Vrai si la carte a été déverrouillée pour l'écriture.
pub fn is_unlocked(product_id: u16) -> bool {
    unlocked_guard().contains(&product_id)
}

/// Déverrouille une carte pour l'écriture.
pub fn unlock(product_id: u16) {
    unlocked_guard().insert(product_id);
}

/// Reverrouille une carte (écritures de nouveau bloquées).
pub fn lock(product_id: u16) {
    unlocked_guard().remove(&product_id);
}

/// Équivalent direct du `unlock(product_id, on=True)` Python.
pub fn set_unlocked(product_id: u16, on: bool) {
    if on {
        unlock(product_id);
    } else {
        lock(product_id);
    }
}

/// Les product IDs déverrouillés, en ordre croissant.
pub fn unlocked_ids() -> Vec<u16> {
    unlocked_guard().iter().copied().collect()
}

/// Erreurs de lecture/écriture de la liste des cartes déverrouillées.
#[derive(Debug, Error)]
pub enum UnlockError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid unlocked entry {entry:?}")]
    InvalidEntry { entry: String },
}

/// Lit la liste des cartes déverrouillées.
///
/// Un fichier absent ou illisible signifie « aucune », exactement comme en
/// Python. La différence est que la raison est renvoyée à l'appelant au lieu
/// d'être avalée : le démarrage peut la journaliser, et ignorer le `Result` reste
/// possible quand elle n'intéresse personne.
///
/// Le format accepté est celui écrit par [`save_unlocked`] — des chaînes
/// `"0x1058"` — mais aussi le format historique en entiers, et les préfixes
/// `0x`/`0o`/`0b` que `int(x, 0)` accepte côté Python.
///
/// Une clé `unlocked` absente ou qui n'est pas une liste vaut liste vide : le
/// Python lèverait une exception en itérant autre chose qu'une liste, mais il
/// vide l'ensemble dans tous les cas, donc l'état final est le même.
pub fn load_unlocked(path: impl AsRef<Path>) -> Result<(), UnlockError> {
    let path = path.as_ref();
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            unlocked_guard().clear();
            return Ok(());
        }
        Err(source) => {
            unlocked_guard().clear();
            return Err(UnlockError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let parsed: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(source) => {
            unlocked_guard().clear();
            return Err(UnlockError::Parse {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let mut fresh = BTreeSet::new();
    if let Some(entries) = parsed.get("unlocked").and_then(Value::as_array) {
        for entry in entries {
            let value = match parse_unlocked_entry(entry) {
                Ok(value) => value,
                Err(error) => {
                    // Le Python vide l'ensemble et laisse remonter : une liste
                    // à moitié comprise serait pire qu'aucune liste.
                    unlocked_guard().clear();
                    return Err(error);
                }
            };
            fresh.insert(value);
        }
    }

    let mut guard = unlocked_guard();
    *guard = fresh;
    Ok(())
}

/// Décode une entrée de la liste : entier JSON, ou chaîne dans une base
/// reconnue par `int(x, 0)`.
fn parse_unlocked_entry(entry: &Value) -> Result<u16, UnlockError> {
    match entry {
        Value::Number(number) => number
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .ok_or_else(|| UnlockError::InvalidEntry {
                entry: number.to_string(),
            }),
        Value::String(text) => {
            let trimmed = text.trim();
            let (radix, digits) = if let Some(rest) = trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
            {
                (16, rest)
            } else if let Some(rest) = trimmed
                .strip_prefix("0b")
                .or_else(|| trimmed.strip_prefix("0B"))
            {
                (2, rest)
            } else if let Some(rest) = trimmed
                .strip_prefix("0o")
                .or_else(|| trimmed.strip_prefix("0O"))
            {
                (8, rest)
            } else {
                (10, trimmed)
            };
            u16::from_str_radix(digits, radix).map_err(|_| UnlockError::InvalidEntry {
                entry: text.clone(),
            })
        }
        other => Err(UnlockError::InvalidEntry {
            entry: other.to_string(),
        }),
    }
}

/// Écrit la liste des cartes déverrouillées.
///
/// Le fichier produit est identique à l'octet près à celui du Python
/// (`json.dumps({"unlocked": [...]}, indent=2)`, sans saut de ligne final), pour
/// que les deux versions puissent coexister sur une même installation.
pub fn save_unlocked(path: impl AsRef<Path>) -> Result<(), UnlockError> {
    let path = path.as_ref();
    let ids: Vec<String> = unlocked_ids()
        .into_iter()
        .map(|product_id| format!("{product_id:#06x}"))
        .collect();

    // Écrit à la main plutôt qu'avec serde_json : `to_string_pretty` sérialise
    // une clé d'objet différemment et ne garantit pas cette mise en page.
    let json = if ids.is_empty() {
        "{\n  \"unlocked\": []\n}".to_owned()
    } else {
        let entries: Vec<String> = ids
            .iter()
            .map(|id| format!("    \"{id}\""))
            .collect();
        format!("{{\n  \"unlocked\": [\n{}\n  ]\n}}", entries.join(",\n"))
    };

    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(path, json)
    };

    write().map_err(|source| UnlockError::Write {
        path: path.to_path_buf(),
        source,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// Les valeurs de référence (nombre d'entrées, IDs, noms, géométries, facteurs
// de forme) viennent de `src/mad68/devices.py`.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use super::*;

    /// Les tests qui touchent à l'état global `UNLOCKED` partagent une ressource
    /// unique par processus : ils doivent s'exécuter l'un après l'autre, sinon
    /// le ramasse-miettes de l'un vide l'ensemble que l'autre vient de remplir.
    static UNLOCK_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn board_table_matches_python_order_and_contents() {
        assert_eq!(BOARDS.len(), 25);

        // Ordre du fichier Python, product ID par product ID.
        let ids: Vec<u16> = BOARDS.iter().map(|b| b.product_id).collect();
        assert_eq!(
            ids,
            vec![
                0x1053, 0x1054, 0x1055, 0x1056, 0x10AD, 0x1120, 0x10F6, 0x110B, 0x112A, 0x1057,
                0x1058, 0x1059, 0x105A, 0x10A7, 0x10D6, 0x1123, 0x1144, 0x1136, 0x1139, 0x10A3,
                0x10A9, 0x10AA, 0x10A4, 0x10AB, 0x107D,
            ]
        );

        // Toutes les cartes font 5 lignes et 4 couches : seul le nombre de
        // colonnes varie, et c'est ce qui rend la géométrie par carte vitale.
        assert!(BOARDS.iter().all(|b| b.rows == 5 && b.layers == 4));
        assert!(BOARDS.iter().all(|b| matches!(b.cols, 6 | 14 | 15)));

        // Seules deux cartes ont été exercées sur matériel.
        let verified: Vec<u16> = BOARDS
            .iter()
            .filter(|b| b.verified)
            .map(|b| b.product_id)
            .collect();
        assert_eq!(verified, vec![0x1053, 0x1058]);

        // Quelques noms et bootloaders précis, y compris les doublons FIRE68.
        let fire68: Vec<&BoardSpec> = BOARDS.iter().filter(|b| b.name == "FIRE68").collect();
        assert_eq!(fire68.len(), 2);
        assert_eq!(fire68[0].bootloader_id, 0x204A);
        assert_eq!(fire68[1].bootloader_id, 0x2050);

        let smart = lookup(0x107D).expect("MAD Smart");
        assert_eq!(smart.name, "MAD Smart");
        assert_eq!(smart.bootloader_id, 0x2035);
        assert_eq!(smart.total_keys(), 30);
        assert_eq!(smart.form_factor(), FormFactor::Macropad);
    }

    #[test]
    fn default_board_is_the_mad68_he_rgb() {
        let default = default_board();
        assert_eq!(default.product_id, 0x1058);
        assert_eq!(default.name, "MAD68 HE RGB");
        assert_eq!(default.rows, 5);
        assert_eq!(default.cols, 15);
        assert_eq!(default.layers, 4);
        assert_eq!(default.bootloader_id, 0x2024);
        assert!(default.verified);
        assert_eq!(default.total_keys(), 75);
        assert_eq!(default.form_factor(), FormFactor::SixtyFive);
        // La constante de protocole et la table doivent rester d'accord.
        assert_eq!(DEFAULT_BOARD_PRODUCT_ID, crate::protocol::PRODUCT_ID);
    }

    #[test]
    fn form_factor_boundaries_match_python() {
        // cols <= 8 -> macropad ; cols <= 14 -> "60" ; sinon "65".
        for (cols, expected) in [
            (6u8, FormFactor::Macropad),
            (8, FormFactor::Macropad),
            (9, FormFactor::Sixty),
            (14, FormFactor::Sixty),
            (15, FormFactor::SixtyFive),
        ] {
            let spec = board(0x0001, "test", 5, cols, 4, 0, false);
            assert_eq!(spec.form_factor(), expected, "colonnes {cols}");
        }
        assert_eq!(FormFactor::Macropad.as_str(), "macropad");
        assert_eq!(FormFactor::Sixty.as_str(), "60");
        assert_eq!(FormFactor::SixtyFive.as_str(), "65");
        // La carte 5x14 est bien classée 60 % et la 5x15 en 65 %.
        assert_eq!(lookup(0x1053).expect("MAD60 HE").form_factor(), FormFactor::Sixty);
        assert_eq!(lookup(0x1058).expect("MAD68 HE RGB").form_factor(), FormFactor::SixtyFive);
    }

    #[test]
    fn describe_falls_back_to_common_geometry_for_unknown_ids() {
        let known = describe(0x1053);
        assert_eq!(known.name, "MAD60 HE");
        assert_eq!(known.cols, 14);

        let unknown = describe(0x9999);
        assert_eq!(unknown.product_id, 0x9999);
        // Formatage Python `f"Unrecognised board ({pid:#06x})"`.
        assert_eq!(unknown.name, "Unrecognised board (0x9999)");
        assert_eq!(unknown.rows, default_board().rows);
        assert_eq!(unknown.cols, default_board().cols);
        assert_eq!(unknown.layers, default_board().layers);
        assert_eq!(unknown.bootloader_id, 0);
        assert!(!unknown.verified);
        assert_eq!(lookup(0x9999), None);
    }

    #[test]
    fn bootloader_ids_are_deduplicated_and_sorted() {
        let ids = bootloader_ids();
        assert_eq!(ids.len(), BOARDS.len(), "aucun bootloader ne doit être partagé");
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(is_bootloader_id(0x2024));
        assert!(is_bootloader_id(0x2035));
        assert!(!is_bootloader_id(0x1058));
    }

    #[test]
    fn unlock_state_is_global_and_sorted() {
        let _serialized = UNLOCK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // État partagé entre tests : on repart d'un ensemble vide.
        unlocked_guard().clear();

        assert!(!is_unlocked(0x1058));
        unlock(0x1058);
        unlock(0x1053);
        set_unlocked(0x107D, true);
        assert!(is_unlocked(0x1058));
        assert_eq!(unlocked_ids(), vec![0x1053, 0x1058, 0x107D]);

        lock(0x1053);
        set_unlocked(0x107D, false);
        assert_eq!(unlocked_ids(), vec![0x1058]);
        unlocked_guard().clear();
    }

    /// Chemin de fichier unique par test, dans le répertoire temporaire système.
    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "openmad-unlock-{}-{tag}.json",
            std::process::id()
        ))
    }

    #[test]
    fn save_writes_the_same_bytes_as_python_json_dumps() {
        let _serialized = UNLOCK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unlocked_guard().clear();
        unlock(0x1058);

        let path = temp_path("save");
        save_unlocked(&path).expect("écriture");
        // json.dumps({"unlocked": ["0x1058"]}, indent=2), sans saut de ligne.
        assert_eq!(
            fs::read_to_string(&path).expect("relecture"),
            "{\n  \"unlocked\": [\n    \"0x1058\"\n  ]\n}"
        );
        let _ = fs::remove_file(&path);

        // Liste vide : `json.dumps({"unlocked": []}, indent=2)` tient sur la
        // même paire de lignes.
        unlocked_guard().clear();
        let path = temp_path("save-empty");
        save_unlocked(&path).expect("écriture");
        assert_eq!(
            fs::read_to_string(&path).expect("relecture"),
            "{\n  \"unlocked\": []\n}"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_accepts_hex_strings_and_plain_integers() {
        let _serialized = UNLOCK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unlocked_guard().clear();

        let path = temp_path("load");
        // `int(x, 0)` côté Python accepte un entier JSON, une chaîne décimale,
        // et les préfixes 0x / 0b / 0o.
        fs::write(&path, r#"{"unlocked": ["0x1058", 4248, "0b1"]}"#)
            .expect("écriture du fixture");
        load_unlocked(&path).expect("lecture");
        assert_eq!(unlocked_ids(), vec![1, 0x1058, 0x1098]);
        let _ = fs::remove_file(&path);

        // Une entrée invalide vide l'ensemble et remonte l'erreur.
        let path = temp_path("load-bad");
        fs::write(&path, r#"{"unlocked": ["0x1058", "pas un nombre"]}"#)
            .expect("écriture du fixture");
        assert!(load_unlocked(&path).is_err());
        assert!(unlocked_ids().is_empty());
        let _ = fs::remove_file(&path);

        // Fichier absent : aucun déverrouillage, sans erreur.
        let missing = temp_path("load-missing");
        let _ = fs::remove_file(&missing);
        unlock(0x1058);
        load_unlocked(&missing).expect("absent = vide");
        assert!(unlocked_ids().is_empty());
    }

    #[test]
    fn load_and_save_round_trip() {
        let _serialized = UNLOCK_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        unlocked_guard().clear();
        unlock(0x1058);
        unlock(0x1053);

        let path = temp_path("roundtrip");
        save_unlocked(&path).expect("écriture");
        unlocked_guard().clear();
        assert!(unlocked_ids().is_empty());
        load_unlocked(&path).expect("lecture");
        assert_eq!(unlocked_ids(), vec![0x1053, 0x1058]);
        let _ = fs::remove_file(&path);
    }
}
