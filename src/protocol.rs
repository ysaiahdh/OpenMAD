//! Protocole filaire de l'interface de configuration embarquée DuckBread.
//!
//! Traduction stricte de `src/mad68/protocol.py`. Le clavier parle le protocole
//! raw-HID de VIA (rapports de 32 octets, octet 0 = identifiant de commande)
//! plus un « canal vendeur » 0x96 qui transporte toutes les fonctions
//! Hall-effect : point d'actionnement par touche, rapid trigger, dead band, DKS,
//! SOCD/OKS, calibration et IAP (flash du firmware).
//!
//! Deux règles gouvernent ce fichier :
//!
//! 1. **Parité à l'octet.** Chaque trame produite ici doit être identique à
//!    celle produite par le Python, octet pour octet, y compris le remplissage
//!    à zéro jusqu'à 32 octets et l'absence de checksum (VIA n'en a pas).
//!    Les tests en bas de fichier portent les trames de référence calculées
//!    par `protocol.py` lui-même.
//! 2. **Parité d'arrondi.** Python arrondit avec `round()`, qui arrondit au
//!    pair le plus proche (*banker's rounding*) et non « au supérieur sur les
//!    demis ». Rust `f64::round()` arrondit au contraire loin de zéro : utiliser
//!    `round_ties_even()` partout où l'on convertit un flottant en entier,
//!    sinon les valeurs de travel tombant sur un demi-centième divergent d'un
//!    millième de millimètre sur le fil.
//!
//! Provenance : contrôleur DuckBread du configurateur web officiel, voir
//! `research/notes/protocol.md`.

use std::fmt;

use thiserror::Error;

// ---------------------------------------------------------------------------
// Identifiants USB
// ---------------------------------------------------------------------------

pub const VENDOR_ID: u16 = 0x373B;
pub const PRODUCT_ID: u16 = 0x1058;
pub const BOOTLOADER_PRODUCT_ID: u16 = 0x2024;

/// Version de protocole contre laquelle ce pilote a été rétro-ingénieré et
/// confirmé sur matériel. Le clavier renvoie sa propre valeur pour
/// `Cmd::GET_PROTOCOL_VERSION` (0x01), et **toutes** les positions d'octets et
/// énumérations de ce fichier viennent d'un firmware reportant 9.
///
/// Ce n'est pas une formalité : l'application vendeur elle-même conditionne
/// l'affichage des « advanced keys » à un numéro très proche
/// (`advancedKeyVersion`), donc un changement de format sur le fil est un
/// scénario réel. Si la valeur lue cesse de correspondre à cette constante,
/// considérer chaque disposition de paquet ci-dessous comme non vérifiée pour
/// ce firmware tant qu'elle n'a pas été recontrôlée sur l'appareil.
pub const KNOWN_PROTOCOL_VERSION: u8 = 9;

/// Interface de configuration raw-HID (convention QMK/VIA).
pub const USAGE_PAGE: u16 = 0xFF60;
pub const USAGE: u16 = 0x0061;

/// Confirmé par le descripteur de rapport HID : `Report Count 0x20`,
/// `Report Size 8`, pour l'Input (usage 0x62) comme pour l'Output (usage 0x63).
pub const REPORT_SIZE: usize = 0x20; // 32 octets

/// L'octet 0 est l'identifiant de commande ; la charge utile démarre à l'octet 1.
pub const PAYLOAD_OFFSET: usize = 0x01;
pub const MAX_PAYLOAD: usize = REPORT_SIZE - PAYLOAD_OFFSET; // 31

/// Les commandes « buffer » portent un sous-en-tête de 3 octets
/// (offset u16 big-endian, taille u8) avant les données.
pub const BUFFER_HEADER: usize = 3;
pub const MAX_BUFFER_CHUNK: usize = MAX_PAYLOAD - BUFFER_HEADER; // 28, comme le 0x1C de l'app

/// Un rapport host -> device tel qu'il part sur le fil.
pub type Report = [u8; REPORT_SIZE];

// ---------------------------------------------------------------------------
// Erreurs
// ---------------------------------------------------------------------------

/// Erreurs de construction ou de lecture de trame.
///
/// Les variantes reproduisent les `ValueError`/`IndexError` du Python, avec des
/// messages équivalents, pour que la migration ne change pas le comportement
/// observable des appelants.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    /// `build()` : charge utile plus longue que les 31 octets disponibles.
    #[error("payload {len} exceeds {max}")]
    PayloadTooLarge { len: usize, max: usize },

    /// `build_buffer_read()` / `build_buffer_write()` : morceau > 28 octets.
    #[error("size {size} exceeds {max}")]
    ChunkTooLarge { size: usize, max: usize },

    /// Un octet du fil qui ne correspond à aucune variante connue.
    #[error("unknown {kind} id {value:#04x}")]
    UnknownId { kind: &'static str, value: u8 },

    /// Rapport device -> host vide : impossible à indexer comme le fait Python.
    #[error("empty report")]
    EmptyReport,
}

// ---------------------------------------------------------------------------
// Énumérations d'identifiants
// ---------------------------------------------------------------------------

