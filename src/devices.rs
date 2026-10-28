//! Geräteauswahl für die Hardware-Busse A1..A5.
//!
//! Jeder Hardware-Bus (A1..A5) hängt an einem Ausgabegerät. Auf Linux
//! ist das ein PipeWire-Sink (`media.class: Audio/Sink`); die Verbindung stellt
//! `pw-link` her, die Ports stehen im Graphen:
//!
//! ```text
//! crosspoint:busA1_L  ->  <sink>:playback_FL
//! crosspoint:busA1_R  ->  <sink>:playback_FR
//! ```
//!
//! Der Mixer heißt in der Graph-Sicht `crosspoint` (siehe `engine`/`jack`); die
//! Sink-Ports heißen je Gerät `<sink>:playback_FL`/`_FR`.
//!
//! Quelle der Wahrheit ist [`crate::pw`]: Knoten, Ports und bestehende Links
//! kommen aus `pw-dump`. Dadurch werden Anzeigenamen nicht mehr aus
//! `pactl`-Text geschnitten, Monitore sind an ihren Ports erkennbar, und beim
//! Umstecken werden die Links getrennt, die wirklich existieren.
//!
//! Gespeichert wird nur der technische Name (`node.name`) eines Geräts:
//!
//! ```json
//! {"A1":"alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo"}
//! ```
//!
//! Beschreibungen entstehen zur Laufzeit aus dem Graphen. Ein älteres Format
//! `{"A1":{"name":"…","desc":"…"}}` wird weiterhin gelesen; `desc` bleibt dabei
//! unbeachtet.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::json::Value;
use crate::pw::{self, CommandRunner, Graph, Richtung};

pub use crate::pw::{Call, FakeRunner, RealRunner};

/// Hardware-Busse des Originals. B1..B3 sind virtuell und haben kein Gerät.
pub const HARDWARE_BUSES: [&str; 5] = ["A1", "A2", "A3", "A4", "A5"];

/// Client-Name des Mixers in der PipeWire-Graph-Sicht.
pub const CLIENT: &str = "crosspoint";

/// Kanalpaar eines Busses: Port-Suffix am Mixer -> Port-Suffix am Sink.
const CHANNELS: [(&str, &str); 2] = [("L", "playback_FL"), ("R", "playback_FR")];

/// Dateiname der gespeicherten Zuordnung.
const CONFIG_FILE: &str = "devices.json";

// ---------------------------------------------------------------------------
// Geräteliste aus dem Graphen
// ---------------------------------------------------------------------------

/// Ein Ausgabegerät, wie es der PipeWire-Graph beschreibt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sink {
    /// Knoten-ID im Graphen (nur zur Anzeige, nicht speichern).
    pub id: u32,
    /// Technischer Name (`node.name`) – wird für die `pw-link`-Ports gebraucht
    /// und ist das Einzige, was die Zuordnung speichert.
    pub name: String,
    /// Anzeigename (`node.description`), vollständig mit Leerzeichen.
    pub desc: String,
}

impl Sink {
    /// Gerät ohne Graphbezug (Tests, Vorgaben).
    pub fn new(name: impl Into<String>, desc: impl Into<String>) -> Sink {
        Sink {
            id: 0,
            name: name.into(),
            desc: desc.into(),
        }
    }

    /// Gerät aus einem Graphen-Knoten.
    pub fn from_node(node: &pw::Node) -> Sink {
        Sink {
            id: node.id,
            name: node.name.clone(),
            desc: node.anzeige().to_string(),
        }
    }

    /// Port am Sink für einen Kanal (`playback_FL` -> `<sink>:playback_FL`).
    pub fn port(&self, sink_suffix: &str) -> String {
        format!("{}:{sink_suffix}", self.name)
    }
}

/// Ausgabegeräte aus dem Graphen, alphabetisch nach Anzeigename.
/// `mit_monitoren` schaltet Knoten dazu, die selbst ein Monitor sind – im
/// Normalfall sind das keine Geräte und bleiben unsichtbar.
pub fn sinks(graph: &Graph, mit_monitoren: bool) -> Vec<Sink> {
    graph
        .geraete()
        .into_iter()
        .filter(|n| n.ist_ausgabegeraet() && (mit_monitoren || !n.ist_monitor))
        .map(Sink::from_node)
        .collect()
}

