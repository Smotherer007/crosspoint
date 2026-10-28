//! Persistenz der Tastenbelegung als JSON.
//!
//! Format (Version 1), bewusst flach und stabil:
//!
//! ```json
//! {
//!   "version": 1,
//!   "buttons": [
//!     {
//!       "name": "Intro",
//!       "state": "on",
//!       "trigger": "press",
//!       "gui": 3,
//!       "actions": [
//!         {"type": "set", "target": "strip", "index": 0, "param": "mute", "value": 1},
//!         {"type": "midi", "channel": 1, "note": 60, "velocity": 127},
//!         {"type": "wait", "ms": 250},
//!         {"type": "toggle", "target": "bus", "index": 0, "param": "mute"}
//!       ]
//!     }
//!   ]
//! }
//! ```
//!
//! Gelesen wird grosszuegig: fehlende Felder bekommen Standardwerte,
//! unbekannte Schluessel werden ignoriert, `state`/`trigger` duerfen als Zahl
//! oder Text stehen. Geschrieben wird immer das vollstaendige Bild mit allen
//! 80 Slots, damit ein Rundlauf verlustfrei ist.

use crate::json::{self, Wert};
use crate::model::{Action, Bank, Button, ButtonState, Trigger, MAX_BUTTONS};

/// Aktuelle Formatversion. Hoehere Versionen werden abgelehnt, weil sonst
/// unbekannte Felder stillschweigend verloren gingen.
pub const FORMAT_VERSION: u32 = 1;

/// Belegung als JSON-Text.
pub fn bank_to_json(bank: &Bank) -> String {
    let buttons: Vec<Wert> = bank.iter().map(button_to_wert).collect();
    let wurzel = Wert::objekt(vec![
        ("version", Wert::zahl(FORMAT_VERSION as f64)),
        ("buttons", Wert::Liste(buttons)),
    ]);
    // Einrueckung je Taste auf eigener Zeile: die Datei wird von Hand gepflegt.
    let kompakt = wurzel.to_json();
    kompakt.replace("},{\"name\":", "},\n{\"name\":")
}

fn button_to_wert(button: &Button) -> Wert {
    let actions: Vec<Wert> = button.actions.iter().map(action_to_wert).collect();
    Wert::objekt(vec![
        ("name", Wert::text(button.name.clone())),
        ("state", Wert::text(button.state.as_str())),
        ("trigger", Wert::text(button.trigger.as_str())),
        (
            "gui",
            match button.gui {
                Some(slot) => Wert::zahl(slot as f64),
                None => Wert::Null,
            },
        ),
        ("actions", Wert::Liste(actions)),
    ])
}

fn action_to_wert(action: &Action) -> Wert {
    match action {
        Action::SetParam {
            target,
            index,
            param,
            value,
        } => Wert::objekt(vec![
            ("type", Wert::text("set")),
            ("target", Wert::text(target.clone())),
            ("index", Wert::zahl(*index as f64)),
            ("param", Wert::text(param.clone())),
            ("value", Wert::zahl(*value)),
        ]),
        Action::MidiNote {
            channel,
            note,
            velocity,
        } => Wert::objekt(vec![
            ("type", Wert::text("midi")),
            ("channel", Wert::zahl(*channel as f64)),
            ("note", Wert::zahl(*note as f64)),
            ("velocity", Wert::zahl(*velocity as f64)),
        ]),
        Action::Wait { ms } => Wert::objekt(vec![
            ("type", Wert::text("wait")),
            ("ms", Wert::zahl(*ms as f64)),
        ]),
        Action::Toggle {
            target,
            index,
            param,
            to,
        } => Wert::objekt(vec![
            ("type", Wert::text("toggle")),
            ("target", Wert::text(target.clone())),
            ("index", Wert::zahl(*index as f64)),
            ("param", Wert::text(param.clone())),
            (
                "to",
                match to {
                    Some(v) => Wert::zahl(*v),
                    None => Wert::Null,
                },
            ),
        ]),
    }
}

/// Belegung aus JSON-Text lesen. Fehler nennen Taste und Aktion, weil die
/// Datei von Hand entstehen kann.
pub fn bank_from_json(text: &str) -> Result<Bank, String> {
    let wurzel = json::parse(text)?;

    if let Some(v) = wurzel.get("version") {
        let version = v
            .als_zahl()
            .ok_or_else(|| "Feld 'version' ist keine Zahl".to_string())?;
        if version > FORMAT_VERSION as f64 {
            return Err(format!(
                "Formatversion {version} ist neuer als die unterstuetzte {FORMAT_VERSION}"
            ));
        }
    }

    let liste = wurzel
        .get("buttons")
        .and_then(|w| w.als_liste())
        .ok_or_else(|| "Feld 'buttons' fehlt oder ist keine Liste".to_string())?;

    if liste.len() > MAX_BUTTONS {
        return Err(format!(
            "{} Tasten in der Datei, erlaubt sind hoechstens {MAX_BUTTONS}",
            liste.len()
        ));
    }

    let mut buttons = Vec::with_capacity(MAX_BUTTONS);
    for (i, w) in liste.iter().enumerate() {
        buttons.push(button_from_wert(i, w)?);
    }

    let bank = Bank::from_buttons(buttons).map_err(|e| e.to_string())?;
    bank.validate()?;
    Ok(bank)
}