/// Déclare une énumération d'identifiants `u8` avec conversion dans les deux
/// sens, sur le modèle des `IntEnum` Python.
///
/// Deux écarts assumés par rapport au Python :
///
/// * Les noms de variantes restent en `SCREAMING_SNAKE_CASE` comme dans le
///   Python, pour que la correspondance ligne à ligne reste évidente pendant la
///   migration ; d'où le `#[allow(non_camel_case_types)]`.
/// * Rust refuse deux discriminants égaux dans une même énumération, là où un
///   `IntEnum` Python accepte des alias (`Vendor.ENTRY_OP` et
///   `Vendor.BUFFER_TOG_TH` valent tous deux 0x0D). Les alias sont donc déclarés
///   en constantes associées, ce qui garde le même nom d'accès.
macro_rules! id_enum {
    (
        $(#[$meta:meta])*
        $name:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident = $value:expr
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[allow(non_camel_case_types)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum $name {
            $(
                $(#[$vmeta])*
                $variant = $value
            ),*
        }

        impl $name {
            /// Valeur transmise sur le fil.
            pub const fn id(self) -> u8 {
                self as u8
            }

            /// Toutes les variantes déclarées.
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// Conversion depuis l'octet du fil ; `None` si inconnu.
            pub const fn from_id(value: u8) -> Option<Self> {
                match value {
                    $($value => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }

        impl From<$name> for u8 {
            fn from(value: $name) -> u8 {
                value.id()
            }
        }

        impl TryFrom<u8> for $name {
            type Error = ProtocolError;

            fn try_from(value: u8) -> Result<Self, Self::Error> {
                Self::from_id(value).ok_or(ProtocolError::UnknownId {
                    kind: stringify!($name),
                    value,
                })
            }
        }
    };
}

id_enum! {
    /// Identifiants de commande VIA (octet 0). Miroir de l'enum `Mu` du bundle web.
    Cmd {
        GET_PROTOCOL_VERSION = 0x01,
        GET_KEYBOARD_VALUE = 0x02,
        SET_KEYBOARD_VALUE = 0x03,
        DYNAMIC_KEYMAP_GET_KEYCODE = 0x04,
        DYNAMIC_KEYMAP_SET_KEYCODE = 0x05,
        DYNAMIC_KEYMAP_RESET = 0x06,
        CUSTOM_SET_VALUE = 0x07,
        CUSTOM_GET_VALUE = 0x08,
        CUSTOM_SAVE = 0x09,
        EEPROM_RESET = 0x0A,
        BOOTLOADER_JUMP = 0x0B,
        DYNAMIC_KEYMAP_MACRO_GET_COUNT = 0x0C,
        DYNAMIC_KEYMAP_MACRO_GET_BUFFER_SIZE = 0x0D,
        DYNAMIC_KEYMAP_MACRO_GET_BUFFER = 0x0E,
        DYNAMIC_KEYMAP_MACRO_SET_BUFFER = 0x0F,
        DYNAMIC_KEYMAP_MACRO_RESET = 0x10,
        DYNAMIC_KEYMAP_GET_LAYER_COUNT = 0x11,
        DYNAMIC_KEYMAP_GET_BUFFER = 0x12,
        DYNAMIC_KEYMAP_SET_BUFFER = 0x13,
        VIAL_PREFIX = 0xFE,
        UNHANDLED = 0xFF,
    }
}

/// Canal vendeur personnalisé. Il voyage sur `GET_KEYBOARD_VALUE` /
/// `SET_KEYBOARD_VALUE` : octet 1 = `CUSTOM_CHANNEL`, octet 2 = sous-commande.
pub const CUSTOM_CHANNEL: u8 = 0x96;

/// Les rapports de télémétrie non sollicités commencent par ces trois octets,
/// puis un discriminateur de type à l'octet 3.
///
/// Extrait du bundle : `V(D, "dataReporting", [id_get_keyboard_value, customId,
/// dataReporting])`.
pub const DATA_REPORTING_PREFIX: [u8; 3] = [0x02, CUSTOM_CHANNEL, 0x14];

id_enum! {
    /// Sous-commandes vendeur sur le canal 0x96. Miroir de l'enum `Bu`.
    Vendor {
        TAP_DANCE_GET = 0x01,
        TAP_DANCE_SET = 0x02,
        /// Même valeur que `BUFFER_TOG_TH` ; désambiguïsé par la commande.
        /// `from_id(0x0D)` renvoie cette variante, comme en Python.
        ENTRY_OP = 0x0D,
        /// Point d'actionnement d'une seule touche.
        ONE_TOG_TH = 0x03,
        CALIBRATE = 0x08,
        REALTIME_ADC_AXLE = 0x09,
        REALTIME_TRIP_AXLE = 0x0A,
        DEAD_BAND = 0x0B,
        /// Rapid trigger en masse.
        BUFFER_RT = 0x0E,
        /// Dynamic keystroke.
        DKS = 0x0F,
        /// Rapid trigger d'une seule touche.
        ONE_RT = 0x07,
        MIX_AXLE = 0x10,
        FEATURE = 0x11,
        SAVE_LAMPLIGHT = 0x12,
        LAYER = 0x13,
        DATA_REPORTING = 0x14,
        RESET_ALL = 0x15,
        REALTIME_ADC_AXLE_BUFFER = 0x16,
        REALTIME_TRIP_AXLE_BUFFER = 0x17,
        CALIBRATION_START = 0x18,
        CALIBRATION_FINISH = 0x19,
        COMPLETE_STATUS_BUFFER = 0x1B,
        ADC_TRIP_COMP_STATUS_BUFFER = 0x1C,
        BOTTOM_OPTIMIZE_SWITCH = 0x1D,
        GAME_MODE = 0x1E,
        CALIBRATION = 0x1F,
        RS = 0x20,
        BOX_LIGHT = 0x21,
        LIGHT_INFO = 0x41,
        SET_CUSTOM_LAMPLIGHT = 0x42,
        GET_CUSTOM_LAMPLIGHT = 0x45,
        /// In-application programming (flash du firmware).
        IAP = 0x98,
    }
}

impl Vendor {
    /// Alias de [`Vendor::ENTRY_OP`] : même octet 0x0D, autre nom côté vendeur.
    ///
    /// Le Python en fait un second membre d'`IntEnum` (`BUFFER_TOG_TH`), ce que
    /// Rust interdit — deux variantes ne peuvent pas partager un discriminant.
    /// Une constante associée garde le nom d'accès, la valeur et l'égalité, mais
    /// ne peut pas servir de motif dans un `match` : c'est `ENTRY_OP` qui s'y
    /// emploie, et les deux valeurs sont de toute façon indistinguables sur le
    /// fil (la commande hôte, GET ou SET, lève l'ambiguïté).
    pub const BUFFER_TOG_TH: Vendor = Vendor::ENTRY_OP;
}

id_enum! {
    /// Verbe d'accès utilisé par certains paquets vendeur. Miroir de `Fu`/`Nu`.
    SubField {
        GET = 0x01,
        GET_BUFFER = 0x02,
        SET = 0x03,
        SET_BUFFER = 0x04,
    }
}

id_enum! {
    /// Ce qu'une écriture vendeur en masse doit faire de la mémoire non volatile.
    ///
    /// Miroir de l'enum `Eu`. C'est ce mécanisme qui rend le changement de profil
    /// par application sûr : `Normal` applique les valeurs sans toucher à la
    /// flash, donc un alt-tab ne coûte aucune endurance d'écriture. Seul un
    /// enregistrement explicitement demandé par l'utilisateur doit utiliser
    /// `EraseAndWrite`.
    FlashOp {
        /// Applique seulement ; ne valide pas en flash.
        Normal = 0x00,
        Erase = 0x01,
        Write = 0x02,
        /// Persiste au travers d'une coupure d'alimentation.
        EraseAndWrite = 0x03,
    }
}

id_enum! {
    /// Sous-identifiants standards de `id_get/set_keyboard_value` de VIA.
    ///
    /// Ce sont les IDs de VIA, confirmés sur matériel : interroger 0x01 renvoie
    /// un compteur de millisecondes croissant (uptime), pas un numéro de couche.
    ///
    /// Note : l'enum `Gu` du bundle web (`defaultLayerSt` / `swapWasdSt` /
    /// `macosSt` / `winLockSt` / `nKroSt`) est un état d'*interface* et **n'est
    /// pas** cet espace d'identifiants. C'est cette confusion qui faisait lire
    /// 0x01 comme « couche par défaut ».
    KeyboardValue {
        Uptime = 0x01,
        LayoutOptions = 0x02,
        SwitchMatrixState = 0x03,
        FirmwareVersion = 0x04,
        DeviceIndication = 0x05,
    }
}

/// Valeurs clavier qui changent d'elles-mêmes et ne doivent jamais être prises
/// pour de la configuration : l'uptime qui s'incrémente, et l'état de la matrice
/// qui reflète les touches physiquement enfoncées à cet instant.
pub const VOLATILE_KEYBOARD_VALUES: &[KeyboardValue] =
    &[KeyboardValue::Uptime, KeyboardValue::SwitchMatrixState];

id_enum! {
    /// Mode avancé par touche. Miroir de l'enum `Vu`.
    AdvancedKeyMode {
        None = 0x00,
        Rs = 0x01,
        Socd = 0x02,
        SocdKey1 = 0x03,
        SocdKey2 = 0x04,
        SocdBalance = 0x05,
        Oks = 0x06,
    }
}

impl Default for AdvancedKeyMode {
    /// La dataclass Python déclare `mode: AdvancedKeyMode = AdvancedKeyMode.NONE`,
    /// donc un `AdvancedKey` construit par défaut n'est jamais actif.
    fn default() -> Self {
        AdvancedKeyMode::None
    }
}

id_enum! {
    /// Opcodes du flux d'octets des macros. Miroir de l'enum `Ru`.
    MacroAction {
        SsTapCode = 0x01,
        SsDownCode = 0x02,
        SsUpCode = 0x03,
        SsDelayCode = 0x04,
        VialMacroExtTap = 0x05,
        VialMacroExtDown = 0x06,
        VialMacroExtUp = 0x07,
    }
}

// ---------------------------------------------------------------------------
// Classification de sûreté
// ---------------------------------------------------------------------------
//
// Ces listes sont appliquées par `device.rs`, pas seulement documentées.

/// Commandes VIA qui modifient un état persistant.
pub const WRITE_COMMANDS: &[Cmd] = &[
    Cmd::SET_KEYBOARD_VALUE,
    Cmd::DYNAMIC_KEYMAP_SET_KEYCODE,
    Cmd::DYNAMIC_KEYMAP_SET_BUFFER,
    Cmd::DYNAMIC_KEYMAP_MACRO_SET_BUFFER,
    Cmd::CUSTOM_SET_VALUE,
    Cmd::CUSTOM_SAVE,
    Cmd::DYNAMIC_KEYMAP_RESET,
    Cmd::DYNAMIC_KEYMAP_MACRO_RESET,
    Cmd::EEPROM_RESET,
    Cmd::BOOTLOADER_JUMP,
];

/// Commandes VIA qui effacent la mémoire embarquée ou laissent le clavier
/// inutilisable jusqu'au reflash. Jamais envoyées sans opt-in explicite.
pub const DANGEROUS_COMMANDS: &[Cmd] = &[
    Cmd::DYNAMIC_KEYMAP_RESET,
    Cmd::DYNAMIC_KEYMAP_MACRO_RESET,
    Cmd::EEPROM_RESET,
    Cmd::BOOTLOADER_JUMP,
];

/// Sous-commandes vendeur destructrices quel que soit l'habillage get/set.
///
/// La calibration est isolée parce qu'une mauvaise calibration est la seule
/// chose qui dégrade matériellement un clavier Hall-effect, et IAP démarre un
/// flash de firmware.
pub const DANGEROUS_VENDOR: &[Vendor] = &[
    Vendor::CALIBRATE,
    Vendor::CALIBRATION_START,
    Vendor::CALIBRATION_FINISH,
    Vendor::RESET_ALL,
    Vendor::IAP,
];

/// Vrai si la commande écrit un état persistant.
pub fn is_write_command(cmd: Cmd) -> bool {
    WRITE_COMMANDS.contains(&cmd)
}

/// Vrai si la commande efface la mémoire ou lance un flash.
pub fn is_dangerous_command(cmd: Cmd) -> bool {
    DANGEROUS_COMMANDS.contains(&cmd)
}

/// Vrai si la sous-commande est destructrice quel que soit l'habillage.
pub fn is_dangerous_vendor(sub: Vendor) -> bool {
    DANGEROUS_VENDOR.contains(&sub)
}

/// Vrai si la valeur clavier est volatile (uptime, matrice).
pub fn is_volatile_keyboard_value(value: KeyboardValue) -> bool {
    VOLATILE_KEYBOARD_VALUES.contains(&value)
}

// ---------------------------------------------------------------------------
// Géométrie
// ---------------------------------------------------------------------------
//
// Ce sont des **valeurs par défaut**, pas des faits sur le clavier connecté. Le
// contrôleur est partagé par des cartes dont les matrices font 5x15, 5x14 et 5x6
// (voir `devices.rs`), et chaque tampon sur le fil — keymap, tableau
// d'actionnement, table de couleurs par touche — est tassé en row-major sur le
// nombre de colonnes *de la carte*. Une carte 60 % tasse 14 touches par ligne :
// lire sa keymap avec un pas de 15 décale toutes les lignes après la première.
//
// Tout code tenant un clavier en direct doit donc prendre sa géométrie dans sa
// `BoardSpec` (rows/cols/layers) plutôt que dans ces constantes, qui ne
// décrivent que la carte 5x15 de développement.

pub const LAYER_COUNT: usize = 4;
pub const MATRIX_ROWS: usize = 5;
pub const MATRIX_COLS: usize = 15;
/// keyClass + keyId, soit un keycode QMK 16 bits, big-endian.
pub const KEYCODE_SIZE: usize = 2;
pub const KEYMAP_SIZE: usize = LAYER_COUNT * MATRIX_ROWS * MATRIX_COLS * KEYCODE_SIZE; // 600
pub const TOTAL_KEYS: usize = MATRIX_ROWS * MATRIX_COLS; // 75

/// Nombre d'octets du tampon de keymap dynamique pour une matrice donnée.
pub const fn keymap_size_for(rows: usize, cols: usize, layers: usize) -> usize {
    layers * rows * cols * KEYCODE_SIZE
}

/// Décalage en octets d'un keycode dans un tampon de keymap dynamique.
///
/// Disposition row-major : couche, puis ligne, puis colonne, chacune sur le
/// nombre de colonnes réel de la carte ; deux octets (big-endian) par case.
pub const fn keymap_offset(layer: usize, row: usize, col: usize, rows: usize, cols: usize) -> usize {
    ((layer * rows + row) * cols + col) * KEYCODE_SIZE
}

// ---------------------------------------------------------------------------
// Ordre des canaux LED et gains
// ---------------------------------------------------------------------------

/// Ordre des octets attendu par les LED.
///
/// Les LED adressables sont souvent en GRB plutôt qu'en RGB, et le firmware
/// recopie nos octets tels quels : la valeur relue est toujours ce qui a été
/// écrit, donc un mauvais ordre est invisible sur le fil et n'apparaît que
/// comme des couleurs fausses sur le clavier. Régler sur ce que rapporte
/// `tools/led_order.py`. `Rgb` signifie aucun réordonnancement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LedOrder {
    #[default]
    Rgb,
    Rbg,
    Grb,
    Gbr,
    Brg,
    Bgr,
}

impl LedOrder {
    /// Indices source de chaque position sur le fil.
    ///
    /// `[1, 0, 2]` (GRB) se lit : premier octet émis = vert, deuxième = rouge,
    /// troisième = bleu.
    pub const fn channels(self) -> [usize; 3] {
        match self {
            LedOrder::Rgb => [0, 1, 2],
            LedOrder::Rbg => [0, 2, 1],
            LedOrder::Grb => [1, 0, 2],
            LedOrder::Gbr => [1, 2, 0],
            LedOrder::Brg => [2, 0, 1],
            LedOrder::Bgr => [2, 1, 0],
        }
    }

    /// Nom à trois lettres, identique aux clés du dictionnaire Python `_ORDERS`.
    pub const fn as_str(self) -> &'static str {
        match self {
            LedOrder::Rgb => "RGB",
            LedOrder::Rbg => "RBG",
            LedOrder::Grb => "GRB",
            LedOrder::Gbr => "GBR",
            LedOrder::Brg => "BRG",
            LedOrder::Bgr => "BGR",
        }
    }

    /// Analyse un nom à trois lettres ; `None` hors des six permutations.
    /// La comparaison est sensible à la casse, comme les clés `_ORDERS`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "RGB" => Some(LedOrder::Rgb),
            "RBG" => Some(LedOrder::Rbg),
            "GRB" => Some(LedOrder::Grb),
            "GBR" => Some(LedOrder::Gbr),
            "BRG" => Some(LedOrder::Brg),
            "BGR" => Some(LedOrder::Bgr),
            _ => None,
        }
    }
}

/// Ordre actif. `Rgb` = identité.
pub const LED_CHANNEL_ORDER: LedOrder = LedOrder::Rgb;

/// Gain appliqué par canal avant l'envoi, en (rouge, vert, bleu).
///
/// Les trois couleurs ne sont pas également lumineuses sur cette carte : le
/// rouge fonctionne seul et avec le bleu, mais il est écrasé dès que le vert est
/// fort — le jaune rend vert, le blanc rend cyan. C'est de la physique de LED
/// (tension directe plus basse pour les puces rouges, œil bien plus sensible au
/// vert), pas un problème de protocole, d'où la correction ici plutôt qu'un
/// contournement dans l'interface.
///
/// La correction retient le vert et le bleu au lieu de pousser le rouge : en
/// blanc et en jaune le rouge est déjà à 255, il n'y a pas de marge. Le coût est
/// un clavier globalement plus sombre. `[1.0, 1.0, 1.0]` désactive tout.
pub const LED_CHANNEL_GAIN: [f64; 3] = [1.0, 0.23, 0.27];

/// Luminosité maximale acceptée par le firmware dans un `LightInfo`.
///
/// Ce n'est pas une limite souple. Un paquet `LightInfo` portant une valeur plus
/// haute voit son champ luminosité ignoré : l'effet, la vitesse et la couleur du
/// même paquet s'appliquent quand même, donc l'écriture semble réussir alors que
/// la carte conserve silencieusement l'ancienne luminosité. Mesuré sur
/// l'appareil : 210 est stocké, 211 et au-delà ne le sont pas.
///
/// L'application web d'origine porte le même nombre en surcharge par modèle,
/// indexée sur `custom.name` (`{"MAD 68 RGB": 0xd2, ...}`), voir
/// `research/notes/protocol.md` §7.
pub const BRIGHTNESS_MAX: u8 = 210;

/// Effets de lumière qui s'animent et réclament donc une vitesse dans 1..=255.
///
/// Repris de la table d'effets de l'app d'origine (liste `Bt`), où un effet ne
/// porte une entrée `{min: 1, max: 255}` que s'il bouge. Les autres sont fixes
/// par construction : 4 et 5 sont « Rainbow *static* », une image immobile.
///
/// La vitesse 0 est hors plage. Le firmware l'accepte et fige l'animation sur sa
/// première image, ce qui est indiscernable d'un effet qui ne s'applique pas.
pub const ANIMATED_EFFECTS: &[u8] = &[6, 7, 8, 9, 11, 13, 19, 23, 26, 28, 31, 36, 39];

/// Vitesse à utiliser lorsqu'un effet animé est demandé sans vitesse valable.
pub const DEFAULT_EFFECT_SPEED: u8 = 128;

/// Effets de lumière qui prennent une couleur unique du paquet `LightInfo`.
///
/// Même table que `ANIMATED_EFFECTS` : un effet y figure quand il porte une
/// entrée couleur. 1 (Customization) en est volontairement absent — il rend la
/// table de couleurs par touche, donc la couleur du paquet n'est pas ce qu'on
/// voit, et en forcer une écraserait les couleurs de l'utilisateur.
pub const COLOUR_EFFECTS: &[u8] = &[2, 3, 4, 5, 6, 7, 8, 9, 11, 26, 28, 31, 36];

/// Couleur à utiliser lorsqu'un effet coloré est demandé sans couleur définie.
///
/// Le noir n'est pas rendu par les LED : il effondre ces effets sur une teinte
/// morte, ce qui faisait perdre tout aspect arc-en-ciel aux deux modes Rainbow
/// static dans les profils où aucune couleur n'avait jamais été choisie.
pub const DEFAULT_EFFECT_COLOUR: (u8, u8, u8) = (255, 255, 255);

/// Arrondi « Python » : au pair le plus proche, comme `round(float)`.
///
/// Rust `f64::round()` arrondit au contraire en s'éloignant de zéro, ce qui
/// décale d'une unité toutes les valeurs tombant exactement sur un demi
/// (11.5 -> 11 en Rust, 12 en Python).
#[inline]
fn py_round(value: f64) -> f64 {
    value.round_ties_even()
}

/// Applique le gain d'un canal et borne le résultat dans 0..=255.
///
/// `round()` Python sur un flottant négatif est géré par le même arrondi au
/// pair ; le `clamp` final rend le comportement identique à
/// `max(0, min(255, round(value * gain)))`.
#[inline]
fn apply_gain(value: u8, gain: f64) -> u8 {
    py_round(f64::from(value) * gain).clamp(0.0, 255.0) as u8
}

/// Applique le gain par canal, puis réordonne dans l'ordre attendu par les LED.
///
/// Deux opérations distinctes, faciles à confondre :
///  1. gain : chaque canal logique (r, g, b) est multiplié par son gain ;
///  2. ordre : la sortie est `src[order[0]], src[order[1]], src[order[2]]`,
///     donc `order[i]` est le canal *source* de la i-ème position émise.
pub fn encode_rgb(r: u8, g: u8, b: u8) -> [u8; 3] {
    let src = [
        apply_gain(r, LED_CHANNEL_GAIN[0]),
        apply_gain(g, LED_CHANNEL_GAIN[1]),
        apply_gain(b, LED_CHANNEL_GAIN[2]),
    ];
    let order = LED_CHANNEL_ORDER.channels();
    [src[order[0]], src[order[1]], src[order[2]]]
}

/// Inverse de [`encode_rgb`] : ordre du fil puis gain, vers du RGB logique.
///
/// Le démapping est l'inverse exact du mapping : `order[i]` reçoit
/// `data[i]`, donc l'octet 0 du fil alimente le canal logique `order[0]`.
/// La division par le gain est ignorée quand le gain est nul (Python teste
/// `if g else v`), et le résultat est borné comme à l'encodage.
pub fn decode_rgb(data: [u8; 3]) -> [u8; 3] {
    let order = LED_CHANNEL_ORDER.channels();
    let mut wire = [0u8; 3];
    for (wire_pos, logical) in order.iter().copied().enumerate() {
        wire[logical] = data[wire_pos];
    }
    let mut out = [0u8; 3];
    for (i, value) in wire.iter().copied().enumerate() {
        let gain = LED_CHANNEL_GAIN[i];
        out[i] = if gain == 0.0 {
            value
        } else {
            py_round(f64::from(value) / gain).clamp(0.0, 255.0) as u8
        };
    }
    out
}

// ---------------------------------------------------------------------------
// Travel (point d'actionnement, press/release rapid trigger)
// ---------------------------------------------------------------------------
//
// Les valeurs de travel sont des uint16 big-endian en unités de 0,01 mm sur ce
// clavier (échelle V2 ; la V1 utilisait un seul octet à 0,02 mm). DuckBread
// force `isV2`, donc c'est toujours la V2 qui s'applique.

pub const TRAVEL_UNIT_MM: f64 = 0.01;
pub const TRAVEL_MAX_RAW: u16 = 0xFFFF;

/// Décode une valeur de travel brute (uint16) en millimètres.
///
/// Python fait `round(raw * 0.01, 2)`, c'est-à-dire un arrondi au pair sur deux
/// décimales. `(x * 100).round_ties_even() / 100.0` produit exactement le même
/// double : vérifié sur les 65536 valeurs possibles de `raw`, aucune divergence
/// (aucun produit `raw * 0.01 * 100` ne tombe sur un demi exact, et la division
/// par 100 redonne le double le plus proche du centième voulu).
pub fn travel_to_mm(raw: u16) -> f64 {
    let mm = f64::from(raw) * TRAVEL_UNIT_MM;
    py_round(mm * 100.0) / 100.0
}

/// Encode des millimètres en valeur de travel brute, bornée à 0..=65535.
///
/// La division par `TRAVEL_UNIT_MM` (et non une multiplication par 100) est
/// volontaire : c'est ce que fait Python, et les deux opérations ne donnent pas
/// toujours le même double. Un `NaN` devient 0 par la conversion `as`, un infini
/// est ramené à la borne par le `clamp`.
pub fn mm_to_travel(mm: f64) -> u16 {
    py_round(mm / TRAVEL_UNIT_MM).clamp(0.0, f64::from(TRAVEL_MAX_RAW)) as u16
}

/// Encode des millimètres en la paire uint16 big-endian du fil.
pub fn travel_bytes(mm: f64) -> [u8; 2] {
    mm_to_travel(mm).to_be_bytes()
}

/// Décode une paire uint16 big-endian du fil en millimètres.
///
/// Le Python accepte une tranche de longueur quelconque (`int.from_bytes`), ce
/// que le type `[u8; 2]` rend inutile : la longueur est garantie.
pub fn travel_from_bytes(data: [u8; 2]) -> f64 {
    travel_to_mm(u16::from_be_bytes(data))
}

// ---------------------------------------------------------------------------
// Géométrie des paquets vendeur « bulk » (dispositions V2)
// ---------------------------------------------------------------------------
//
//   requête/réponse : [cmd, 0x96, sub, _, _, off_hi, off_lo, size, ...]
//   lecture : données à raw[8]
//   écriture : flashOp à raw[8], données à raw[9]
//
// `size` compte des TOUCHES, pas des octets, et les offsets sont des index
// linéaires (row * cols + col).

pub const BULK_OFFSET_AT: usize = 5; // uint16 big-endian
pub const BULK_SIZE_AT: usize = 7;
pub const BULK_READ_DATA_AT: usize = 8;
pub const BULK_FLASH_OP_AT: usize = 8;
pub const BULK_WRITE_DATA_AT: usize = 9;

/// Taille d'une entrée d'actionnement : apc uint16be.
pub const ACTUATION_ENTRY_V2: usize = 2;
/// Taille d'une entrée de rapid trigger : on u8, release u16be, press u16be.
pub const RAPID_TRIGGER_ENTRY_V2: usize = 5;

/// Touches par paquet, dérivées de la taille de rapport de 32 octets.
pub const MAX_ACTUATION_READ: usize = (REPORT_SIZE - BULK_READ_DATA_AT) / ACTUATION_ENTRY_V2; // 12
pub const MAX_ACTUATION_WRITE: usize = (REPORT_SIZE - BULK_WRITE_DATA_AT) / ACTUATION_ENTRY_V2; // 11
pub const MAX_RT_READ: usize = (REPORT_SIZE - BULK_READ_DATA_AT) / RAPID_TRIGGER_ENTRY_V2; // 4
pub const MAX_RT_WRITE: usize = (REPORT_SIZE - BULK_WRITE_DATA_AT) / RAPID_TRIGGER_ENTRY_V2; // 4

// ---------------------------------------------------------------------------
// Construction de paquets
// ---------------------------------------------------------------------------

/// Construit un rapport host -> device de 32 octets. Pas de checksum : VIA n'en
/// a pas.
///
/// `buf[0] = cmd`, la charge utile est recopiée à partir de l'octet 1 et le
/// reste du tampon reste à zéro (initialisation `[0u8; REPORT_SIZE]`).
pub fn build(cmd: Cmd, payload: &[u8]) -> Result<Report, ProtocolError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(ProtocolError::PayloadTooLarge {
            len: payload.len(),
            max: MAX_PAYLOAD,
        });
    }
    let mut buf = [0u8; REPORT_SIZE];
    buf[0] = cmd.id();
    buf[PAYLOAD_OFFSET..PAYLOAD_OFFSET + payload.len()].copy_from_slice(payload);
    Ok(buf)
}

