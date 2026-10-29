//! Oberflaechen-Konfiguration: `~/.config/crosspoint/ui.json`.
//!
//! Eine Datei, zwei getrennte Modelle: `theme` (Farben/Schrift/Masse) und
//! `layout` (Anordnung). Beide Abschnitte sind optional; eine kaputte Datei
//! fuehrt zu den eingebauten Vorgaben plus einer Logzeile - nie zum Absturz.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::{LayoutAbschnitt, LayoutSet};
use crate::theme::{konfig_pfad, ThemeAbschnitt, ThemeSet};

/// Rohform der Datei. Unbekannte Abschnitte/Felder werden ignoriert.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UiDatei {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeAbschnitt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutAbschnitt>,
}

/// Gelesene Konfiguration samt Logzeilen.
#[derive(Clone, Debug)]
pub struct Konfiguration {
    pub theme: ThemeSet,
    pub layout: LayoutSet,
    pub pfad: PathBuf,
    /// Hinweise fuer die Statuszeile ("Theme unbekannt", "Datei kaputt", ...).
    pub log: Vec<String>,
    /// Eigene (nicht eingebaute) Namen, damit nur diese geschrieben werden.
    pub eigene_themes: Vec<String>,
    pub eigene_layouts: Vec<String>,
}

impl Default for Konfiguration {
    fn default() -> Self {
        Konfiguration {
            theme: ThemeSet::default(),
            layout: LayoutSet::default(),
            pfad: konfig_pfad(),
            log: Vec::new(),
            eigene_themes: Vec::new(),
            eigene_layouts: Vec::new(),
        }
    }
}

impl Konfiguration {
    /// Datei lesen. Fehlt sie, gelten die Vorgaben ohne Logzeile.
    pub fn laden(pfad: impl AsRef<Path>) -> Konfiguration {
        let pfad = pfad.as_ref().to_path_buf();
        let mut k = Konfiguration {
            pfad,
            ..Konfiguration::default()
        };
        let text = match std::fs::read_to_string(&k.pfad) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return k,
            Err(e) => {
                k.log.push(format!("{}: {}", k.pfad.display(), e));
                return k;
            }
        };
        let datei: UiDatei = match serde_json::from_str(&text) {
            Ok(d) => d,
            Err(e) => {
                // Kaputte Datei: Vorgaben, Logzeile, kein Absturz. Die Datei
                // bleibt unangetastet, bis der Nutzer etwas aendert.
                k.log
                    .push(format!("ui.json unlesbar ({e}) - Vorgaben aktiv"));
                return k;
            }
        };
        let (theme, log_t) = ThemeSet::aus_abschnitt(datei.theme.as_ref());
        let (layout, log_l) = LayoutSet::aus_abschnitt(datei.layout.as_ref());
        k.log.extend(log_t);
        k.log.extend(log_l);
        k.eigene_themes = eigene_namen(
            datei.theme.as_ref().and_then(|a| a.themes.as_ref()),
            crate::theme::eingebaute_themes().keys().cloned().collect(),
        );
        k.eigene_layouts = eigene_namen(
            datei.layout.as_ref().and_then(|a| a.layouts.as_ref()),
            crate::layout::Layout::vorgaben()
                .into_iter()
                .map(|(n, _)| n.to_string())
                .collect(),
        );
        k.theme = theme;
        k.layout = layout;
        k
    }

    /// Aktuellen Stand schreiben. Fehler landen im Log, nicht im Absturz.
    pub fn speichern(&mut self) {
        let datei = UiDatei {
            theme: Some(self.theme.als_abschnitt(&self.eigene_themes)),
            layout: Some(self.layout.als_abschnitt(&self.eigene_layouts)),
        };
        if let Some(ordner) = self.pfad.parent() {
            if let Err(e) = std::fs::create_dir_all(ordner) {
                self.log.push(format!("{}: {}", ordner.display(), e));
                return;
            }
        }
        match serde_json::to_string_pretty(&datei) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&self.pfad, text + "\n") {
                    self.log.push(format!("ui.json nicht schreibbar: {e}"));
                }
            }
            Err(e) => self.log.push(format!("ui.json nicht serialisierbar: {e}")),
        }
    }
}

/// Namen aus der Datei, die nicht zur eingebauten Liste gehoeren.
fn eigene_namen<T>(abschnitt: Option<&BTreeMap<String, T>>, eingebaut: Vec<String>) -> Vec<String> {
    let Some(abschnitt) = abschnitt else {
        return Vec::new();
    };
    abschnitt
        .keys()
        .filter(|n| !eingebaut.contains(n))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_pfad(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("crosspoint-ui-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ui.json")
    }

    #[test]
    fn fehlende_datei_ergibt_vorgaben() {
        let k = Konfiguration::laden(temp_pfad("fehlt").with_file_name("gibtsnicht.json"));
        assert!(k.log.is_empty());
        assert_eq!(k.theme.aktiv, crate::theme::STANDARD);
        assert_eq!(k.layout.aktiv, "klassisch");
    }

    #[test]
    fn kaputte_datei_ergibt_vorgaben_mit_logzeile() {
        let pfad = temp_pfad("kaputt");
        std::fs::write(&pfad, "{ das ist kein json").unwrap();
        let k = Konfiguration::laden(&pfad);
        assert_eq!(k.log.len(), 1);
        assert_eq!(k.theme.aktiv, crate::theme::STANDARD);
        let _ = std::fs::remove_dir_all(pfad.parent().unwrap());
    }

    #[test]
    fn beide_abschnitte_werden_gelesen() {
        let pfad = temp_pfad("beide");
        std::fs::write(
            &pfad,
            r##"{
                "theme": { "aktiv": "crosspoint-hell" },
                "layout": { "aktiv": "breit" },
                "unbekannt": { "egal": 1 }
            }"##,
        )
        .unwrap();
        let k = Konfiguration::laden(&pfad);
        assert!(k.log.is_empty(), "{:?}", k.log);
        assert_eq!(k.theme.aktiv, "crosspoint-hell");
        assert_eq!(k.layout.aktiv, "breit");
        let _ = std::fs::remove_dir_all(pfad.parent().unwrap());
    }

    #[test]
    fn speichern_und_wieder_laden_ist_stabil() {
        let pfad = temp_pfad("rundlauf");
        let mut k = Konfiguration::laden(&pfad);
        k.theme.aktiv = "mono".into();
        k.layout.aktiv = "kompakt".into();
        k.speichern();
        assert!(k.log.is_empty(), "{:?}", k.log);
        let zurueck = Konfiguration::laden(&pfad);
        assert_eq!(zurueck.theme.aktiv, "mono");
        assert_eq!(zurueck.layout.aktiv, "kompakt");
        // Ohne eigene Themes bleibt die Datei frei davon.
        let text = std::fs::read_to_string(&pfad).unwrap();
        assert!(!text.contains("themes"));
        let _ = std::fs::remove_dir_all(pfad.parent().unwrap());
    }
}