/// Ausgabegeräte über `pw-dump`; Monitore bleiben ausgeblendet.
pub fn list_sinks(runner: &dyn CommandRunner) -> Result<Vec<Sink>, String> {
    list_sinks_mit(runner, false)
}

/// Wie [`list_sinks`], aber mit zuschaltbaren Monitoren.
pub fn list_sinks_mit(
    runner: &dyn CommandRunner,
    mit_monitoren: bool,
) -> Result<Vec<Sink>, String> {
    Ok(sinks(&pw::dump(runner)?, mit_monitoren))
}

// ---------------------------------------------------------------------------
// Zuordnung Bus -> Gerät
// ---------------------------------------------------------------------------

/// Prüft einen Busnamen und liefert die kanonische Schreibweise.
///
/// Erlaubt sind ausschließlich A1..A5 (Groß-/Kleinschreibung egal). B1..B3 sind
/// virtuell, A6.. gibt es nicht.
pub fn normalisiere_bus(bus: &str) -> Result<String, String> {
    let kandidat = bus.trim().to_ascii_uppercase();
    if HARDWARE_BUSES.contains(&kandidat.as_str()) {
        Ok(kandidat)
    } else {
        Err(format!(
            "Bus '{bus}' ist kein Hardware-Bus (erlaubt: {})",
            HARDWARE_BUSES.join(", ")
        ))
    }
}

/// Mixer-Port eines Bus-Kanals, z. B. `crosspoint:busA1_L`.
pub fn bus_port(bus: &str, kanal: &str) -> String {
    format!("{CLIENT}:bus{bus}_{kanal}")
}

/// Speicherort der Zuordnung: `$XDG_CONFIG_HOME/crosspoint/devices.json`,
/// sonst `~/.config/crosspoint/devices.json`.
pub fn default_path() -> PathBuf {
    crate::pfade::datei(CONFIG_FILE)
}

/// Zuordnung Hardware-Bus -> technischer Gerätename, gespiegelt nach JSON.
pub struct DeviceMap {
    pfad: PathBuf,
    busse: BTreeMap<String, String>,
}

impl DeviceMap {
    /// Leere Zuordnung, die nach `pfad` schreibt.
    pub fn new(pfad: impl Into<PathBuf>) -> DeviceMap {
        DeviceMap {
            pfad: pfad.into(),
            busse: BTreeMap::new(),
        }
    }

    /// Lädt die Zuordnung; eine fehlende oder unlesbare Datei ergibt eine leere
    /// Zuordnung (erster Start), statt die Oberfläche scheitern zu lassen.
    pub fn load(pfad: impl Into<PathBuf>) -> DeviceMap {
        let pfad = pfad.into();
        let mut map = DeviceMap {
            pfad,
            busse: BTreeMap::new(),
        };
        if let Ok(text) = std::fs::read_to_string(&map.pfad) {
            map.uebernehme(&text);
        }
        map
    }

    /// Wie [`DeviceMap::load`], aber am Standardpfad.
    pub fn load_default() -> DeviceMap {
        DeviceMap::load(default_path())
    }

    pub fn path(&self) -> &std::path::Path {
        &self.pfad
    }

    /// Technischer Name des zugeordneten Geräts eines Hardware-Busses.
    pub fn get(&self, bus: &str) -> Option<&str> {
        let bus = normalisiere_bus(bus).ok()?;
        self.busse.get(&bus).map(String::as_str)
    }

    /// Gerät eines Busses, mit Beschreibung aus dem Graphen aufgelöst.
    /// `None`, wenn der Bus belegt ist, das Gerät aber nicht (mehr) existiert.
    pub fn get_in(&self, bus: &str, graph: &Graph) -> Option<Sink> {
        let name = self.get(bus)?;
        graph.knoten(name).map(Sink::from_node)
    }

    /// Alle gesetzten Zuordnungen in Bus-Reihenfolge A1..A5.
    pub fn entries(&self) -> Vec<(&str, &str)> {
        self.busse
            .iter()
            .map(|(b, n)| (b.as_str(), n.as_str()))
            .collect()
    }

