//! Transport USB HID pour l'interface de configuration DuckBread (protocole VIA).
//!
//! Traduction directe de `src/mad68/device.py`.
//!
//! Modèle de sécurité :
//! - Le descripteur est en lecture seule sauf si `writes = true`.
//! - Les commandes destructives (réinitialisation EEPROM, bootloader, calibration)
//!   nécessitent en plus `dangerous = true`.
//! - La vérification est centralisée dans `send()`, aucun appelant ne peut contourner ce sas.
//! - Le transport HID est abstrait via le trait [`HidTransport`] pour permettre des tests unitaires
//!   exhaustifs et hermétiques sans matériel physique branché.

use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

use crate::devices::{self, BoardSpec};
use crate::features::{
    decode_macro, encode_macro, split_macro_buffer, AdvancedKey, BoxLight, DeadBand, Dks,
    KeyboardFeature, LightInfo, MacroStep, TapDance, DKS_ACTION_COUNT, DKS_ACTION_SIZE,
};
use crate::protocol::{
    build, build_buffer_read, build_buffer_write, build_vendor, decode_rgb, encode_rgb,
    is_dangerous_command, is_dangerous_vendor, is_write_command, keymap_offset, keymap_size_for,
    travel_bytes, travel_from_bytes, ACTUATION_ENTRY_V2, BRIGHTNESS_MAX, BULK_FLASH_OP_AT,
    BULK_OFFSET_AT, BULK_READ_DATA_AT, BULK_SIZE_AT, BULK_WRITE_DATA_AT, CUSTOM_CHANNEL,
    KNOWN_PROTOCOL_VERSION, MAX_ACTUATION_READ, MAX_ACTUATION_WRITE, MAX_BUFFER_CHUNK,
    MAX_PAYLOAD, MAX_RT_READ, MAX_RT_WRITE, RAPID_TRIGGER_ENTRY_V2, REPORT_SIZE, USAGE,
    USAGE_PAGE, VENDOR_ID, AdvancedKeyMode, Cmd, FlashOp, Reply, SubField, Vendor,
};

/// Erreurs spécifiques à la communication USB et au protocole Mad68.
#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("périphérique non trouvé : {0}")]
    NotFound(String),

    #[error("le périphérique USB n'est pas ouvert")]
    NotOpen,

    #[error("écriture bloquée par le sas de sécurité : {0}")]
    WriteBlocked(String),

    #[error("erreur HID sous-jacente : {0}")]
    Hid(String),

    #[error("erreur de protocole : {0}")]
    Protocol(String),

    #[error("délai dépassé en attente de réponse : {0}")]
    Timeout(String),

    #[error("argument invalide : {0}")]
    InvalidArgument(String),

    #[error("erreur E/S : {0}")]
    Io(#[from] std::io::Error),
}

/// Configuration du Rapid Trigger pour une touche physique.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RapidTrigger {
    pub row: u8,
    pub col: u8,
    pub enabled: bool,
    pub release_mm: f64,
    pub press_mm: f64,
}

/// Trait abstrayant les entrées/sorties brutes USB HID.
///
/// Découple le protocole de la bibliothèque `hidapi` afin de permettre
/// l'émulation et les tests unitaires complets en mémoire.
pub trait HidTransport: Send {
    /// Écrit un rapport USB HID.
    ///
    /// Sous Windows avec `hidapi`, le Report ID (0x00 pour ce matériel) doit préfixer
    /// les données, formant ainsi une trame de 33 octets pour un rapport utile de 32 octets.
    fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError>;

    /// Lit un rapport d'entrée avec un délai maximal en millisecondes.
    ///
    /// Retourne le nombre d'octets lus ou 0 si le délai expire sans paquet reçu.
    fn read_timeout(&mut self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, DeviceError>;
}

/// Implémentation concrète de [`HidTransport`] basée sur `hidapi::HidDevice`.
pub struct RealHidTransport {
    device: hidapi::HidDevice,
}

impl RealHidTransport {
    pub fn new(device: hidapi::HidDevice) -> Self {
        Self { device }
    }
}

impl HidTransport for RealHidTransport {
    fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError> {
        self.device
            .write(data)
            .map_err(|e| DeviceError::Hid(e.to_string()))
    }

    fn read_timeout(&mut self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, DeviceError> {
        self.device
            .read_timeout(buf, timeout_ms)
            .map_err(|e| DeviceError::Hid(e.to_string()))
    }
}

/// Découverte : vérifie si un clavier reconnu est en mode bootloader.
pub fn in_bootloader() -> bool {
    if let Ok(api) = hidapi::HidApi::new() {
        let boot_ids = devices::bootloader_ids();
        for dev in api.device_list() {
            if dev.vendor_id() == VENDOR_ID && boot_ids.contains(&dev.product_id()) {
                return true;
            }
        }
    }
    false
}

/// Découverte : localise l'interface de configuration Raw HID (usage page 0xFF60, usage 0x61).
pub fn find_config_interface(
    product_id: Option<u16>,
) -> Result<hidapi::DeviceInfo, DeviceError> {
    let api = hidapi::HidApi::new().map_err(|e| DeviceError::Hid(e.to_string()))?;
    let mut candidates: Vec<hidapi::DeviceInfo> = api
        .device_list()
        .filter(|d| {
            d.vendor_id() == VENDOR_ID
                && d.usage_page() == USAGE_PAGE
                && d.usage() == USAGE
                && product_id.map_or(true, |pid| d.product_id() == pid)
                && (product_id.is_some() || devices::lookup(d.product_id()).is_some())
        })
        .cloned()
        .collect();

    if candidates.is_empty() {
        if in_bootloader() {
            return Err(DeviceError::NotFound(
                "clavier en mode BOOTLOADER — débranchez et rebranchez-le.".to_string(),
            ));
        }
        return Err(DeviceError::NotFound(format!(
            "aucune interface Raw HID ({USAGE_PAGE:#06x}:{USAGE:#04x}) détectée pour {VENDOR_ID:#06x}."
        )));
    }

    // Tri stable pour sélectionner de manière déterministe en présence de plusieurs cartes
    candidates.sort_by_key(|d| (d.product_id(), d.path().to_string_lossy().to_string()));
    Ok(candidates.remove(0))
}

/// Résumé des métadonnées du périphérique USB.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfoSummary {
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub manufacturer_string: Option<String>,
    pub product_string: Option<String>,
}

/// Gestionnaire d'un clavier magnétique de la famille DuckBread / MAD68.
pub struct Mad68 {
    pub writes: bool,
    pub dangerous: bool,
    pub timeout_ms: i32,
    pub product_id: Option<u16>,
    pub allow_unverified: bool,
    transport: Option<Box<dyn HidTransport>>,
    pub spec: BoardSpec,
    pub info: DeviceInfoSummary,
    protocol_version: Option<u16>,
    populated: Option<Vec<bool>>,
}

impl Mad68 {
    /// Crée une nouvelle instance déconnectée.
    pub fn new(
        writes: bool,
        dangerous: bool,
        timeout_ms: i32,
        product_id: Option<u16>,
        allow_unverified: bool,
    ) -> Self {
        Self {
            writes,
            dangerous,
            timeout_ms,
            product_id,
            allow_unverified,
            transport: None,
            spec: devices::default_board().clone(),
            info: DeviceInfoSummary::default(),
            protocol_version: None,
            populated: None,
        }
    }

