# Instructions de migration : OpenMAD (Python -> Rust)

## Contexte du projet

Ce dépôt contient `OpenMAD`, un outil écrit en Python permettant de gérer, configurer et afficher un HUD pour les claviers magnétiques (série MAD68) via USB HID.
L'objectif est de migrer l'intégralité du code Python (`src/mad68/` et `tools/`) vers une application native en **Rust** performante, sécurisée et sans dépendances Python.

## Règles strictes pour le code Rust

1. **Commentaires obligatoires** : Ajoute systématiquement des commentaires détaillés expliquant les parties complexes (notamment la construction des paquets HID, les masques binaires, les offsets dans `protocol.rs` et le multi-threading).
2. **Gestion des erreurs** : N'utilise jamais `.unwrap()` en production. Utilise `thiserror` pour les erreurs de bibliothèque (`src/mad68/`) et `anyhow` pour les exécutables/outils.
3. **Architecture cible** :
   - `src/protocol.rs` : Traduction stricte bit-à-bit de `src/mad68/protocol.py`.
   - `src/device.rs` & `src/devices.rs` : Gestion USB via la crate `hidapi`.
   - `src/keycodes.rs` : Énumérations typées (`#[repr(u8)]` ou `#[repr(u16)]`) pour les keycodes.
   - `src/profile.rs` & `src/settings.rs` : Structures sérialisables avec `serde` et `serde_json` compatibles à 100% avec le format `settings.json` existant.
   - `src/hud.rs` : Fenêtre d'overlay légère et transparente.
   - `src/main.rs` : Point d'entrée unique avec `clap` (sous-commandes `backup`, `restore`, `hud`, `tray`).
4. **Validation** : Chaque module migré doit compiler avec `cargo check` et inclure des tests unitaires vérifiant que les trames d'octets générées en Rust sont identiques à celles de Python.

## Spécificités Windows (Cible principale)

- **Compilateur** : Cible `x86_64-pc-windows-msvc`.
- **Switcher (`src/switcher.rs`)** : Utilise la crate `windows` (`GetForegroundWindow`, `GetWindowThreadProcessId`, `OpenProcess` et `QueryFullProcessImageNameW`) pour identifier le nom de l'exécutable actif au premier plan sans dépendance externe lourde.
- **HUD (`src/hud.rs`)** : Crée un overlay léger toujours au premier plan (`WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW`) afin de ne jamais voler le focus clavier/souris pendant un jeu plein écran et de ne pas apparaître dans `Alt+Tab`.
- **Packaging** : Conserve la compatibilité avec le script Inno Setup `packaging/mad68.iss` en générant un binaire autonome dans `target/release/`.
