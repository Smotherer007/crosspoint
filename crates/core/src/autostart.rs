//! Autostart-Eintrag: Crosspoint startet beim Anmelden im Hintergrund (Tray).
//!
//! Geschrieben wird eine gewöhnliche `.desktop`-Datei nach
//! `$XDG_CONFIG_HOME/autostart` bzw. `~/.config/autostart` – das lesen alle
//! gängigen Sitzungsverwalter (KDE, GNOME, Xfce). Der Startbefehl hängt davon
//! ab, wo Crosspoint läuft:
//!
//! * im Flatpak: `flatpak run de.weppelmann.Crosspoint --minimized` (der
//!   Host-Sitzungsverwalter startet die App von außen, nicht im Sandbox),
//! * auf dem Host: `<repo>/scripts/crosspoint-launch.sh --minimized`.
//!
//! Beide Wege geben `--minimized` weiter; das Fenster startet damit ohne
//! Fenster und legt nur sein Tray-Icon an.
//!
//! `CROSSPOINT_AUTOSTART_DIR` verlegt das Zielverzeichnis (Tests, andere
//! Sitzungen), `CROSSPOINT_LAUNCH` den Startbefehl auf dem Host.

use std::path::{Path, PathBuf};

use crate::json::Value;

/// App-Kennung – Name der Autostart-Datei und des Flatpak-Refs.
pub const APP_ID: &str = "de.weppelmann.Crosspoint";

/// Dateiname im Autostart-Verzeichnis.
const DATEI: &str = "de.weppelmann.Crosspoint.desktop";

/// Läuft der Prozess im Flatpak-Sandbox?
pub fn im_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").is_file()
}

/// Verzeichnis der Autostart-Dateien.
///
/// Im Flatpak zeigt `XDG_CONFIG_HOME` in den Sandbox-Ordner
/// (`~/.var/app/<id>/config`); der Sitzungsanmelder liest aber den echten
/// `~/.config/autostart` des Hosts. `$HOME` bleibt auch im Sandbox das echte
/// Zuhause, und `--filesystem=xdg-config/autostart:create` gibt genau diesen
/// Pfad frei – deshalb dort nachsehen statt in `pfade::basis()`.
pub fn verzeichnis() -> PathBuf {
    if let Some(p) = std::env::var_os("CROSSPOINT_AUTOSTART_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    if im_flatpak() {
        if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            return PathBuf::from(home).join(".config").join("autostart");
        }
    }
    crate::pfade::basis().join("autostart")
}

/// Pfad der Autostart-Datei.
pub fn pfad() -> PathBuf {
    verzeichnis().join(DATEI)
}

/// Startbefehl für die Autostart-Datei.
pub fn exec() -> String {
    if im_flatpak() {
        return format!("flatpak run {APP_ID} --minimized");
    }
    if let Some(p) = std::env::var_os("CROSSPOINT_LAUNCH").filter(|p| !p.is_empty()) {
        return format!("{} --minimized", PathBuf::from(p).display());
    }
    match startskript() {
        Some(s) => format!("{} --minimized", s.display()),
        None => "crosspoint-launch --minimized".to_string(),
    }
}

/// Startskript des Repos: gesucht wird von der Binary aufwärts
/// (`target/release/crosspoint-engine` → `target` → Repo-Wurzel).
pub fn startskript() -> Option<PathBuf> {
    let mut kandidaten: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(Path::to_path_buf);
        for _ in 0..4 {
            match dir {
                Some(d) => {
                    kandidaten.push(d.join("scripts/crosspoint-launch.sh"));
                    kandidaten.push(d.join("crosspoint-launch.sh"));
                    dir = d.parent().map(Path::to_path_buf);
                }
                None => break,
            }
        }
    }
    kandidaten.push(PathBuf::from("scripts/crosspoint-launch.sh"));
    kandidaten.into_iter().find(|p| p.is_file())
}

/// Inhalt der `.desktop`-Datei. `X-GNOME-Autostart-enabled` liest GNOME,
/// `Hidden=false` hält KDE und Xfce bei der Stange; `NoDisplay=true` sorgt
/// dafür, dass der Eintrag nicht zusätzlich im Anwendungsmenü auftaucht.
pub fn inhalt(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Crosspoint\n\
         GenericName=Audio-Mixer\n\
         Comment=Crosspoint im Hintergrund starten (Tray-Icon)\n\
         Exec={exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         NoDisplay=true\n\
         Hidden=false\n\
         X-GNOME-Autostart-enabled=true\n\
         Categories=AudioVideo;Audio;Mixer;\n"
    )
}