    /// Übernimmt eine JSON-Datei. Unbekannte Busse und Einträge ohne Namen
    /// werden übersprungen, damit eine handgeschriebene Datei nichts kaputt macht.
    /// Gelesen wird sowohl `"A1": "name"` als auch das ältere
    /// `"A1": {"name": "…", "desc": "…"}`.
    fn uebernehme(&mut self, text: &str) {
        let Ok(Value::Obj(pairs)) = crate::json::parse(text) else {
            return;
        };
        for (bus, wert) in pairs {
            let Ok(bus) = normalisiere_bus(&bus) else {
                continue;
            };
            let name = match &wert {
                Value::Str(s) => s.clone(),
                Value::Obj(_) => wert
                    .get("name")
                    .and_then(|v| match v {
                        Value::Str(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default(),
                _ => String::new(),
            };
            if name.trim().is_empty() {
                continue;
            }
            self.busse.insert(bus, name.trim().to_string());
        }
    }

    /// Schreibt die Zuordnung. Ein nicht anlegbares Verzeichnis ist ein Fehler,
    /// eine fehlgeschlagene Verbindung dagegen nicht.
    pub fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.pfad.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("{} nicht anlegbar: {e}", dir.display()))?;
        }
        std::fs::write(&self.pfad, self.to_json())
            .map_err(|e| format!("{} nicht schreibbar: {e}", self.pfad.display()))
    }

    /// Die Zuordnung als JSON-Objekt `{"A1":"<node.name>", …}` – nur Namen.
    pub fn to_json(&self) -> String {
        Value::Obj(
            self.busse
                .iter()
                .map(|(bus, name)| (bus.clone(), Value::str(name.clone())))
                .collect(),
        )
        .to_json()
    }

    /// Verdrahtet `bus` auf den Sink `sink` (technischer Name).
    ///
    /// Zuerst werden alle Links getrennt, die laut Graph wirklich an den
    /// Busports hängen – egal von wem sie stammen. Danach werden die echten
    /// Eingangsports des Sinks verbunden. Ein Fehler beim Verbinden hinterlässt
    /// keine Zuordnung, damit Datei und Graph deckungsgleich bleiben.
    pub fn assign(
        &mut self,
        runner: &dyn CommandRunner,
        bus: &str,
        sink: &str,
    ) -> Result<(), String> {
        let bus = normalisiere_bus(bus)?;
        let sink = sink.trim();
        if sink.is_empty() {
            return Err("Sink ohne Namen".into());
        }
        let graph = pw::dump(runner)?;
        let node = graph
            .knoten(sink)
            .ok_or_else(|| format!("Sink '{sink}' steht nicht im PipeWire-Graph"))?;
        if !node.ist_ausgabegeraet() {
            return Err(format!(
                "'{sink}' ist kein Ausgabegerät (media.class '{}')",
                node.media_class
            ));
        }

        let bus_ports = [bus_port(&bus, "L"), bus_port(&bus, "R")];
        for verbindung in graph.offene_links(&bus_ports) {
            let _ = runner.run("pw-link", &["-d", &verbindung.von, &verbindung.nach]);
        }

        let eingaenge = graph.pfade(node.id, Richtung::In);
        let ziele = pw::kanalpaar(&eingaenge);
        if ziele.is_empty() {
            return Err(format!("Sink '{sink}' hat keinen Eingangsport"));
        }

        for (i, kanal) in ["L", "R"].iter().enumerate() {
            let ziel = &ziele[if ziele.len() == 1 { 0 } else { i }];
            runner
                .run("pw-link", &[&bus_port(&bus, kanal), ziel])
                .map_err(|e| format!("{bus} auf '{sink}' verbinden fehlgeschlagen: {e}"))?;
        }

        self.busse.insert(bus, sink.to_string());
        self.save()
    }