    /// Ouvre la connexion USB physique avec le clavier via `hidapi`.
    pub fn open(&mut self) -> Result<(), DeviceError> {
        let iface = find_config_interface(self.product_id)?;
        let api = hidapi::HidApi::new().map_err(|e| DeviceError::Hid(e.to_string()))?;
        let dev = api
            .open_path(iface.path())
            .map_err(|e| DeviceError::Hid(e.to_string()))?;

        let pid = iface.product_id();
        self.spec = devices::describe(pid);
        self.info = DeviceInfoSummary {
            vendor_id: iface.vendor_id(),
            product_id: pid,
            serial_number: iface.serial_number().map(|s| s.to_string()),
            manufacturer_string: iface.manufacturer_string().map(|s| s.to_string()),
            product_string: iface.product_string().map(|s| s.to_string()),
        };
        if devices::is_unlocked(pid) {
            self.allow_unverified = true;
        }

        self.transport = Some(Box::new(RealHidTransport::new(dev)));

        match self.protocol_version() {
            Ok(ver) => self.protocol_version = Some(ver),
            Err(_) => self.protocol_version = None,
        }

        Ok(())
    }

    /// Ouvre le gestionnaire avec un transport personnalisé (utilisé pour les tests avec un mock).
    pub fn open_with_transport(
        &mut self,
        transport: Box<dyn HidTransport>,
        spec: BoardSpec,
    ) -> Result<(), DeviceError> {
        let pid = spec.product_id;
        self.spec = spec.clone();
        self.info = DeviceInfoSummary {
            vendor_id: VENDOR_ID,
            product_id: pid,
            serial_number: Some("TEST-SERIAL-12345".to_string()),
            manufacturer_string: Some("Madlions".to_string()),
            product_string: Some(spec.name.to_string()),
        };
        if devices::is_unlocked(pid) {
            self.allow_unverified = true;
        }
        self.transport = Some(transport);
        match self.protocol_version() {
            Ok(ver) => self.protocol_version = Some(ver),
            Err(_) => self.protocol_version = None,
        }
        Ok(())
    }

    /// Ferme la connexion avec le périphérique.
    pub fn close(&mut self) {
        self.transport = None;
    }

    // Propriétés géométriques

    pub fn total_keys(&self) -> usize {
        self.spec.total_keys() as usize
    }

    pub fn matrix_rows(&self) -> usize {
        self.spec.rows as usize
    }

    pub fn matrix_cols(&self) -> usize {
        self.spec.cols as usize
    }

    pub fn layer_count_spec(&self) -> usize {
        self.spec.layers as usize
    }

    pub fn keymap_size(&self) -> usize {
        keymap_size_for(self.matrix_rows(), self.matrix_cols(), self.layer_count_spec())
    }

    pub fn firmware_version(&self) -> Option<u16> {
        self.protocol_version
    }

    pub fn firmware_verified(&self) -> bool {
        self.protocol_version == Some(KNOWN_PROTOCOL_VERSION as u16)
    }

    // Sas de sécurité

    fn check(&self, packet: &[u8]) -> Result<(), DeviceError> {
        if packet.is_empty() {
            return Err(DeviceError::InvalidArgument("paquet vide".to_string()));
        }

        let cmd_byte = packet[0];
        let cmd = Cmd::from_id(cmd_byte).ok_or_else(|| {
            DeviceError::Protocol(format!("commande inconnue refusée : {cmd_byte:#04x}"))
        })?;

        let mut vendor_sub: Option<Vendor> = None;
        if (cmd == Cmd::GET_KEYBOARD_VALUE || cmd == Cmd::SET_KEYBOARD_VALUE) && packet.len() > 2 {
            if packet[1] == CUSTOM_CHANNEL {
                vendor_sub = Vendor::from_id(packet[2]);
            }
        }

        if let Some(sub) = vendor_sub {
            if is_dangerous_vendor(sub) && !self.dangerous {
                return Err(DeviceError::WriteBlocked(format!(
                    "sous-commande constructeur {sub:?} destructrice (calibration / reset / flash) — requiert dangerous=true."
                )));
            }
        }

        // Tap dance / hold-click utilise le préfixe VIAL
        if cmd == Cmd::VIAL_PREFIX
            && packet.len() > 2
            && packet[1] == Vendor::ENTRY_OP.id()
            && packet[2] == Vendor::TAP_DANCE_SET.id()
            && !self.writes
        {
            return Err(DeviceError::WriteBlocked(
                "l'écriture d'une liaison tap-dance modifie la mémoire embarquée (lecture seule active)."
                    .to_string(),
            ));
        }

        if is_dangerous_command(cmd) && !self.dangerous {
            return Err(DeviceError::WriteBlocked(format!(
                "la commande {cmd:?} efface l'état ou redémarre en bootloader — requiert dangerous=true."
            )));
        }

        if is_write_command(cmd)
            && self.writes
            && !self.allow_unverified
            && !(self.firmware_verified() && self.spec.verified)
        {
            let got = self
                .protocol_version
                .map_or("aucune réponse".to_string(), |v| v.to_string());
            let why = if !self.firmware_verified() {
                format!(
                    "version protocole rapportée {got}, attendue {KNOWN_PROTOCOL_VERSION}"
                )
            } else {
                format!("le modèle {} n'a pas été validé avec ce pilote", self.spec.name)
            };
            return Err(DeviceError::WriteBlocked(format!(
                "écriture {cmd:?} refusée : {why}."
            )));
        }

        if is_write_command(cmd) && !self.writes {
            return Err(DeviceError::WriteBlocked(format!(
                "la commande {cmd:?} modifie la mémoire embarquée et ce descripteur est en lecture seule."
            )));
        }

        Ok(())
    }

    // Échange USB de base

    /// Envoie un paquet de 32 octets et attend la réponse correspondante.
    ///
    /// Construction de la trame :
    /// - Sous Windows avec `hidapi`, le Report ID 0x00 est ajouté en préfixe (33 octets au total).
    /// - Le filtrage de la réponse s'effectue sur le code de commande et, pour les canaux
    ///   constructeur, sur les sous-identifiants afin d'éviter d'intercepter des paquets
    ///   de télémétrie asynchrones.
    pub fn send(&mut self, packet: &[u8], expect_reply: bool) -> Result<Option<Reply>, DeviceError> {
        if packet.len() != REPORT_SIZE {
            return Err(DeviceError::InvalidArgument(format!(
                "un paquet doit faire exactement {REPORT_SIZE} octets, reçu {}",
                packet.len()
            )));
        }

        self.check(packet)?;

        let transport = self.transport.as_mut().ok_or(DeviceError::NotOpen)?;

        // hidapi requiert le Report ID (0x00) en premier octet sous Windows (total 33 octets)
        let mut out_report = [0u8; REPORT_SIZE + 1];
        out_report[0] = 0x00;
        out_report[1..].copy_from_slice(packet);

        transport.write(&out_report)?;

        if !expect_reply {
            return Ok(None);
        }

        let want = packet[0];
        let want_vendor: Option<Vec<u8>> = if packet[1] == CUSTOM_CHANNEL {
            Some(vec![packet[1], packet[2]])
        } else if want == Cmd::CUSTOM_GET_VALUE.id() || want == Cmd::CUSTOM_SET_VALUE.id() {
            Some(vec![packet[1]])
        } else {
            None
        };

        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms as u64);

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let timeout_chunk = remaining.as_millis().clamp(1, 200) as i32;

            let mut in_buf = [0u8; REPORT_SIZE];
            let read_len = transport.read_timeout(&mut in_buf, timeout_chunk)?;
            if read_len == 0 {
                continue;
            }

            let reply = Reply::new(in_buf.to_vec())
                .map_err(|e| DeviceError::Protocol(e.to_string()))?;
            let mut matches = reply.command_id() == want;
            if matches {
                if let Some(ref wv) = want_vendor {
                    let raw = reply.raw();
                    if raw.len() < 1 + wv.len() || &raw[1..1 + wv.len()] != wv.as_slice() {
                        matches = false;
                    }
                }
            }

            if matches {
                return Ok(Some(reply));
            }

            if reply.is_unhandled() {
                return Err(DeviceError::Protocol(format!(
                    "le firmware a signalé la commande {want:#04x} comme non gérée"
                )));
            }
            // Ignore les trames de télémétrie ou paquets orphelins et continue d'attendre
        }