/// Construit un rapport sur le canal vendeur : `[cmd, 0x96, sub, ...payload]`.
///
/// Le canal et la sous-commande occupent deux octets de la charge utile, donc la
/// charge utile appelante est limitée à `MAX_PAYLOAD - 2` = 29 octets. On
/// assemble la charge utile complète avant d'appeler `build` pour que le
/// message d'erreur soit exactement celui du Python (qui compte les 31 octets).
pub fn build_vendor(cmd: Cmd, sub: Vendor, payload: &[u8]) -> Result<Report, ProtocolError> {
    let mut inner = Vec::with_capacity(2 + payload.len());
    inner.push(CUSTOM_CHANNEL);
    inner.push(sub.id());
    inner.extend_from_slice(payload);
    build(cmd, &inner)
}

/// Construit une lecture de tampon VIA : `[cmd, offset_hi, offset_lo, size]`.
pub fn build_buffer_read(cmd: Cmd, offset: u16, size: usize) -> Result<Report, ProtocolError> {
    if size > MAX_BUFFER_CHUNK {
        return Err(ProtocolError::ChunkTooLarge {
            size,
            max: MAX_BUFFER_CHUNK,
        });
    }
    let mut payload = [0u8; BUFFER_HEADER];
    payload[..2].copy_from_slice(&offset.to_be_bytes());
    payload[2] = size as u8;
    build(cmd, &payload)
}

