//! Wächter über abgesteckte Geräte: Zustand melden, Zuordnung behalten,
//! selbst heilen.
//!
//! Die Zuordnung in `devices.json`/`inputs.conf` ist eine **Absicht**, kein
//! Ist-Zustand. Fällt das Gerät weg (USB abgezogen), bleibt der Eintrag
//! gespeichert und wird als `fehlt` gemeldet – verworfen wird nichts, sonst
//! müsste man nach jedem Umstecken neu zuordnen.
//!
//! Drei Aufgaben:
//!
//! * **Melden** ([`fehlend_json`]): was gemerkt ist und gerade fehlt, mit Ziel
//!   und Grund („Gerät ist nicht angeschlossen (abgesteckt?)“). Das landet in
//!   `/api/state` und in der Oberfläche.
//! * **Heilen** ([`abgleich`]): ändert sich der Graph, wird geprüft, ob ein
//!   gemerktes Gerät wieder da ist; dann wird die Zuordnung neu gelegt
//!   („Geraet wieder da: … -> Streifen n“). Umgekehrt wird die Verbindung
//!   gelöst, wenn das Gerät verschwindet – der Eintrag bleibt.
//! * **Wachen** ([`spawn_wache`]): ein Thread, der den Graphen beobachtet.
//!
//! Auslöser sind die Registry-Ereignisse, die [`crate::pw`] ohnehin sammelt:
//! der Wächter vergleicht eine Signatur aus Knotennamen und Links und wird nur
//! tätig, wenn sich etwas geändert hat.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::devices::{self, DeviceMap, Zustand};
use crate::inputs;
use crate::json::Value;
use crate::pw::{Graph, Zugriff};

/// Prüftakt des Wächters. Kurz genug, dass ein angestecktes Gerät „sofort“
/// wieder greift, lang genug, dass er im Leerlauf nicht auffällt.
pub const TAKT_MS: u64 = 500;

// ---------------------------------------------------------------------------
// Zustand melden
// ---------------------------------------------------------------------------

/// Ein gemerktes Gerät, das gerade fehlt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fehlt {
    /// „Streifen 1“ oder „Bus A1“.
    pub ziel: String,
    /// `"eingang"` oder `"ausgang"`.
    pub art: &'static str,
    /// Technischer Name (`node.name`), wie er gespeichert ist.
    pub name: String,
}

impl Fehlt {
    fn json(&self) -> Value {
        Value::obj(vec![
            ("ziel", Value::str(self.ziel.clone())),
            ("art", Value::str(self.art)),
            ("name", Value::str(self.name.clone())),
            ("vorhanden", Value::Bool(false)),
            ("zustand", Value::str(Zustand::Fehlt.text())),
            (
                "grund",
                Value::str(Zustand::Fehlt.grund().unwrap_or("Gerät fehlt")),
            ),
        ])
    }
}

/// Alle gemerkten Geräte, die im Graphen fehlen – Streifen 1..5 und Busse
/// A1..A5, in dieser Reihenfolge.
pub fn fehlende(graph: &Graph, map: &DeviceMap, inputs_pfad: &Path) -> Vec<Fehlt> {
    let mut liste = Vec::new();
    for (i, quelle) in inputs::mapping_mit(inputs_pfad).iter().enumerate() {
        let Some(name) = quelle else { continue };
        if graph.knoten(name).is_none() {
            liste.push(Fehlt {
                ziel: format!("Streifen {}", i + 1),
                art: "eingang",
                name: name.clone(),
            });
        }
    }
    for bus in devices::HARDWARE_BUSES {
        let Some(name) = map.get(bus) else { continue };
        if graph.knoten(name).is_none() {
            liste.push(Fehlt {
                ziel: format!("Bus {bus}"),
                art: "ausgang",
                name: name.to_string(),
            });
        }
    }
    liste
}

/// `{"anzahl":n,"liste":[…]}` für `/api/state`.
///
/// Ohne erreichbares PipeWire bleibt die Liste leer: dann ist nicht „alles
/// fehlt“, sondern nichts bekannt.
pub fn fehlend_json(zugriff: &dyn Zugriff, map: &DeviceMap, inputs_pfad: &Path) -> Value {
    let liste: Vec<Value> = match zugriff.graph() {
        Ok(graph) => fehlende(&graph, map, inputs_pfad)
            .iter()
            .map(Fehlt::json)
            .collect(),
        Err(_) => Vec::new(),
    };
    Value::obj(vec![
        ("anzahl", Value::num(liste.len() as f64)),
        ("liste", Value::Arr(liste)),
    ])
}

