//! Restauration de la configuration matérielle depuis un fichier de sauvegarde JSON.
//!
//! Traduction directe de `tools/restore.py`.
//!
//! Sécurité et intégrité :
//! - Par défaut, fonctionne en simulation sans risque (`dry-run`) : lit le clavier, calcule
//!   le diff et affiche les modifications prévues sans jamais écrire.
//! - Le drapeau `--apply` est indispensable pour déverrouiller les écritures USB.
//! - Validation stricte du numéro de série du matériel pour éviter de restaurer la sauvegarde
//!   d'un autre clavier.
//! - Ordre d'écriture rigoureux : les touches avancées (SOCD) sont appliquées AVANT les points
//!   d'actionnement pour empêcher l'écrasement de ces touches par le matériel.

use std::path::PathBuf;
use clap::Args;

use crate::device::{DeviceError, Mad68, RapidTrigger};
use crate::features::{
    AdvancedKey, BoxLight, DeadBand, Dks, DksAction, KeyboardFeature, LightInfo,
};
use crate::protocol::{AdvancedKeyMode, Cmd};

pub const UNCOVERED_NOTE: &str = "mix axle et tap dance (schémas d'écriture non confirmés). Tout le reste d'une sauvegarde v2 est restauré.";

/// Arguments CLI pour la commande `restore`.
#[derive(Debug, Clone, Args)]
pub struct RestoreArgs {
    /// Chemin vers le fichier de sauvegarde JSON généré par `backup`.
    #[arg(value_name = "BACKUP")]
    pub backup: PathBuf,

    /// Écrire réellement les modifications sur le clavier (sinon simulation / dry-run).
    #[arg(long)]
    pub apply: bool,
}

/// Plan de restauration détaillant les différences à appliquer.
#[derive(Debug, Default)]
pub struct RestorePlan {
    pub keymap: Option<Vec<u8>>,
    pub macros: Option<Vec<u8>>,
    pub actuation: Vec<(u8, u8, f64)>,
    pub rapid_trigger: Vec<RapidTrigger>,
    pub dead_band: Option<DeadBand>,
    pub feature_flags: Option<KeyboardFeature>,
    pub game_mode: Option<u8>,
    pub bottom_optimize: Option<u8>,
    pub box_light: Option<BoxLight>,
    pub light_info: Option<LightInfo>,
    pub key_colors: Option<Vec<[u8; 3]>>,
    pub advanced_keys: Vec<AdvancedKey>,
    pub dks: Vec<Dks>,
}