    /// Löst die Verbindungen eines Busses und entfernt die Zuordnung.
    /// Ist nichts verbunden und nichts zugeordnet, passiert nichts (idempotent).
    pub fn clear(&mut self, runner: &dyn CommandRunner, bus: &str) -> Result<(), String> {
        let bus = normalisiere_bus(bus)?;
        let graph = pw::dump(runner)?;
        let bus_ports = [bus_port(&bus, "L"), bus_port(&bus, "R")];
        let mut etwas_getan = false;
        for verbindung in graph.offene_links(&bus_ports) {
            etwas_getan = true;
            let _ = runner.run("pw-link", &["-d", &verbindung.von, &verbindung.nach]);
        }

        // Kein Link (mehr) im Graphen, aber eine Zuordnung: die alten
        // Kanalports trotzdem trennen, damit nichts hängen bleibt.
        if let Some(alt) = self.busse.get(&bus).cloned() {
            if !etwas_getan {
                for (kanal, ziel) in CHANNELS {
                    let _ = runner.run(
                        "pw-link",
                        &["-d", &bus_port(&bus, kanal), &format!("{alt}:{ziel}")],
                    );
                }
            }
        }

        let entfernt = self.busse.remove(&bus).is_some();
        if !entfernt && !etwas_getan {
            return Ok(());
        }
        self.save()
    }

