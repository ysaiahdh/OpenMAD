//! OpenMAD — cœur natif Rust du pilote de claviers magnétiques MAD (série 68).
//!
//! Migration de `src/mad68/` (Python) vers Rust. Ce crate expose la couche
//! « métier » du pilote, indépendante de l'interface : protocole filaire,
//! keycodes, table des cartes et structures des fonctions avancées. Les modules
//! dépendant de Windows (USB, overlay, tray) viennent s'y brancher ensuite.
//!
//! Correspondance avec le paquet Python :
//!
//! | Rust           | Python                        |
//! |----------------|-------------------------------|
//! | [`protocol`]   | `mad68/protocol.py`           |
//! | [`keycodes`]   | `mad68/keycodes.py`           |
//! | [`devices`]    | `mad68/devices.py`            |
//! | [`features`]   | `mad68/features.py`           |
//!
//! Comme côté Python, `protocol` ne dépend de rien, `devices` et `features`
//! dépendent de `protocol`, et `keycodes` dépend de la géométrie de `protocol`.

pub mod devices;
pub mod features;
pub mod keycodes;
pub mod protocol;

// Ré-exports alignés sur le sous-ensemble du `__all__` de `mad68/__init__.py`
// qui existe déjà en Rust, pour que les appelants trouvent les types au même
// niveau d'import qu'avant la migration. Les noms encore propres à `device.py`
// (Mad68, find_config_interface, ...) viendront avec le module USB.
pub use protocol::{
    build, build_buffer_read, build_buffer_write, build_vendor, mm_to_travel, travel_to_mm, Cmd,
    KeyboardValue, Reply, Vendor, VOLATILE_KEYBOARD_VALUES,
};