/// Compare le clavier physique avec les données de la sauvegarde et produit le plan de restauration.
pub fn plan(kb: &mut Mad68, backup: &serde_json::Value) -> Result<RestorePlan, DeviceError> {
    let mut changes = RestorePlan::default();

    // 1. Keymap dynamique
    if let Some(hex) = backup["keymap"]["hex"].as_str() {
        let want_keymap = hex_to_bytes(hex);
        if want_keymap.len() != kb.keymap_size() {
            return Err(DeviceError::InvalidArgument(format!(
                "la keymap de sauvegarde fait {} octets, mais la {} attend {} octets ; carte différente.",
                want_keymap.len(),
                kb.spec.name,
                kb.keymap_size()
            )));
        }
        if kb.read_keymap()? != want_keymap {
            changes.keymap = Some(want_keymap);
        }
    }

    // 2. Buffer de macros
    if let Some(hex) = backup["macros"]["hex"].as_str() {
        let want_macros = hex_to_bytes(hex);
        let have_macros = kb.read_macros().unwrap_or_default();
        if have_macros != want_macros {
            if want_macros.len() != have_macros.len() {
                return Err(DeviceError::InvalidArgument(format!(
                    "taille du buffer de macros incohérente : sauvegarde {}, clavier {}",
                    want_macros.len(),
                    have_macros.len()
                )));
            }
            changes.macros = Some(want_macros);
        }
    }

    // 3. Actionnement par touche
    if let Some(act) = backup["actuation"].as_array() {
        for entry in act {
            let row = entry["row"].as_u64().unwrap_or(0) as u8;
            let col = entry["col"].as_u64().unwrap_or(0) as u8;
            let mm = entry["mm"].as_f64().unwrap_or(1.50);
            if let Ok(cur_mm) = kb.read_actuation_mm(row, col) {
                if (cur_mm - mm).abs() > 1e-4 {
                    changes.actuation.push((row, col, mm));
                }
            }
        }
    }

    // 4. Rapid Trigger par touche
    if let Some(rt) = backup["rapid_trigger"].as_array() {
        for entry in rt {
            let row = entry["row"].as_u64().unwrap_or(0) as u8;
            let col = entry["col"].as_u64().unwrap_or(0) as u8;
            let enabled = entry["enabled"].as_bool().unwrap_or(false);
            let press_mm = entry["press_mm"].as_f64().unwrap_or(0.50);
            let release_mm = entry["release_mm"].as_f64().unwrap_or(0.50);

            let want_rt = RapidTrigger {
                row,
                col,
                enabled,
                release_mm,
                press_mm,
            };

            if let Ok(cur_rt) = kb.read_rapid_trigger(row, col) {
                if cur_rt != want_rt {
                    changes.rapid_trigger.push(want_rt);
                }
            }
        }
    }

    // 5. Fonctions structurées (sauvegardes v2+)
    let feats = &backup["features"];
    if !feats.is_null() {
        if let (Some(top), Some(bot)) = (feats["dead_band"]["top_mm"].as_f64(), feats["dead_band"]["bottom_mm"].as_f64()) {
            let want_db = DeadBand { top_mm: top, bottom_mm: bot };
            if kb.read_dead_band().map_or(true, |cur| cur != want_db) {
                changes.dead_band = Some(want_db);
            }
        }

        let ff_val = &feats["feature_flags"];
        if !ff_val.is_null() && ff_val.get("error").is_none() {
            let ff = KeyboardFeature {
                rgb_area: ff_val["rgb_area"].as_u64().unwrap_or(0) as u8,
                wasd_switch: ff_val["wasd_switch"].as_u64().unwrap_or(0) as u8,
                mac_switch: ff_val["mac_switch"].as_u64().unwrap_or(0) as u8,
                win_lock: ff_val["win_lock"].as_u64().unwrap_or(0) as u8,
                nkro_switch: ff_val["nkro_switch"].as_u64().unwrap_or(0) as u8,
            };
            if kb.read_feature().map_or(true, |cur| cur != ff) {
                changes.feature_flags = Some(ff);
            }
        }

        if let Some(gm) = feats["game_mode"].as_u64().map(|v| v as u8) {
            if kb.read_game_mode().map_or(true, |cur| cur != gm) {
                changes.game_mode = Some(gm);
            }
        }

        if let Some(bo) = feats["bottom_optimize"].as_u64().map(|v| v as u8) {
            if kb.read_bottom_optimize().map_or(true, |cur| cur != bo) {
                changes.bottom_optimize = Some(bo);
            }
        }

        let bl_val = &feats["box_light"];
        if !bl_val.is_null() && bl_val.get("error").is_none() {
            let bl = BoxLight {
                mode: bl_val["mode"].as_u64().unwrap_or(0) as u8,
                colorful: bl_val["colorful"].as_u64().unwrap_or(0) as u8,
                brightness: bl_val["brightness"].as_u64().unwrap_or(0) as u8,
                speed: bl_val["speed"].as_u64().unwrap_or(0) as u8,
            };
            if kb.read_box_light().map_or(true, |cur| cur != bl) {
                changes.box_light = Some(bl);
            }
        }

        let li_val = &feats["light_info"];
        if !li_val.is_null() && li_val.get("error").is_none() {
            let li = LightInfo {
                effect: li_val["effect"].as_u64().unwrap_or(0) as u8,
                speed: li_val["speed"].as_u64().unwrap_or(0) as u8,
                r: li_val["r"].as_u64().unwrap_or(0) as u8,
                g: li_val["g"].as_u64().unwrap_or(0) as u8,
                b: li_val["b"].as_u64().unwrap_or(0) as u8,
                brightness: li_val["brightness"].as_u64().unwrap_or(0) as u8,
            };
            if kb.read_light_info().map_or(true, |cur| cur != li) {
                changes.light_info = Some(li);
            }
        }

        if let Some(kc) = feats["key_colors"].as_array() {
            let mut want_kc = Vec::with_capacity(kc.len());
            for c in kc {
                if let Some(arr) = c.as_array() {
                    if arr.len() >= 3 {
                        let r = arr[0].as_u64().unwrap_or(0) as u8;
                        let g = arr[1].as_u64().unwrap_or(0) as u8;
                        let b = arr[2].as_u64().unwrap_or(0) as u8;
                        want_kc.push([r, g, b]);
                    }
                }
            }
            if !want_kc.is_empty() && kb.read_all_key_colors().map_or(true, |cur| cur != want_kc) {
                changes.key_colors = Some(want_kc);
            }
        }
    }

    // 6. Touches avancées (SOCD)
    if let Some(aks) = backup["advanced_keys"].as_array() {
        for entry in aks {
            if entry.get("error").is_some() {
                continue;
            }
            let idx = entry["index"].as_u64().unwrap_or(0) as u8;
            let mode = match entry["mode"].as_str() {
                Some("RS") => AdvancedKeyMode::Rs,
                Some("SOCD") => AdvancedKeyMode::Socd,
                Some("SOCDKEY1") => AdvancedKeyMode::SocdKey1,
                Some("SOCDKEY2") => AdvancedKeyMode::SocdKey2,
                Some("SOCDBALANCE") => AdvancedKeyMode::SocdBalance,
                Some("OKS") => AdvancedKeyMode::Oks,
                _ => AdvancedKeyMode::None,
            };
            let want_ak = AdvancedKey {
                index: idx,
                mode,
                id: entry["id"].as_u64().unwrap_or(0) as u8,
                rs_apc_lv: entry["rs_apc_lv"].as_u64().unwrap_or(0) as u16,
                gapc_sw: entry["gapc_sw"].as_u64().unwrap_or(0) as u8,
                rt_sw: entry["rt_sw"].as_u64().unwrap_or(0) as u8,
                key1_row: entry["key1_row"].as_u64().unwrap_or(0) as u8,
                key1_col: entry["key1_col"].as_u64().unwrap_or(0) as u8,
                key2_row: entry["key2_row"].as_u64().unwrap_or(0) as u8,
                key2_col: entry["key2_col"].as_u64().unwrap_or(0) as u8,
                layer: entry["layer"].as_u64().unwrap_or(0) as u8,
            };

            if kb.read_advanced_key(idx).map_or(true, |cur| cur != want_ak) {
                changes.advanced_keys.push(want_ak);
            }
        }
    }

    // 7. DKS
    if let Some(dks_list) = backup["dks"].as_array() {
        for entry in dks_list {
            if entry.get("error").is_some() {
                continue;
            }
            let idx = entry["index"].as_u64().unwrap_or(0) as u8;
            let mut actions = [DksAction::EMPTY; crate::features::DKS_ACTION_COUNT];
            if let Some(acts) = entry["actions"].as_array() {
                for (i, a) in acts.iter().take(crate::features::DKS_ACTION_COUNT).enumerate() {
                    let kc = a["keycode"].as_u64().unwrap_or(0) as u16;
                    let st = a["status"].as_u64().unwrap_or(0) as u16;
                    actions[i] = DksAction::from_keycode(kc, st);
                }
            }
            let want_dks = Dks { index: idx, actions };
            if kb.read_dks(idx).map_or(true, |cur| cur != want_dks) {
                changes.dks.push(want_dks);
            }
        }
    }

    Ok(changes)
}