/// Construit une écriture de tampon VIA :
/// `[cmd, offset_hi, offset_lo, size, ...data]`.
///
/// Le champ `size` est la longueur réelle des données, pas la capacité.
pub fn build_buffer_write(cmd: Cmd, offset: u16, data: &[u8]) -> Result<Report, ProtocolError> {
    if data.len() > MAX_BUFFER_CHUNK {
        return Err(ProtocolError::ChunkTooLarge {
            size: data.len(),
            max: MAX_BUFFER_CHUNK,
        });
    }
    let mut payload = Vec::with_capacity(BUFFER_HEADER + data.len());
    payload.extend_from_slice(&offset.to_be_bytes());
    payload.push(data.len() as u8);
    payload.extend_from_slice(data);
    build(cmd, &payload)
}

// ---------------------------------------------------------------------------
// Réponses
// ---------------------------------------------------------------------------

/// Un rapport device -> host analysé.
///
/// Contrairement au Python, la longueur est vérifiée à la construction : un
/// rapport vide provoquerait un `IndexError` là-bas, ici c'est
/// [`ProtocolError::EmptyReport`]. Les accès restent tolérants aux rapports
/// tronqués, comme les tranches Python.
#[derive(Clone, PartialEq, Eq)]
pub struct Reply {
    raw: Vec<u8>,
}

