//! MIDI-Zuordnung als MIDIMonster-Konfiguration.
//!
//! Kein ALSA-Code in der Oberflaeche: die Zuordnung ist eine Datei
//! `~/.config/crosspoint/midimonster.cfg` in MIDIMonster-Syntax
//! (`cc1.<nr>` als Quelle, `/strip/<n>/gain` als Ziel). MIDIMonster liest sie,
//! uebersetzt MIDI nach OSC und schickt alles an den OSC-Eingang des Kerns.

use std::path::{Path, PathBuf};

/// Seiten des Dialogs (wie im Original).
pub const SEITEN: usize = 6;
/// CC-Nummern der Zusatzzeilen "Control Change n".
pub const FREIE_CCS: usize = 56;

/// Ein Ziel mit optionaler CC-Bindung.
#[derive(Clone, Debug, PartialEq)]
pub struct Zeile {
    /// Beschriftung im Dialog, z. B. "Gain Fader Strip #1".
    pub titel: String,
    /// OSC-Ziel im Kern, z. B. `/strip/0/gain`.
    pub ziel: String,
    /// Gebundene CC-Nummer (None = "-").
    pub cc: Option<u8>,
}

impl Zeile {
    pub fn neu(titel: impl Into<String>, ziel: impl Into<String>) -> Zeile {
        Zeile {
            titel: titel.into(),
            ziel: ziel.into(),
            cc: None,
        }
    }
}

/// Ein- und Ausgabegeraet sowie die Zuordnungen.
#[derive(Clone, Debug, PartialEq)]
pub struct Karte {
    pub name: String,
    pub eingang: String,
    pub ausgang: String,
    pub zeilen: Vec<Zeile>,
}

/// Ziele einer Seite - wie im Original benannt.
pub fn seiten_ziele(seite: usize) -> Vec<Zeile> {
    let s = seite.min(SEITEN - 1);
    let mut z = Vec::new();
    match s {
        0 => {
            for i in 0..8 {
                z.push(Zeile::neu(
                    format!("Gain Fader Strip #{}", i + 1),
                    format!("/strip/{i}/gain"),
                ));
            }
        }
        1 => {
            for i in 0..8 {
                z.push(Zeile::neu(
                    format!("Gain of Virtual Strip #{}", i + 1),
                    format!("/strip/{}/gain", i + 5),
                ));
            }
        }
        2 => {
            for i in 0..8 {
                z.push(Zeile::neu(
                    format!("LR PAN Strip #{}", i + 1),
                    format!("/strip/{i}/pan"),
                ));
            }
        }
        3 => {
            for i in 0..8 {
                z.push(Zeile::neu(
                    format!("Mute Strip #{}", i + 1),
                    format!("/strip/{i}/mute"),
                ));
            }
        }
        4 => {
            for i in 0..8 {
                z.push(Zeile::neu(
                    format!("Solo Virtual Strip #{}", i + 1),
                    format!("/strip/{}/solo", i + 5),
                ));
            }
        }
        _ => {
            for i in 0..FREIE_CCS {
                z.push(Zeile::neu(
                    format!("Control Change {i}"),
                    format!("/midi/cc/{i}"),
                ));
            }
        }
    }
    z
}

impl Karte {
    /// Leere Karte mit den Zielen der ersten Seite.
    pub fn leer() -> Karte {
        Karte {
            name: "Crosspoint".into(),
            eingang: "kein Geraet".into(),
            ausgang: "kein Geraet".into(),
            zeilen: seiten_ziele(0),
        }
    }

    /// Alle Zeilen aller Seiten (die Datei kennt keine Seiten).
    pub fn alle(&mut self) {
        if self.zeilen.len() >= SEITEN * 8 {
            return;
        }
        let mut neu = Vec::new();
        for s in 0..SEITEN {
            for z in seiten_ziele(s) {
                neu.push(z);
            }
        }
        // Bereits gebundene CCs uebernehmen (gleiches Ziel).
        for alt in &self.zeilen {
            if alt.cc.is_some() {
                if let Some(n) = neu.iter_mut().find(|n| n.ziel == alt.ziel) {
                    n.cc = alt.cc;
                }
            }
        }
        self.zeilen = neu;
    }