        Err(DeviceError::Timeout(format!("commande {want:#04x}")))
    }

    /// Écoute les rapports entrants pendant la durée spécifiée sans émettre de commande.
    pub fn listen(&mut self, duration: Duration, only_stream: bool) -> Result<Vec<Reply>, DeviceError> {
        let transport = self.transport.as_mut().ok_or(DeviceError::NotOpen)?;
        let deadline = Instant::now() + duration;
        let mut replies = Vec::new();

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let timeout_chunk = remaining.as_millis().clamp(1, 200) as i32;

            let mut in_buf = [0u8; REPORT_SIZE];
            let read_len = transport.read_timeout(&mut in_buf, timeout_chunk)?;
            if read_len == 0 {
                continue;
            }

            let reply = Reply::new(in_buf.to_vec())
                .map_err(|e| DeviceError::Protocol(e.to_string()))?;
            if only_stream && !reply.is_data_reporting() {
                continue;
            }
            replies.push(reply);
        }

        Ok(replies)
    }

    pub fn cmd(&mut self, cmd: Cmd, payload: &[u8]) -> Result<Reply, DeviceError> {
        let frame = build(cmd, payload).map_err(|e| DeviceError::Protocol(e.to_string()))?;
        let reply = self.send(&frame, true)?;
        reply.ok_or_else(|| DeviceError::Protocol("réponse manquante".to_string()))
    }

    pub fn vendor(&mut self, cmd: Cmd, sub: Vendor, payload: &[u8]) -> Result<Reply, DeviceError> {
        let frame = build_vendor(cmd, sub, payload).map_err(|e| DeviceError::Protocol(e.to_string()))?;
        let reply = self.send(&frame, true)?;
        reply.ok_or_else(|| DeviceError::Protocol("réponse manquante".to_string()))
    }

    pub fn vendor_get(&mut self, sub: Vendor, payload: &[u8]) -> Result<Reply, DeviceError> {
        self.vendor(Cmd::GET_KEYBOARD_VALUE, sub, payload)
    }

    pub fn vendor_set(&mut self, sub: Vendor, payload: &[u8]) -> Result<Reply, DeviceError> {
        self.vendor(Cmd::SET_KEYBOARD_VALUE, sub, payload)
    }

    // Requêtes basiques

    pub fn protocol_version(&mut self) -> Result<u16, DeviceError> {
        let r = self.cmd(Cmd::GET_PROTOCOL_VERSION, &[])?;
        let p = r.payload();
        if p.len() < 2 {
            return Err(DeviceError::Protocol("réponse GET_PROTOCOL_VERSION trop courte".to_string()));
        }
        Ok(u16::from_be_bytes([p[0], p[1]]))
    }

    pub fn layer_count(&mut self) -> Result<u8, DeviceError> {
        let r = self.cmd(Cmd::DYNAMIC_KEYMAP_GET_LAYER_COUNT, &[])?;
        let p = r.payload();
        if p.is_empty() {
            return Err(DeviceError::Protocol("réponse GET_LAYER_COUNT vide".to_string()));
        }
        Ok(p[0])
    }

    pub fn macro_count(&mut self) -> Result<u8, DeviceError> {
        let r = self.cmd(Cmd::DYNAMIC_KEYMAP_MACRO_GET_COUNT, &[])?;
        let p = r.payload();
        if p.is_empty() {
            return Err(DeviceError::Protocol("réponse MACRO_GET_COUNT vide".to_string()));
        }
        Ok(p[0])
    }

    pub fn macro_buffer_size(&mut self) -> Result<u16, DeviceError> {
        let r = self.cmd(Cmd::DYNAMIC_KEYMAP_MACRO_GET_BUFFER_SIZE, &[])?;
        let p = r.payload();
        if p.len() < 2 {
            return Err(DeviceError::Protocol("réponse MACRO_GET_BUFFER_SIZE trop courte".to_string()));
        }
        Ok(u16::from_be_bytes([p[0], p[1]]))
    }

    pub fn keyboard_value(&mut self, value: u8) -> Result<Reply, DeviceError> {
        self.cmd(Cmd::GET_KEYBOARD_VALUE, &[value])
    }

    // Gestion de buffers paginés VIA

    pub fn read_buffer(&mut self, cmd: Cmd, size: usize, base: usize) -> Result<Vec<u8>, DeviceError> {
        let mut out = Vec::with_capacity(size);
        while out.len() < size {
            let want = MAX_BUFFER_CHUNK.min(size - out.len());
            let offset = (base + out.len()) as u16;
            let frame = build_buffer_read(cmd, offset, want)
                .map_err(|e| DeviceError::Protocol(e.to_string()))?;
            let reply = self.send(&frame, true)?.ok_or_else(|| {
                DeviceError::Protocol(format!("{cmd:?} à l'offset {offset:#06x} : pas de réponse"))
            })?;
            let chunk = reply.buffer_data();
            if chunk.is_empty() {
                return Err(DeviceError::Protocol(format!(
                    "{cmd:?} à l'offset {offset:#06x} a retourné 0 octets"
                )));
            }
            out.extend_from_slice(&chunk[..want.min(chunk.len())]);
        }
        out.truncate(size);
        Ok(out)
    }

    pub fn write_buffer(&mut self, cmd: Cmd, data: &[u8], base: usize) -> Result<(), DeviceError> {
        for (i, chunk) in data.chunks(MAX_BUFFER_CHUNK).enumerate() {
            let offset = (base + i * MAX_BUFFER_CHUNK) as u16;
            let frame = build_buffer_write(cmd, offset, chunk)
                .map_err(|e| DeviceError::Protocol(e.to_string()))?;
            self.send(&frame, true)?;
        }
        Ok(())
    }

    // Keymap dynamique

    pub fn read_keymap(&mut self) -> Result<Vec<u8>, DeviceError> {
        let size = self.keymap_size();
        self.read_buffer(Cmd::DYNAMIC_KEYMAP_GET_BUFFER, size, 0)
    }

    pub fn write_keymap(&mut self, data: &[u8]) -> Result<(), DeviceError> {
        let expected = self.keymap_size();
        if data.len() != expected {
            return Err(DeviceError::InvalidArgument(format!(
                "taille de keymap invalide pour {} : attendu {} octets, reçu {}",
                self.spec.name,
                expected,
                data.len()
            )));
        }
        self.write_buffer(Cmd::DYNAMIC_KEYMAP_SET_BUFFER, data, 0)
    }

    pub fn keymap_index(&self, layer: usize, row: usize, col: usize) -> Result<usize, DeviceError> {
        if layer >= self.layer_count_spec() {
            return Err(DeviceError::InvalidArgument(format!("layer {layer} hors limites")));
        }
        if row >= self.matrix_rows() {
            return Err(DeviceError::InvalidArgument(format!("row {row} hors limites")));
        }
        if col >= self.matrix_cols() {
            return Err(DeviceError::InvalidArgument(format!("col {col} hors limites")));
        }
        Ok(keymap_offset(layer, row, col, self.matrix_rows(), self.matrix_cols()))
    }

    pub fn read_keycode(&mut self, layer: u8, row: u8, col: u8) -> Result<u16, DeviceError> {
        let r = self.cmd(Cmd::DYNAMIC_KEYMAP_GET_KEYCODE, &[layer, row, col])?;
        let p = r.payload();
        if p.len() < 5 {
            return Err(DeviceError::Protocol("réponse GET_KEYCODE trop courte".to_string()));
        }
        Ok(u16::from_be_bytes([p[3], p[4]]))
    }

    pub fn write_keycode(&mut self, layer: u8, row: u8, col: u8, keycode: u16) -> Result<(), DeviceError> {
        let kc_bytes = keycode.to_be_bytes();
        self.cmd(Cmd::DYNAMIC_KEYMAP_SET_KEYCODE, &[layer, row, col, kc_bytes[0], kc_bytes[1]])?;
        Ok(())
    }

    pub fn read_macros(&mut self) -> Result<Vec<u8>, DeviceError> {
        let size = self.macro_buffer_size()? as usize;
        self.read_buffer(Cmd::DYNAMIC_KEYMAP_MACRO_GET_BUFFER, size, 0)
    }

    pub fn read_macro_steps(&mut self) -> Result<Vec<Vec<MacroStep>>, DeviceError> {
        let count = self.macro_count()? as usize;
        let buf = self.read_macros()?;
        let raw_macros = split_macro_buffer(&buf, count);
        Ok(raw_macros.iter().map(|m| decode_macro(m)).collect())
    }

    pub fn write_macro_steps(&mut self, macros: &[Vec<MacroStep>], preserve_tail: bool) -> Result<(), DeviceError> {
        let size = self.macro_buffer_size()? as usize;
        let mut encoded: Vec<Vec<u8>> = Vec::with_capacity(macros.len());
        for s in macros {
            encoded.push(encode_macro(s).map_err(|e| DeviceError::InvalidArgument(e.to_string()))?);
        }

        let mut body = Vec::new();
        for m in &encoded {
            body.extend_from_slice(m);
            body.push(0x00);
        }

        if body.len() > size {
            return Err(DeviceError::InvalidArgument(format!(
                "les macros nécessitent {} octets, buffer de taille {}",
                body.len(),
                size
            )));
        }

        let mut tail = vec![0u8; size - body.len()];
        if preserve_tail {
            let current = self.read_macros()?;
            let mut pos = 0;
            let mut seen = 0;
            while seen < encoded.len() && pos < current.len() {
                if let Some(idx) = current[pos..].iter().position(|&b| b == 0x00) {
                    pos += idx + 1;
                    seen += 1;
                } else {
                    pos = current.len();
                    break;
                }
            }
            if pos < current.len() {
                let keep = &current[pos..];
                let copy_len = tail.len().min(keep.len());
                tail[..copy_len].copy_from_slice(&keep[..copy_len]);
            }
        }

        body.extend_from_slice(&tail);
        self.write_buffer(Cmd::DYNAMIC_KEYMAP_MACRO_SET_BUFFER, &body, 0)
    }

    pub fn write_macro_slot(&mut self, index: usize, steps: &[MacroStep]) -> Result<(), DeviceError> {
        let count = self.macro_count()? as usize;
        if index >= count {
            return Err(DeviceError::InvalidArgument(format!(
                "macro index {index} hors limites 0..{count}"
            )));
        }
        let mut macros = self.read_macro_steps()?;
        if index < macros.len() {
            macros[index] = steps.to_vec();
        }
        self.write_macro_steps(&macros, true)
    }

    // Fonctions magnétiques (Hall Effect)

    pub fn key_index(&self, row: u8, col: u8) -> usize {
        row as usize * self.matrix_cols() + col as usize
    }

    pub fn key_rowcol(&self, index: usize) -> (u8, u8) {
        let row = (index / self.matrix_cols()) as u8;
        let col = (index % self.matrix_cols()) as u8;
        (row, col)
    }

    pub fn iter_keys(&self) -> Vec<(u8, u8)> {
        let mut keys = Vec::with_capacity(self.total_keys());
        for r in 0..self.spec.rows {
            for c in 0..self.spec.cols {
                keys.push((r, c));
            }
        }
        keys
    }

    pub fn read_actuation_mm(&mut self, row: u8, col: u8) -> Result<f64, DeviceError> {
        let r = self.vendor_get(Vendor::ONE_TOG_TH, &[row, col])?;
        let p = r.payload();
        if p.len() < 6 {
            return Err(DeviceError::Protocol("réponse ONE_TOG_TH trop courte".to_string()));
        }
        let val = travel_from_bytes([p[4], p[5]]);
        Ok(val)
    }

    pub fn write_actuation_mm(&mut self, row: u8, col: u8, mm: f64) -> Result<(), DeviceError> {
        let tb = travel_bytes(mm);
        self.vendor_set(Vendor::ONE_TOG_TH, &[row, col, tb[0], tb[1]])?;
        Ok(())
    }

    pub fn read_rapid_trigger(&mut self, row: u8, col: u8) -> Result<RapidTrigger, DeviceError> {
        let r = self.vendor_get(Vendor::ONE_RT, &[row, col])?;
        let p = r.payload();
        if p.len() < 9 {
            return Err(DeviceError::Protocol("réponse ONE_RT trop courte".to_string()));
        }
        Ok(RapidTrigger {
            row: p[2],
            col: p[3],
            enabled: p[4] != 0,
            release_mm: travel_from_bytes([p[5], p[6]]),
            press_mm: travel_from_bytes([p[7], p[8]]),
        })
    }

    pub fn write_rapid_trigger(&mut self, rt: &RapidTrigger) -> Result<(), DeviceError> {
        let rel = travel_bytes(rt.release_mm);
        let press = travel_bytes(rt.press_mm);
        let payload = [
            rt.row,
            rt.col,
            if rt.enabled { 1 } else { 0 },
            rel[0],
            rel[1],
            press[0],
            press[1],
        ];
        self.vendor_set(Vendor::ONE_RT, &payload)?;
        Ok(())
    }

    // Échanges par blocs (Bulk)

    fn bulk_request(&self, sub: Vendor, offset: usize, count: usize) -> Result<[u8; REPORT_SIZE], DeviceError> {
        let mut payload = [0u8; MAX_PAYLOAD];
        payload[0] = CUSTOM_CHANNEL;
        payload[1] = sub.id();
        let off_be = (offset as u16).to_be_bytes();
        payload[BULK_OFFSET_AT - 1] = off_be[0];
        payload[BULK_OFFSET_AT] = off_be[1];
        payload[BULK_SIZE_AT - 1] = count as u8;
        build(Cmd::GET_KEYBOARD_VALUE, &payload).map_err(|e| DeviceError::Protocol(e.to_string()))
    }

    pub fn read_actuation_bulk(
        &mut self,
        offset: usize,
        count: Option<usize>,
    ) -> Result<Vec<f64>, DeviceError> {
        let total = count.unwrap_or_else(|| self.total_keys().saturating_sub(offset));
        let mut out = Vec::with_capacity(total);

        while out.len() < total {
            let want = MAX_ACTUATION_READ.min(total - out.len());
            let base = offset + out.len();
            let frame = self.bulk_request(Vendor::BUFFER_TOG_TH, base, want)?;
            let reply = self.send(&frame, true)?.ok_or_else(|| {
                DeviceError::Protocol(format!("lecture actuation bulk à {base} : pas de réponse"))
            })?;
            let raw = reply.raw();
            let span = want * ACTUATION_ENTRY_V2;
            if raw.len() < BULK_READ_DATA_AT + span {
                return Err(DeviceError::Protocol(format!(
                    "lecture actuation bulk à {base} : trame incomplète"
                )));
            }
            let data = &raw[BULK_READ_DATA_AT..BULK_READ_DATA_AT + span];
            for i in 0..want {
                let mm = travel_from_bytes([data[i * 2], data[i * 2 + 1]]);
                out.push(mm);
            }
        }
        Ok(out)
    }

    pub fn read_rapid_trigger_bulk(
        &mut self,
        offset: usize,
        count: Option<usize>,
    ) -> Result<Vec<RapidTrigger>, DeviceError> {
        let total = count.unwrap_or_else(|| self.total_keys().saturating_sub(offset));
        let mut out = Vec::with_capacity(total);

        while out.len() < total {
            let want = MAX_RT_READ.min(total - out.len());
            let base = offset + out.len();
            let frame = self.bulk_request(Vendor::BUFFER_RT, base, want)?;
            let reply = self.send(&frame, true)?.ok_or_else(|| {
                DeviceError::Protocol(format!("lecture rapid trigger bulk à {base} : pas de réponse"))
            })?;
            let raw = reply.raw();
            let span = want * RAPID_TRIGGER_ENTRY_V2;
            if raw.len() < BULK_READ_DATA_AT + span {
                return Err(DeviceError::Protocol(format!(
                    "lecture rapid trigger bulk à {base} : trame incomplète"
                )));
            }
            let data = &raw[BULK_READ_DATA_AT..BULK_READ_DATA_AT + span];
            for i in 0..want {
                let e = &data[i * 5..i * 5 + 5];
                let (row, col) = self.key_rowcol(base + i);
                out.push(RapidTrigger {
                    row,
                    col,
                    enabled: e[0] != 0,
                    release_mm: travel_from_bytes([e[1], e[2]]),
                    press_mm: travel_from_bytes([e[3], e[4]]),
                });
            }
        }
        Ok(out)
    }

    fn bulk_write(
        &mut self,
        sub: Vendor,
        offset: usize,
        count: usize,
        data: &[u8],
        flash_op: FlashOp,
    ) -> Result<(), DeviceError> {
        let mut payload = [0u8; MAX_PAYLOAD];
        payload[0] = CUSTOM_CHANNEL;
        payload[1] = sub.id();
        let off_be = (offset as u16).to_be_bytes();
        payload[BULK_OFFSET_AT - 1] = off_be[0];
        payload[BULK_OFFSET_AT] = off_be[1];
        payload[BULK_SIZE_AT - 1] = count as u8;
        payload[BULK_FLASH_OP_AT - 1] = flash_op.id();
        let start = BULK_WRITE_DATA_AT - 1;
        payload[start..start + data.len()].copy_from_slice(data);
        let frame = build(Cmd::SET_KEYBOARD_VALUE, &payload)
            .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        self.send(&frame, true)?;
        Ok(())
    }

    pub fn write_actuation_bulk(
        &mut self,
        values: &[f64],
        offset: usize,
        flash_op: FlashOp,
    ) -> Result<usize, DeviceError> {
        let mut packets = 0;
        let mut i = 0;
        while i < values.len() {
            let chunk = &values[i..values.len().min(i + MAX_ACTUATION_WRITE)];
            let mut data = Vec::with_capacity(chunk.len() * 2);
            for &v in chunk {
                data.extend_from_slice(&travel_bytes(v));
            }
            self.bulk_write(Vendor::BUFFER_TOG_TH, offset + i, chunk.len(), &data, flash_op)?;
            packets += 1;
            i += chunk.len();
        }
        Ok(packets)
    }

    pub fn write_rapid_trigger_bulk(
        &mut self,
        specs: &[RapidTrigger],
        offset: usize,
        flash_op: FlashOp,
    ) -> Result<usize, DeviceError> {
        let mut packets = 0;
        let mut i = 0;
        while i < specs.len() {
            let chunk = &specs[i..specs.len().min(i + MAX_RT_WRITE)];
            let mut data = Vec::with_capacity(chunk.len() * 5);
            for rt in chunk {
                data.push(if rt.enabled { 1 } else { 0 });
                data.extend_from_slice(&travel_bytes(rt.release_mm));
                data.extend_from_slice(&travel_bytes(rt.press_mm));
            }
            self.bulk_write(Vendor::BUFFER_RT, offset + i, chunk.len(), &data, flash_op)?;
            packets += 1;
            i += chunk.len();
        }
        Ok(packets)
    }

    // Télémétrie et capteurs analogiques

    const TELEMETRY_ENTRY: usize = 2;
    const MAX_TELEMETRY_READ: usize = (REPORT_SIZE - BULK_READ_DATA_AT) / Self::TELEMETRY_ENTRY; // 12

    fn read_u16_bulk(&mut self, sub: Vendor, count: usize, offset: usize) -> Result<Vec<u16>, DeviceError> {
        let mut out = Vec::with_capacity(count);
        while out.len() < count {
            let want = Self::MAX_TELEMETRY_READ.min(count - out.len());
            let base = offset + out.len();
            let frame = self.bulk_request(sub, base, want)?;

            let mut reply_opt = None;
            for attempt in 0..3 {
                match self.send(&frame, true) {
                    Ok(rep) => {
                        reply_opt = rep;
                        break;
                    }
                    Err(e) => {
                        if attempt == 2 {
                            return Err(e);
                        }
                    }
                }
            }

            let reply = reply_opt.ok_or_else(|| {
                DeviceError::Protocol(format!("{sub:?} à {base} : réponse manquante"))
            })?;
            let raw = reply.raw();
            let span = want * Self::TELEMETRY_ENTRY;
            if raw.len() < BULK_READ_DATA_AT + span {
                return Err(DeviceError::Protocol(format!(
                    "{sub:?} à {base} : octets reçus insuffisants"
                )));
            }
            let data = &raw[BULK_READ_DATA_AT..BULK_READ_DATA_AT + span];
            for i in 0..want {
                out.push(u16::from_be_bytes([data[i * 2], data[i * 2 + 1]]));
            }
        }
        Ok(out)
    }

    pub fn read_adc_raw(&mut self, count: Option<usize>) -> Result<Vec<u16>, DeviceError> {
        let total = count.unwrap_or_else(|| self.total_keys());
        self.read_u16_bulk(Vendor::REALTIME_ADC_AXLE_BUFFER, total, 0)
    }

    pub fn read_trip_raw(&mut self, count: Option<usize>) -> Result<Vec<u16>, DeviceError> {
        let total = count.unwrap_or_else(|| self.total_keys());
        self.read_u16_bulk(Vendor::REALTIME_TRIP_AXLE_BUFFER, total, 0)
    }

    pub fn read_trip_mm(&mut self, row: u8, col: u8) -> Result<f64, DeviceError> {
        let r = self.vendor_get(Vendor::REALTIME_TRIP_AXLE, &[row, col])?;
        let p = r.payload();
        if p.len() < 6 {
            return Err(DeviceError::Protocol("réponse REALTIME_TRIP_AXLE trop courte".to_string()));
        }
        let u16_val = u16::from_be_bytes([p[4], p[5]]);
        Ok((u16_val as f64 * 0.01 * 100.0).round() / 100.0)
    }

    pub fn read_adc_key(&mut self, row: u8, col: u8) -> Result<u16, DeviceError> {
        let r = self.vendor_get(Vendor::REALTIME_ADC_AXLE, &[row, col])?;
        let p = r.payload();
        if p.len() < 6 {
            return Err(DeviceError::Protocol("réponse REALTIME_ADC_AXLE trop courte".to_string()));
        }
        Ok(u16::from_be_bytes([p[4], p[5]]))
    }

    pub fn read_calibration_status(&mut self, count: Option<usize>) -> Result<Vec<u8>, DeviceError> {
        let total = count.unwrap_or_else(|| self.total_keys());
        let mut out = Vec::with_capacity(total);
        let per_packet = REPORT_SIZE - BULK_READ_DATA_AT;

        while out.len() < total {
            let want = per_packet.min(total - out.len());
            let base = out.len();
            let frame = self.bulk_request(Vendor::CALIBRATION, base, want)?;
            let reply = self.send(&frame, true)?.ok_or_else(|| {
                DeviceError::Protocol("lecture calibration sans réponse".to_string())
            })?;
            let raw = reply.raw();
            if raw.len() < BULK_READ_DATA_AT + want {
                return Err(DeviceError::Protocol("trame de calibration tronquée".to_string()));
            }
            out.extend_from_slice(&raw[BULK_READ_DATA_AT..BULK_READ_DATA_AT + want]);
        }
        out.truncate(total);
        Ok(out)
    }

    pub fn populated_mask(&mut self) -> Vec<bool> {
        if let Some(ref mask) = self.populated {
            return mask.clone();
        }
        let mask = match self.read_calibration_status(None) {
            Ok(status) => status.into_iter().map(|v| v != 0).collect(),
            Err(_) => vec![true; self.total_keys()],
        };
        self.populated = Some(mask.clone());
        mask
    }

    // Dynamic Keystroke (DKS)

    pub fn read_dks(&mut self, index: u8) -> Result<Dks, DeviceError> {
        let mut payload = [0u8; MAX_PAYLOAD];
        payload[0] = CUSTOM_CHANNEL;
        payload[1] = Vendor::DKS.id();
        payload[4] = SubField::GET.id();
        payload[5] = index;
        let frame = build(Cmd::GET_KEYBOARD_VALUE, &payload)
            .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        let reply = self.send(&frame, true)?.ok_or_else(|| {
            DeviceError::Protocol(format!("lecture DKS slot {index} sans réponse"))
        })?;
        let raw = reply.raw();
        let span = DKS_ACTION_COUNT * DKS_ACTION_SIZE;
        if raw.len() < 7 + span {
            return Err(DeviceError::Protocol(format!("trame DKS slot {index} incomplète")));
        }
        Dks::from_bytes(index, &raw[7..7 + span]).map_err(|e| DeviceError::Protocol(e.to_string()))
    }

    pub fn write_dks(&mut self, dks: &Dks) -> Result<(), DeviceError> {
        let mut payload = [0u8; MAX_PAYLOAD];
        payload[0] = CUSTOM_CHANNEL;
        payload[1] = Vendor::DKS.id();
        payload[4] = SubField::SET.id();
        payload[5] = dks.index;
        let body = dks.to_bytes();
        payload[6..6 + body.len()].copy_from_slice(&body);
        let frame = build(Cmd::SET_KEYBOARD_VALUE, &payload)
            .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        self.send(&frame, true)?;
        Ok(())
    }

    // Tap Dance / Hold-Click

    pub fn read_tap_dance(&mut self, index: u8) -> Result<TapDance, DeviceError> {
        let mut payload = [0u8; MAX_PAYLOAD];
        payload[0] = Vendor::ENTRY_OP.id();
        payload[1] = Vendor::TAP_DANCE_GET.id();
        payload[2] = index;
        let frame = build(Cmd::VIAL_PREFIX, &payload)
            .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        let reply = self.send(&frame, true)?.ok_or_else(|| {
            DeviceError::Protocol(format!("lecture TapDance slot {index} sans réponse"))
        })?;
        let raw = reply.raw();
        if raw.len() < 10 {
            return Err(DeviceError::Protocol("trame TapDance trop courte".to_string()));
        }
        Ok(TapDance {
            index: raw[3],
            tap: ((raw[4] as u16) << 8) | (raw[5] as u16),
            hold: ((raw[6] as u16) << 8) | (raw[7] as u16),
            timer_ms: u16::from_be_bytes([raw[8], raw[9]]),
        })
    }

    pub fn write_tap_dance(&mut self, td: &TapDance) -> Result<(), DeviceError> {
        let mut payload = [0u8; MAX_PAYLOAD];
        payload[0] = Vendor::ENTRY_OP.id();
        payload[1] = Vendor::TAP_DANCE_SET.id();
        payload[2] = td.index;
        payload[3] = (td.tap >> 8) as u8;
        payload[4] = (td.tap & 0xFF) as u8;
        payload[5] = (td.hold >> 8) as u8;
        payload[6] = (td.hold & 0xFF) as u8;
        let timer_be = td.timer_ms.to_be_bytes();
        payload[7] = timer_be[0];
        payload[8] = timer_be[1];
        let frame = build(Cmd::VIAL_PREFIX, &payload)
            .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        self.send(&frame, true)?;
        Ok(())
    }

    // Advanced Keys (SOCD / RS / OKS)

    pub fn read_advanced_key(&mut self, index: u8) -> Result<AdvancedKey, DeviceError> {
        let mut payload = [0u8; MAX_PAYLOAD];
        payload[0] = CUSTOM_CHANNEL;
        payload[1] = Vendor::RS.id();
        payload[4] = SubField::GET.id();
        payload[5] = index;
        payload[6] = index;
        let frame = build(Cmd::GET_KEYBOARD_VALUE, &payload)
            .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        let reply = self.send(&frame, true)?.ok_or_else(|| {
            DeviceError::Protocol(format!("lecture AdvancedKey slot {index} sans réponse"))
        })?;
        let raw = reply.raw();
        if raw.len() < 18 {
            return Err(DeviceError::Protocol("trame AdvancedKey trop courte".to_string()));
        }
        let mode = AdvancedKeyMode::from_id(raw[7]).unwrap_or(AdvancedKeyMode::None);
        Ok(AdvancedKey {
            index: raw[6],
            mode,
            id: raw[8],
            rs_apc_lv: u16::from_be_bytes([raw[9], raw[10]]),
            gapc_sw: raw[11],
            rt_sw: raw[12],
            key1_row: raw[13],
            key1_col: raw[14],
            key2_row: raw[15],
            key2_col: raw[16],
            layer: raw[17],
        })
    }

    pub fn write_advanced_key(&mut self, key: &AdvancedKey) -> Result<(), DeviceError> {
        let mut payload = [0u8; MAX_PAYLOAD];
        payload[0] = CUSTOM_CHANNEL;
        payload[1] = Vendor::RS.id();
        payload[4] = SubField::SET.id();
        payload[5] = key.index;
        payload[6] = key.mode.id();
        payload[7] = if key.id >= 0x1A { key.id - 0x1A } else { key.id };
        let apc_be = key.rs_apc_lv.to_be_bytes();
        payload[8] = apc_be[0];
        payload[9] = apc_be[1];
        payload[10] = key.gapc_sw;
        payload[11] = key.rt_sw;
        payload[12] = key.key1_row;
        payload[13] = key.key1_col;
        payload[14] = key.key2_row;
        payload[15] = key.key2_col;
        payload[16] = key.layer;
        let frame = build(Cmd::SET_KEYBOARD_VALUE, &payload)
            .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        self.send(&frame, true)?;
        Ok(())
    }

    // Éclairage RVB par touche et global

    pub const MAX_RGB_PER_PACKET: usize = (REPORT_SIZE - 5) / 3; // 9

    pub fn read_key_colors(&mut self, row: u8, col: u8, count: usize) -> Result<Vec<[u8; 3]>, DeviceError> {
        let want = count.min(Self::MAX_RGB_PER_PACKET);
        let frame = build(
            Cmd::CUSTOM_GET_VALUE,
            &[Vendor::GET_CUSTOM_LAMPLIGHT.id(), row, col, want as u8],
        )
        .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        let reply = self.send(&frame, true)?.ok_or_else(|| {
            DeviceError::Protocol("lecture couleurs touches sans réponse".to_string())
        })?;
        let raw = reply.raw();
        let mut out = Vec::with_capacity(want);
        for i in 0..want {
            let base = 5 + i * 3;
            if raw.len() >= base + 3 {
                let [r, g, b] = decode_rgb([raw[base], raw[base + 1], raw[base + 2]]);
                out.push([r, g, b]);
            }
        }
        Ok(out)
    }

    pub fn write_key_colors(&mut self, row: u8, col: u8, colors: &[[u8; 3]]) -> Result<(), DeviceError> {
        if colors.len() > Self::MAX_RGB_PER_PACKET {
            return Err(DeviceError::InvalidArgument(format!(
                "au maximum {} couleurs par paquet, reçu {}",
                Self::MAX_RGB_PER_PACKET,
                colors.len()
            )));
        }
        let mut body = Vec::with_capacity(4 + colors.len() * 3);
        body.push(Vendor::SET_CUSTOM_LAMPLIGHT.id());
        body.push(row);
        body.push(col);
        body.push(colors.len() as u8);
        for c in colors {
            let encoded = encode_rgb(c[0], c[1], c[2]);
            body.extend_from_slice(&encoded);
        }
        let frame = build(Cmd::CUSTOM_SET_VALUE, &body)
            .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        self.send(&frame, true)?;
        Ok(())
    }

    pub fn read_all_key_colors(&mut self) -> Result<Vec<[u8; 3]>, DeviceError> {
        let total = self.total_keys();
        let mut out = Vec::with_capacity(total);
        while out.len() < total {
            let idx = out.len();
            let want = Self::MAX_RGB_PER_PACKET.min(total - idx);
            let (row, col) = self.key_rowcol(idx);
            let colors = self.read_key_colors(row, col, want)?;
            out.extend_from_slice(&colors);
        }
        out.truncate(total);
        Ok(out)
    }

    pub fn write_all_key_colors(&mut self, colors: &[[u8; 3]]) -> Result<usize, DeviceError> {
        let mut packets = 0;
        let mut i = 0;
        let total = colors.len().min(self.total_keys());
        while i < total {
            let chunk = &colors[i..total.min(i + Self::MAX_RGB_PER_PACKET)];
            let (row, col) = self.key_rowcol(i);
            self.write_key_colors(row, col, chunk)?;
            packets += 1;
            i += chunk.len();
        }
        Ok(packets)
    }

    pub fn save_lighting(&mut self) -> Result<(), DeviceError> {
        self.vendor_set(Vendor::SAVE_LAMPLIGHT, &[])?;
        Ok(())
    }

    pub fn read_light_info(&mut self) -> Result<LightInfo, DeviceError> {
        let frame = build(Cmd::CUSTOM_GET_VALUE, &[Vendor::LIGHT_INFO.id()])
            .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        let reply = self.send(&frame, true)?.ok_or_else(|| {
            DeviceError::Protocol("lecture light info sans réponse".to_string())
        })?;
        let raw = reply.raw();
        if raw.len() < 9 {
            return Err(DeviceError::Protocol("trame light info trop courte".to_string()));
        }
        let [r, g, b] = decode_rgb([raw[5], raw[6], raw[7]]);
        Ok(LightInfo {
            effect: raw[2],
            speed: raw[4],
            r,
            g,
            b,
            brightness: raw[8],
        })
    }

    pub fn write_light_info(&mut self, info: &LightInfo) -> Result<(), DeviceError> {
        let mut payload = [0u8; MAX_PAYLOAD];
        payload[0] = Vendor::LIGHT_INFO.id();
        payload[1] = info.effect;
        payload[3] = info.speed;
        let rgb_enc = encode_rgb(info.r, info.g, info.b);
        payload[4..7].copy_from_slice(&rgb_enc);
        payload[7] = info.brightness.min(BRIGHTNESS_MAX);
        let frame = build(Cmd::CUSTOM_SET_VALUE, &payload)
            .map_err(|e| DeviceError::Protocol(e.to_string()))?;
        self.send(&frame, true)?;
        Ok(())
    }

    // Scalaires constructeur

    fn scalar_get(&mut self, sub: Vendor) -> Result<Vec<u8>, DeviceError> {
        let rep = self.vendor_get(sub, &[])?;
        let p = rep.payload();
        if p.len() < 2 {
            return Err(DeviceError::Protocol("scalaire sans données".to_string()));
        }
        Ok(p[2..].to_vec())
    }

    pub fn read_dead_band(&mut self) -> Result<DeadBand, DeviceError> {
        let d = self.scalar_get(Vendor::DEAD_BAND)?;
        if d.len() < 4 {
            return Err(DeviceError::Protocol("DEAD_BAND trop court".to_string()));
        }
        let top = u16::from_be_bytes([d[0], d[1]]) as f64 * 0.01;
        let bot = u16::from_be_bytes([d[2], d[3]]) as f64 * 0.01;
        Ok(DeadBand {
            top_mm: top,
            bottom_mm: bot,
        })
    }

    pub fn write_dead_band(&mut self, band: &DeadBand) -> Result<(), DeviceError> {
        let top_u16 = (band.top_mm * 100.0).round().clamp(0.0, 65535.0) as u16;
        let bot_u16 = (band.bottom_mm * 100.0).round().clamp(0.0, 65535.0) as u16;
        let top_be = top_u16.to_be_bytes();
        let bot_be = bot_u16.to_be_bytes();
        self.vendor_set(Vendor::DEAD_BAND, &[top_be[0], top_be[1], bot_be[0], bot_be[1]])?;
        Ok(())
    }

    pub fn read_feature(&mut self) -> Result<KeyboardFeature, DeviceError> {
        let d = self.scalar_get(Vendor::FEATURE)?;
        if d.len() < 5 {
            return Err(DeviceError::Protocol("FEATURE trop court".to_string()));
        }
        Ok(KeyboardFeature {
            rgb_area: d[0],
            wasd_switch: d[1],
            mac_switch: d[2],
            win_lock: d[3],
            nkro_switch: d[4],
        })
    }

    pub fn write_feature(&mut self, f: &KeyboardFeature) -> Result<(), DeviceError> {
        self.vendor_set(
            Vendor::FEATURE,
            &[f.rgb_area, f.wasd_switch, f.mac_switch, f.win_lock, f.nkro_switch],
        )?;
        Ok(())
    }

    pub fn read_game_mode(&mut self) -> Result<u8, DeviceError> {
        let d = self.scalar_get(Vendor::GAME_MODE)?;
        d.first().copied().ok_or_else(|| DeviceError::Protocol("GAME_MODE vide".to_string()))
    }

    pub fn write_game_mode(&mut self, mode: u8) -> Result<(), DeviceError> {
        self.vendor_set(Vendor::GAME_MODE, &[mode])?;
        Ok(())
    }

    pub fn read_bottom_optimize(&mut self) -> Result<u8, DeviceError> {
        let d = self.scalar_get(Vendor::BOTTOM_OPTIMIZE_SWITCH)?;
        d.first().copied().ok_or_else(|| DeviceError::Protocol("BOTTOM_OPTIMIZE vide".to_string()))
    }

    pub fn write_bottom_optimize(&mut self, opt: u8) -> Result<(), DeviceError> {
        self.vendor_set(Vendor::BOTTOM_OPTIMIZE_SWITCH, &[opt])?;
        Ok(())
    }

    pub fn read_box_light(&mut self) -> Result<BoxLight, DeviceError> {
        let d = self.scalar_get(Vendor::BOX_LIGHT)?;
        if d.len() < 4 {
            return Err(DeviceError::Protocol("BOX_LIGHT trop court".to_string()));
        }
        Ok(BoxLight {
            mode: d[0],
            colorful: d[1],
            brightness: d[2],
            speed: d[3],
        })
    }

    pub fn write_box_light(&mut self, box_light: &BoxLight) -> Result<(), DeviceError> {
        self.vendor_set(
            Vendor::BOX_LIGHT,
            &[box_light.mode, box_light.colorful, box_light.brightness, box_light.speed],
        )?;
        Ok(())
    }

    pub fn read_layer_setting(&mut self) -> Result<u8, DeviceError> {
        let d = self.scalar_get(Vendor::LAYER)?;
        d.first().copied().ok_or_else(|| DeviceError::Protocol("LAYER vide".to_string()))
    }

    pub fn commit_to_flash(&mut self) -> Result<(), DeviceError> {
        let actuation = self.read_actuation_bulk(0, None)?;
        let triggers = self.read_rapid_trigger_bulk(0, None)?;
        self.write_actuation_bulk(&actuation, 0, FlashOp::EraseAndWrite)?;
        self.write_rapid_trigger_bulk(&triggers, 0, FlashOp::EraseAndWrite)?;
        Ok(())
    }
}

