// Masque automatiquement la fenêtre d'invite de commandes Windows uniquement en mode --release (subsystem windows)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "openmad-rs",
    version = openmad_rs::version::app_version(),
    about = "Pilote et utilitaire natif Windows pour claviers magnétiques MAD68"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Sauvegarde complète de la configuration matérielle du clavier vers un fichier JSON
    Backup(openmad_rs::backup::BackupArgs),

    /// Restauration de la configuration matérielle depuis un fichier de sauvegarde
    Restore(openmad_rs::restore::RestoreArgs),

    /// Serveur local et overlay HUD de télémétrie en temps réel
    Hud(openmad_rs::hud::HudArgs),

    /// Icône de barre des tâches avec bascule automatique des profils
    Tray(openmad_rs::tray::TrayArgs),
}

/// Rattache le processus à la console Windows parente (PowerShell / CMD) si elle existe.
///
/// Sous Windows, un binaire compilé avec `#![windows_subsystem = "windows"]` ne possède
/// pas de console par défaut lors d'un double-clic (évite le popup noir parasite).
/// En revanche, lorsqu'il est exécuté depuis un terminal en ligne de commande, cette fonction
/// attache stdout/stderr pour que les affichages et aides `--help` restent visibles.
#[cfg(windows)]
fn attach_console() {
    unsafe {
        use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_console() {}

fn main() -> anyhow::Result<()> {
    // Si des arguments sont fournis en ligne de commande, on attache la console pour voir les sorties
    if std::env::args().len() > 1 {
        attach_console();
    }

    let cli = Cli::parse();
    let code = match cli.command {
        Some(Commands::Backup(args)) => openmad_rs::backup::run(&args)?,
        Some(Commands::Restore(args)) => openmad_rs::restore::run(&args)?,
        Some(Commands::Hud(args)) => openmad_rs::hud::run(&args)?,
        Some(Commands::Tray(args)) => openmad_rs::tray::run(&args)?,
        None => {
            // Lancement par défaut lors d'un double-clic sans arguments : mode barre des tâches (tray)
            openmad_rs::tray::run(&openmad_rs::tray::TrayArgs::default())?
        }
    };

    if code != 0 {
        std::process::exit(code);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn test_cli_parse_default_is_none() {
        let cli = Cli::try_parse_from(["openmad-rs"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_cli_parse_backup_defaults() {
        let cli = Cli::try_parse_from(["openmad-rs", "backup"]).unwrap();
        match cli.command {
            Some(Commands::Backup(args)) => {
                assert!(args.show.is_none());
                assert!(args.out.is_none());
                assert!(args.verify.is_none());
            }
            _ => panic!("Sous-commande inattendue"),
        }
    }

    #[test]
    fn test_cli_parse_backup_options() {
        let cli = Cli::try_parse_from(["openmad-rs", "backup", "--out", "dump.json"]).unwrap();
        match cli.command {
            Some(Commands::Backup(args)) => {
                assert_eq!(args.out, Some(PathBuf::from("dump.json")));
            }
            _ => panic!("Sous-commande inattendue"),
        }

        let cli_show = Cli::try_parse_from(["openmad-rs", "backup", "--show", "dump.json"]).unwrap();
        match cli_show.command {
            Some(Commands::Backup(args)) => {
                assert_eq!(args.show, Some(PathBuf::from("dump.json")));
            }
            _ => panic!("Sous-commande inattendue"),
        }

        let cli_verify = Cli::try_parse_from(["openmad-rs", "backup", "--verify", "dump.json"]).unwrap();
        match cli_verify.command {
            Some(Commands::Backup(args)) => {
                assert_eq!(args.verify, Some(PathBuf::from("dump.json")));
            }
            _ => panic!("Sous-commande inattendue"),
        }
    }

    #[test]
    fn test_cli_parse_restore_nominal() {
        let cli = Cli::try_parse_from(["openmad-rs", "restore", "backup_2026.json", "--apply"]).unwrap();
        match cli.command {
            Some(Commands::Restore(args)) => {
                assert_eq!(args.backup, PathBuf::from("backup_2026.json"));
                assert!(args.apply);
            }
            _ => panic!("Sous-commande inattendue"),
        }

        let cli_dry = Cli::try_parse_from(["openmad-rs", "restore", "backup_2026.json"]).unwrap();
        match cli_dry.command {
            Some(Commands::Restore(args)) => {
                assert_eq!(args.backup, PathBuf::from("backup_2026.json"));
                assert!(!args.apply);
            }
            _ => panic!("Sous-commande inattendue"),
        }
    }

    #[test]
    fn test_cli_parse_hud_options() {
        let cli = Cli::try_parse_from(["openmad-rs", "hud", "--port", "9090", "--no-browser", "--overlay"]).unwrap();
        match cli.command {
            Some(Commands::Hud(args)) => {
                assert_eq!(args.port, 9090);
                assert!(args.no_browser);
                assert!(args.overlay);
            }
            _ => panic!("Sous-commande inattendue"),
        }
    }

    #[test]
    fn test_cli_parse_tray_options() {
        let cli = Cli::try_parse_from(["openmad-rs", "tray", "--dry-run"]).unwrap();
        match cli.command {
            Some(Commands::Tray(args)) => {
                assert!(args.dry_run);
            }
            _ => panic!("Sous-commande inattendue"),
        }
    }
}