/// Affiche le descriptif détaillé des modifications en attente.
pub fn describe(plan: &RestorePlan) -> usize {
    let mut total = 0;

    if let Some(ref km) = plan.keymap {
        println!("  keymap:        {} octets à écrire", km.len());
        total += 1;
    } else {
        println!("  keymap:        déjà conforme");
    }

    if let Some(ref m) = plan.macros {
        println!("  macros:        {} octets à écrire", m.len());
        total += 1;
    } else {
        println!("  macros:        déjà conforme");
    }

    if !plan.actuation.is_empty() {
        println!("  actuation:     {} touche(s) à modifier", plan.actuation.len());
        for (r, c, mm) in &plan.actuation {
            println!("                   touche {r},{c} -> {mm:.2} mm");
        }
        total += plan.actuation.len();
    } else {
        println!("  actuation:     déjà conforme");
    }

    if !plan.rapid_trigger.is_empty() {
        println!("  rapid trigger: {} touche(s) à modifier", plan.rapid_trigger.len());
        for rt in &plan.rapid_trigger {
            println!("                   touche {},{} -> {} press={:.2}mm rel={:.2}mm",
                rt.row, rt.col, if rt.enabled { "activé" } else { "désactivé" }, rt.press_mm, rt.release_mm);
        }
        total += plan.rapid_trigger.len();
    } else {
        println!("  rapid trigger: déjà conforme");
    }

    if plan.dead_band.is_some() { println!("  dead_band      à écrire"); total += 1; }
    if plan.feature_flags.is_some() { println!("  feature_flags  à écrire"); total += 1; }
    if plan.game_mode.is_some() { println!("  game_mode      à écrire"); total += 1; }
    if plan.bottom_optimize.is_some() { println!("  bottom_opt     à écrire"); total += 1; }
    if plan.box_light.is_some() { println!("  box_light      à écrire"); total += 1; }
    if plan.light_info.is_some() { println!("  light_info     à écrire"); total += 1; }
    if let Some(ref kc) = plan.key_colors {
        println!("  key_colors:    {} touches à réécrire", kc.len());
        total += 1;
    }

    if !plan.advanced_keys.is_empty() {
        println!("  advanced_keys: {} slot(s) à écrire", plan.advanced_keys.len());
        total += plan.advanced_keys.len();
    }

    if !plan.dks.is_empty() {
        println!("  dks:           {} slot(s) à écrire", plan.dks.len());
        total += plan.dks.len();
    }

    total
}