/// Ist der Autostart eingetragen?
pub fn enabled() -> bool {
    pfad().is_file()
}

/// Wie [`enabled`], aber für einen ausdrücklichen Pfad.
pub fn enabled_in(pfad: &Path) -> bool {
    pfad.is_file()
}

/// Schreibt oder entfernt die Autostart-Datei an `pfad`.
pub fn set_in(pfad: &Path, exec: &str, enabled: bool) -> Result<(), String> {
    if !enabled {
        if pfad.is_file() {
            std::fs::remove_file(pfad)
                .map_err(|e| format!("{} nicht entfernbar: {e}", pfad.display()))?;
        }
        return Ok(());
    }
    if let Some(dir) = pfad.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("{} nicht anlegbar: {e}", dir.display()))?;
    }
    std::fs::write(pfad, inhalt(exec))
        .map_err(|e| format!("{} nicht schreibbar: {e}", pfad.display()))
}

/// Schaltet den Autostart ein bzw. aus; liefert den Pfad der Datei.
pub fn set(enabled: bool) -> Result<PathBuf, String> {
    let pfad = pfad();
    set_in(&pfad, &exec(), enabled)?;
    Ok(pfad)
}

/// Zustand für die Oberfläche/API: `{enabled, path, exec, flatpak}`.
pub fn status_json() -> String {
    let pfad = pfad();
    Value::obj(vec![
        ("enabled", Value::Bool(enabled_in(&pfad))),
        ("path", Value::str(pfad.display().to_string())),
        ("exec", Value::str(exec())),
        ("flatpak", Value::Bool(im_flatpak())),
    ])
    .to_json()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("crosspoint-autostart-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("autostart").join(DATEI)
    }

    #[test]
    fn inhalt_traegt_exec_und_autostart_schalter() {
        let text = inhalt("flatpak run de.weppelmann.Crosspoint --minimized");
        assert!(text.starts_with("[Desktop Entry]\n"), "{text}");
        assert!(text.contains("Exec=flatpak run de.weppelmann.Crosspoint --minimized\n"));
        assert!(text.contains("Icon=de.weppelmann.Crosspoint\n"));
        assert!(text.contains("X-GNOME-Autostart-enabled=true\n"));
        assert!(
            text.contains("NoDisplay=true\n"),
            "kein zweiter Menueeintrag"
        );
    }

    #[test]
    fn set_schreibt_und_entfernt_wieder() {
        let pfad = tmp("set");
        assert!(!enabled_in(&pfad));
        set_in(&pfad, "crosspoint-launch.sh --minimized", true).unwrap();
        assert!(enabled_in(&pfad));
        let text = std::fs::read_to_string(&pfad).unwrap();
        assert!(text.contains("Exec=crosspoint-launch.sh --minimized"));

        // Erneut einschalten ist folgenlos (idempotent).
        set_in(&pfad, "crosspoint-launch.sh --minimized", true).unwrap();
        assert!(enabled_in(&pfad));

        set_in(&pfad, "egal", false).unwrap();
        assert!(!enabled_in(&pfad));
        // Ausschalten ohne Datei ist ebenfalls folgenlos.
        set_in(&pfad, "egal", false).unwrap();
        let _ = std::fs::remove_dir_all(pfad.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn status_json_meldet_zustand_und_pfad() {
        let wert = crate::json::parse(&status_json()).unwrap();
        assert!(wert.get("path").is_some());
        assert!(wert.get("exec").is_some());
        assert_eq!(wert.get("enabled").unwrap(), &Value::Bool(enabled()));
        assert!(wert.get("flatpak").unwrap().as_bool().is_some());
        assert!(
            wert.get("path")
                .unwrap()
                .to_json()
                .contains("autostart/de.weppelmann.Crosspoint.desktop"),
            "{}",
            wert.get("path").unwrap().to_json()
        );
    }

    #[test]
    fn exec_enthaelt_minimized_und_flatpak_pfad() {
        let e = exec();
        assert!(e.ends_with("--minimized"), "{e}");
        if im_flatpak() {
            assert_eq!(e, "flatpak run de.weppelmann.Crosspoint --minimized");
        } else {
            assert!(e.contains("crosspoint-launch"), "{e}");
        }
    }
}
