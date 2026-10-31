//! Noms de keycodes QMK, assez pour étiqueter une keymap de façon lisible.
//!
//! Traduction de `src/mad68/keycodes.py`. Volontairement non exhaustif : la
//! plage d'usages HID de base plus les plages spéciales QMK que ce clavier
//! utilise réellement (touches de couche, modificateurs). Tout ce qui n'est pas
//! reconnu est rendu en hexadécimal plutôt que deviné — une étiquette fausse
//! est pire qu'une étiquette absente quand on diagnostique une keymap.

use std::collections::HashMap;

use crate::protocol::keymap_offset;

/// Lettres A..Z, usages HID 0x04..=0x1D (dans l'ordre alphabétique).
const LETTERS: [&str; 26] = [
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S",
    "T", "U", "V", "W", "X", "Y", "Z",
];

/// Chiffres de la rangée numérique, usages HID 0x1E..=0x27 : « 1 » à « 9 » puis
/// « 0 », l'ordre physique du clavier et non l'ordre numérique.
const DIGITS: [&str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"];

/// F1..F24 en continu : F1..F12 occupent 0x3A..=0x45, F13..F24 occupent
/// 0x68..=0x73. Le saut de 0x45 à 0x68 correspond aux usages HID réservés, donc
/// la table reste contiguë côté étiquettes.
const FUNCTION_KEYS: [&str; 24] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
];

/// Plages 16 bits de QMK pour les touches de couche, disposition moderne
/// (post-refonte des keycodes), espacées de 0x20.
///
/// Confirmé sur cet appareil : la touche Fn en ligne 4 colonne 10 renvoie
/// 0x5221, soit `MO(1)` dans ce schéma. Cela correspond aussi à
/// `Tu.layer = 0x52` dans le bundle.
const LAYER_RANGES: [(u16, &str); 5] = [
    (0x5200, "TO"),
    (0x5220, "MO"),
    (0x5240, "DF"),
    (0x5260, "TG"),
    (0x5280, "OSL"),
];

/// Largeur d'une plage de couche : 0x5200..=0x521F, soit 32 couches.
const LAYER_SPAN: u16 = 0x1F;

/// Nom court d'un usage HID de base, `None` si l'usage n'est pas tabulé.
pub fn basic(keycode: u8) -> Option<&'static str> {
    Some(match keycode {
        0x00 => "----", // KC_NO
        0x01 => "XXXX", // KC_TRNS / transparent
        // Lettres : index = usage - 0x04, dans l'ordre alphabétique.
        0x04..=0x1D => LETTERS[(keycode - 0x04) as usize],
        // Chiffres : 1..9 puis 0.
        0x1E..=0x27 => DIGITS[(keycode - 0x1E) as usize],
        0x28 => "Enter",
        0x29 => "Esc",
        0x2A => "Bspc",
        0x2B => "Tab",
        0x2C => "Space",
        0x2D => "-",
        0x2E => "=",
        0x2F => "[",
        0x30 => "]",
        0x31 => "\\",
        0x32 => "#",
        0x33 => ";",
        0x34 => "'",
        0x35 => "`",
        0x36 => ",",
        0x37 => ".",
        0x38 => "/",
        0x39 => "Caps",
        0x46 => "PrtSc",
        0x47 => "ScrLk",
        0x48 => "Pause",
        0x49 => "Ins",
        0x4A => "Home",
        0x4B => "PgUp",
        0x4C => "Del",
        0x4D => "End",
        0x4E => "PgDn",
        0x4F => "Rgt",
        0x50 => "Lft",
        0x51 => "Dn",
        0x52 => "Up",
        0x65 => "App",
        0xE0 => "LCtl",
        0xE1 => "LSft",
        0xE2 => "LAlt",
        0xE3 => "LGui",
        0xE4 => "RCtl",
        0xE5 => "RSft",
        0xE6 => "RAlt",
        0xE7 => "RGui",
        0x3A..=0x45 => FUNCTION_KEYS[(keycode - 0x3A) as usize],
        0x68..=0x73 => FUNCTION_KEYS[(keycode - 0x68 + 12) as usize],
        _ => return None,
    })
}