/// Applique le plan de restauration sur le matériel dans l'ordre strict requis.
pub fn apply(kb: &mut Mad68, plan: &RestorePlan) -> Result<(), DeviceError> {
    if let Some(ref km) = plan.keymap {
        println!("  écriture de la keymap ...");
        kb.write_keymap(km)?;
    }

    if let Some(ref m) = plan.macros {
        println!("  écriture du buffer de macros ...");
        kb.write_buffer(Cmd::DYNAMIC_KEYMAP_MACRO_SET_BUFFER, m, 0)?;
    }

    // Touches avancées impérativement AVANT l'actionnement
    for ak in &plan.advanced_keys {
        println!("  écriture touche avancée slot {} ({:?})", ak.index, ak.mode);
        kb.write_advanced_key(ak)?;
    }

    for (r, c, mm) in &plan.actuation {
        println!("  écriture actionnement touche {r},{c} = {mm:.2} mm");
        kb.write_actuation_mm(*r, *c, *mm)?;
    }

    for rt in &plan.rapid_trigger {
        println!("  écriture rapid trigger touche {},{}", rt.row, rt.col);
        kb.write_rapid_trigger(rt)?;
    }

    if let Some(ref db) = plan.dead_band { kb.write_dead_band(db)?; }
    if let Some(ref ff) = plan.feature_flags { kb.write_feature(ff)?; }
    if let Some(gm) = plan.game_mode { kb.write_game_mode(gm)?; }
    if let Some(bo) = plan.bottom_optimize { kb.write_bottom_optimize(bo)?; }
    if let Some(ref bl) = plan.box_light { kb.write_box_light(bl)?; }
    if let Some(ref li) = plan.light_info { kb.write_light_info(li)?; }

    if let Some(ref kc) = plan.key_colors {
        println!("  écriture des couleurs par touche ...");
        kb.write_all_key_colors(kc)?;
    }

    for d in &plan.dks {
        println!("  écriture dks slot {}", d.index);
        kb.write_dks(d)?;
    }

    Ok(())
}

/// Point d'entrée de la commande `restore`.
pub fn run(args: &RestoreArgs) -> anyhow::Result<i32> {
    let bytes = std::fs::read(&args.backup)?;
    let slice = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) { &bytes[3..] } else { &bytes[..] };
    let backup: serde_json::Value = serde_json::from_slice(slice)?;

    let mut kb = Mad68::new(args.apply, false, 1000, None, false);
    kb.open()?;
    run_with_device(&mut kb, args, &backup)
}