impl Reply {
    /// Enveloppe un rapport brut lu sur l'interface de configuration.
    pub fn new(raw: impl Into<Vec<u8>>) -> Result<Self, ProtocolError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(ProtocolError::EmptyReport);
        }
        Ok(Self { raw })
    }

    /// Les octets bruts, longueur d'origine conservée.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Identifiant de commande (octet 0).
    pub fn command_id(&self) -> u8 {
        self.raw[0]
    }

    /// Charge utile : tout sauf l'octet 0.
    pub fn payload(&self) -> &[u8] {
        self.raw.get(PAYLOAD_OFFSET..).unwrap_or(&[])
    }

    /// Partie données d'une réponse de tampon, après le sous-en-tête de 3 octets.
    ///
    /// `size` est l'octet 3 du rapport ; la tranche est tronquée à la longueur
    /// réellement reçue, comme le slicing Python.
    pub fn buffer_data(&self) -> &[u8] {
        let size = self
            .raw
            .get(PAYLOAD_OFFSET + 2)
            .copied()
            .unwrap_or(0) as usize;
        let start = PAYLOAD_OFFSET + BUFFER_HEADER;
        match self.raw.get(start..) {
            Some(tail) => &tail[..size.min(tail.len())],
            None => &[],
        }
    }

    /// Un firmware VIA répond à une commande inconnue en l'écho-ant avec 0xFF.
    pub fn is_unhandled(&self) -> bool {
        self.command_id() == Cmd::UNHANDLED.id()
    }

    /// Vrai pour un rapport de télémétrie poussé sans sollicitation
    /// (`02 96 14 ...`).
    ///
    /// Le bundle web les identifie en comparant les trois premiers octets à
    /// `[id_get_keyboard_value, customId, dataReporting]`.
    pub fn is_data_reporting(&self) -> bool {
        self.raw.len() > 3 && self.raw[0..3] == DATA_REPORTING_PREFIX
    }

    /// Le discriminateur de type à l'octet 3 d'un rapport de télémétrie.
    pub fn stream_type(&self) -> Option<u8> {
        if self.is_data_reporting() {
            self.raw.get(3).copied()
        } else {
            None
        }
    }
}

