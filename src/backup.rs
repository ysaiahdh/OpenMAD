//! Sauvegarde complète de la mémoire embarquée pour claviers magnétiques DuckBread / MAD68.
//!
//! Traduction directe de `tools/backup.py`.
//!
//! Sécurité et intégrité :
//! - Le transport USB est toujours ouvert en lecture seule (`writes = false`).
//! - Les données sont capturées sous deux formes : brutes (hexadécimal pour restauration exacte)
//!   et décodées (pour comparaison humaine et inspection).
//! - Sauvegarde horodatée dans le dossier `backups/mad68-backup-<UTC timestamp>.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use clap::Args;
use serde_json::json;

use crate::device::{DeviceError, Mad68};
use crate::protocol::{KeyboardValue, Vendor, VOLATILE_KEYBOARD_VALUES};
use crate::updater::sha256_hex;

pub const BACKUP_VERSION: u32 = 2;
pub const ADVANCED_KEY_SLOTS: u8 = 16;
pub const DKS_SLOTS: u8 = 16;

pub const SCALAR_VENDOR: &[Vendor] = &[
    Vendor::DEAD_BAND,
    Vendor::FEATURE,
    Vendor::GAME_MODE,
    Vendor::BOTTOM_OPTIMIZE_SWITCH,
    Vendor::LIGHT_INFO,
    Vendor::LAYER,
    Vendor::RS,
    Vendor::BOX_LIGHT,
    Vendor::MIX_AXLE,
    Vendor::GET_CUSTOM_LAMPLIGHT,
];

/// Arguments CLI pour la commande `backup`.
#[derive(Debug, Clone, Args)]
pub struct BackupArgs {
    /// Relire le périphérique et le comparer au fichier de sauvegarde spécifié.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["show", "out"])]
    pub verify: Option<PathBuf>,

    /// Afficher le résumé d'un fichier de sauvegarde sans toucher au matériel.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["verify", "out"])]
    pub show: Option<PathBuf>,

    /// Chemin de sortie explicite pour le nouveau fichier de sauvegarde JSON.
    #[arg(long, value_name = "FILE")]
    pub out: Option<PathBuf>,
}

