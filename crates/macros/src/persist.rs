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
//! Gelesen und geschrieben wird mit `serde`/`serde_json`. Kein eigener Parser
//! mehr: die Feldreihenfolge kommt aus der Reihenfolge der Felder in den
//! Strukturen unten, die Zahlenform aus [`Zahlform`] (ganze Werte ohne
//! Nachkommastellen, sonst die kuerzeste Schreibweise - genau wie vorher).
//!
//! Gelesen wird grosszuegig: fehlende Felder bekommen Standardwerte,
//! unbekannte Schluessel werden ignoriert, `state`/`trigger` duerfen als Zahl
//! oder Text stehen, Zahlen auch als Text (`"250"`). Geschrieben wird immer das
//! vollstaendige Bild mit allen 80 Slots, damit ein Rundlauf verlustfrei ist.

use std::io;

use serde::Serialize;
use serde_json::Value;

use crate::model::{Action, Bank, Button, ButtonState, Trigger, MAX_BUTTONS};

/// Aktuelle Formatversion. Hoehere Versionen werden abgelehnt, weil sonst
/// unbekannte Felder stillschweigend verloren gingen.
pub const FORMAT_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Schreiben
// ---------------------------------------------------------------------------

/// Wurzel der Datei. Die Feldreihenfolge ist Teil des Formats.
#[derive(Serialize)]
struct Dokument<'a> {
    version: u32,
    buttons: Vec<Taste<'a>>,
}

/// Eine Taste in der Datei. Die Feldreihenfolge ist Teil des Formats.
#[derive(Serialize)]
struct Taste<'a> {
    name: &'a str,
    state: &'static str,
    trigger: &'static str,
    gui: Option<u16>,
    actions: Vec<Aktion<'a>>,
}

/// Eine Aktion in der Datei. `type` steht zuerst, danach die Felder der Art.
#[derive(Serialize)]
#[serde(tag = "type")]
enum Aktion<'a> {
    #[serde(rename = "set")]
    Set {
        target: &'a str,
        index: u32,
        param: &'a str,
        value: f64,
    },
    #[serde(rename = "midi")]
    Midi { channel: u8, note: u8, velocity: u8 },
    #[serde(rename = "wait")]
    Wait { ms: u32 },
    #[serde(rename = "toggle")]
    Toggle {
        target: &'a str,
        index: u32,
        param: &'a str,
        to: Option<f64>,
    },
}

/// Zahlenform der Datei: ganze Werte ohne Nachkommastellen (`1`, nicht `1.0`),
/// sonst die kuerzeste wieder einlesbare Schreibweise (`-12.5`, `0.1`).
///
/// `serde_json` schriebe `1.0` und damit eine andere Datei als bisher. Das
/// wird hier im Schreiber erledigt, nicht nachtraeglich am Text, damit auch
/// Zahlen in tief verschachtelten Aktionen stimmen.
struct Zahlform;

impl serde_json::ser::Formatter for Zahlform {
    fn write_f64<W>(&mut self, w: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if !value.is_finite() {
            // NaN und Unendlich sind in JSON nicht darstellbar.
            return w.write_all(b"0");
        }
        if value.fract() == 0.0 && value.abs() < 1e15 {
            write!(w, "{}", value as i64)
        } else {
            write!(w, "{value}")
        }
    }
}

/// Belegung als JSON-Text.
pub fn bank_to_json(bank: &Bank) -> String {
    let dokument = Dokument {
        version: FORMAT_VERSION,
        buttons: bank.iter().map(taste_schreiben).collect(),
    };
    let mut puffer = Vec::with_capacity(16 * 1024);
    let mut schreiber = serde_json::Serializer::with_formatter(&mut puffer, Zahlform);
    dokument
        .serialize(&mut schreiber)
        .expect("Serialisieren in einen Vec<u8> kann nicht fehlschlagen");
    let kompakt = String::from_utf8(puffer).expect("serde_json schreibt UTF-8");
    // Einrueckung je Taste auf eigener Zeile: die Datei wird von Hand gepflegt.
    // Die Paare "},{"name":" kommen nur zwischen zwei Tasten vor, nicht in
    // Aktionslisten (dort steht "type" zuerst).
    kompakt.replace("},{\"name\":", "},\n{\"name\":")
}

