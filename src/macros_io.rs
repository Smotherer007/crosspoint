//! Anbindung der Macro-Engine an den Hauptprozess.
//!
//! Das Crate `macros` rechnet nur aus, *welche* Aktionen eine Taste auslöst; es
//! kennt weder Uhr noch Seiteneffekte. Dieses Modul ist die Naht dazwischen: es
//! hält die Engine hinter einem `Mutex`, stempelt die Frame-Zeit, nimmt
//! Tastendrücke aus der Oberfläche entgegen und legt die fertigen Kommandos in
//! eine Warteschlange, aus der der Mischer sie abholt.
//!
//! ```text
//! UI press(0) -> Engine::tick + apply -> "strip 0 gain -6" -> drain() -> Parameter setzen
//! ```
//!
//! # Kommandoformat
//!
//! Ein Kommando ist eine Zeile mit Leerzeichen als Trenner, wie sie die
//! Steuer-API des Haupt-Crates ohnehin spricht:
//!
//! | Aktion                     | Kommando                     |
//! |----------------------------|------------------------------|
//! | `SetParam`                 | `strip 0 gain -6`            |
//! | `Toggle` (Zielwert bekannt)| `strip 0 mute 1`             |
//! | `Toggle` (Zielwert offen)  | `strip 0 mute toggle`        |
//! | `MidiNote`                 | `midi 1 60 100`              |
//! | `Wait`                     | `wait 250`                   |
//!
//! Zahlen werden ohne überflüssige Nullen geschrieben (`-6`, `-6.5`).
//!
//! # Warteschlange
//!
//! `press`/`release` führen den Frame sofort aus und legen die Kommandos ab;
//! `drain` leert die Schlange in einem Zug. Wer die Tasten zyklisch abfragt
//! (MIDI, Skripte), ruft zusätzlich `tick(now_ms)`.
//!
//! # Grenzen (ehrlich)
//!
//! * `display_state` der Engine ist ein *Flanken*zustand: eine `Trigger`-Taste
//!   meldet nur in dem Frame etwas anderes als `off`, in dem sie gefeuert hat.
//!   `status_json` zeigt deshalb für `trigger`-Tasten im Ruhezustand `off` -
//!   die Oberfläche soll das Feld als "hat gerade ausgelöst" lesen, nicht als
//!   Dauerzustand.
//! * Die Warteschlange ist unbegrenzt. Wer `drain` nie aufruft, sammelt an;
//!   der Mischer ruft es pro Block auf.
//! * Die Datei wird nicht gesperrt. Zwei Prozesse auf demselben `macros.json`
//!   überschreiben sich beim `save` (letzter gewinnt).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use macros::json::Wert;
use macros::{Action, Bank, Button, Engine, Input, MAX_BUTTONS};

/// Ordnername unter `$XDG_CONFIG_HOME` bzw. `~/.config`.
pub const KONFIG_ORDNER: &str = "crosspoint";

/// Dateiname der Tastenbelegung.
pub const MAKROS_DATEI: &str = "macros.json";

/// Standardablage: `~/.config/crosspoint/macros.json` (bzw. `$XDG_CONFIG_HOME`).
pub fn standard_pfad() -> PathBuf {
    let basis = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => match std::env::var_os("HOME") {
            Some(h) if !h.is_empty() => PathBuf::from(h).join(".config"),
            _ => PathBuf::from(".config"),
        },
    };
    basis.join(KONFIG_ORDNER).join(MAKROS_DATEI)
}