/// Capture l'intégralité de la configuration embarquée du clavier.
pub fn capture(kb: &mut Mad68) -> Result<serde_json::Value, DeviceError> {
    let keymap = kb.read_keymap()?;
    let macro_buf_size = kb.macro_buffer_size().unwrap_or(0);
    let macros = kb.read_macros().unwrap_or_default();

    let mut actuation = Vec::new();
    let mut rapid_trigger = Vec::new();

    for (row, col) in kb.iter_keys() {
        let act_res = kb.vendor_get(Vendor::ONE_TOG_TH, &[row, col]);
        let rt_res = kb.vendor_get(Vendor::ONE_RT, &[row, col]);

        let act_payload = act_res.as_ref().map(|r| r.payload()).unwrap_or(&[]);
        let act_mm = if act_payload.len() >= 6 {
            let u16_val = u16::from_be_bytes([act_payload[4], act_payload[5]]);
            (u16_val as f64 * 0.01 * 100.0).round() / 100.0
        } else {
            0.0
        };

        actuation.push(json!({
            "row": row,
            "col": col,
            "mm": act_mm,
            "raw": bytes_to_hex(act_payload)
        }));

        let rt_payload = rt_res.as_ref().map(|r| r.payload()).unwrap_or(&[]);
        let (enabled, press_mm, rel_mm) = if rt_payload.len() >= 9 {
            let on = rt_payload[4] != 0;
            let rel = (u16::from_be_bytes([rt_payload[5], rt_payload[6]]) as f64 * 0.01 * 100.0).round() / 100.0;
            let press = (u16::from_be_bytes([rt_payload[7], rt_payload[8]]) as f64 * 0.01 * 100.0).round() / 100.0;
            (on, press, rel)
        } else {
            (false, 0.0, 0.0)
        };

        rapid_trigger.push(json!({
            "row": row,
            "col": col,
            "enabled": enabled,
            "press_mm": press_mm,
            "release_mm": rel_mm,
            "raw": bytes_to_hex(rt_payload)
        }));
    }

    let mut keyboard_values = BTreeMap::new();
    let mut volatile_values = BTreeMap::new();
    for &kv in KeyboardValue::ALL {
        let hexed = match kb.keyboard_value(kv.id()) {
            Ok(rep) => bytes_to_hex(rep.payload()),
            Err(e) => format!("ERROR: {e}"),
        };
        let name = format!("{kv:?}");
        if VOLATILE_KEYBOARD_VALUES.contains(&kv) {
            volatile_values.insert(name, hexed);
        } else {
            keyboard_values.insert(name, hexed);
        }
    }

    let mut vendor_scalars = BTreeMap::new();
    for &sub in SCALAR_VENDOR {
        let hexed = match kb.vendor_get(sub, &[]) {
            Ok(rep) => bytes_to_hex(rep.payload()),
            Err(e) => format!("ERROR: {e}"),
        };
        vendor_scalars.insert(format!("{sub:?}"), hexed);
    }

    let mut dks_probe = Vec::new();
    for (row, col) in kb.iter_keys() {
        match kb.vendor_get(Vendor::DKS, &[row, col]) {
            Ok(rep) => dks_probe.push(json!({
                "row": row,
                "col": col,
                "raw": bytes_to_hex(rep.payload())
            })),
            Err(e) => dks_probe.push(json!({
                "row": row,
                "col": col,
                "error": e.to_string()
            })),
        }
    }

    let mut features = json!({});
    if let Ok(db) = kb.read_dead_band() {
        features["dead_band"] = db.to_json();
    }
    if let Ok(ff) = kb.read_feature() {
        features["feature_flags"] = ff.to_json();
    }
    if let Ok(gm) = kb.read_game_mode() {
        features["game_mode"] = json!(gm);
    }
    if let Ok(bo) = kb.read_bottom_optimize() {
        features["bottom_optimize"] = json!(bo);
    }
    if let Ok(bl) = kb.read_box_light() {
        features["box_light"] = bl.to_json();
    }
    if let Ok(li) = kb.read_light_info() {
        features["light_info"] = li.to_json();
    }
    if let Ok(kc) = kb.read_all_key_colors() {
        features["key_colors"] = json!(kc);
    }

    let mut advanced_keys = Vec::new();
    for i in 0..ADVANCED_KEY_SLOTS {
        match kb.read_advanced_key(i) {
            Ok(ak) => advanced_keys.push(ak.to_json()),
            Err(e) => advanced_keys.push(json!({"index": i, "error": e.to_string()})),
        }
    }

    let mut dks_entries = Vec::new();
    for i in 0..DKS_SLOTS {
        match kb.read_dks(i) {
            Ok(d) => dks_entries.push(d.to_json()),
            Err(e) => dks_entries.push(json!({"index": i, "error": e.to_string()})),
        }
    }

    let macros_decoded = match kb.read_macro_steps() {
        Ok(steps) => json!(steps.iter().map(|s| s.iter().map(|step| step.to_json()).collect::<Vec<_>>()).collect::<Vec<_>>()),
        Err(e) => json!([{"error": e.to_string()}]),
    };

    let now_utc = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    Ok(json!({
        "backup_version": BACKUP_VERSION,
        "captured_utc": format!("{now_utc}"),
        "host": {
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH
        },
        "device": {
            "vendor_id": kb.info.vendor_id,
            "product_id": kb.info.product_id,
            "manufacturer": kb.info.manufacturer_string,
            "product": kb.info.product_string,
            "serial": kb.info.serial_number,
            "protocol_version": kb.firmware_version().unwrap_or(0),
            "layer_count": kb.layer_count().unwrap_or(0),
            "macro_count": kb.macro_count().unwrap_or(0),
            "macro_buffer_size": macro_buf_size,
        },
        "geometry": {
            "board": kb.spec.name.to_string(),
            "layers": kb.spec.layers,
            "rows": kb.matrix_rows(),
            "cols": kb.matrix_cols(),
        },
        "keymap": {
            "size": keymap.len(),
            "sha256": sha256_hex(&keymap),
            "hex": bytes_to_hex(&keymap)
        },
        "macros": {
            "size": macros.len(),
            "sha256": sha256_hex(&macros),
            "hex": bytes_to_hex(&macros)
        },
        "actuation": actuation,
        "rapid_trigger": rapid_trigger,
        "keyboard_values": keyboard_values,
        "volatile_observations": volatile_values,
        "vendor_scalars": vendor_scalars,
        "features": features,
        "advanced_keys": advanced_keys,
        "dks": dks_entries,
        "macros_decoded": macros_decoded,
        "dks_probe": dks_probe,
    }))
}