fn taste_schreiben(button: &Button) -> Taste<'_> {
    Taste {
        name: &button.name,
        state: button.state.as_str(),
        trigger: button.trigger.as_str(),
        gui: button.gui,
        actions: button.actions.iter().map(aktion_schreiben).collect(),
    }
}

fn aktion_schreiben(action: &Action) -> Aktion<'_> {
    match action {
        Action::SetParam {
            target,
            index,
            param,
            value,
        } => Aktion::Set {
            target,
            index: *index,
            param,
            value: *value,
        },
        Action::MidiNote {
            channel,
            note,
            velocity,
        } => Aktion::Midi {
            channel: *channel,
            note: *note,
            velocity: *velocity,
        },
        Action::Wait { ms } => Aktion::Wait { ms: *ms },
        Action::Toggle {
            target,
            index,
            param,
            to,
        } => Aktion::Toggle {
            target,
            index: *index,
            param,
            to: *to,
        },
    }
}

// ---------------------------------------------------------------------------
// Lesen
// ---------------------------------------------------------------------------

/// Belegung aus JSON-Text lesen. Fehler nennen Taste und Aktion, weil die
/// Datei von Hand entstehen kann.
pub fn bank_from_json(text: &str) -> Result<Bank, String> {
    let wurzel: Value =
        serde_json::from_str(text).map_err(|e| format!("kein gueltiges JSON: {e}"))?;

    if let Some(v) = wurzel.get("version") {
        let version = als_zahl(v).ok_or_else(|| "Feld 'version' ist keine Zahl".to_string())?;
        if version > FORMAT_VERSION as f64 {
            return Err(format!(
                "Formatversion {version} ist neuer als die unterstuetzte {FORMAT_VERSION}"
            ));
        }
    }

    let liste = wurzel
        .get("buttons")
        .and_then(als_liste)
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

fn button_from_wert(index: usize, w: &Value) -> Result<Button, String> {
    if !w.is_object() {
        return Err(format!("Taste {index}: kein Objekt"));
    }

    let name = match w.get("name").and_then(als_text) {
        Some(s) => s.to_string(),
        None => format!("Makro {}", index + 1),
    };

    let state = match w.get("state") {
        None | Some(Value::Null) => ButtonState::Off,
        Some(v) => {
            if let Some(s) = als_text(v) {
                ButtonState::parse(s)
                    .ok_or_else(|| format!("Taste {index}: unbekannter Zustand '{s}'"))?
            } else {
                let n = als_zahl(v)
                    .ok_or_else(|| format!("Taste {index}: 'state' ist weder Text noch Zahl"))?;
                ButtonState::from_u8(n as u8)
                    .ok_or_else(|| format!("Taste {index}: Zustand {n} liegt ausserhalb 0..=3"))?
            }
        }
    };

    let trigger = match w.get("trigger") {
        None | Some(Value::Null) => Trigger::Press,
        Some(v) => {
            if let Some(s) = als_text(v) {
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
        None | Some(Value::Null) => None,
        Some(v) => {
            let n = als_zahl(v).ok_or_else(|| format!("Taste {index}: 'gui' ist keine Zahl"))?;
            if n < 0.0 || n > 255.0 || n.fract() != 0.0 {
                return Err(format!(
                    "Taste {index}: GUI-Slot {n} liegt ausserhalb 0..=255"
                ));
            }
            Some(n as u16)
        }
    };

    let mut actions = Vec::new();
    if let Some(liste) = w.get("actions").and_then(als_liste) {
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

fn action_from_wert(button: usize, pos: usize, w: &Value) -> Result<Action, String> {
    let ort = |s: &str| format!("Taste {button}, Aktion {pos}: {s}");
    let art = w
        .get("type")
        .and_then(als_text)
        .ok_or_else(|| ort("'type' fehlt oder ist kein Text"))?;

    match art.trim().to_ascii_lowercase().as_str() {
        "set" | "setparam" | "param" => Ok(Action::SetParam {
            target: pflicht_text(w, "target", &ort)?,
            index: u32_feld(w, "index", 0)?,
            param: pflicht_text(w, "param", &ort)?,
            value: w
                .get("value")
                .and_then(als_zahl)
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
                None | Some(Value::Null) => None,
                Some(v) => Some(als_zahl(v).ok_or_else(|| ort("'to' ist keine Zahl"))?),
            },
        }),
        andere => Err(ort(&format!("unbekannter Aktionstyp '{andere}'"))),
    }
}

fn pflicht_text(o: &Value, name: &str, ort: &dyn Fn(&str) -> String) -> Result<String, String> {
    o.get(name)
        .and_then(als_text)
        .map(|s| s.to_string())
        .ok_or_else(|| ort(&format!("'{name}' fehlt oder ist kein Text")))
}

/// Nichtnegative Ganzzahl aus einem Feld; fehlt es, gilt `standard`.
fn u32_feld(o: &Value, name: &str, standard: u32) -> Result<u32, String> {
    match o.get(name) {
        None | Some(Value::Null) => Ok(standard),
        Some(v) => {
            let n = als_zahl(v).ok_or_else(|| format!("Feld '{name}' ist keine Zahl"))?;
            if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                return Err(format!(
                    "Feld '{name}' ist keine nichtnegative Ganzzahl: {n}"
                ));
            }
            Ok(n as u32)
        }
    }
}

fn u8_feld(o: &Value, name: &str, standard: u8) -> Result<u8, String> {
    let n = u32_feld(o, name, standard as u32)?;
    if n > 255 {
        return Err(format!("Feld '{name}' ist groesser als 255: {n}"));
    }
    Ok(n as u8)
}

/// Zahl lesen. Wahrheitswerte zaehlen als 1/0, Zahlen als Text werden
/// akzeptiert, damit handgeschriebene Dateien (`{"ms": "250"}`) nicht scheitern.
fn als_zahl(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn als_text(v: &Value) -> Option<&str> {
    match v {
        Value::String(s) => Some(s.as_str()),
        _ => None,
    }
}

fn als_liste(v: &Value) -> Option<&Vec<Value>> {
    match v {
        Value::Array(l) => Some(l),
        _ => None,
    }
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

    /// Die Datei muss Zeichen fuer Zeichen so aussehen wie vor der Umstellung
    /// auf `serde_json`: Feldreihenfolge, `1` statt `1.0`, `null` fuer fehlende
    /// GUI-Slots, eine Taste je Zeile.
    #[test]
    fn schreibweise_bleibt_wie_bisher() {
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
            1,
            Button::neu(
                "Ende \"quoted\"",
                ButtonState::Color,
                Trigger::ValueChange,
                vec![
                    Action::set("bus", 7, "gain", -12.5),
                    Action::Toggle {
                        target: "strip".into(),
                        index: 2,
                        param: "gain".into(),
                        to: Some(0.1),
                    },
                ],
            ),
        )
        .unwrap();

        let text = bank_to_json(&bank);
        let erwartet = concat!(
            "{\"version\":1,\"buttons\":[",
            "{\"name\":\"Intro\",\"state\":\"on\",\"trigger\":\"press\",\"gui\":3,",
            "\"actions\":[",
            "{\"type\":\"set\",\"target\":\"strip\",\"index\":0,\"param\":\"mute\",\"value\":1},",
            "{\"type\":\"midi\",\"channel\":1,\"note\":60,\"velocity\":127},",
            "{\"type\":\"wait\",\"ms\":250},",
            "{\"type\":\"toggle\",\"target\":\"bus\",\"index\":0,\"param\":\"mute\",\"to\":null}",
            "]},",
            "\n",
            "{\"name\":\"Ende \\\"quoted\\\"\",\"state\":\"color\",\"trigger\":\"value_change\",",
            "\"gui\":null,\"actions\":[",
            "{\"type\":\"set\",\"target\":\"bus\",\"index\":7,\"param\":\"gain\",\"value\":-12.5},",
            "{\"type\":\"toggle\",\"target\":\"strip\",\"index\":2,\"param\":\"gain\",\"to\":0.1}",
            "]},",
            "\n",
            "{\"name\":\"Makro 3\",\"state\":\"off\",\"trigger\":\"press\",\"gui\":null,\"actions\":[]}",
        );
        assert!(
            text.starts_with(erwartet),
            "Schreibweise weicht ab. Anfang war:\n{}",
            &text[..text.len().min(400)]
        );
        // Und die letzte Taste schliesst die Liste.
        assert!(text.ends_with("\"actions\":[]}]}"), "Ende war: {text:.80}");
    }

    /// Nachgebaute Datei in der alten Form, wie sie in
    /// `~/.config/crosspoint/macros.json` liegen kann: eine Taste je Zeile,
    /// Aktionen mit allen Feldnamen, `state`/`trigger` teils als Text, teils
    /// als Zahl, Zahlen teils als Text.
    const ALTE_DATEI: &str = r#"{"version":1,"buttons":[
{"name":"Szene A","state":"on","trigger":"press","gui":0,"actions":[{"type":"set","target":"strip","index":0,"param":"mute","value":1},{"type":"wait","ms":250},{"type":"midi","channel":1,"note":60,"velocity":127},{"type":"toggle","target":"bus","index":2,"param":"mute","to":null}]},
{"name":"Szene B","state":3,"trigger":2,"gui":null,"actions":[{"type":"set","target":"bus","index":7,"param":"gain","value":-12.5},{"type":"toggle","target":"strip","index":2,"param":"gain","to":0.1}]},
{"name":"Umlaut: Größe ä","state":"off","trigger":"on release","gui":17,"actions":[]},
{"name":"Handarbeit","state":"2","trigger":"value change","actions":[{"type":"wait","ms":"300"}]}
]}
"#;

    #[test]
    fn alte_datei_wird_gelesen() {
        let bank = bank_from_json(ALTE_DATEI).expect("alte Datei muss lesbar sein");

        let a = bank.get(0).expect("Taste 0");
        assert_eq!(a.name, "Szene A");
        assert_eq!(a.state, ButtonState::On);
        assert_eq!(a.trigger, Trigger::Press);
        assert_eq!(a.gui, Some(0));
        assert_eq!(
            a.actions,
            vec![
                Action::set("strip", 0, "mute", 1.0),
                Action::wait(250),
                Action::midi(1, 60, 127),
                Action::toggle("bus", 2, "mute"),
            ]
        );

        let b = bank.get(1).expect("Taste 1");
        assert_eq!(b.state, ButtonState::Color);
        assert_eq!(b.trigger, Trigger::ValueChange);
        assert_eq!(b.gui, None);
        assert_eq!(b.actions[0], Action::set("bus", 7, "gain", -12.5));
        assert_eq!(
            b.actions[1],
            Action::Toggle {
                target: "strip".into(),
                index: 2,
                param: "gain".into(),
                to: Some(0.1)
            }
        );

        let c = bank.get(2).expect("Taste 2");
        assert_eq!(c.name, "Umlaut: Größe ä");
        assert_eq!(c.trigger, Trigger::Release);
        assert_eq!(c.gui, Some(17));

        let d = bank.get(3).expect("Taste 3");
        assert_eq!(d.state, ButtonState::Trigger);
        assert_eq!(d.trigger, Trigger::ValueChange);
        assert_eq!(d.actions, vec![Action::wait(300)]);

        // Rest der 80 Slots sind Standardtasten.
        assert_eq!(bank.len(), MAX_BUTTONS);
        assert_eq!(bank.get(4).unwrap().name, "Makro 5");

        // Und das Gelesene laesst sich wieder schreiben und erneut lesen.
        let text = bank_to_json(&bank);
        assert_eq!(bank_from_json(&text).unwrap(), bank);
    }

    #[test]
    fn unbekannte_felder_werden_ignoriert() {
        let text = r##"{
            "version": 1,
            "kommentar": "von Hand angelegt",
            "erzeugt": "2026-10-29T12:00:00Z",
            "buttons": [
                {
                    "name": "A",
                    "farbe": "#ff0000",
                    "state": "on",
                    "trigger": "press",
                    "gui": 2,
                    "notizen": ["x", {"y": 1}],
                    "actions": [
                        {"type": "set", "target": "strip", "index": 1, "param": "gain",
                         "value": -6, "kommentar": "leise"},
                        {"type": "wait", "ms": 5, "extra": null}
                    ]
                }
            ]
        }"##;
        let bank = bank_from_json(text).expect("unbekannte Felder stoeren nicht");
        let a = bank.get(0).unwrap();
        assert_eq!(a.name, "A");
        assert_eq!(a.state, ButtonState::On);
        assert_eq!(a.gui, Some(2));
        assert_eq!(a.actions[0], Action::set("strip", 1, "gain", -6.0));
        assert_eq!(a.actions[1], Action::wait(5));
    }

    #[test]
    fn fehlende_felder_bekommen_vorgaben() {
        // Nur die Liste: alle Tasten sind Standardtasten.
        let bank = bank_from_json(r#"{"buttons": []}"#).unwrap();
        assert_eq!(bank, Bank::new());

        // Leere Taste: Name, Zustand, Trigger, GUI und Aktionen aus Vorgaben.
        let bank = bank_from_json(r#"{"buttons": [{}]}"#).unwrap();
        let b = bank.get(0).unwrap();
        assert_eq!(b.name, "Makro 1");
        assert_eq!(b.state, ButtonState::Off);
        assert_eq!(b.trigger, Trigger::Press);
        assert_eq!(b.gui, None);
        assert!(b.actions.is_empty());

        // Die Formatversion darf fehlen (alte, handgeschriebene Datei).
        assert!(bank_from_json(r#"{"buttons": [{"name": "X"}]}"#).is_ok());
    }

    #[test]
    fn sonderzeichen_und_zahlen_im_rundlauf() {
        let name = "Zeile1\n\"zitat\"\\pfad\ttab \u{1} ende äöü €";
        let mut bank = Bank::new();
        bank.set(
            5,
            Button::neu(
                name,
                ButtonState::Trigger,
                Trigger::Release,
                vec![
                    Action::set("strip", 3, "gain", 0.1),
                    Action::set("strip", 4, "gain", -0.0000001),
                    Action::wait(u32::MAX),
                    Action::midi(16, 127, 0),
                ],
            )
            .mit_gui(255),
        )
        .unwrap();
        bank.set(
            6,
            Button::neu(
                "gross",
                ButtonState::Off,
                Trigger::Press,
                vec![Action::set("bus", 0, "gain", 1e20)],
            ),
        )
        .unwrap();

        let text = bank_to_json(&bank);
        let zurueck = bank_from_json(&text).expect("Rundlauf");
        assert_eq!(zurueck, bank);
        assert_eq!(zurueck.get(5).unwrap().name, name);
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
        // Abgeschnitten, kein JSON, leer.
        assert!(bank_from_json("{\"buttons\": [").is_err());
        assert!(bank_from_json("").is_err());
        assert!(bank_from_json("   ").is_err());
        assert!(bank_from_json("null").is_err());
        assert!(bank_from_json("[]").is_err());
        assert!(bank_from_json("{\"buttons\": []} danach").is_err());
        assert!(bank_from_json("{\"buttons\": [],}").is_err());
        // Wurzel ohne brauchbare Tastenliste.
        assert!(bank_from_json("{}").is_err());
        assert!(bank_from_json("{\"buttons\": 5}").is_err());
        assert!(bank_from_json("{\"buttons\": null}").is_err());
        // Inhaltlich falsch.
        assert!(bank_from_json("{\"buttons\": [7]}").is_err());
        assert!(bank_from_json("{\"buttons\": [{\"actions\": {}}]}").is_err());
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
        assert!(bank_from_json("{\"buttons\":[{\"actions\":[{\"type\":\"quatsch\"}]}]}").is_err());
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