fn button_from_wert(index: usize, w: &Wert) -> Result<Button, String> {
    if !matches!(w, Wert::Objekt(_)) {
        return Err(format!("Taste {index}: kein Objekt"));
    }

    let name = match w.get("name").and_then(|x| x.als_text()) {
        Some(s) => s.to_string(),
        None => format!("Makro {}", index + 1),
    };

    let state = match w.get("state") {
        None | Some(Wert::Null) => ButtonState::Off,
        Some(v) => {
            if let Some(s) = v.als_text() {
                ButtonState::parse(s)
                    .ok_or_else(|| format!("Taste {index}: unbekannter Zustand '{s}'"))?
            } else {
                let n = v
                    .als_zahl()
                    .ok_or_else(|| format!("Taste {index}: 'state' ist weder Text noch Zahl"))?;
                ButtonState::from_u8(n as u8)
                    .ok_or_else(|| format!("Taste {index}: Zustand {n} liegt ausserhalb 0..=3"))?
            }
        }
    };

    let trigger = match w.get("trigger") {
        None | Some(Wert::Null) => Trigger::Press,
        Some(v) => {
            if let Some(s) = v.als_text() {
                Trigger::parse(s)
                    .ok_or_else(|| format!("Taste {index}: unbekannter Trigger '{s}'"))?
            } else {
                match u32_feld(w, "trigger", 0)? {
                    0 => Trigger::Press,
                    1 => Trigger::Release,
                    2 => Trigger::ValueChange,
                    n => return Err(format!("Taste {index}: Trigger {n} liegt ausserhalb 0..=2")),
                }
            }
        }
    };

    let gui = match w.get("gui") {
        None | Some(Wert::Null) => None,
        Some(v) => {
            let n = v
                .als_zahl()
                .ok_or_else(|| format!("Taste {index}: 'gui' ist keine Zahl"))?;
            if n < 0.0 || n > 255.0 || n.fract() != 0.0 {
                return Err(format!(
                    "Taste {index}: GUI-Slot {n} liegt ausserhalb 0..=255"
                ));
            }
            Some(n as u16)
        }
    };

    let mut actions = Vec::new();
    if let Some(liste) = w.get("actions").and_then(|x| x.als_liste()) {
        for (j, aw) in liste.iter().enumerate() {
            actions.push(action_from_wert(index, j, aw)?);
        }
    } else if w.get("actions").is_some() {
        return Err(format!("Taste {index}: 'actions' ist keine Liste"));
    }

    Ok(Button {
        name,
        state,
        trigger,
        actions,
        gui,
    })
}

fn action_from_wert(button: usize, pos: usize, w: &Wert) -> Result<Action, String> {
    let ort = |s: &str| format!("Taste {button}, Aktion {pos}: {s}");
    let art = w
        .get("type")
        .and_then(|x| x.als_text())
        .ok_or_else(|| ort("'type' fehlt oder ist kein Text"))?;

    match art.trim().to_ascii_lowercase().as_str() {
        "set" | "setparam" | "param" => Ok(Action::SetParam {
            target: pflicht_text(w, "target", &ort)?,
            index: u32_feld(w, "index", 0)?,
            param: pflicht_text(w, "param", &ort)?,
            value: w
                .get("value")
                .and_then(|v| v.als_zahl())
                .ok_or_else(|| ort("'value' fehlt oder ist keine Zahl"))?,
        }),
        "midi" | "note" | "midinote" => Ok(Action::MidiNote {
            channel: u8_feld(w, "channel", 1)?,
            note: u8_feld(w, "note", 60)?,
            velocity: u8_feld(w, "velocity", 127)?,
        }),
        "wait" | "pause" | "delay" => Ok(Action::Wait {
            ms: u32_feld(w, "ms", 0)?,
        }),
        "toggle" | "flip" => Ok(Action::Toggle {
            target: pflicht_text(w, "target", &ort)?,
            index: u32_feld(w, "index", 0)?,
            param: pflicht_text(w, "param", &ort)?,
            to: match w.get("to") {
                None | Some(Wert::Null) => None,
                Some(v) => Some(v.als_zahl().ok_or_else(|| ort("'to' ist keine Zahl"))?),
            },
        }),
        andere => Err(ort(&format!("unbekannter Aktionstyp '{andere}'"))),
    }
}

fn pflicht_text(o: &Wert, name: &str, ort: &dyn Fn(&str) -> String) -> Result<String, String> {
    o.get(name)
        .and_then(|v| v.als_text())
        .map(|s| s.to_string())
        .ok_or_else(|| ort(&format!("'{name}' fehlt oder ist kein Text")))
}