/// Nom court et lisible d'un keycode QMK 16 bits.
///
/// Ordre des cas identique au Python, et c'est important :
///  1. tout ce qui tient dans un octet passe par la table HID de base (donc
///     0x01 est « XXXX » et pas autre chose) ;
///  2. les plages de couche 16 bits `TO/MO/DF/TG/OSL(n)` ;
///  3. sinon l'hexadécimal, sur 4 chiffres.
pub fn label(keycode: u16) -> String {
    if keycode <= 0xFF {
        return match basic(keycode as u8) {
            Some(name) => name.to_owned(),
            // `{:#04x}` en Python comme en Rust : « 0x » + deux chiffres.
            None => format!("{keycode:#04x}"),
        };
    }
    for (base, prefix) in LAYER_RANGES {
        if (base..=base + LAYER_SPAN).contains(&keycode) {
            return format!("{prefix}({})", keycode - base);
        }
    }
    format!("{keycode:#06x}")
}

/// Lit un keycode big-endian à un décalage donné.
///
/// Le Python écrit `int.from_bytes(keymap[off:off + 2], "big")` : une tranche
/// hors bornes est silencieusement raccourcie, donc un tampon tronqué donne un
/// keycode sur un octet, voire 0. On reproduit exactement ça au lieu de
/// paniquer, l'appelant décide ensuite s'il veut traiter le cas comme une erreur.
fn keycode_at(keymap: &[u8], offset: usize) -> u16 {
    match (keymap.get(offset), keymap.get(offset + 1)) {
        (Some(hi), Some(lo)) => u16::from_be_bytes([*hi, *lo]),
        (Some(only), None) => u16::from(*only),
        _ => 0,
    }
}

/// Découpe un tampon de keymap brut en lignes de keycodes 16 bits pour une couche.
///
/// Le pas de ligne est le nombre de colonnes **de la carte** : tout appelant qui
/// tient un appareil doit passer ses propres rows/cols. Lire un tampon de carte
/// 14 colonnes avec un pas de 15 décale chaque ligne après la première.
///
/// Contrairement au Python, il n'y a pas de géométrie par défaut ici : Rust n'a
/// pas d'arguments nommés par défaut, et prendre silencieusement 5x15 est
/// précisément le bug que ce paramètre existe pour éviter. Passer
/// `(MATRIX_ROWS, MATRIX_COLS)` de [`crate::protocol`] reproduit les valeurs par
/// défaut du Python.
pub fn decode_keymap(keymap: &[u8], layer: usize, rows: usize, cols: usize) -> Vec<Vec<u16>> {
    let mut out = Vec::with_capacity(rows);
    for row in 0..rows {
        let mut cells = Vec::with_capacity(cols);
        for col in 0..cols {
            cells.push(keycode_at(keymap, keymap_offset(layer, row, col, rows, cols)));
        }
        out.push(cells);
    }
    out
}