    /// Ziele einer Seite mit den aktuell gebundenen CCs.
    pub fn seite(&self, seite: usize) -> Vec<Zeile> {
        let mut ziele = seiten_ziele(seite);
        for z in ziele.iter_mut() {
            if let Some(gebunden) = self.zeilen.iter().find(|g| g.ziel == z.ziel) {
                z.cc = gebunden.cc;
            }
        }
        ziele
    }

    /// Bindet eine CC an ein Ziel (ersetzt eine alte Bindung derselben CC).
    pub fn binden(&mut self, ziel: &str, cc: u8) {
        for z in self.zeilen.iter_mut() {
            if z.cc == Some(cc) && z.ziel != ziel {
                z.cc = None;
            }
        }
        if let Some(z) = self.zeilen.iter_mut().find(|z| z.ziel == ziel) {
            z.cc = Some(cc);
        }
    }

    /// Loescht die Bindung einer Zeile.
    pub fn loesen(&mut self, ziel: &str) {
        if let Some(z) = self.zeilen.iter_mut().find(|z| z.ziel == ziel) {
            z.cc = None;
        }
    }

    /// Leert alle Bindungen ("Reset Map").
    pub fn zuruecksetzen(&mut self) {
        for z in self.zeilen.iter_mut() {
            z.cc = None;
        }
    }

    /// Anzahl gebundener Zeilen.
    pub fn gebunden(&self) -> usize {
        self.zeilen.iter().filter(|z| z.cc.is_some()).count()
    }
}

/// Standardpfad: `~/.config/crosspoint/midimonster.cfg`.
pub fn pfad() -> PathBuf {
    crate::theme::konfig_pfad()
        .parent()
        .map(|p| p.join("midimonster.cfg"))
        .unwrap_or_else(|| PathBuf::from("midimonster.cfg"))
}

/// Wertet `cc1.<nr>` bzw. `ch<kanal>.cc<nr>` als CC-Nummer aus.
fn cc_aus(quelle: &str) -> Option<u8> {
    let teil = quelle.rsplit('.').next()?;
    let nr = teil
        .strip_prefix("cc")
        .or_else(|| teil.strip_prefix("note"))?;
    nr.trim_matches(|c| c == '{' || c == '}').parse().ok()
}

impl Karte {
    /// Datei lesen. Fehlt sie, kommt eine leere Karte.
    pub fn laden(p: impl AsRef<Path>) -> Karte {
        let mut k = Karte::leer();
        k.alle();
        let Ok(text) = std::fs::read_to_string(p) else {
            return k;
        };
        let mut abschnitt = String::new();
        for roh in text.lines() {
            let zeile = roh.trim();
            if zeile.starts_with(';') || zeile.is_empty() {
                continue;
            }
            if zeile.starts_with('[') {
                abschnitt = zeile.trim_matches(|c| c == '[' || c == ']').to_string();
                let mut teile = abschnitt.split_whitespace();
                match (teile.next(), teile.next()) {
                    (Some("midi"), Some(name)) => {
                        if k.eingang == "kein Geraet" {
                            k.eingang = name.to_string();
                            k.ausgang = name.to_string();
                        }
                        k.name = name.to_string();
                    }
                    _ => {}
                }
                continue;
            }
            if !abschnitt.starts_with("map") || !zeile.contains("<>") {
                continue;
            }
            let (von, nach) = zeile.split_once("<>").unwrap_or((zeile, ""));
            let ziel = nach.trim().replace("crosspoint.", "");
            let cc = cc_aus(von.trim());
            if let (Some(cc), true) = (cc, ziel.starts_with('/')) {
                k.binden(&ziel, cc);
            }
        }
        k
    }

