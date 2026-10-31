// Masque automatiquement la fenêtre d'invite de commandes Windows uniquement en mode --release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    println!("OpenMAD Rust initialisé !");
}