/// Affiche un résumé structuré du contenu d'une sauvegarde.
pub fn summarise(data: &serde_json::Value) {
    let dev = &data["device"];
    let prod = dev["product"].as_str().unwrap_or("Périphérique inconnu");
    let serial = dev["serial"].as_str().unwrap_or("inconnu");
    let proto = dev["protocol_version"].as_u64().unwrap_or(0);

    println!("  device:    {prod} (série '{serial}'), protocole VIA {proto}");
    if let (Some(size), Some(hash)) = (data["keymap"]["size"].as_u64(), data["keymap"]["sha256"].as_str()) {
        let short_hash = if hash.len() >= 16 { &hash[..16] } else { hash };
        println!("  keymap:    {size} octets, sha256 {short_hash}...");
    }
    if let (Some(size), Some(hash)) = (data["macros"]["size"].as_u64(), data["macros"]["sha256"].as_str()) {
        let short_hash = if hash.len() >= 16 { &hash[..16] } else { hash };
        println!("  macros:    {size} octets, sha256 {short_hash}...");
    }

    if let Some(act) = data["actuation"].as_array() {
        let first_mm = act.first().and_then(|a| a["mm"].as_f64()).unwrap_or(1.50);
        let diff_count = act.iter().filter(|a| a["mm"].as_f64().map_or(false, |m| (m - first_mm).abs() > 1e-4)).count();
        println!("  actuation: {} touches ({} diffèrent de la touche 0,0 à {first_mm:.2} mm)", act.len(), diff_count);
    }

    if let Some(rt) = data["rapid_trigger"].as_array() {
        let on_count = rt.iter().filter(|r| r["enabled"].as_bool().unwrap_or(false)).count();
        println!("  rapid trigger: {}/{} touches activées", on_count, rt.len());
    }
}

/// Compare deux captures JSON et affiche les différences. Retourne le nombre total de divergences.
pub fn compare(old: &serde_json::Value, new: &serde_json::Value) -> usize {
    let mut diffs = 0;

    for section in &["keymap", "macros"] {
        let old_h = old[section]["sha256"].as_str().unwrap_or("");
        let new_h = new[section]["sha256"].as_str().unwrap_or("");
        if old_h != new_h {
            println!("  DIFF {section} : {old_h} -> {new_h}");
            diffs += 1;
        } else {
            let size = old[section]["size"].as_u64().unwrap_or(0);
            println!("  ok   {section} ({size} octets, sha256 identique)");
        }
    }

    for section in &["actuation", "rapid_trigger"] {
        let old_arr = old[section].as_array();
        let new_arr = new[section].as_array();
        let mut section_diff = 0;
        if let (Some(oa), Some(na)) = (old_arr, new_arr) {
            for (o, n) in oa.iter().zip(na.iter()) {
                if o["raw"] != n["raw"] {
                    let r = o["row"].as_u64().unwrap_or(0);
                    let c = o["col"].as_u64().unwrap_or(0);
                    println!("  DIFF {section} row {r} col {c} : {} -> {}", o["raw"], n["raw"]);
                    section_diff += 1;
                }
            }
        }
        if section_diff > 0 {
            diffs += section_diff;
        } else {
            println!("  ok   {section} (entrées identiques)");
        }
    }

    diffs
}