    /// Datei schreiben (nur gebundene Zeilen landen in `[map]`).
    pub fn speichern(&self, p: impl AsRef<Path>) -> Result<(), String> {
        let p = p.as_ref();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let geraet = if self.eingang == "kein Geraet" || self.eingang.is_empty() {
            "nanokontrol2".to_string()
        } else {
            self.eingang.clone()
        };
        let mut s = String::new();
        s.push_str("; Crosspoint: MIDI -> OSC ueber MIDIMonster\n");
        s.push_str("; Erzeugt von der GTK-Oberflaeche (Dialog MIDI-Mapping).\n");
        s.push_str("; Start: crosspoint-engine, dann midimonster <diese Datei>.\n\n");
        s.push_str(&format!("[midi {geraet}]\n"));
        s.push_str(&format!("read = {geraet}\n"));
        s.push_str(&format!("write = {geraet}\n\n"));
        s.push_str("[osc crosspoint]\n");
        s.push_str("bind = 127.0.0.1 9001\n");
        s.push_str("dest = 127.0.0.1 9000\n\n");
        s.push_str("; Rueckmeldung an die Oberflaeche (Lernmodus): jede eingehende\n");
        s.push_str("; CC-Nachricht zusaetzlich als /midi/cc <kanal> <cc> <wert>.\n");
        s.push_str("[map]\n");
        for z in self.zeilen.iter().filter(|z| z.cc.is_some()) {
            s.push_str(&format!(
                "{geraet}.ch0.cc{} <> crosspoint.{}\n",
                z.cc.unwrap(),
                z.ziel
            ));
        }
        std::fs::write(p, s).map_err(|e| format!("{}: {e}", p.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("crosspoint-midi-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("midimonster.cfg")
    }

    #[test]
    fn rundlauf_schreiben_und_lesen() {
        let p = temp("rundlauf");
        let mut k = Karte::leer();
        k.alle();
        k.eingang = "nanokontrol2".into();
        k.ausgang = "nanokontrol2".into();
        k.binden("/strip/0/gain", 0);
        k.binden("/strip/7/pan", 17);
        k.binden("/strip/6/solo", 43);
        k.speichern(&p).unwrap();

        let gelesen = Karte::laden(&p);
        assert_eq!(gelesen.name, "nanokontrol2");
        assert_eq!(gelesen.zeilen.len(), k.zeilen.len());
        let cc = |ziel: &str| gelesen.zeilen.iter().find(|z| z.ziel == ziel).unwrap().cc;
        assert_eq!(cc("/strip/0/gain"), Some(0));
        assert_eq!(cc("/strip/7/pan"), Some(17));
        assert_eq!(cc("/strip/6/solo"), Some(43));
        assert_eq!(cc("/strip/1/gain"), None);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn reset_leert_die_karte_und_die_datei() {
        let p = temp("reset");
        let mut k = Karte::leer();
        k.alle();
        k.binden("/strip/0/gain", 5);
        k.binden("/bus/0/mute", 9);
        k.speichern(&p).unwrap();
        assert!(std::fs::read_to_string(&p).unwrap().contains("cc5"));

        k.zuruecksetzen();
        assert_eq!(k.gebunden(), 0);
        k.speichern(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            !text.contains("<>"),
            "nach Reset darf keine Bindung bleiben: {text}"
        );
        assert_eq!(Karte::laden(&p).gebunden(), 0);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn seiten_haben_die_ziele_des_originals() {
        let s0 = seiten_ziele(0);
        assert_eq!(s0[0].titel, "Gain Fader Strip #1");
        assert_eq!(s0[0].ziel, "/strip/0/gain");
        let s2 = seiten_ziele(2);
        assert_eq!(s2[0].titel, "LR PAN Strip #1");
        assert_eq!(s2[0].ziel, "/strip/0/pan");
        let s3 = seiten_ziele(3);
        assert_eq!(s3[0].titel, "Mute Strip #1");
        let s4 = seiten_ziele(4);
        assert_eq!(s4[0].titel, "Solo Virtual Strip #1");
        assert_eq!(s4[0].ziel, "/strip/5/solo");
        let s5 = seiten_ziele(5);
        assert_eq!(s5.len(), FREIE_CCS);
        assert_eq!(s5[55].titel, "Control Change 55");
    }

    #[test]
    fn eine_cc_gehoert_nur_zu_einem_ziel() {
        let mut k = Karte::leer();
        k.alle();
        k.binden("/strip/0/gain", 7);
        k.binden("/strip/1/gain", 7);
        assert_eq!(k.gebunden(), 1);
        assert_eq!(
            k.zeilen
                .iter()
                .find(|z| z.ziel == "/strip/0/gain")
                .unwrap()
                .cc,
            None
        );
        k.loesen("/strip/1/gain");
        assert_eq!(k.gebunden(), 0);
    }

    #[test]
    fn kaputte_datei_ergibt_leere_karte() {
        let p = temp("kaputt");
        std::fs::write(&p, "das ist keine cfg").unwrap();
        let k = Karte::laden(&p);
        assert_eq!(k.gebunden(), 0);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