/// Vergiftete Mutexe (Panik im Vorbesitzer) legen die Makros nicht lahm: wir
/// arbeiten mit dem letzten gültigen Inhalt weiter.
fn sperre<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Zahl für ein Kommando: ganzzahlig ohne Nachkommastellen, sonst gekürzt.
fn zahl(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    if v.fract() == 0.0 && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Eine Engine-Aktion als Kommandozeile (siehe Modul-Doku).
pub fn kommando(action: &Action) -> String {
    match action {
        Action::SetParam { target, index, param, value } => {
            format!("{target} {index} {param} {}", zahl(*value))
        }
        Action::Toggle { target, index, param, to } => match to {
            Some(v) => format!("{target} {index} {param} {}", zahl(*v)),
            None => format!("{target} {index} {param} toggle"),
        },
        Action::MidiNote { channel, note, velocity } => {
            format!("midi {channel} {note} {velocity}")
        }
        Action::Wait { ms } => format!("wait {ms}"),
    }
}

/// Die Macro-Engine, wie der Hauptprozess sie sieht.
///
/// Getrennte Mutexe für Engine und Warteschlange, damit ein `drain` aus dem
/// Mischer nicht auf einen laufenden `tick` warten muss und umgekehrt.
pub struct MacroControl {
    engine: Mutex<Engine>,
    pendenzen: Mutex<Vec<String>>,
    /// Tastenzustand je Slot, wie ihn die Oberfläche zuletzt gemeldet hat.
    /// Grundlage für `tick`: ein Frame ohne neue Flanke muss die *gehaltenen*
    /// Tasten melden, nicht "alles losgelassen".
    halten: Mutex<Vec<bool>>,
    pfad: PathBuf,
    start: Instant,
    /// Letzter Ladefehler, für `status_json` - ein kaputtes `macros.json`
    /// verhindert den Start nicht, soll aber sichtbar sein.
    ladefehler: Mutex<Option<String>>,
}

impl MacroControl {
    /// Standardpfad, beim Start laden, sonst leere Bank mit 80 Slots.
    pub fn new() -> MacroControl {
        MacroControl::mit_pfad(standard_pfad())
    }

    /// Wie `new`, aber mit eigenem Dateipfad (Tests, portable Installation).
    /// Ein Ladefehler wird vermerkt und über `status_json` gemeldet, nicht
    /// als Panik durchgereicht.
    pub fn mit_pfad(pfad: impl Into<PathBuf>) -> MacroControl {
        let ctl = MacroControl {
            engine: Mutex::new(Engine::new(Bank::new())),
            pendenzen: Mutex::new(Vec::new()),
            halten: Mutex::new(vec![false; MAX_BUTTONS]),
            pfad: pfad.into(),
            start: Instant::now(),
            ladefehler: Mutex::new(None),
        };
        let _ = ctl.load();
        ctl
    }

    pub fn pfad(&self) -> &Path {
        &self.pfad
    }

    /// Kopie der aktuellen Belegung (80 Slots).
    pub fn belegung(&self) -> Bank {
        sperre(&self.engine).bank().clone()
    }

    /// Belegung tauschen; setzt Laufzeitzustand und gehaltene Tasten zurück.
    pub fn set_bank(&self, bank: Bank) {
        sperre(&self.engine).set_bank(bank);
        let mut h = sperre(&self.halten);
        for x in h.iter_mut() {
            *x = false;
        }
    }

    /// Eine Taste belegen. `index` muss in `0..80` liegen, die Taste gültig sein.
    pub fn set_button(&self, index: usize, button: Button) -> Result<(), String> {
        let mut e = sperre(&self.engine);
        let n = e.bank().len();
        if index >= n {
            return Err(format!("Slot {index} liegt ausserhalb 0..{n}"));
        }
        button.validate()?;
        e.bank_mut()
            .set(index, button)
            .map_err(|err| err.to_string())
    }

    /// Parameterspiegel füttern, damit `Toggle` den Zielwert kennt.
    pub fn set_param(&self, target: &str, index: u32, param: &str, value: f64) {
        sperre(&self.engine).set_param(target, index, param, value);
    }

    /// Ist die Taste eingerastet (`on`/`color`)?
    pub fn latched(&self, index: usize) -> bool {
        sperre(&self.engine).is_latched(index)
    }

    /// Millisekunden seit dem Start dieses Steuerteils.
    pub fn jetzt_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// Taste drücken: Flanke setzen, Frame rechnen, Aktionen ablegen.
    pub fn press(&self, index: usize) -> Result<(), String> {
        self.kante(index, true)
    }

    /// Taste loslassen.
    pub fn release(&self, index: usize) -> Result<(), String> {
        self.kante(index, false)
    }

    fn kante(&self, index: usize, down: bool) -> Result<(), String> {
        {
            let mut h = sperre(&self.halten);
            if index >= h.len() {
                return Err(format!("Slot {index} liegt ausserhalb 0..{MAX_BUTTONS}"));
            }
            h[index] = down;
        }
        self.frame(self.jetzt_ms());
        Ok(())
    }

    /// Frame mit der übergebenen Zeit rechnen (für zyklische Aufrufer).
    /// Liefert die Zahl der neu abgelegten Kommandos.
    ///
    /// `press`/`release` haben ihren Frame bereits selbst gerechnet; hier
    /// werden die *gehaltenen* Tasten erneut gemeldet, was keine neue Flanke
    /// ergibt. Nötig ist der Aufruf, wenn sich Werte ohne Tastendruck ändern
    /// (`value_change`) oder die Wiederholsperre mitlaufen soll.
    pub fn tick(&self, now_ms: u64) -> usize {
        self.frame(now_ms)
    }

    fn frame(&self, now_ms: u64) -> usize {
        let eingaenge: Vec<Input> = {
            let h = sperre(&self.halten);
            h.iter()
                .map(|down| if *down { Input::gedrueckt() } else { Input::idle() })
                .collect()
        };

        // Engine-Lock nur für tick+apply halten, danach die Schlange füllen -
        // so wartet ein drain nie auf die Auswertung.
        let kommandos: Vec<String> = {
            let mut e = sperre(&self.engine);
            let aktionen = e.tick(now_ms, &eingaenge);
            let mut aus = Vec::with_capacity(aktionen.len());
            for a in &aktionen {
                aus.push(kommando(a));
                e.apply(a);
            }
            aus
        };

        if kommandos.is_empty() {
            return 0;
        }
        let n = kommandos.len();
        sperre(&self.pendenzen).extend(kommandos);
        n
    }

    /// Alle offenen Kommandos abholen und die Warteschlange leeren.
    pub fn drain(&self) -> Vec<String> {
        let mut q = sperre(&self.pendenzen);
        std::mem::take(&mut *q)
    }

    /// Belegung als JSON schreiben. Erst in eine Nebendatei, dann umbenennen -
    /// ein Absturz mitten im Schreiben lässt keine halbe Belegung zurück.
    pub fn save(&self) -> Result<(), String> {
        let text = {
            let e = sperre(&self.engine);
            macros::bank_to_json(e.bank())
        };
        if let Some(dir) = self.pfad.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("Ordner {} anlegen: {e}", dir.display()))?;
            }
        }
        let mut tmp = self.pfad.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, text)
            .map_err(|e| format!("{} schreiben: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.pfad).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{} umbenennen: {e}", self.pfad.display())
        })?;
        *sperre(&self.ladefehler) = None;
        Ok(())
    }

    /// Belegung von Platte laden. Fehlt die Datei, gilt die leere Bank mit
    /// 80 Slots (Erststart). Eine kaputte Datei lässt die laufende Belegung
    /// unangetastet und kommt als Fehler zurück.
    pub fn load(&self) -> Result<(), String> {
        let text = match std::fs::read_to_string(&self.pfad) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.set_bank(Bank::new());
                *sperre(&self.ladefehler) = None;
                return Ok(());
            }
            Err(e) => {
                let m = format!("{} lesen: {e}", self.pfad.display());
                *sperre(&self.ladefehler) = Some(m.clone());
                return Err(m);
            }
        };
        match Engine::load(&text) {
            Ok(engine) => {
                *sperre(&self.engine) = engine;
                let mut h = sperre(&self.halten);
                for x in h.iter_mut() {
                    *x = false;
                }
                *sperre(&self.ladefehler) = None;
                Ok(())
            }
            Err(e) => {
                let m = format!("{}: {e}", self.pfad.display());
                *sperre(&self.ladefehler) = Some(m.clone());
                Err(m)
            }
        }
    }

    /// Zustand für die Oberfläche: Pfad, 80 Tasten mit Name, Anzeigezustand
    /// und Rastzustand, dazu Zähler und Warteschlangenlänge.
    pub fn status_json(&self) -> String {
        let (tasten, frames, fires, konflikte) = {
            let e = sperre(&self.engine);
            let bank = e.bank();
            let mut tasten = Vec::with_capacity(bank.len());
            for i in 0..bank.len() {
                if let Some(b) = bank.get(i) {
                    tasten.push(Wert::objekt(vec![
                        ("index", Wert::zahl(i as f64)),
                        ("name", Wert::text(b.name.clone())),
                        ("state", Wert::text(b.state.as_str())),
                        (
                            "display",
                            Wert::text(e.display_state(i).map(|s| s.as_str()).unwrap_or("off")),
                        ),
                        ("latched", Wert::Wahr(e.is_latched(i))),
                        ("actions", Wert::zahl(b.actions.len() as f64)),
                    ]));
                }
            }
            (tasten, e.frames(), e.fires(), e.conflicts().len() as f64)
        };
        let offen = sperre(&self.pendenzen).len();
        let ladefehler = sperre(&self.ladefehler).clone();
        Wert::objekt(vec![
            ("pfad", Wert::text(self.pfad.display().to_string())),
            ("slots", Wert::zahl(MAX_BUTTONS as f64)),
            ("buttons", Wert::Liste(tasten)),
            ("warteschlange", Wert::zahl(offen as f64)),
            ("frames", Wert::zahl(frames as f64)),
            ("fires", Wert::zahl(fires as f64)),
            ("konflikte", Wert::zahl(konflikte)),
            (
                "ladefehler",
                match ladefehler {
                    Some(m) => Wert::text(m),
                    None => Wert::Null,
                },
            ),
        ])
        .to_json()
    }
}

