//! JSON-Werte der Steuer-API.
//!
//! Bis v0.2.0 stand hier ein eigener Parser und Serialisierer (~270 Zeilen).
//! Der ist ersetzt: **gelesen und geschrieben wird mit `serde_json`**. Die
//! Fehler der letzten Tage (Feldfehler in `pw-dump`, Escape-Sonderfälle) lagen
//! genau dort – ein JSON-Parser ist gelöste Arbeit.
//!
//! Was bleibt, ist die Form der Werte: [`Value`] hält Objekte als geordnete
//! Liste von Schlüssel/Wert-Paaren, nicht als `serde_json::Map`. Das ist
//! Absicht – die Reihenfolge der Felder ist Teil der öffentlichen API-Form
//! (`/api/state`, `/api/eq`, …) und `serde_json::Map` sortiert ohne das Feature
//! `preserve_order` alphabetisch. Geschrieben wird deshalb über
//! [`Serialize`] auf dem geordneten Wert, gelesen über `serde_json::Value` und
//! danach umgesetzt ([`Value::von_serde`]).

use std::fmt;
use std::io;

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

/// Ein JSON-Wert mit erhaltener Feldreihenfolge.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    /// Objekt als geordnete Paarliste (Reihenfolge = Ausgabereihenfolge).
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn obj(pairs: Vec<(&str, Value)>) -> Value {
        Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    pub fn num(v: f64) -> Value {
        Value::Num(v)
    }
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }

    /// Feld eines Objekts (erster Treffer, wie bisher).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(p) => p.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Str(s) => s.parse().ok(),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Num(n) => Some(*n != 0.0),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }

    /// Ausgabe als kompakter JSON-Text (keine Leerzeichen, Feldreihenfolge
    /// erhalten, Zahlen wie bisher formatiert).
    pub fn to_json(&self) -> String {
        let mut buf = Vec::with_capacity(512);
        let mut ser = serde_json::Serializer::with_formatter(&mut buf, Zahlform);
        match self.serialize(&mut ser) {
            Ok(()) => String::from_utf8(buf).unwrap_or_else(|_| "null".to_string()),
            // Vec<u8> kann nicht fehlschlagen; trotzdem kein panic im API-Pfad.
            Err(_) => "null".to_string(),
        }
    }

    /// Setzt einen `serde_json`-Wert in die geordnete Form um.
    ///
    /// Objekte kommen dabei in der Reihenfolge an, die `serde_json` liefert
    /// (alphabetisch, solange `preserve_order` nicht aktiv ist). Für gelesene
    /// Daten ist das ohne Belang: zugegriffen wird über [`Value::get`].
    pub fn von_serde(v: serde_json::Value) -> Value {
        use serde_json::Value as J;
        match v {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(b),
            J::Number(n) => Value::Num(n.as_f64().unwrap_or(0.0)),
            J::String(s) => Value::Str(s),
            J::Array(a) => Value::Arr(a.into_iter().map(Value::von_serde).collect()),
            J::Object(m) => Value::Obj(
                m.into_iter()
                    .map(|(k, v)| (k, Value::von_serde(v)))
                    .collect(),
            ),
        }
    }
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Null => s.serialize_unit(),
            Value::Bool(b) => s.serialize_bool(*b),
            // `serde_json` schreibt NaN/Inf als `null`. Die API hat dafür
            // seit jeher `0` geliefert (eine Pegelanzeige soll nicht in null
            // laufen); das bleibt so.
            Value::Num(n) if !n.is_finite() => s.serialize_f64(0.0),
            Value::Num(n) => s.serialize_f64(*n),
            Value::Str(t) => s.serialize_str(t),
            Value::Arr(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for v in a {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            Value::Obj(p) => {
                let mut map = s.serialize_map(Some(p.len()))?;
                for (k, v) in p {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

/// Zahlenformat der API: ganze Werte ohne Nachkommastellen, sonst drei.
///
/// Das ist die Form, die die Oberfläche seit v0.1 liest (`-140`, `0.700`,
/// `1000`). `serde_json` würde `0.700` als `0.7` und `1000` als `1000.0`
/// schreiben – deshalb eigener `Formatter` statt Nachbearbeitung am Text.
struct Zahlform;

impl serde_json::ser::Formatter for Zahlform {
    fn write_f64<W>(&mut self, w: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if !value.is_finite() {
            return w.write_all(b"0");
        }
        if value.fract() == 0.0 && value.abs() < 1e15 {
            write!(w, "{}", value as i64)
        } else {
            write!(w, "{value:.3}")
        }
    }
}

/// Zeichenkette als JSON-Literal (für handgebaute Antworten wie
/// `{"error":…}`).
pub fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// JSON lesen. Fehlertexte kommen von `serde_json` und nennen Zeile und Spalte.
pub fn parse(s: &str) -> Result<Value, String> {
    match serde_json::from_str::<serde_json::Value>(s) {
        Ok(v) => Ok(Value::von_serde(v)),
        Err(e) => Err(e.to_string()),
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rundlauf() {
        let v = Value::obj(vec![
            ("a", Value::num(1.5)),
            ("b", Value::Arr(vec![Value::Bool(true), Value::str("x")])),
        ]);
        let text = v.to_json();
        assert_eq!(text, r#"{"a":1.500,"b":[true,"x"]}"#);
        let back = parse(&text).unwrap();
        assert_eq!(back.get("a").unwrap().as_f64(), Some(1.5));
        assert_eq!(back.get("b").unwrap().as_arr().unwrap().len(), 2);
    }

    #[test]
    fn zahlen_und_bools() {
        let v = parse(r#"{"gain":-12.5,"mute":true}"#).unwrap();
        assert_eq!(v.get("gain").unwrap().as_f64(), Some(-12.5));
        assert_eq!(v.get("mute").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn feldreihenfolge_bleibt_wie_gebaut() {
        // Die API-Form haengt an dieser Reihenfolge (zustand -> Oberflaeche).
        let v = Value::obj(vec![
            ("name", Value::str("HW In 1")),
            ("gain", Value::num(0.0)),
            ("mute", Value::Bool(false)),
        ]);
        assert_eq!(v.to_json(), r#"{"name":"HW In 1","gain":0,"mute":false}"#);
    }

    #[test]
    fn zahlenform_wie_bisher() {
        // Werte aus echten Antworten: /api/levels (-140), /api/eq (0.700, 1000)
        let f = |n: f64| Value::num(n).to_json();
        assert_eq!(f(-140.0), "-140");
        assert_eq!(f(0.0), "0");
        assert_eq!(f(0.7), "0.700");
        assert_eq!(f(1000.0), "1000");
        assert_eq!(f(-6.5), "-6.500");
        assert_eq!(f(1.0 / 3.0), "0.333");
        assert_eq!(f(-0.0001), "-0.000");
        assert_eq!(f(f64::NAN), "0", "nicht-endliche Zahlen wie bisher 0");
        assert_eq!(f(1e20), "100000000000000000000.000");
    }

    #[test]
    fn texte_werden_wie_bisher_maskiert() {
        assert_eq!(quote("a\"b\\c\nd\te"), r#""a\"b\\c\nd\te""#);
        assert_eq!(quote("äöü"), "\"äöü\"", "UTF-8 bleibt UTF-8");
        assert_eq!(quote("\u{1}"), r#""\u0001""#);
    }

    #[test]
    fn fremde_daten_lesen() {
        // Ausschnitt aus pw-dump: verschachtelt, Zahlen, null, Unicode
        let text = r#"{"id":287,"info":{"props":{"node.description":"Ryzen HD Audio – Digitales Stereo","monitor":null}},"rates":[44100,48000]}"#;
        let v = parse(text).unwrap();
        assert_eq!(v.get("id").unwrap().as_f64(), Some(287.0));
        let desc = v
            .get("info")
            .and_then(|i| i.get("props"))
            .and_then(|p| p.get("node.description"))
            .and_then(|d| match d {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            });
        assert_eq!(desc.as_deref(), Some("Ryzen HD Audio – Digitales Stereo"));
        assert_eq!(v.get("rates").unwrap().as_arr().unwrap().len(), 2);
    }

    #[test]
    fn fehler_nennt_stelle() {
        let e = parse("{\"target\":\"strip\",").unwrap_err();
        assert!(e.contains("line 1"), "serde_json-Fehler erwartet: {e}");
        assert!(parse("").is_err());
        assert!(parse("[1,2,]").is_err());
    }

    #[test]
    fn grosszahlen_bleiben_lesbar() {
        let v = parse(r#"{"frame":9007199254740993}"#).unwrap();
        assert!(v.get("frame").unwrap().as_f64().is_some());
    }

    /// Das Feature `derive` ist aktiv und wird auch gebraucht (Tape-Metadaten,
    /// spaetere Konfigurationsdateien lesen strukturiert statt feldweise).
    #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
    struct Beispiel {
        gain: f32,
        mute: bool,
        name: String,
    }

    #[test]
    fn derive_steht_bereit() {
        let b = Beispiel {
            gain: -6.0,
            mute: true,
            name: "HW In 1".into(),
        };
        let text = serde_json::to_string(&b).unwrap();
        assert_eq!(text, r#"{"gain":-6.0,"mute":true,"name":"HW In 1"}"#);
        assert_eq!(serde_json::from_str::<Beispiel>(&text).unwrap(), b);
    }
}