/// Version testable avec un gestionnaire `Mad68` déjà ouvert (mock ou physique).
pub fn run_with_device(kb: &mut Mad68, args: &RestoreArgs, backup: &serde_json::Value) -> anyhow::Result<i32> {
    let version = backup["backup_version"].as_u64().unwrap_or(1);
    let captured = backup["captured_utc"].as_str().unwrap_or("inconnu");
    let target_serial = backup["device"]["serial"].as_str().unwrap_or("");

    println!("Sauvegarde : {}", args.backup.display());
    println!("Capturée :   {captured} (backup_version {version})");
    println!("Cible :      {} (série '{target_serial}')", backup["device"]["product"].as_str().unwrap_or(""));

    if version < 2 {
        println!("\n  ATTENTION : sauvegarde en version 1 (ne contient pas les réglages avancés v2).\n");
    }

    // Vérification de sécurité sur le numéro de série
    let current_serial = kb.info.serial_number.as_deref().unwrap_or("");
    if !target_serial.is_empty() && current_serial != target_serial {
        eprintln!(
            "Refus de restaurer : le numéro de série du clavier branché ('{current_serial}') \
             ne correspond pas à la sauvegarde ('{target_serial}')"
        );
        return Ok(2);
    }

    println!("Comparaison du matériel avec la sauvegarde ...");
    let restore_plan = plan(kb, backup)?;
    println!();
    let total = describe(&restore_plan);
    println!("\nNon couvert par la restauration : {UNCOVERED_NOTE}");

    if total == 0 {
        println!("\nRien à faire — le clavier est déjà strictement identique à la sauvegarde.");
        return Ok(0);
    }

    if !args.apply {
        println!("\nSimulation (dry-run) : {total} modification(s) en attente, aucune écriture effectuée.");
        println!("Relancez avec l'option --apply pour valider les écritures.");
        return Ok(0);
    }

    println!("\nApplication de {total} modification(s) ...");
    apply(kb, &restore_plan)?;

    println!("\nRe-lecture de confirmation ...");
    let remaining = plan(kb, backup)?;
    let left = (if remaining.keymap.is_some() { 1 } else { 0 })
        + (if remaining.macros.is_some() { 1 } else { 0 })
        + remaining.actuation.len()
        + remaining.rapid_trigger.len();

    if left > 0 {
        println!("  AVERTISSEMENT : {left} élément(s) diffèrent encore après écriture.");
        return Ok(1);
    }

    println!("  Toutes les régions restaurées correspondent à présent à la sauvegarde.");
    println!("\nTerminé. Débranchez et rebranchez le clavier, puis lancez :");
    println!("  openmad-rs backup --verify {}", args.backup.display());
    Ok(0)
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restore_plan_describe_nominal() {
        let mut plan = RestorePlan::default();
        plan.actuation.push((0, 0, 1.20));
        assert_eq!(describe(&plan), 1);
    }

    #[test]
    fn restore_serial_mismatch_check() {
        let backup = json!({
            "backup_version": 2,
            "captured_utc": "2026-10-31",
            "device": { "product": "MAD68", "serial": "TARGET-SERIAL-999" },
            "keymap": { "hex": "" },
            "macros": { "hex": "" }
        });
        let args = RestoreArgs {
            backup: PathBuf::from("test.json"),
            apply: false,
        };
        let mut kb = Mad68::new(false, false, 500, None, true);
        kb.info.serial_number = Some("DIFFERENT-SERIAL".to_string());

        let res = run_with_device(&mut kb, &args, &backup).unwrap();
        assert_eq!(res, 2); // Doit refuser la restauration
    }

    #[test]
    fn restore_plan_detects_actuation_difference() {
        use crate::device::mock::MockHidTransport;
        use crate::devices::default_board;
        use crate::protocol::{Cmd, Vendor};

        let backup = json!({
            "backup_version": 2,
            "captured_utc": "2026-10-31",
            "device": { "product": "MAD68", "serial": "MY-SERIAL" },
            "actuation": [
                { "row": 0, "col": 0, "mm": 1.50 }
            ]
        });
        let mut kb = Mad68::new(false, false, 500, None, true);
        let mock = MockHidTransport::new();
        kb.open_with_transport(Box::new(mock.clone()), default_board().clone()).unwrap();

        // Réponse pour la lecture d'actionnement de (0,0) renvoyant 2.00 mm (différent de 1.50 mm)
        // Format: [cmd, 0x96, sub, row, col, tb[0], tb[1], ...]
        let tb = crate::protocol::travel_bytes(2.00);
        let mut resp = [0u8; 32];
        resp[0] = Cmd::GET_KEYBOARD_VALUE.id();
        resp[1] = 0x96;
        resp[2] = Vendor::ONE_TOG_TH.id();
        resp[3] = 0; // row
        resp[4] = 0; // col
        resp[5] = tb[0];
        resp[6] = tb[1];
        mock.queue_response(&resp);

        let plan = plan(&mut kb, &backup).unwrap();
        assert_eq!(plan.actuation.len(), 1);
        assert_eq!(plan.actuation[0], (0, 0, 1.50));
    }
}
