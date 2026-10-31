//! OpenMAD — cœur natif Rust du pilote de claviers magnétiques MAD (série 68).
//!
//! Migration de `src/mad68/` (Python) vers Rust. Ce crate expose la couche
//! « métier » du pilote, indépendante de l'interface : protocole filaire,
//! keycodes, table des cartes, structures des fonctions avancées, profils
//! déclaratifs, paramètres et gestion USB HID.
//!
//! Correspondance avec le paquet Python :
//!
//! | Rust           | Python                        |
//! |----------------|-------------------------------|
//! | [`protocol`]   | `mad68/protocol.py`           |
//! | [`keycodes`]   | `mad68/keycodes.py`           |
//! | [`devices`]    | `mad68/devices.py`            |
//! | [`features`]   | `mad68/features.py`           |
//! | [`version`]    | `mad68/version.py`            |
//! | [`paths`]      | `mad68/paths.py`              |
//! | [`settings`]   | `mad68/settings.py`           |
//! | [`device`]     | `mad68/device.py`             |
//! | [`profile`]    | `mad68/profile.py`            |

pub mod device;
pub mod devices;
pub mod features;
pub mod keycodes;
pub mod paths;
pub mod profile;
pub mod protocol;
pub mod settings;
pub mod version;

// Ré-exports alignés sur `mad68/__init__.py`
pub use device::{
    find_config_interface, in_bootloader, DeviceError, HidTransport, Mad68, RapidTrigger,
};
pub use profile::{
    default_profile, ensure_default_profile, ApplyPlan, Profile, ProfileError, RapidTriggerSpec,
};
pub use protocol::{
    build, build_buffer_read, build_buffer_write, build_vendor, mm_to_travel, travel_to_mm, Cmd,
    KeyboardValue, Reply, Vendor, VOLATILE_KEYBOARD_VALUES,
};
pub use settings::AppSettings;