impl fmt::Debug for Reply {
    /// Même forme que le `__repr__` Python :
    /// `Reply(cmd=NAME, payload=01 02 03 ...)`, huit premiers octets seulement.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match Cmd::from_id(self.command_id()) {
            Some(cmd) => format!("{cmd:?}"),
            None => format!("{:#04x}", self.command_id()),
        };
        let hex: Vec<String> = self
            .payload()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect();
        write!(f, "Reply(cmd={name}, payload={}...)", hex.join(" "))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// Les trames attendues en hexadécimal ne sont pas écrites à la main : elles ont
// été produites par `src/mad68/protocol.py` (import direct du module, sans le
// paquet `mad68`), expression Python indiquée en commentaire pour chaque cas.

#[cfg(test)]
mod tests {
    use super::*;

    /// Décode un hexadécimal en `Report` de 32 octets, en exigeant la longueur.
    fn report(hex: &str) -> Report {
        let bytes = hex_to_vec(hex);
        assert_eq!(bytes.len(), REPORT_SIZE, "longueur de trame inattendue");
        let mut out = [0u8; REPORT_SIZE];
        out.copy_from_slice(&bytes);
        out
    }

    fn hex_to_vec(hex: &str) -> Vec<u8> {
        assert!(hex.len() % 2 == 0, "hexadécimal de longueur impaire");
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hexadécimal invalide"))
            .collect()
    }

    // -- Constantes dérivées ------------------------------------------------

    #[test]
    fn geometry_matches_python_derivations() {
        assert_eq!(MAX_PAYLOAD, 31);
        assert_eq!(MAX_BUFFER_CHUNK, 28);
        assert_eq!(KEYMAP_SIZE, 600);
        assert_eq!(TOTAL_KEYS, 75);
        assert_eq!(keymap_size_for(5, 14, 4), 560);
        assert_eq!(keymap_size_for(5, 15, 4), 600);
        assert_eq!(keymap_size_for(5, 6, 4), 240);
        // Division entière : 23 // 2 = 11 en écriture, pas 11.5.
        assert_eq!(MAX_ACTUATION_READ, 12);
        assert_eq!(MAX_ACTUATION_WRITE, 11);
        assert_eq!(MAX_RT_READ, 4);
        assert_eq!(MAX_RT_WRITE, 4);
    }

    #[test]
    fn keymap_offsets_are_row_major_big_endian_pairs() {
        // ((layer * rows + row) * cols + col) * 2
        assert_eq!(keymap_offset(0, 0, 0, 5, 15), 0);
        assert_eq!(keymap_offset(0, 0, 1, 5, 15), 2);
        assert_eq!(keymap_offset(0, 1, 0, 5, 15), 30);
        assert_eq!(keymap_offset(1, 0, 0, 5, 15), 150);
        assert_eq!(keymap_offset(3, 4, 14, 5, 15), 598);
        // Une carte 5x14 tasse différemment : la ligne 1 commence à 28, pas 30.
        assert_eq!(keymap_offset(0, 1, 0, 5, 14), 28);
    }

    // -- build --------------------------------------------------------------

    #[test]
    fn build_protocol_version_frame() {
        // p.build(p.Cmd.GET_PROTOCOL_VERSION)
        assert_eq!(
            build(Cmd::GET_PROTOCOL_VERSION, &[]).expect("trame valide"),
            report("0100000000000000000000000000000000000000000000000000000000000000"),
        );
    }

    #[test]
    fn build_keyboard_value_frame_places_payload_at_byte_one() {
        // p.build(p.Cmd.GET_KEYBOARD_VALUE, bytes([0x96, 0x41]))
        assert_eq!(
            build(Cmd::GET_KEYBOARD_VALUE, &[0x96, 0x41]).expect("trame valide"),
            report("0296410000000000000000000000000000000000000000000000000000000000"),
        );
    }