/// Point d'entrée de la commande `backup`.
pub fn run(args: &BackupArgs) -> anyhow::Result<i32> {
    if let Some(ref path) = args.show {
        let content = std::fs::read_to_string(path)?;
        let data: serde_json::Value = serde_json::from_str(&content)?;
        println!("Sauvegarde {} :", path.display());
        summarise(&data);
        return Ok(0);
    }

    let mut kb = Mad68::new(false, false, 1000, None, false);
    kb.open()?;
    run_with_device(&mut kb, args)
}

/// Version testable avec un gestionnaire `Mad68` déjà ouvert (mock ou physique).
pub fn run_with_device(kb: &mut Mad68, args: &BackupArgs) -> anyhow::Result<i32> {
    if let Some(ref path) = args.verify {
        let bytes = std::fs::read(path)?;
        let slice = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { &bytes[3..] } else { &bytes[..] };
        let old: serde_json::Value = serde_json::from_slice(slice)?;

        println!("Re-lecture du périphérique pour vérification par rapport à {} ...", path.display());
        let new = capture(kb)?;
        println!();
        let diffs = compare(&old, &new);
        println!();
        if diffs > 0 {
            println!("{diffs} différence(s) détectée(s) — le périphérique ne correspond plus à la sauvegarde.");
            return Ok(1);
        }
        println!("Le périphérique correspond exactement à la sauvegarde.");
        return Ok(0);
    }

    let out_path = if let Some(ref p) = args.out {
        p.clone()
    } else {
        let backup_dir = crate::paths::data_dir().join("backups");
        std::fs::create_dir_all(&backup_dir)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        backup_dir.join(format!("mad68-backup-{stamp}.json"))
    };

    if out_path.exists() {
        eprintln!("Refus d'écraser la sauvegarde existante {}", out_path.display());
        return Ok(2);
    }

    println!("Lecture complète de la configuration embarquée (lecture seule) ...");
    let data = capture(kb)?;
    let json_str = serde_json::to_string_pretty(&data)?;
    std::fs::write(&out_path, json_str)?;

    println!("\nSauvegarde enregistrée dans {} ({} octets)\n", out_path.display(), std::fs::metadata(&out_path)?.len());
    summarise(&data);
    println!("\nVérification possible à tout moment avec :\n  openmad-rs backup --verify {}", out_path.display());
    Ok(0)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{:02x}", b);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::mock::MockHidTransport;
    use crate::devices::default_board;
    use crate::protocol::Cmd;

    #[test]
    fn backup_show_and_compare_mock() {
        let mut kb = Mad68::new(false, false, 500, None, true);
        let mock = MockHidTransport::new();
        kb.open_with_transport(Box::new(mock.clone()), default_board().clone()).unwrap();

        // Répond à la lecture de la keymap
        let mut km_rep = [0u8; 32];
        km_rep[0] = Cmd::DYNAMIC_KEYMAP_GET_BUFFER.id();
        km_rep[3] = 28;
        mock.queue_response(&km_rep);

        let data = json!({
            "keymap": { "size": 600, "sha256": "abc12345" },
            "macros": { "size": 100, "sha256": "def67890" },
            "actuation": [],
            "rapid_trigger": [],
            "device": { "product": "MAD68 HE RGB", "serial": "123", "protocol_version": 9 }
        });

        // Test summarise sans panique
        summarise(&data);

        // Test compare identique
        assert_eq!(compare(&data, &data), 0);
    }
}