impl Default for MacroControl {
    fn default() -> Self {
        MacroControl::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use macros::{ButtonState, Trigger};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Eigener Dateipfad je Test: kein Test schreibt in ein echtes Home.
    fn testpfad() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("crosspoint-macros-io-{}-{n}.json", std::process::id()))
    }

    fn taste(name: &str, state: ButtonState, actions: Vec<Action>) -> Button {
        Button::neu(name, state, Trigger::Press, actions)
    }

    fn steuer(belegung: &[(usize, Button)]) -> MacroControl {
        let ctl = MacroControl::mit_pfad(testpfad());
        for (i, b) in belegung {
            ctl.set_button(*i, b.clone()).expect("Taste belegen");
        }
        ctl
    }

    /// Ein Druck, eine Aktion - auch wenn die Oberfläche mehrfach "gedrückt"
    /// meldet, ohne dass dazwischen losgelassen wurde.
    #[test]
    fn press_feuert_genau_einmal() {
        let c = steuer(&[(
            0,
            taste("Leise", ButtonState::Trigger, vec![Action::set("strip", 0, "gain", -6.0)]),
        )]);

        assert!(c.drain().is_empty(), "frisches Steuer ist leer");

        c.press(0).expect("press");
        c.press(0).expect("press");
        c.press(0).expect("press");

        let k = c.drain();
        assert_eq!(k, vec!["strip 0 gain -6".to_string()]);
        assert!(c.drain().is_empty(), "drain leert die Schlange");
        assert_eq!(sperre(&c.engine).fires(), 1);
    }

    /// Loslassen ist keine zweite Auslösung: eine `press`-Taste schweigt,
    /// eine `release`-Taste feuert genau beim Loslassen einmal.
    #[test]
    fn release_erzeugt_keine_zweite_aktion() {
        let c = steuer(&[(
            0,
            taste("Leise", ButtonState::Trigger, vec![Action::set("strip", 0, "gain", -6.0)]),
        )]);
        c.press(0).unwrap();
        assert_eq!(c.drain(), vec!["strip 0 gain -6".to_string()]);

        c.release(0).unwrap();
        c.release(0).unwrap();
        assert!(c.drain().is_empty());
        assert_eq!(sperre(&c.engine).fires(), 1);

        // Und der nächste Druck feuert wieder - genau einmal.
        c.press(0).unwrap();
        assert_eq!(c.drain().len(), 1);

        // Taste mit Trigger::Release: nichts beim Druck, einmal beim Loslassen.
        let r = steuer(&[(
            2,
            Button::neu("Los", ButtonState::On, Trigger::Release, vec![Action::midi(1, 60, 0)]),
        )]);
        r.press(2).unwrap();
        assert!(r.drain().is_empty(), "Release-Taste schweigt beim Druck");
        r.release(2).unwrap();
        assert_eq!(r.drain(), vec!["midi 1 60 0".to_string()]);
        r.release(2).unwrap();
        assert!(r.drain().is_empty());
    }

    /// Toggle wird zum Kommando: bekannter Zielwert wird ausgeschrieben,
    /// unbekannter bleibt offen (`toggle`) und der Ausführende entscheidet.
    #[test]
    fn toggle_landet_als_kommando_in_der_warteschlange() {
        let c = steuer(&[(
            1,
            taste("Mute", ButtonState::On, vec![Action::toggle("strip", 1, "mute")]),
        )]);
        c.set_param("strip", 1, "mute", 0.0);

        c.press(1).unwrap();
        assert_eq!(c.drain(), vec!["strip 1 mute 1".to_string()]);
        assert!(c.latched(1));

        c.release(1).unwrap();
        c.press(1).unwrap();
        assert_eq!(c.drain(), vec!["strip 1 mute 0".to_string()]);
        assert!(!c.latched(1));

        // Ohne Parameterspiegel kennt die Engine den Zielwert nicht.
        let o = steuer(&[(
            1,
            taste("Mute", ButtonState::On, vec![Action::toggle("strip", 1, "mute")]),
        )]);
        o.press(1).unwrap();
        assert_eq!(o.drain(), vec!["strip 1 mute toggle".to_string()]);
    }

    /// Schreiben, frisch laden, vergleichen - über eine echte Datei.
    #[test]
    fn save_und_load_rundlauf() {
        let pfad = testpfad();
        let c = MacroControl::mit_pfad(&pfad);
        c.set_button(
            7,
            taste(
                "Intro",
                ButtonState::Color,
                vec![Action::set("bus", 0, "mute", 1.0), Action::wait(250)],
            ),
        )
        .unwrap();
        c.save().expect("save");
        assert!(pfad.exists());
        let text = std::fs::read_to_string(&pfad).unwrap();
        assert!(text.contains("\"Intro\""), "Datei war: {text}");

        let d = MacroControl::mit_pfad(&pfad);
        let b = d.belegung();
        assert_eq!(b.len(), MAX_BUTTONS);
        let gelesen = b.get(7).unwrap();
        assert_eq!(gelesen.name, "Intro");
        assert_eq!(gelesen.state, ButtonState::Color);
        assert_eq!(gelesen.actions.len(), 2);

        // Die geladene Belegung funktioniert, nicht nur das Feld.
        d.press(7).unwrap();
        assert_eq!(d.drain(), vec!["bus 0 mute 1".to_string(), "wait 250".to_string()]);

        std::fs::remove_file(&pfad).ok();
    }

    /// 80 Slots sind die Grenze: 0..79 belegbar, 80 nicht, leere Datei füllt
    /// auf 80 Standardtasten auf.
    #[test]
    fn grenze_80_slots() {
        let pfad = testpfad();
        assert!(!pfad.exists());
        let c = MacroControl::mit_pfad(&pfad);

        // Erststart ohne Datei: leere Bank mit 80 Slots.
        assert_eq!(c.belegung().len(), MAX_BUTTONS);
        assert!(c.press(MAX_BUTTONS).is_err());
        assert!(c.release(MAX_BUTTONS).is_err());
        assert!(c.press(99).is_err());
        assert_eq!(c.drain().len(), 0, "abgewiesene Slots legen nichts ab");

        assert!(c
            .set_button(MAX_BUTTONS, taste("Zuviel", ButtonState::On, vec![]))
            .is_err());
        assert!(c
            .set_button(
                79,
                taste("Letzte", ButtonState::Trigger, vec![Action::midi(16, 127, 127)]),
            )
            .is_ok());

        c.press(79).unwrap();
        assert_eq!(c.drain(), vec!["midi 16 127 127".to_string()]);

        // Die UI-Liste hat genau 80 Einträge, 0..79.
        let s = c.status_json();
        let v = macros::json::parse(&s).expect("status_json ist JSON");
        assert_eq!(v.get("slots").and_then(|x| x.als_zahl()), Some(80.0));
        let tasten = v.get("buttons").and_then(|x| x.als_liste()).expect("buttons[]");
        assert_eq!(tasten.len(), MAX_BUTTONS);
        assert_eq!(
            tasten[79].get("name"),
            Some(&Wert::text("Letzte")),
        );
    }

    /// `status_json` ist für die UI lesbar: Name, display, latched, Zähler.
    #[test]
    fn status_json_liefert_tasten_fuer_die_ui() {
        let c = steuer(&[(
            0,
            taste("Mute A1", ButtonState::On, vec![Action::toggle("strip", 0, "mute")]),
        )]);
        c.press(0).unwrap();
        let vor_drain = c.status_json();
        c.drain();
        let s = c.status_json();

        let v = macros::json::parse(&s).expect("status_json ist JSON");
        assert_eq!(v.get("warteschlange").and_then(|x| x.als_zahl()), Some(0.0));
        assert_eq!(v.get("fires").and_then(|x| x.als_zahl()), Some(1.0));
        // Vor dem drain stand ein Kommando in der Schlange.
        let vorher = macros::json::parse(&vor_drain).unwrap();
        assert_eq!(vorher.get("warteschlange").and_then(|x| x.als_zahl()), Some(1.0));

        let tasten = v.get("buttons").and_then(|x| x.als_liste()).unwrap();
        assert_eq!(tasten[0].get("name"), Some(&Wert::text("Mute A1")));
        assert_eq!(tasten[0].get("state"), Some(&Wert::text("on")));
        assert_eq!(tasten[0].get("display"), Some(&Wert::text("on")));
        assert_eq!(
            tasten[0].get("latched").and_then(|x| x.als_zahl()),
            Some(1.0)
        );
        // Slot 1 ist unbelegt: Standardname, off.
        assert_eq!(tasten[1].get("name"), Some(&Wert::text("Makro 2")));
        assert_eq!(tasten[1].get("display"), Some(&Wert::text("off")));
        assert_eq!(v.get("ladefehler"), Some(&Wert::Null));
    }

    /// Standardpfad zeigt auf die Konfiguration im Home, nicht ins Arbeits-
    /// verzeichnis.
    #[test]
    fn standardpfad_zeigt_auf_konfiguration() {
        let p = standard_pfad();
        assert!(p.ends_with(Path::new(KONFIG_ORDNER).join(MAKROS_DATEI)), "war {}", p.display());
        assert!(p.is_absolute(), "war {}", p.display());
    }

    /// Kaputte Datei verhindert den Start nicht, wird aber gemeldet.
    #[test]
    fn kaputte_datei_wird_gemeldet() {
        let pfad = testpfad();
        std::fs::write(&pfad, "{ das ist kein json").unwrap();
        let c = MacroControl::mit_pfad(&pfad);
        assert_eq!(c.belegung().len(), MAX_BUTTONS, "leere Bank statt Panik");
        let v = macros::json::parse(&c.status_json()).unwrap();
        assert!(v.get("ladefehler").and_then(|x| match x {
            Wert::Text(s) => Some(s.clone()),
            _ => None,
        }).is_some(), "ladefehler fehlt");
        assert!(c.load().is_err());
        std::fs::remove_file(&pfad).ok();
    }
}