    #[test]
    fn build_full_payload_has_no_checksum_and_zero_padding() {
        // p.build(p.Cmd.CUSTOM_SET_VALUE, bytes(range(1, 32)))
        assert_eq!(
            build(Cmd::CUSTOM_SET_VALUE, &(1u8..=31).collect::<Vec<_>>()).expect("trame valide"),
            report("070102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
        );
    }

    #[test]
    fn build_rejects_payload_over_31_bytes() {
        let err = build(Cmd::CUSTOM_SET_VALUE, &[0u8; 32]).expect_err("doit échouer");
        assert_eq!(err, ProtocolError::PayloadTooLarge { len: 32, max: 31 });
        assert_eq!(err.to_string(), "payload 32 exceeds 31");
    }

    // -- build_vendor -------------------------------------------------------

    #[test]
    fn build_vendor_save_lamplight() {
        // p.build_vendor(p.Cmd.SET_KEYBOARD_VALUE, p.Vendor.SAVE_LAMPLIGHT)
        assert_eq!(
            build_vendor(Cmd::SET_KEYBOARD_VALUE, Vendor::SAVE_LAMPLIGHT, &[])
                .expect("trame valide"),
            report("0396120000000000000000000000000000000000000000000000000000000000"),
        );
    }

    #[test]
    fn build_vendor_single_key_actuation() {
        // p.build_vendor(p.Cmd.SET_KEYBOARD_VALUE, p.Vendor.ONE_TOG_TH,
        //                bytes([2, 4]) + p.travel_bytes(1.5))
        assert_eq!(
            build_vendor(
                Cmd::SET_KEYBOARD_VALUE,
                Vendor::ONE_TOG_TH,
                &[2, 4, 0x00, 0x96],
            )
            .expect("trame valide"),
            report("0396030204009600000000000000000000000000000000000000000000000000"),
        );
    }

    #[test]
    fn build_vendor_single_key_rapid_trigger() {
        // p.build_vendor(p.Cmd.SET_KEYBOARD_VALUE, p.Vendor.ONE_RT,
        //                bytes([1, 13, 1]) + travel_bytes(0.3) + travel_bytes(1.2))
        assert_eq!(
            build_vendor(
                Cmd::SET_KEYBOARD_VALUE,
                Vendor::ONE_RT,
                &[1, 13, 1, 0x00, 0x1e, 0x00, 0x78],
            )
            .expect("trame valide"),
            report("039607010d01001e007800000000000000000000000000000000000000000000"),
        );
    }

    #[test]
    fn build_vendor_payload_limit_is_inclusive_of_channel_bytes() {
        // 31 octets de charge utile vendeur : 2 octets de canal + 29 utiles.
        assert!(build_vendor(Cmd::SET_KEYBOARD_VALUE, Vendor::RS, &[0u8; 29]).is_ok());
        // 30 octets appelants font 32 sur le fil : refus, message Python à l'appui.
        let err = build_vendor(Cmd::SET_KEYBOARD_VALUE, Vendor::RS, &[0u8; 30])
            .expect_err("doit échouer");
        assert_eq!(err, ProtocolError::PayloadTooLarge { len: 32, max: 31 });
    }

    // -- build_buffer_* -----------------------------------------------------

    #[test]
    fn build_buffer_read_header() {
        // p.build_buffer_read(p.Cmd.DYNAMIC_KEYMAP_GET_BUFFER, 0x0200, 28)
        assert_eq!(
            build_buffer_read(Cmd::DYNAMIC_KEYMAP_GET_BUFFER, 0x0200, 28).expect("trame valide"),
            report("1202001c00000000000000000000000000000000000000000000000000000000"),
        );
    }

    #[test]
    fn build_buffer_write_header_and_data() {
        // p.build_buffer_write(p.Cmd.DYNAMIC_KEYMAP_SET_BUFFER, 0x0004,
        //                      bytes([0x52, 0x21, 0x29, 0x2C]))
        assert_eq!(
            build_buffer_write(
                Cmd::DYNAMIC_KEYMAP_SET_BUFFER,
                0x0004,
                &[0x52, 0x21, 0x29, 0x2C],
            )
            .expect("trame valide"),
            report("130004045221292c000000000000000000000000000000000000000000000000"),
        );
    }

    #[test]
    fn build_buffer_rejects_chunk_over_28_bytes() {
        let err = build_buffer_read(Cmd::DYNAMIC_KEYMAP_GET_BUFFER, 0, 29).expect_err("doit échouer");
        assert_eq!(err, ProtocolError::ChunkTooLarge { size: 29, max: 28 });
        let err = build_buffer_write(Cmd::DYNAMIC_KEYMAP_SET_BUFFER, 0, &[0u8; 29])
            .expect_err("doit échouer");
        assert_eq!(err, ProtocolError::ChunkTooLarge { size: 29, max: 28 });
    }

    // -- travel -------------------------------------------------------------

    #[test]
    fn travel_encoding_matches_python() {
        // {mm: p.travel_bytes(mm).hex() for mm in [...]}
        let cases = [
            (0.0, [0x00, 0x00]),
            (0.01, [0x00, 0x01]),
            (0.1, [0x00, 0x0a]),
            (0.25, [0x00, 0x19]),
            (0.5, [0x00, 0x32]),
            (1.0, [0x00, 0x64]),
            (1.5, [0x00, 0x96]),
            (2.0, [0x00, 0xc8]),
            (3.5, [0x01, 0x5e]),
            (4.0, [0x01, 0x90]),
            (12.34, [0x04, 0xd2]),
            (100.0, [0x27, 0x10]),
            (655.35, [0xff, 0xff]),
            // Bornes : au-delà de 655,35 mm on sature, en dessous de 0 aussi.
            (700.0, [0xff, 0xff]),
            (-1.0, [0x00, 0x00]),
        ];
        for (mm, bytes) in cases {
            assert_eq!(travel_bytes(mm), bytes, "{mm} mm");
        }
    }

    #[test]
    fn travel_decoding_matches_python() {
        // {raw: p.travel_to_mm(raw) for raw in [...]}
        for (raw, mm) in [
            (0u16, 0.0),
            (1, 0.01),
            (5, 0.05),
            (50, 0.5),
            (100, 1.0),
            (150, 1.5),
            (999, 9.99),
            (65535, 655.35),
        ] {
            assert_eq!(travel_to_mm(raw), mm, "raw {raw}");
            assert_eq!(travel_from_bytes(raw.to_be_bytes()), mm, "raw {raw}");
        }
    }

    #[test]
    fn travel_round_trips_across_the_whole_domain() {
        // Le trajet mm -> raw -> mm doit être l'identité au centième près, avec
        // les mêmes arrondis au pair que Python.
        for raw in 0u16..=u16::MAX {
            let mm = travel_to_mm(raw);
            assert_eq!(mm_to_travel(mm), raw, "raw {raw} -> {mm} mm");
        }
    }

    #[test]
    fn travel_uses_round_half_to_even_not_half_away_from_zero() {
        // 0.005 mm : le double vaut 0.005000000000000000104... donc 0.5 en
        // centièmes, arrondi au pair -> 0. Rust `round()` ferait 1.
        assert_eq!(mm_to_travel(0.005), 0);
        // 1.005 mm : 100.49999999999999 -> 100.
        assert_eq!(mm_to_travel(1.005), 100);
    }

    // -- RGB ----------------------------------------------------------------

    #[test]
    fn rgb_encoding_matches_python() {
        // {f"{r},{g},{b}": p.encode_rgb(r, g, b).hex() for ...}
        for (rgb, wire) in [
            ((0, 0, 0), [0x00, 0x00, 0x00]),
            ((255, 255, 255), [0xff, 0x3b, 0x45]),
            ((255, 0, 0), [0xff, 0x00, 0x00]),
            ((0, 255, 0), [0x00, 0x3b, 0x00]),
            ((0, 0, 255), [0x00, 0x00, 0x45]),
            ((128, 64, 32), [0x80, 0x0f, 0x09]),
            ((10, 200, 3), [0x0a, 0x2e, 0x01]),
            ((255, 10, 255), [0xff, 0x02, 0x45]),
        ] {
            assert_eq!(encode_rgb(rgb.0, rgb.1, rgb.2), wire, "{rgb:?}");
        }
    }

    #[test]
    fn rgb_decoding_matches_python() {
        // {wire: list(p.decode_rgb(bytes.fromhex(wire)))}
        for (wire, rgb) in [
            ([0x00, 0x00, 0x00], [0, 0, 0]),
            ([0xff, 0xff, 0xff], [255, 255, 255]),
            ([0xff, 0x00, 0x00], [255, 0, 0]),
            ([0x00, 0xff, 0x00], [0, 255, 0]),
            ([0x00, 0x00, 0xff], [0, 0, 255]),
            ([0x80, 0x40, 0x20], [128, 255, 119]),
            ([0x0a, 0xc8, 0x03], [10, 255, 11]),
        ] {
            assert_eq!(decode_rgb(wire), rgb, "{wire:?}");
        }
    }

    #[test]
    fn rgb_encode_decode_round_trips_when_gain_is_identity_on_the_wire() {
        // Le gain n'est pas inversible dans tous les cas (saturation), mais pour
        // des valeurs basses il l'est : on vérifie la cohérence des deux sens.
        for value in 0u8..=40 {
            let wire = encode_rgb(value, value, value / 4);
            let back = decode_rgb(wire);
            assert_eq!(back[0], value, "canal rouge de {value}");
        }
    }

    #[test]
    fn led_order_names_cover_the_six_permutations() {
        let mut names: Vec<&str> = [LedOrder::Rgb, LedOrder::Rbg, LedOrder::Grb,
                                   LedOrder::Gbr, LedOrder::Brg, LedOrder::Bgr]
            .iter()
            .map(|o| o.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 6);
        assert_eq!(LedOrder::from_name("GRB"), Some(LedOrder::Grb));
        assert_eq!(LedOrder::from_name("grb"), None);
        // Chaque permutation doit couvrir les trois canaux exactement une fois.
        for order in [LedOrder::Rgb, LedOrder::Rbg, LedOrder::Grb,
                      LedOrder::Gbr, LedOrder::Brg, LedOrder::Bgr] {
            let mut channels = order.channels();
            channels.sort_unstable();
            assert_eq!(channels, [0, 1, 2]);
        }
    }

    // -- Énumérations -------------------------------------------------------

    #[test]
    fn enum_ids_and_aliases_match_python_int_enums() {
        assert_eq!(Cmd::UNHANDLED.id(), 0xFF);
        assert_eq!(Cmd::VIAL_PREFIX.id(), 0xFE);
        assert_eq!(Vendor::IAP.id(), 0x98);
        assert_eq!(Vendor::GET_CUSTOM_LAMPLIGHT.id(), 0x45);
        assert_eq!(FlashOp::EraseAndWrite.id(), 0x03);
        assert_eq!(KeyboardValue::DeviceIndication.id(), 0x05);
        assert_eq!(AdvancedKeyMode::SocdBalance.id(), 0x05);
        assert_eq!(MacroAction::VialMacroExtUp.id(), 0x07);

        // Alias : 0x0D renvoie le premier membre déclaré, comme `Vendor(0x0D)`.
        assert_eq!(Vendor::from_id(0x0D), Some(Vendor::ENTRY_OP));
        assert_eq!(Vendor::BUFFER_TOG_TH.id(), Vendor::ENTRY_OP.id());

        // Tous les identifiants déclarés doivent se relire.
        for cmd in Cmd::ALL {
            assert_eq!(Cmd::from_id(cmd.id()), Some(*cmd));
        }
        for sub in SubField::ALL {
            assert_eq!(SubField::from_id(sub.id()), Some(*sub));
        }
        for op in FlashOp::ALL {
            assert_eq!(FlashOp::from_id(op.id()), Some(*op));
        }
        assert_eq!(Cmd::from_id(0x42), None);
        assert!(matches!(
            Cmd::try_from(0x42),
            Err(ProtocolError::UnknownId { value: 0x42, .. })
        ));
    }

    #[test]
    fn safety_sets_match_python() {
        assert!(is_write_command(Cmd::CUSTOM_SAVE));
        assert!(!is_write_command(Cmd::GET_PROTOCOL_VERSION));
        assert_eq!(WRITE_COMMANDS.len(), 10);
        assert_eq!(DANGEROUS_COMMANDS.len(), 4);
        assert!(is_dangerous_command(Cmd::BOOTLOADER_JUMP));
        assert!(!is_dangerous_command(Cmd::SET_KEYBOARD_VALUE));
        assert_eq!(DANGEROUS_VENDOR.len(), 5);
        assert!(is_dangerous_vendor(Vendor::IAP));
        assert!(is_volatile_keyboard_value(KeyboardValue::Uptime));
        assert!(!is_volatile_keyboard_value(KeyboardValue::FirmwareVersion));
    }

    // -- Reply --------------------------------------------------------------

    #[test]
    fn reply_exposes_payload_and_command() {
        let mut raw = [0u8; REPORT_SIZE];
        raw[0] = Cmd::GET_KEYBOARD_VALUE.id();
        raw[1] = CUSTOM_CHANNEL;
        raw[2] = Vendor::ONE_TOG_TH.id();
        raw[3] = 2;
        raw[4] = 4;
        raw[5] = 0x00;
        raw[6] = 0x96;

        let reply = Reply::new(raw).expect("rapport non vide");
        assert_eq!(reply.command_id(), Cmd::GET_KEYBOARD_VALUE.id());
        assert_eq!(&reply.payload()[..6], &[0x96, 0x03, 2, 4, 0x00, 0x96]);
        assert!(!reply.is_unhandled());
        assert!(!reply.is_data_reporting());
        assert_eq!(reply.stream_type(), None);
        assert_eq!(
            format!("{reply:?}"),
            "Reply(cmd=GET_KEYBOARD_VALUE, payload=96 03 02 04 00 96 00 00...)"
        );
    }

    #[test]
    fn reply_buffer_data_uses_the_header_size_and_clamps_to_the_report() {
        // [cmd, off_hi, off_lo, size, data...]
        let mut raw = [0u8; REPORT_SIZE];
        raw[0] = Cmd::DYNAMIC_KEYMAP_GET_BUFFER.id();
        raw[1] = 0x02;
        raw[2] = 0x00;
        raw[3] = 4;
        raw[4..8].copy_from_slice(&[0x52, 0x21, 0x29, 0x2C]);
        let reply = Reply::new(raw).expect("rapport non vide");
        assert_eq!(reply.buffer_data(), &[0x52, 0x21, 0x29, 0x2C]);

        // `size` qui dépasse le rapport : la tranche est tronquée à ce qui a
        // réellement été reçu, exactement comme le slicing Python.
        let mut raw = [0u8; REPORT_SIZE];
        raw[3] = 0xFF;
        raw[4] = 0xAB;
        let reply = Reply::new(raw).expect("rapport non vide");
        assert_eq!(reply.buffer_data(), &raw[4..]);

        // Rapport plus court que l'en-tête : aucune donnée, pas de panique.
        let reply = Reply::new(vec![0x12, 0x00]).expect("rapport non vide");
        assert!(reply.buffer_data().is_empty());
    }

    #[test]
    fn reply_detects_unhandled_and_telemetry() {
        let reply = Reply::new([0xFFu8; REPORT_SIZE]).expect("rapport non vide");
        assert!(reply.is_unhandled());

        let mut raw = [0u8; REPORT_SIZE];
        raw[..4].copy_from_slice(&[0x02, CUSTOM_CHANNEL, 0x14, 0x07]);
        let reply = Reply::new(raw).expect("rapport non vide");
        assert!(reply.is_data_reporting());
        assert_eq!(reply.stream_type(), Some(0x07));

        assert!(matches!(
            Reply::new(Vec::new()),
            Err(ProtocolError::EmptyReport)
        ));
    }
}