/// Grille texte d'une couche, avec annotation optionnelle de positions.
///
/// Mise en page identique au Python : une ligne d'en-tête de numéros de
/// colonnes préfixée de six espaces, puis `  r<ligne>  ` suivi des cellules
/// cadrées à droite sur `width` caractères. Une position annotée voit son texte
/// suffixé d'une astérisque, comme la carte de calibration de `hud.rs`.
pub fn render_matrix(
    keymap: &[u8],
    layer: usize,
    annotate: Option<&HashMap<(usize, usize), String>>,
    width: usize,
    rows: usize,
    cols: usize,
) -> String {
    let grid = decode_keymap(keymap, layer, rows, cols);
    let mut lines = Vec::with_capacity(rows + 1);

    let mut header = String::from("      ");
    for col in 0..cols {
        header.push_str(&format!("{col:>width$}"));
    }
    lines.push(header);

    for (row, cells) in grid.iter().enumerate() {
        let mut line = format!("  r{row}  ");
        for (col, keycode) in cells.iter().enumerate() {
            let mut text = label(*keycode);
            if annotate.is_some_and(|map| map.contains_key(&(row, col))) {
                text.push('*');
            }
            // Cadrage à droite appliqué après l'astérisque, comme en Python.
            line.push_str(&format!("{text:>width$}"));
        }
        lines.push(line);
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
//
// Les étiquettes et la grille de référence ont été produites par
// `src/mad68/keycodes.py` (appel `label`, `render_matrix` et `decode_keymap`).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_match_python_for_the_basic_hid_range() {
        // {hex(kc): k.label(kc) for kc in [...]}
        let cases = [
            (0x00, "----"),
            (0x01, "XXXX"),
            (0x04, "A"),
            (0x0A, "G"),
            (0x1D, "Z"),
            (0x1E, "1"),
            (0x27, "0"),
            (0x28, "Enter"),
            (0x29, "Esc"),
            (0x35, "`"),
            (0x39, "Caps"),
            (0x3A, "F1"),
            (0x45, "F12"),
            (0x46, "PrtSc"),
            (0x52, "Up"),
            (0x65, "App"),
            (0x68, "F13"),
            (0x73, "F24"),
            (0xE0, "LCtl"),
            (0xE7, "RGui"),
            // Usage non tabulé : hexadécimal, pas d'invention.
            (0xFF, "0xff"),
        ];
        for (keycode, expected) in cases {
            assert_eq!(label(keycode), expected, "keycode {keycode:#06x}");
        }
    }

    #[test]
    fn labels_cover_the_qmk_layer_ranges() {
        // {hex(kc): k.label(kc) for kc in [...]}
        let cases = [
            (0x5200, "TO(0)"),
            (0x5221, "MO(1)"),
            (0x5222, "MO(2)"),
            (0x523F, "MO(31)"),
            (0x5240, "DF(0)"),
            (0x5261, "TG(1)"),
            (0x5280, "OSL(0)"),
            (0x529F, "OSL(31)"),
            // Juste au-dessus de la dernière plage : plus aucune correspondance.
            (0x52A0, "0x52a0"),
            (0x0100, "0x0100"),
            (0xFFFF, "0xffff"),
            (0x1234, "0x1234"),
        ];
        for (keycode, expected) in cases {
            assert_eq!(label(keycode), expected, "keycode {keycode:#06x}");
        }
    }

    #[test]
    fn every_basic_usage_is_either_named_or_hex() {
        // Aucune panique, et le nom hexadécimal reste stable sur toute la plage.
        for keycode in 0u16..=0x1FF {
            let text = label(keycode);
            assert!(!text.is_empty());
            if keycode <= 0xFF && basic(keycode as u8).is_none() {
                assert_eq!(text, format!("{keycode:#04x}"));
            }
        }
    }

    #[test]
    fn decode_keymap_reads_big_endian_pairs_row_major() {
        // k.decode_keymap(bytes(range(64)), 0, 3, 4)
        let keymap: Vec<u8> = (0u8..64).collect();
        assert_eq!(
            decode_keymap(&keymap, 0, 3, 4),
            vec![
                vec![1, 515, 1029, 1543],
                vec![2057, 2571, 3085, 3599],
                vec![4113, 4627, 5141, 5655],
            ]
        );
    }

    #[test]
    fn decode_keymap_tolerates_truncated_buffers_like_python_slices() {
        // Un seul octet disponible : keycode sur 8 bits, comme int.from_bytes.
        assert_eq!(decode_keymap(&[0x52], 0, 1, 1), vec![vec![0x52]]);
        // Tampon vide : des zéros, sans panique.
        assert_eq!(decode_keymap(&[], 0, 1, 2), vec![vec![0, 0]]);
        // Décalage exactement à la fin : fin de tampon, keycode 0.
        assert_eq!(decode_keymap(&[0xAA, 0xBB], 0, 2, 1), vec![vec![0xAABB], vec![0]]);
    }

    #[test]
    fn render_matrix_matches_python_layout() {
        // k.render_matrix(bytes(range(64)), 0, rows=3, cols=4,
        //                annotate={(1, 2): "x"})
        let keymap: Vec<u8> = (0u8..64).collect();
        let mut annotate = HashMap::new();
        annotate.insert((1usize, 2usize), "x".to_owned());
        assert_eq!(
            render_matrix(&keymap, 0, Some(&annotate), 8, 3, 4),
            "             0       1       2       3\n  \
             r0      XXXX  0x0203  0x0405  0x0607\n  \
             r1    0x0809  0x0a0b 0x0c0d*  0x0e0f\n  \
             r2    0x1011  0x1213  0x1415  0x1617"
        );
    }

    #[test]
    fn render_matrix_without_annotation_has_no_asterisk() {
        let keymap: Vec<u8> = (0u8..64).collect();
        let text = render_matrix(&keymap, 0, None, 8, 2, 2);
        assert!(!text.contains('*'));
        assert_eq!(text.lines().count(), 3);
    }

    #[test]
    fn default_geometry_of_the_development_board_is_5x15() {
        // Les valeurs par défaut du Python : MATRIX_ROWS x MATRIX_COLS.
        use crate::protocol::{MATRIX_COLS, MATRIX_ROWS};
        assert_eq!((MATRIX_ROWS, MATRIX_COLS), (5, 15));
        // 600 octets de keymap remplissent bien 4 couches de 5x15 keycodes.
        let keymap = vec![0u8; 600];
        let layer = decode_keymap(&keymap, 3, MATRIX_ROWS, MATRIX_COLS);
        assert_eq!(layer.len(), MATRIX_ROWS);
        assert_eq!(layer[0].len(), MATRIX_COLS);
        assert_eq!(layer[4][14], 0x0000);
    }
}
