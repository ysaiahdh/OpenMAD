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
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Sauvegarde complète de la configuration matérielle du clavier vers un fichier JSON
    Backup(openmad_rs::backup::BackupArgs),

    /// Restauration de la configuration matérielle depuis un fichier de sauvegarde
    Restore(openmad_rs::restore::RestoreArgs),
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
    attach_console();

    let cli = Cli::parse();
    let code = match cli.command {
        Commands::Backup(args) => openmad_rs::backup::run(&args)?,
        Commands::Restore(args) => openmad_rs::restore::run(&args)?,
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
    fn test_cli_parse_backup_defaults() {
        let cli = Cli::try_parse_from(["openmad-rs", "backup"]).unwrap();
        match cli.command {
            Commands::Backup(args) => {
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
            Commands::Backup(args) => {
                assert_eq!(args.out, Some(PathBuf::from("dump.json")));
            }
            _ => panic!("Sous-commande inattendue"),
        }

        let cli_show = Cli::try_parse_from(["openmad-rs", "backup", "--show", "dump.json"]).unwrap();
        match cli_show.command {
            Commands::Backup(args) => {
                assert_eq!(args.show, Some(PathBuf::from("dump.json")));
            }
            _ => panic!("Sous-commande inattendue"),
        }

        let cli_verify = Cli::try_parse_from(["openmad-rs", "backup", "--verify", "dump.json"]).unwrap();
        match cli_verify.command {
            Commands::Backup(args) => {
                assert_eq!(args.verify, Some(PathBuf::from("dump.json")));
            }
            _ => panic!("Sous-commande inattendue"),
        }
    }

    #[test]
    fn test_cli_parse_restore_nominal() {
        let cli = Cli::try_parse_from(["openmad-rs", "restore", "backup_2026.json", "--apply"]).unwrap();
        match cli.command {
            Commands::Restore(args) => {
                assert_eq!(args.backup, PathBuf::from("backup_2026.json"));
                assert!(args.apply);
            }
            _ => panic!("Sous-commande inattendue"),
        }

        let cli_dry = Cli::try_parse_from(["openmad-rs", "restore", "backup_2026.json"]).unwrap();
        match cli_dry.command {
            Commands::Restore(args) => {
                assert_eq!(args.backup, PathBuf::from("backup_2026.json"));
                assert!(!args.apply);
            }
            _ => panic!("Sous-commande inattendue"),
        }
    }
}