#[cfg(test)]
pub mod mock {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// Transport de test enregistrant les écritures et servant une file de réponses prévues.
    #[derive(Clone, Default)]
    pub struct MockHidTransport {
        pub written: Arc<Mutex<Vec<Vec<u8>>>>,
        pub responses: Arc<Mutex<VecDeque<Vec<u8>>>>,
        pub auto_reply: bool,
    }

    impl MockHidTransport {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with_auto_reply() -> Self {
            Self {
                written: Arc::new(Mutex::new(Vec::new())),
                responses: Arc::new(Mutex::new(VecDeque::new())),
                auto_reply: true,
            }
        }

        pub fn queue_response(&self, rep: &[u8]) {
            let mut q = self.responses.lock().unwrap();
            q.push_back(rep.to_vec());
        }

        pub fn last_written(&self) -> Option<Vec<u8>> {
            let w = self.written.lock().unwrap();
            w.last().cloned()
        }
    }

    impl HidTransport for MockHidTransport {
        fn write(&mut self, data: &[u8]) -> Result<usize, DeviceError> {
            let mut w = self.written.lock().unwrap();
            w.push(data.to_vec());
            Ok(data.len())
        }

        fn read_timeout(&mut self, buf: &mut [u8], _timeout_ms: i32) -> Result<usize, DeviceError> {
            let mut q = self.responses.lock().unwrap();
            if let Some(resp) = q.pop_front() {
                let len = buf.len().min(resp.len());
                buf[..len].copy_from_slice(&resp[..len]);
                Ok(len)
            } else if self.auto_reply {
                let mut resp = [0u8; 32];
                let w = self.written.lock().unwrap();
                if let Some(last) = w.last() {
                    let slice = if last.len() == 33 { &last[1..] } else { &last[..] };
                    let len = slice.len().min(resp.len());
                    resp[..len].copy_from_slice(&slice[..len]);
                }
                let len = buf.len().min(resp.len());
                buf[..len].copy_from_slice(&resp[..len]);
                Ok(len)
            } else {
                Ok(0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::MockHidTransport;
    use super::*;

    #[test]
    fn safety_gate_blocks_write_on_read_only_handle() {
        let mut kb = Mad68::new(false, false, 500, None, false);
        let mock = MockHidTransport::new();
        kb.open_with_transport(Box::new(mock), devices::default_board().clone()).unwrap();

        // Tentative d'écriture d'un keycode sur handle lecture seule
        let res = kb.write_keycode(0, 0, 0, 0x0004);
        assert!(matches!(res, Err(DeviceError::WriteBlocked(_))));
    }

    #[test]
    fn safety_gate_blocks_dangerous_command_without_flag() {
        let mut kb = Mad68::new(true, false, 500, None, true);
        let mock = MockHidTransport::new();
        kb.open_with_transport(Box::new(mock), devices::default_board().clone()).unwrap();

        // Réinitialisation EEPROM est une commande dangereuse
        let res = kb.cmd(Cmd::EEPROM_RESET, &[]);
        assert!(matches!(res, Err(DeviceError::WriteBlocked(_))));
    }

    #[test]
    fn write_adds_0x00_report_id_prefix_on_wire() {
        let mut kb = Mad68::new(true, false, 500, None, true);
        let mock = MockHidTransport::new();
        let mock_clone = mock.clone();
        kb.open_with_transport(Box::new(mock), devices::default_board().clone()).unwrap();

        // Prépare une réponse au GET_KEYBOARD_VALUE
        let mut rep = [0u8; 32];
        rep[0] = Cmd::GET_KEYBOARD_VALUE.id();
        rep[1] = 0x01; // value
        mock_clone.queue_response(&rep);

        let _ = kb.keyboard_value(0x01).unwrap();
        let written = mock_clone.last_written().expect("trame écrite");

        // Vérifie que la trame envoyée fait 33 octets et débute par le report ID 0x00
        assert_eq!(written.len(), 33);
        assert_eq!(written[0], 0x00);
        assert_eq!(written[1], Cmd::GET_KEYBOARD_VALUE.id());
    }

    #[test]
    fn read_actuation_bulk_decodes_correctly() {
        let mut kb = Mad68::new(false, false, 500, None, true);
        let mock = MockHidTransport::new();
        let mock_clone = mock.clone();
        kb.open_with_transport(Box::new(mock), devices::default_board().clone()).unwrap();

        // Simule 2 touches : 1.50 mm (150 = 0x0096) et 2.00 mm (200 = 0x00C8)
        let mut rep = [0u8; 32];
        rep[0] = Cmd::GET_KEYBOARD_VALUE.id();
        rep[1] = CUSTOM_CHANNEL;
        rep[2] = Vendor::BUFFER_TOG_TH.id();
        // Données à partir de l'offset BULK_READ_DATA_AT (8)
        rep[8] = 0x00;
        rep[9] = 150;
        rep[10] = 0x00;
        rep[11] = 200;

        mock_clone.queue_response(&rep);

        let vals = kb.read_actuation_bulk(0, Some(2)).unwrap();
        assert_eq!(vals.len(), 2);
        assert!((vals[0] - 1.50).abs() < 1e-4);
        assert!((vals[1] - 2.00).abs() < 1e-4);
    }

    #[test]
    fn buffer_read_pagination_across_chunks() {
        let mut kb = Mad68::new(false, false, 500, None, true);
        let mock = MockHidTransport::new();
        let mock_clone = mock.clone();
        kb.open_with_transport(Box::new(mock), devices::default_board().clone()).unwrap();

        // Demande de 40 octets : nécessite 2 paquets (chunk 1 : 28 octets, chunk 2 : 12 octets)
        let mut rep1 = [0u8; 32];
        rep1[0] = Cmd::DYNAMIC_KEYMAP_GET_BUFFER.id();
        rep1[1] = 0x00; // offset hi
        rep1[2] = 0x00; // offset lo
        rep1[3] = 28;   // size
        for i in 0..28 {
            rep1[4 + i] = (i + 1) as u8;
        }

        let mut rep2 = [0u8; 32];
        rep2[0] = Cmd::DYNAMIC_KEYMAP_GET_BUFFER.id();
        rep2[1] = 0x00; // offset hi
        rep2[2] = 28;   // offset lo
        rep2[3] = 12;   // size
        for i in 0..12 {
            rep2[4 + i] = (29 + i) as u8;
        }

        mock_clone.queue_response(&rep1);
        mock_clone.queue_response(&rep2);

        let buf = kb.read_buffer(Cmd::DYNAMIC_KEYMAP_GET_BUFFER, 40, 0).unwrap();
        assert_eq!(buf.len(), 40);
        for (i, &b) in buf.iter().enumerate() {
            assert_eq!(b, (i + 1) as u8);
        }
    }

    #[test]
    fn send_ignores_interleaved_push_telemetry_stream() {
        let mut kb = Mad68::new(false, false, 500, None, true);
        let mock = MockHidTransport::new();
        let mock_clone = mock.clone();
        kb.open_with_transport(Box::new(mock), devices::default_board().clone()).unwrap();

        // Simule un paquet de télémétrie asynchrone non sollicité (02 96 14 ...)
        let mut push = [0u8; 32];
        push[0] = Cmd::GET_KEYBOARD_VALUE.id();
        push[1] = CUSTOM_CHANNEL;
        push[2] = Vendor::DATA_REPORTING.id(); // 0x14

        // Puis la vraie réponse attendue (02 96 03 ...) pour ONE_TOG_TH
        let mut wanted = [0u8; 32];
        wanted[0] = Cmd::GET_KEYBOARD_VALUE.id();
        wanted[1] = CUSTOM_CHANNEL;
        wanted[2] = Vendor::ONE_TOG_TH.id();
        wanted[3] = 0; // row
        wanted[4] = 0; // col
        wanted[5] = 0x00; // travel hi (100 = 1.00 mm)
        wanted[6] = 100;  // travel lo

        mock_clone.queue_response(&push);
        mock_clone.queue_response(&wanted);

        // Doit ignorer le paquet de push et retourner la réponse de ONE_TOG_TH
        let mm = kb.read_actuation_mm(0, 0).unwrap();
        assert!((mm - 1.00).abs() < 1e-4);
    }
}