/// Nichtnegative Ganzzahl aus einem Feld; fehlt es, gilt `standard`.
fn u32_feld(o: &Wert, name: &str, standard: u32) -> Result<u32, String> {
    match o.get(name) {
        None | Some(Wert::Null) => Ok(standard),
        Some(v) => {
            let n = v
                .als_zahl()
                .ok_or_else(|| format!("Feld '{name}' ist keine Zahl"))?;
            if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                return Err(format!(
                    "Feld '{name}' ist keine nichtnegative Ganzzahl: {n}"
                ));
            }
            Ok(n as u32)
        }
    }
}

fn u8_feld(o: &Wert, name: &str, standard: u8) -> Result<u8, String> {
    let n = u32_feld(o, name, standard as u32)?;
    if n > 255 {
        return Err(format!("Feld '{name}' ist groesser als 255: {n}"));
    }
    Ok(n as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rundlauf() {
        let mut bank = Bank::new();
        bank.set(
            0,
            Button::neu(
                "Intro",
                ButtonState::On,
                Trigger::Press,
                vec![
                    Action::set("strip", 0, "mute", 1.0),
                    Action::midi(1, 60, 127),
                    Action::wait(250),
                    Action::toggle("bus", 0, "mute"),
                ],
            )
            .mit_gui(3),
        )
        .unwrap();
        bank.set(
            79,
            Button::neu(
                "Ende \"quoted\"",
                ButtonState::Color,
                Trigger::ValueChange,
                vec![Action::set("bus", 7, "gain", -12.5)],
            ),
        )
        .unwrap();

        let text = bank_to_json(&bank);
        assert!(text.contains("\"version\":1"));
        let zurueck = bank_from_json(&text).expect("Rueckweg");
        assert_eq!(zurueck, bank);

        // Zweiter Durchlauf muss byte-gleich sein (stabile Schreibweise).
        assert_eq!(bank_to_json(&zurueck), text);
    }

    #[test]
    fn leerzeichen_und_zahlen_werden_gelesen() {
        let text = r#"{
            "buttons": [
                {"name": "A", "state": 2, "trigger": "on release"},
                {"state": "3", "trigger": 2, "actions": [{"type": "wait", "ms": 12}]}
            ]
        }"#;
        let bank = bank_from_json(text).unwrap();
        let a = bank.get(0).unwrap();
        assert_eq!(a.state, ButtonState::Trigger);
        assert_eq!(a.trigger, Trigger::Release);
        let b = bank.get(1).unwrap();
        assert_eq!(b.state, ButtonState::Color);
        assert_eq!(b.trigger, Trigger::ValueChange);
        assert_eq!(b.actions, vec![Action::wait(12)]);
        // Rest der 80 Slots sind Standardtasten.
        assert_eq!(bank.len(), MAX_BUTTONS);
        assert_eq!(bank.get(2).unwrap().name, "Makro 3");
    }

    #[test]
    fn kaputte_dateien_werden_abgelehnt() {
        assert!(bank_from_json("{\"buttons\": [").is_err());
        assert!(bank_from_json("{}").is_err());
        assert!(bank_from_json("{\"version\": 99, \"buttons\": []}").is_err());
        assert!(bank_from_json("{\"buttons\":[{\"state\":\"quatsch\"}]}").is_err());
        assert!(
            bank_from_json("{\"buttons\":[{\"actions\":[{\"type\":\"midi\",\"note\":200}]}]}")
                .is_err()
        );
        assert!(bank_from_json(
            "{\"buttons\":[{\"actions\":[{\"type\":\"set\",\"target\":\"strip\"}]}]}"
        )
        .is_err());
    }

    #[test]
    fn grenze_80_buttons_in_der_datei() {
        // 80 Tasten: in Ordnung.
        let liste: Vec<String> = (0..MAX_BUTTONS)
            .map(|i| format!("{{\"name\":\"T{i}\"}}"))
            .collect();
        let text = format!("{{\"buttons\":[{}]}}", liste.join(","));
        let bank = bank_from_json(&text).unwrap();
        assert_eq!(bank.len(), MAX_BUTTONS);
        assert_eq!(bank.get(79).unwrap().name, "T79");

        // 81 Tasten: abgelehnt, mit Anzahl in der Meldung.
        let mut liste = liste;
        liste.push("{\"name\":\"zu viel\"}".to_string());
        let text = format!("{{\"buttons\":[{}]}}", liste.join(","));
        let fehler = bank_from_json(&text).unwrap_err();
        assert!(fehler.contains("81"), "Meldung war: {fehler}");
        assert!(fehler.contains("80"), "Meldung war: {fehler}");
    }

    #[test]
    fn bank_laesst_hoechstens_80_tasten_zu() {
        assert_eq!(Bank::new().len(), MAX_BUTTONS);
        let zu_viele: Vec<Button> = (0..MAX_BUTTONS + 1).map(|i| Button::standard(i)).collect();
        assert!(Bank::from_buttons(zu_viele).is_err());
        let mut bank = Bank::new();
        assert!(bank.set(80, Button::standard(80)).is_err());
        assert!(bank.set(79, Button::standard(79)).is_ok());
    }
}
