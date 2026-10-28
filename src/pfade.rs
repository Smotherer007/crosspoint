//! Ablageorte der Konfiguration.
//!
//! Alles, was Crosspoint dauerhaft ablegt, liegt in **einem** Verzeichnis:
//! `$XDG_CONFIG_HOME/crosspoint` (Vorgabe `~/.config/crosspoint`). Bis zur
//! Umbenennung hiess es `vm8-linux`; beim Start wird ein vorhandenes altes
//! Verzeichnis einmalig umbenannt, damit Gerätewahl, Streifenbelegung,
//! Preset und Makros erhalten bleiben.

use std::path::{Path, PathBuf};

/// Verzeichnisname unter `$XDG_CONFIG_HOME`.
pub const KONFIG_ORDNER: &str = "crosspoint";

/// Verzeichnisname der Vorgängerversion – nur noch für die Migration.
pub const ALTER_ORDNER: &str = "vm8-linux";

/// Basisverzeichnis: `$XDG_CONFIG_HOME`, sonst `~/.config`, sonst `.`.
pub fn basis() -> PathBuf {
    if let Some(p) = std::env::var_os("XDG_CONFIG_HOME").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    match std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h).join(".config"),
        None => PathBuf::from(".config"),
    }
}

/// Konfigurationsverzeichnis von Crosspoint.
pub fn konfig_dir() -> PathBuf {
    basis().join(KONFIG_ORDNER)
}

/// Datei im Konfigurationsverzeichnis.
pub fn datei(name: &str) -> PathBuf {
    konfig_dir().join(name)
}

/// Benennt `<basis>/vm8-linux` in `<basis>/crosspoint` um, **nur** wenn das
/// alte Verzeichnis existiert und das neue noch nicht. Ein fehlgeschlagenes
/// Umbenennen ist kein Fehler: dann beginnt Crosspoint mit leerer
/// Konfiguration, statt den Start zu verweigern.
pub fn migriere_in(basis: &Path) -> Option<PathBuf> {
    let alt = basis.join(ALTER_ORDNER);
    let neu = basis.join(KONFIG_ORDNER);
    if !alt.is_dir() || neu.exists() {
        return None;
    }
    std::fs::rename(&alt, &neu).ok().map(|_| neu)
}

/// Migration für das echte Konfigurationsverzeichnis (einmal beim Start).
pub fn migriere() -> Option<PathBuf> {
    migriere_in(&basis())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "crosspoint-pfade-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn datei_liegt_im_konfigurationsverzeichnis() {
        let p = datei("devices.json");
        assert!(p.ends_with(Path::new(KONFIG_ORDNER).join("devices.json")), "{p:?}");
    }

    #[test]
    fn migration_benennt_alten_ordner_um() {
        let basis = temp("alt");
        std::fs::create_dir_all(basis.join(ALTER_ORDNER)).unwrap();
        std::fs::write(basis.join(ALTER_ORDNER).join("preset.json"), "{}").unwrap();

        let neu = migriere_in(&basis).expect("umbenannt");
        assert_eq!(neu, basis.join(KONFIG_ORDNER));
        assert!(neu.join("preset.json").is_file());
        assert!(!basis.join(ALTER_ORDNER).exists());
        let _ = std::fs::remove_dir_all(&basis);
    }

    #[test]
    fn migration_laesst_neuen_ordner_in_ruhe() {
        let basis = temp("beide");
        std::fs::create_dir_all(basis.join(ALTER_ORDNER)).unwrap();
        std::fs::create_dir_all(basis.join(KONFIG_ORDNER)).unwrap();
        assert!(migriere_in(&basis).is_none());
        assert!(basis.join(ALTER_ORDNER).is_dir(), "alter Ordner bleibt liegen");
        let _ = std::fs::remove_dir_all(&basis);
    }

    #[test]
    fn migration_ohne_alten_ordner_ist_folgenlos() {
        let basis = temp("leer");
        assert!(migriere_in(&basis).is_none());
        assert!(!basis.join(KONFIG_ORDNER).exists());
        let _ = std::fs::remove_dir_all(&basis);
    }
}