    /// Zustand für die Oberfläche/API: verfügbare Sinks plus Zuordnung der
    /// Busse A1..A5.
    ///
    /// `sink: null` heißt „kein Gerät gewählt“. `desc` wird zur Laufzeit aus dem
    /// Graphen geholt; `vorhanden: false` zeigt an, dass ein gemerktes Gerät
    /// gerade nicht da ist. `verbunden` meldet, ob laut Graph wirklich ein Link
    /// vom Busport zum Gerät besteht. `error` trägt die Meldung, falls `pw-dump`
    /// nicht erreichbar war.
    pub fn status_json(&self, runner: &dyn CommandRunner, mit_monitoren: bool) -> String {
        let graph = pw::dump(runner);
        let (liste, fehler) = match &graph {
            Ok(g) => (sinks(g, mit_monitoren), Value::Null),
            Err(e) => (Vec::new(), Value::str(e.clone())),
        };

        let busse = HARDWARE_BUSES
            .iter()
            .map(|bus| {
                let mut paare: Vec<(String, Value)> = vec![("bus".into(), Value::str(*bus))];
                let name = self.busse.get(*bus);
                let geraet = graph.as_ref().ok().and_then(|g| self.get_in(bus, g));
                paare.push((
                    "sink".into(),
                    name.map(|n| Value::str(n.clone())).unwrap_or(Value::Null),
                ));
                paare.push((
                    "desc".into(),
                    geraet
                        .as_ref()
                        .map(|s| Value::str(s.desc.clone()))
                        .unwrap_or(Value::Null),
                ));
                paare.push((
                    "vorhanden".into(),
                    Value::Bool(name.is_some() && geraet.is_some()),
                ));
                let verbunden = graph
                    .as_ref()
                    .map(|g| {
                        [bus_port(bus, "L"), bus_port(bus, "R")]
                            .iter()
                            .any(|p| !g.offene_links(&[p.clone()]).is_empty())
                    })
                    .unwrap_or(false);
                paare.push(("verbunden".into(), Value::Bool(verbunden)));
                Value::Obj(paare)
            })
            .collect();

        Value::obj(vec![
            (
                "sinks",
                Value::Arr(
                    liste
                        .iter()
                        .map(|s| {
                            Value::obj(vec![
                                ("id", Value::num(s.id as f64)),
                                ("name", Value::str(s.name.clone())),
                                ("desc", Value::str(s.desc.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("buses", Value::Arr(busse)),
            ("error", fehler),
        ])
        .to_json()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pw::tests::DUMP;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "crosspoint-devices-{}-{tag}.json",
            std::process::id()
        ))
    }

    fn fake() -> FakeRunner {
        FakeRunner::new().with_output("pw-dump", DUMP)
    }

    const CORSAIR: &str =
        "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo";
    const HDMI: &str = "alsa_output.pci-0000_03_00.1.hdmi-stereo-extra2";

    #[test]
    fn sinks_kommen_aus_dem_graphen_mit_voller_beschreibung() {
        let liste = list_sinks(&fake()).unwrap();
        assert_eq!(liste.len(), 3, "{liste:?}");
        // Reihenfolge: alphabetisch nach Anzeigename (ohne Rücksicht auf
        // Gross-/Kleinschreibung). Nachgeschlagen wird ueber den Namen, damit
        // der Test nicht an der Sortierung der Anzeigenamen haengt.
        assert_eq!(liste[0].name, CORSAIR);
        let corsair = liste.iter().find(|s| s.name == CORSAIR).expect("Corsair");
        assert_eq!(
            corsair.desc,
            "Corsair HS80 RGB USB Gaming Headset Analoges Stereo"
        );
        assert_eq!(corsair.id, 443);
        assert!(
            corsair.desc.contains(' '),
            "Leerzeichen bleiben: {}",
            corsair.desc
        );
        let hdmi = liste.iter().find(|s| s.name == HDMI).expect("HDMI");
        assert_eq!(
            hdmi.desc,
            "Navi 48 HDMI/DP Audio Controller Digital Stereo (HDMI 3) [AG326UD]"
        );
        // Eigene virtuelle Sinks sind echte Ausgabegeräte des Graphen.
        assert!(
            liste.iter().any(|s| s.name == "crosspoint_out_B1"),
            "{liste:?}"
        );
    }

    #[test]
    fn monitore_sind_ausgeblendet_aber_zuschaltbar() {
        let mit_monitor = r#"[
          {"id": 1, "type": "PipeWire:Interface:Node",
           "info": {"props": {"media.class": "Audio/Sink",
                              "node.name": "alsa_output.hdmi.monitor",
                              "node.description": "HDMI (Monitor)"}}},
          {"id": 2, "type": "PipeWire:Interface:Node",
           "info": {"props": {"media.class": "Audio/Sink",
                              "node.name": "alsa_output.usb", "node.description": "Kopfhörer"}}}
        ]"#;
        let graph = Graph::parse(mit_monitor).unwrap();
        assert_eq!(sinks(&graph, false).len(), 1, "Standardliste ohne Monitore");
        let mit = sinks(&graph, true);
        assert_eq!(mit.len(), 2, "zugeschaltet sichtbar");
        assert!(mit.iter().any(|s| s.name == "alsa_output.hdmi.monitor"));
        assert!(mit.iter().any(|s| s.name == "alsa_output.usb"));

        let fake = FakeRunner::new().with_output("pw-dump", mit_monitor);
        assert_eq!(list_sinks(&fake).unwrap().len(), 1);
        assert_eq!(list_sinks_mit(&fake, true).unwrap().len(), 2);
    }

    #[test]
    fn assign_trennt_vorhandene_links_und_verbindet_echte_ports() {
        let pfad = tmp_path("assign");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = fake();

        // Im Graphen hängt bereits crosspoint:busA1_L -> playback_FL (Link 500).
        map.assign(&fake, "A1", CORSAIR).unwrap();
        let zeilen = fake.lines();
        assert_eq!(
            &zeilen[1..4],
            &[
                format!("pw-link -d crosspoint:busA1_L {CORSAIR}:playback_FL"),
                format!("pw-link crosspoint:busA1_L {CORSAIR}:playback_FL"),
                format!("pw-link crosspoint:busA1_R {CORSAIR}:playback_FR"),
            ],
            "erst den echten Link lösen, dann verbinden: {zeilen:?}"
        );
        assert_eq!(zeilen[0], "pw-dump");
        assert_eq!(
            fake.calls_of("pw-link")
                .iter()
                .filter(|c| c.args[0] == "-d")
                .count(),
            1
        );
        assert_eq!(map.get("A1"), Some(CORSAIR));
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn assign_lehnt_unbekanntes_geraet_ab() {
        let pfad = tmp_path("unbekannt");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = fake();
        assert!(map.assign(&fake, "A1", "alsa_output.gibtsnicht").is_err());
        assert!(map.get("A1").is_none());
        // Eingabegerät ist kein Sink.
        let mic = "alsa_input.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.mono-fallback";
        let fehler = map.assign(&fake, "A1", mic).unwrap_err();
        assert!(fehler.contains("kein Ausgabegerät"), "{fehler}");
        assert!(map.get("A1").is_none());
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn clear_loest_alle_buslinks_und_leert_eintrag() {
        let pfad = tmp_path("clear");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = fake();
        map.assign(&fake, "A1", CORSAIR).unwrap();
        let vorher = fake.calls().len();
        map.clear(&fake, "A1").unwrap();
        let danach = fake.calls();
        let neu: Vec<&Call> = danach[vorher..].iter().collect();
        // Ein pw-dump, ein pw-link -d auf den einzigen vorhandenen Link.
        assert_eq!(neu.len(), 2, "{neu:?}");
        assert_eq!(neu[0].program, "pw-dump");
        assert_eq!(
            neu[1].line(),
            format!("pw-link -d crosspoint:busA1_L {CORSAIR}:playback_FL")
        );
        assert!(map.get("A1").is_none());

        // Zweites clear: kein Eintrag mehr, im Graphen nichts mehr -> nur der Dump.
        let leer = FakeRunner::new().with_output("pw-dump", "[]");
        map.clear(&leer, "A1").unwrap();
        assert_eq!(leer.lines(), vec!["pw-dump".to_string()]);
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn clear_trennt_auch_ohne_graphenlink_ueber_den_gemerkten_namen() {
        let pfad = tmp_path("clear-fallback");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        map.assign(&fake(), "A2", CORSAIR).unwrap();
        // Graph ohne Links (z. B. Verbindung von außen gelöst).
        let leer = FakeRunner::new().with_output("pw-dump", "[]");
        map.clear(&leer, "A2").unwrap();
        let zeilen = leer.lines();
        assert_eq!(
            zeilen,
            vec![
                "pw-dump".to_string(),
                format!("pw-link -d crosspoint:busA2_L {CORSAIR}:playback_FL"),
                format!("pw-link -d crosspoint:busA2_R {CORSAIR}:playback_FR"),
            ]
        );
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn disconnect_fehler_wird_ignoriert() {
        let pfad = tmp_path("ignorieren");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = FakeRunner::new()
            .with_output("pw-dump", DUMP)
            .with_failure("pw-link -d");
        map.assign(&fake, "A3", CORSAIR).unwrap();
        let zeilen = fake.lines();
        assert_eq!(zeilen.len(), 3, "{zeilen:?}");
        assert_eq!(
            zeilen[1],
            format!("pw-link crosspoint:busA3_L {CORSAIR}:playback_FL")
        );
        assert_eq!(
            zeilen[2],
            format!("pw-link crosspoint:busA3_R {CORSAIR}:playback_FR")
        );
        assert!(map.get("A3").is_some());
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn connect_fehler_laesst_zuordnung_unveraendert() {
        let pfad = tmp_path("connectfehler");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = FakeRunner::new()
            .with_output("pw-dump", DUMP)
            .with_failure("playback_FR");
        assert!(map.assign(&fake, "A2", CORSAIR).is_err());
        assert!(map.get("A2").is_none());
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn unbekannter_bus_wird_abgelehnt() {
        let pfad = tmp_path("bus");
        let fake = fake();
        let mut map = DeviceMap::new(&pfad);
        for bus in ["B2", "A6", "A0", "", "AI", "A1 x"] {
            assert!(
                map.assign(&fake, bus, CORSAIR).is_err(),
                "'{bus}' abgelehnt"
            );
            assert!(map.clear(&fake, bus).is_err(), "'{bus}' abgelehnt");
        }
        assert!(
            fake.calls().is_empty(),
            "kein Systemaufruf bei ungültigem Bus"
        );
        assert_eq!(normalisiere_bus("a4").unwrap(), "A4");
        assert_eq!(normalisiere_bus(" A5 ").unwrap(), "A5");
        assert!(DeviceMap::new(&pfad).get("B1").is_none());
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn datei_speichert_nur_den_namen_beschreibung_kommt_zur_laufzeit() {
        let pfad = tmp_path("rundlauf");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = fake();
        map.assign(&fake, "A1", CORSAIR).unwrap();
        map.assign(&fake, "A5", HDMI).unwrap();

        let text = std::fs::read_to_string(&pfad).unwrap();
        assert_eq!(
            text,
            format!(
                "{{\"A1\":{},\"A5\":{}}}",
                crate::json::quote(CORSAIR),
                crate::json::quote(HDMI)
            ),
            "nur node.name, keine Beschreibung"
        );
        assert!(
            !text.contains("Corsair HS80 RGB"),
            "keine Beschreibung in der Datei: {text}"
        );

        let wieder = DeviceMap::load(&pfad);
        assert_eq!(wieder.get("A1"), Some(CORSAIR));
        assert_eq!(wieder.get("a1"), Some(CORSAIR));
        assert!(wieder.get("A2").is_none());
        assert_eq!(wieder.entries().len(), 2);
        // Beschreibung entsteht aus dem Graphen – ohne sie zu speichern.
        let graph = pw::dump(&fake).unwrap();
        assert_eq!(
            wieder.get_in("A1", &graph).unwrap().desc,
            "Corsair HS80 RGB USB Gaming Headset Analoges Stereo"
        );
        assert_eq!(
            wieder.get_in("A5", &graph).unwrap().desc,
            "Navi 48 HDMI/DP Audio Controller Digital Stereo (HDMI 3) [AG326UD]"
        );
        // Gerät verschwunden -> Name bleibt, Beschreibung fehlt.
        let leer = Graph::parse("[]").unwrap();
        assert!(wieder.get_in("A1", &leer).is_none());
        assert_eq!(wieder.get("A1"), Some(CORSAIR));

        // Altes Format mit desc wird weiter gelesen (desc ignoriert).
        std::fs::write(
            &pfad,
            r#"{"A2":{"name":"alsa_output.usb-0d8c_Speaker","desc":"Lautsprecher"}}"#,
        )
        .unwrap();
        let alt = DeviceMap::load(&pfad);
        assert_eq!(alt.get("A2"), Some("alsa_output.usb-0d8c_Speaker"));
        assert_eq!(alt.entries().len(), 1);

        // Kaputte Datei -> leere Zuordnung, kein Absturz.
        std::fs::write(&pfad, "kein json").unwrap();
        assert!(DeviceMap::load(&pfad).entries().is_empty());
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn status_json_enthaelt_sinks_zuordnung_und_echte_links() {
        let pfad = tmp_path("status");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = fake();
        map.assign(&fake, "A1", CORSAIR).unwrap();

        let json = map.status_json(&fake, false);
        let wert = crate::json::parse(&json).unwrap();
        let sinks = wert.get("sinks").unwrap().as_arr().unwrap();
        assert_eq!(sinks.len(), 3);
        assert_eq!(sinks[0].get("id").unwrap().as_f64(), Some(443.0));
        assert_eq!(
            sinks[0].get("desc").unwrap(),
            &Value::str("Corsair HS80 RGB USB Gaming Headset Analoges Stereo")
        );

        let busse = wert.get("buses").unwrap().as_arr().unwrap();
        assert_eq!(busse.len(), 5, "immer A1..A5");
        assert_eq!(busse[0].get("bus").unwrap(), &Value::str("A1"));
        assert_eq!(busse[0].get("sink").unwrap(), &Value::str(CORSAIR));
        assert_eq!(busse[0].get("vorhanden").unwrap(), &Value::Bool(true));
        assert_eq!(
            busse[0].get("verbunden").unwrap(),
            &Value::Bool(true),
            "Link 500 im Graph"
        );
        assert_eq!(busse[1].get("sink").unwrap(), &Value::Null);
        assert_eq!(busse[1].get("verbunden").unwrap(), &Value::Bool(false));
        assert_eq!(wert.get("error").unwrap(), &Value::Null);

        // Ohne erreichbares pw-dump bleibt das JSON gültig.
        let kaputt = FakeRunner::new().with_failure("pw-dump");
        let wert = crate::json::parse(&map.status_json(&kaputt, false)).unwrap();
        assert_eq!(wert.get("sinks").unwrap().as_arr().unwrap().len(), 0);
        assert!(wert.get("error").unwrap() != &Value::Null);
        assert_eq!(
            wert.get("buses").unwrap().as_arr().unwrap()[0]
                .get("verbunden")
                .unwrap(),
            &Value::Bool(false)
        );
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn ports_und_standardpfad() {
        assert_eq!(bus_port("A1", "L"), "crosspoint:busA1_L");
        assert_eq!(bus_port("A5", "R"), "crosspoint:busA5_R");
        assert_eq!(
            Sink::new(CORSAIR, "x").port("playback_FL"),
            format!("{CORSAIR}:playback_FL")
        );
        let pfad = default_path();
        assert!(
            pfad.ends_with("crosspoint/devices.json"),
            "{}",
            pfad.display()
        );
    }
}