/// Kurztext für Meldungen: „Streifen 1 (alsa_input.…)“, leer wenn nichts fehlt.
pub fn kurztext(fehlende: &[Fehlt]) -> String {
    fehlende
        .iter()
        .map(|f| format!("{} ({})", f.ziel, f.name))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Selbstheilung
// ---------------------------------------------------------------------------

/// Kurzform des Graphen, an der Änderungen erkennbar sind: Knotennamen und
/// bestehende Verbindungen.
pub fn signatur(graph: &Graph) -> String {
    let mut knoten: Vec<&str> = graph.nodes.iter().map(|n| n.name.as_str()).collect();
    knoten.sort_unstable();
    let mut verbindungen: Vec<String> = graph
        .verbindungen()
        .iter()
        .map(|v| format!("{}>{}", v.von, v.nach))
        .collect();
    verbindungen.sort();
    format!("{}\n{}", knoten.join("\n"), verbindungen.join("\n"))
}

/// Ein Durchgang: gemerkte Zuordnungen gegen den Graphen prüfen und Wo möglich
/// wiederherstellen.
///
/// `gemeldet` merkt sich, was schon als fehlend gemeldet wurde, damit ein
/// abgestecktes Gerät nicht bei jeder Graphänderung erneut im Protokoll steht.
/// Liefert die Protokollzeilen.
pub fn abgleich(
    zugriff: &dyn Zugriff,
    map: &mut DeviceMap,
    inputs_pfad: &Path,
    gemeldet: &mut BTreeSet<String>,
) -> Vec<String> {
    let Ok(graph) = zugriff.graph() else {
        return Vec::new();
    };
    // Die Oberfläche schreibt direkt in die Datei – erst nachladen, sonst
    // legt der Wächter eine gerade gelöste Zuordnung wieder an.
    map.nachladen();

    let mut zeilen = Vec::new();

    // ----- Busse (Ausgänge) -----
    let busse: Vec<(String, String)> = map
        .entries()
        .into_iter()
        .map(|(bus, name)| (bus.to_string(), name.to_string()))
        .collect();
    for (bus, name) in busse {
        let ziel = format!("Bus {bus}");
        let marke = format!("{ziel}:{name}");
        if graph.knoten(&name).is_none() {
            // Gerät weg: Verbindung lösen (meist hat PipeWire sie schon
            // mitgerissen), Eintrag bleibt stehen.
            for (von, nach) in devices::bus_ports_zu(&bus, &name) {
                let _ = zugriff.trennen(&von, &nach);
            }
            if gemeldet.insert(marke) {
                zeilen.push(format!(
                    "Geraet weg: {name} -> {ziel} (Zuordnung bleibt, Zustand 'fehlt')"
                ));
            }
            continue;
        }
        gemeldet.remove(&marke);
        if map.zustand(&bus, &graph) == Zustand::Verbunden {
            continue;
        }
        match map.assign(zugriff, &bus, &name) {
            Ok(()) => zeilen.push(format!("Geraet wieder da: {name} -> {ziel}")),
            Err(e) => zeilen.push(format!("{ziel} auf '{name}' nicht verbindbar: {e}")),
        }
    }

    // ----- Streifen (Eingänge) -----
    let zuordnung = inputs::mapping_mit(inputs_pfad);
    for (i, quelle) in zuordnung.iter().enumerate() {
        let Some(name) = quelle else { continue };
        let ziel = format!("Streifen {}", i + 1);
        let marke = format!("{ziel}:{name}");
        if graph.knoten(name).is_none() {
            if let Ok(paare) = inputs::ports_zu(i + 1, name) {
                for (von, nach) in paare {
                    let _ = zugriff.trennen(&von, &nach);
                }
            }
            if gemeldet.insert(marke) {
                zeilen.push(format!(
                    "Geraet weg: {name} -> {ziel} (Zuordnung bleibt, Zustand 'fehlt')"
                ));
            }
            continue;
        }
        gemeldet.remove(&marke);
        if inputs::ist_verbunden(&graph, i + 1, name) {
            continue;
        }
        match inputs::assign_mit(zugriff, inputs_pfad, i + 1, Some(name)) {
            Ok(()) => zeilen.push(format!("Geraet wieder da: {name} -> {ziel}")),
            Err(e) => zeilen.push(format!("{ziel} auf '{name}' nicht verbindbar: {e}")),
        }
    }

    zeilen
}

/// Startet den Wächter-Thread.
///
/// Er läuft, solange der Prozess lebt, und schreibt nur ins Protokoll, wenn
/// sich der Graph geändert hat. `CROSSPOINT_NO_RESTORE` schaltet ihn mit ab –
/// wer das Wiederherstellen abschaltet, will auch keine automatischen
/// Zuordnungen.
pub fn spawn_wache(devices: Arc<Mutex<DeviceMap>>, inputs_pfad: PathBuf) {
    if crate::restore::deaktiviert() {
        println!("Geraetewache abgeschaltet (CROSSPOINT_NO_RESTORE)");
        return;
    }
    thread::spawn(move || {
        let zugriff = crate::pw::PipeWire;
        let mut letzte = String::new();
        let mut gemeldet: BTreeSet<String> = BTreeSet::new();
        loop {
            thread::sleep(Duration::from_millis(TAKT_MS));
            let Ok(graph) = zugriff.graph() else {
                continue;
            };
            let jetzt = signatur(&graph);
            if jetzt == letzte {
                continue;
            }
            letzte = jetzt;
            let mut map = match devices.lock() {
                Ok(m) => m,
                Err(vergiftet) => vergiftet.into_inner(),
            };
            for zeile in abgleich(&zugriff, &mut map, &inputs_pfad, &mut gemeldet) {
                println!("{zeile}");
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pw::tests::{Testzugriff, Wandelzugriff, DUMP};

    const MIC: &str = "alsa_input.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.mono-fallback";
    const SINK: &str =
        "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo";

    fn tmp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("crosspoint-wache-{}-{tag}", std::process::id()))
    }

    /// Graph ohne das Mikrofon: der Sink und der Mixer bleiben, der Streifen
    /// hat keinen Eingang mehr.
    fn ohne_mic() -> Graph {
        Graph::parse(
            r#"[
              {"id": 443, "type": "PipeWire:Interface:Node",
               "info": {"props": {"media.class": "Audio/Sink", "node.name": "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo",
                                  "node.description": "Corsair HS80"}}},
              {"id": 297, "type": "PipeWire:Interface:Node",
               "info": {"props": {"node.name": "crosspoint"}}},
              {"id": 209, "type": "PipeWire:Interface:Port",
               "info": {"direction": "input", "props": {"node.id": 443, "port.name": "playback_FL"}}},
              {"id": 226, "type": "PipeWire:Interface:Port",
               "info": {"direction": "input", "props": {"node.id": 443, "port.name": "playback_FR"}}},
              {"id": 427, "type": "PipeWire:Interface:Port",
               "info": {"direction": "input", "props": {"node.id": 297, "port.name": "strip1_L"}}},
              {"id": 271, "type": "PipeWire:Interface:Port",
               "info": {"direction": "input", "props": {"node.id": 297, "port.name": "strip1_R"}}}
            ]"#,
        )
        .unwrap()
    }

    fn map_mit(pfad: &Path, json: &str) -> DeviceMap {
        std::fs::write(pfad, json).unwrap();
        DeviceMap::load(pfad)
    }

    #[test]
    fn fehlende_quelle_und_fehlender_bus_werden_gemeldet() {
        let pfad = tmp("fehlt-inputs.conf");
        let map_pfad = tmp("fehlt-devices.json");
        std::fs::write(&pfad, format!("1=alsa_input.gibtsnicht\n2={MIC}\n")).unwrap();
        let map = map_mit(&map_pfad, r#"{"A1":"alsa_output.gibtsnicht"}"#);

        let graph = Graph::parse(DUMP).unwrap();
        let liste = fehlende(&graph, &map, &pfad);
        assert_eq!(liste.len(), 2, "{liste:?}");
        assert_eq!(liste[0].ziel, "Streifen 1");
        assert_eq!(liste[0].art, "eingang");
        assert_eq!(liste[0].name, "alsa_input.gibtsnicht");
        assert_eq!(liste[1].ziel, "Bus A1");
        assert_eq!(liste[1].art, "ausgang");
        assert_eq!(
            kurztext(&liste),
            "Streifen 1 (alsa_input.gibtsnicht), Bus A1 (alsa_output.gibtsnicht)"
        );

        // Streifen 2 zeigt auf ein vorhandenes Gerät -> nicht in der Liste.
        assert!(!liste.iter().any(|f| f.ziel == "Streifen 2"));

        // JSON für /api/state
        let zugriff = Testzugriff::aus_dump(DUMP);
        let wert = fehlend_json(&zugriff, &map, &pfad);
        assert_eq!(wert.get("anzahl").unwrap().as_f64(), Some(2.0));
        let eintrag = &wert.get("liste").unwrap().as_arr().unwrap()[0];
        assert_eq!(eintrag.get("zustand").unwrap(), &Value::str("fehlt"));
        assert_eq!(eintrag.get("vorhanden").unwrap(), &Value::Bool(false));
        assert!(eintrag
            .get("grund")
            .unwrap()
            .to_json()
            .contains("abgesteckt"));

        // Ohne PipeWire wird nichts behauptet.
        let kaputt = Testzugriff::ohne_graph();
        let wert = fehlend_json(&kaputt, &map, &pfad);
        assert_eq!(wert.get("anzahl").unwrap().as_f64(), Some(0.0));
        let _ = std::fs::remove_file(&pfad);
        let _ = std::fs::remove_file(&map_pfad);
    }

    #[test]
    fn aenderung_am_graphen_aendert_die_signatur() {
        let a = signatur(&Graph::parse(DUMP).unwrap());
        let b = signatur(&ohne_mic());
        assert_ne!(a, b, "fehlendes Mikrofon muss auffallen");
        assert_eq!(a, signatur(&Graph::parse(DUMP).unwrap()), "stabil");
    }

    #[test]
    fn geraet_kommt_zurueck_dann_wird_die_zuordnung_neu_gelegt() {
        let pfad = tmp("heil-inputs.conf");
        std::fs::write(&pfad, format!("1={MIC}\n")).unwrap();
        let map_pfad = tmp("heil-devices.json");
        let mut map = map_mit(&map_pfad, "{}");

        // Erster Durchgang: Mikrofon fehlt -> nur eine Meldung, kein Verbinden.
        let zugriff = Wandelzugriff::neu(ohne_mic());
        let mut gemeldet = BTreeSet::new();
        let zeilen = abgleich(&zugriff, &mut map, &pfad, &mut gemeldet);
        assert_eq!(zeilen.len(), 1, "{zeilen:?}");
        assert!(zeilen[0].contains("Geraet weg"), "{zeilen:?}");
        assert!(zeilen[0].contains("Streifen 1"), "{zeilen:?}");
        // Die Verbindung wird gelöst (die Ports des abgesteckten Geräts sind
        // weg), der Eintrag bleibt.
        assert_eq!(
            zugriff.aktionen(),
            vec![
                format!("trennen {MIC}:capture_FL crosspoint:strip1_L"),
                format!("trennen {MIC}:capture_FR crosspoint:strip1_R"),
            ]
        );
        // Die Datei bleibt: die Zuordnung ist eine Absicht, kein Ist-Zustand.
        assert_eq!(
            std::fs::read_to_string(&pfad).unwrap(),
            format!("1={MIC}\n")
        );

        // Zweiter Durchgang ohne Änderung: keine neue Meldung.
        let zeilen = abgleich(&zugriff, &mut map, &pfad, &mut gemeldet);
        assert!(zeilen.is_empty(), "{zeilen:?}");

        // Gerät steckt wieder: Graph mit Mikrofon und Ports, aber ohne Link.
        zugriff.setzen(Graph::parse(DUMP_GEPLUGT).unwrap());
        let zeilen = abgleich(&zugriff, &mut map, &pfad, &mut gemeldet);
        assert_eq!(zeilen.len(), 1, "{zeilen:?}");
        assert_eq!(zeilen[0], format!("Geraet wieder da: {MIC} -> Streifen 1"));
        let aktionen = zugriff.aktionen();
        assert!(
            aktionen.contains(&format!("verbinden {MIC}:capture_MONO crosspoint:strip1_L")),
            "{aktionen:?}"
        );
        assert!(
            aktionen.contains(&format!("verbinden {MIC}:capture_MONO crosspoint:strip1_R")),
            "{aktionen:?}"
        );

        // Und danach ist Ruhe: die Zuordnung steht.
        assert!(abgleich(&zugriff, &mut map, &pfad, &mut gemeldet).is_empty());
        let _ = std::fs::remove_file(&pfad);
        let _ = std::fs::remove_file(&map_pfad);
    }

    #[test]
    fn geraet_faellt_weg_dann_wird_der_link_geloest_und_der_eintrag_bleibt() {
        let pfad = tmp("weg-inputs.conf");
        std::fs::write(&pfad, format!("1={MIC}\n")).unwrap();
        let map_pfad = tmp("weg-devices.json");
        let mut map = map_mit(&map_pfad, format!(r#"{{"A1":"{SINK}"}}"#).as_str());

        let zugriff = Wandelzugriff::neu(Graph::parse(DUMP).unwrap());
        let mut gemeldet = BTreeSet::new();
        // Zuerst steht alles: kein Eingriff.
        assert!(abgleich(&zugriff, &mut map, &pfad, &mut gemeldet).is_empty());
        assert!(zugriff.aktionen().is_empty());

        // Beide Geräte verschwinden.
        zugriff.setzen(Graph::parse(LEER).unwrap());
        let zeilen = abgleich(&zugriff, &mut map, &pfad, &mut gemeldet);
        assert_eq!(zeilen.len(), 2, "{zeilen:?}");
        assert!(
            zeilen.iter().all(|z| z.contains("Geraet weg")),
            "{zeilen:?}"
        );
        let aktionen = zugriff.aktionen();
        assert!(
            aktionen.contains(&format!("trennen crosspoint:busA1_L {SINK}:playback_FL")),
            "{aktionen:?}"
        );
        assert!(
            aktionen.contains(&format!("trennen crosspoint:busA1_R {SINK}:playback_FR")),
            "{aktionen:?}"
        );
        assert!(
            aktionen.contains(&format!("trennen {MIC}:capture_FL crosspoint:strip1_L")),
            "{aktionen:?}"
        );
        // Einträge stehen weiterhin in den Dateien.
        assert_eq!(
            std::fs::read_to_string(&pfad).unwrap(),
            format!("1={MIC}\n")
        );
        assert_eq!(
            std::fs::read_to_string(&map_pfad).unwrap(),
            format!(r#"{{"A1":"{SINK}"}}"#)
        );
        assert_eq!(map.get("A1"), Some(SINK));
        let _ = std::fs::remove_file(&pfad);
        let _ = std::fs::remove_file(&map_pfad);
    }

    #[test]
    fn datei_gewinnt_gegen_den_speicher() {
        // Die Oberfläche löscht eine Zuordnung direkt in der Datei; der Wächter
        // darf sie nicht wieder anlegen.
        let map_pfad = tmp("datei-devices.json");
        let mut map = map_mit(&map_pfad, format!(r#"{{"A1":"{SINK}"}}"#).as_str());
        std::fs::write(&map_pfad, "{}").unwrap();
        assert!(map.nachladen(), "Änderung erkannt");
        assert!(map.get("A1").is_none());
        assert!(!map.nachladen(), "zweiter Aufruf ändert nichts");

        let pfad = tmp("datei-inputs.conf");
        std::fs::write(&pfad, "").unwrap();
        let zugriff = Wandelzugriff::neu(Graph::parse(DUMP).unwrap());
        let mut gemeldet = BTreeSet::new();
        assert!(abgleich(&zugriff, &mut map, &pfad, &mut gemeldet).is_empty());
        assert!(zugriff.aktionen().is_empty(), "{:?}", zugriff.aktionen());
        let _ = std::fs::remove_file(&pfad);
        let _ = std::fs::remove_file(&map_pfad);
    }

    /// Wie [`DUMP`], aber ohne den Link Mikrofon → Streifen 1: das Gerät ist
    /// gerade aufgewacht, die Verbindung fehlt noch.
    const DUMP_GEPLUGT: &str = r#"[
      {"id": 443, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Sink", "node.name": "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo",
                          "node.description": "Corsair HS80"}}},
      {"id": 269, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Source", "node.name": "alsa_input.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.mono-fallback",
                          "node.description": "Corsair HS80 Mono"}}},
      {"id": 297, "type": "PipeWire:Interface:Node",
       "info": {"props": {"node.name": "crosspoint"}}},
      {"id": 209, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 443, "port.name": "playback_FL"}}},
      {"id": 226, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 443, "port.name": "playback_FR"}}},
      {"id": 76, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 269, "port.name": "capture_MONO"}}},
      {"id": 427, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 297, "port.name": "strip1_L"}}},
      {"id": 271, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 297, "port.name": "strip1_R"}}},
      {"id": 70, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 297, "port.name": "busA1_L"}}},
      {"id": 71, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 297, "port.name": "busA1_R"}}}
    ]"#;

    const LEER: &str = r#"[
      {"id": 297, "type": "PipeWire:Interface:Node",
       "info": {"props": {"node.name": "crosspoint"}}},
      {"id": 427, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 297, "port.name": "strip1_L"}}},
      {"id": 271, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 297, "port.name": "strip1_R"}}}
    ]"#;
}
