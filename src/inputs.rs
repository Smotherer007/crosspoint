//! Eingangszuordnung für die Streifen 1..5 (Hardware-Eingänge).
//!
//! Auf Windows sind das WDM/KS/ASIO-Geräte; hier sind es PipeWire-Knoten.
//! Quelle der Wahrheit ist [`crate::pw`]: echte Aufnahmegeräte haben
//! `media.class: Audio/Source` und Ausgangsports (`capture_*`), Monitore sind
//! dagegen **Ausgangsports eines Sinks** (`monitor_FL`/`monitor_FR`). Deshalb
//! wird ein Monitor als Quelle über den Sink-Knoten geführt, an dem diese Ports
//! hängen – geraten wird nichts mehr.
//!
//! Ein Mikrofon mit nur einem Kanal (Corsair-Headset: `capture_MONO`) wird auf
//! beide Streifenseiten gelegt; welcher Port das ist, steht im Graphen.
//!
//! Gespeichert wird nur der technische Name (`node.name`); die Beschreibung
//! entsteht bei jeder Anzeige neu aus dem Graphen. Ein altes
//! `…​.monitor` aus PulseAudio-Zeiten wird dabei auf den Sink abgebildet.

use std::path::PathBuf;

use crate::json::Value;
use crate::pw::{self, CommandRunner, Graph, Richtung};

pub use crate::pw::{Call, FakeRunner, RealRunner};

pub const MAX_HW_STRIPS: usize = 5;

/// Datei mit der Streifen-Zuordnung (nur Namen).
const CONFIG_FILE: &str = "inputs.conf";

// ---------------------------------------------------------------------------
// Quellen aus dem Graphen
// ---------------------------------------------------------------------------

/// Eine Aufnahmequelle: echtes Gerät oder Monitor eines Sinks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Knoten-ID im Graphen (nur zur Anzeige).
    pub id: u32,
    /// Technischer Name (`node.name`) – bei einem Monitor der Name des Sinks.
    pub name: String,
    /// Anzeigename aus dem Graphen, bei Monitoren mit Zusatz „(Monitor)“.
    pub desc: String,
    /// Monitor eines Ausgangs (Ports `monitor_*`) statt eines Aufnahmegeräts.
    pub is_monitor: bool,
}

impl Source {
    pub fn from_node(node: &pw::Node) -> Source {
        Source {
            id: node.id,
            name: node.name.clone(),
            desc: node.anzeige().to_string(),
            is_monitor: false,
        }
    }
}

/// Ist der Knoten Teil des Mixers selbst? Dann taugt er nicht als Eingang.
pub fn ist_eigen(name: &str) -> bool {
    let name = name.trim();
    name == "crosspoint" || name.starts_with("crosspoint_") || name.starts_with("crosspoint:")
}

/// Kanonischer Name: ein altes `…​.monitor` gehört zum Sink davor.
pub fn kanonisch(name: &str) -> String {
    let name = name.trim();
    name.strip_suffix(".monitor").unwrap_or(name).to_string()
}

/// Quellen aus dem Graphen: Aufnahmegeräte und – auf Wunsch – die Monitore der
/// Ausgabegeräte. Eigene `crosspoint*`-Knoten bleiben außen vor, sonst böte sich der
/// Mixer selbst als Eingang an.
pub fn quellen(graph: &Graph, mit_monitoren: bool) -> Vec<Source> {
    let mut liste: Vec<Source> = graph
        .sources()
        .into_iter()
        .filter(|n| !ist_eigen(&n.name) && !n.ist_monitor)
        .map(Source::from_node)
        .collect();

    if mit_monitoren {
        for sink in graph.sinks() {
            if ist_eigen(&sink.name) || !graph.hat_monitor(sink.id) {
                continue;
            }
            liste.push(Source {
                id: sink.id,
                name: sink.name.clone(),
                desc: format!("{} (Monitor)", sink.anzeige()),
                is_monitor: true,
            });
        }
    }

    liste.sort_by(|a, b| {
        a.is_monitor
            .cmp(&b.is_monitor)
            .then_with(|| a.desc.to_lowercase().cmp(&b.desc.to_lowercase()))
    });
    liste
}

/// Quellen über `pw-dump`; Monitore bleiben standardmäßig ausgeblendet.
pub fn list_sources(runner: &dyn CommandRunner) -> Result<Vec<Source>, String> {
    list_sources_mit(runner, false)
}

/// Wie [`list_sources`], aber mit zuschaltbaren Monitoren.
pub fn list_sources_mit(
    runner: &dyn CommandRunner,
    mit_monitoren: bool,
) -> Result<Vec<Source>, String> {
    Ok(quellen(&pw::dump(runner)?, mit_monitoren))
}

// ---------------------------------------------------------------------------
// Ports und Verbinden
// ---------------------------------------------------------------------------

/// Portnamen für einen Quellknoten (FL, FR) – nur noch als Vorgabe, wenn der
/// Graph keine Ports kennt. Im Betrieb kommen die Ports aus [`source_ports`].
pub fn ports_for(source: &str) -> (String, String) {
    let basis = kanonisch(source);
    if source.trim().ends_with(".monitor") {
        (format!("{basis}:monitor_FL"), format!("{basis}:monitor_FR"))
    } else {
        (format!("{basis}:capture_FL"), format!("{basis}:capture_FR"))
    }
}

/// Streifen 1..5 → Portnamen des Mixers.
pub fn strip_ports(strip: usize) -> Result<(String, String, String, String), String> {
    if strip == 0 || strip > MAX_HW_STRIPS {
        return Err(format!("Streifen {strip} hat keinen Hardware-Eingang (1..{MAX_HW_STRIPS})"));
    }
    Ok((
        format!("crosspoint:strip{strip}_L"),
        format!("crosspoint:strip{strip}_R"),
        format!("crosspoint:strip{strip}_L"),
        format!("crosspoint:strip{strip}_R"),
    ))
}

/// Alle Ausgangsports einer Quelle, so wie PipeWire sie gerade anbietet –
/// als Pfad `"<knoten>:<port>"`.
///
/// Aus dem Graphen, mit `pw-link -o` als Rückfall. Damit klappt auch
/// `capture_MONO` eines Mono-Mikrofons, statt blind `capture_FL`/`capture_FR`
/// zu raten. Bei einem Monitor liefert der Sink-Knoten `monitor_FL`/`_FR`.
pub fn source_ports(runner: &dyn CommandRunner, graph: &Graph, source: &str) -> Vec<String> {
    pw::ports(runner, graph, &kanonisch(source), Richtung::Out)
}

/// Verbindet Quellports mit den beiden Streifen-Eingängen. Mono-Quellen
/// werden auf beide Seiten gelegt, Stereo-Quellen paarweise.
pub fn link_source(
    runner: &dyn CommandRunner,
    graph: &Graph,
    source: &str,
    in_l: &str,
    in_r: &str,
) -> Result<(), String> {
    let ports = source_ports(runner, graph, source);
    let ziele = pw::kanalpaar(&ports);
    match ziele.len() {
        0 => Err(format!("Quelle {source} hat keinen Ausgangsport")),
        1 => {
            runner.run("pw-link", &[&ziele[0], in_l])?;
            runner.run("pw-link", &[&ziele[0], in_r])?;
            Ok(())
        }
        _ => {
            runner.run("pw-link", &[&ziele[0], in_l])?;
            runner.run("pw-link", &[&ziele[1], in_r])?;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Merkliste (nur Namen)
// ---------------------------------------------------------------------------

/// Speicherort der Streifen-Zuordnung: `$CROSSPOINT_INPUTS_FILE` (Tests), sonst
/// `~/.config/crosspoint/inputs.conf`.
pub fn mapping_path() -> PathBuf {
    if let Some(p) = std::env::var_os("CROSSPOINT_INPUTS_FILE").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/crosspoint").join(CONFIG_FILE)
}

/// Gelesene Zuordnung: Streife 1..5 -> technischer Name der Quelle.
pub fn mapping_mit(pfad: &std::path::Path) -> [Option<String>; MAX_HW_STRIPS] {
    let mut out: [Option<String>; MAX_HW_STRIPS] = Default::default();
    if let Ok(text) = std::fs::read_to_string(pfad) {
        for line in text.lines() {
            if let Some((n, src)) = line.split_once('=') {
                if let Ok(i) = n.trim().parse::<usize>() {
                    if (1..=MAX_HW_STRIPS).contains(&i) && !src.trim().is_empty() {
                        out[i - 1] = Some(kanonisch(src));
                    }
                }
            }
        }
    }
    out
}

pub fn mapping() -> [Option<String>; MAX_HW_STRIPS] {
    mapping_mit(&mapping_path())
}

fn save_mapping_mit(pfad: &std::path::Path, strip: usize, source: Option<&str>) {
    let mut m = mapping_mit(pfad);
    m[strip - 1] = source.map(kanonisch);
    let mut text = String::new();
    for (i, eintrag) in m.iter().enumerate() {
        if let Some(s) = eintrag {
            text.push_str(&format!("{}={}\n", i + 1, s));
        }
    }
    if let Some(dir) = pfad.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(pfad, text);
}

// ---------------------------------------------------------------------------
// Zuordnen
// ---------------------------------------------------------------------------

/// Verbindet einen Streifen mit einer Quelle (bzw. löst die Verbindung).
///
/// Vorher werden **alle** Links getrennt, die laut Graph in die beiden
/// Streifenports hineinführen – auch fremde (z. B. `crosspoint:vban_L`). Danach werden
/// die echten Ports der Quelle verbunden.
pub fn assign(runner: &dyn CommandRunner, strip: usize, source: Option<&str>) -> Result<(), String> {
    assign_mit(runner, &mapping_path(), strip, source)
}

/// Wie [`assign`], aber mit eigener Merkliste (Tests, andere Konfiguration).
pub fn assign_mit(
    runner: &dyn CommandRunner,
    pfad: &std::path::Path,
    strip: usize,
    source: Option<&str>,
) -> Result<(), String> {
    let (in_l, in_r, _, _) = strip_ports(strip)?;
    let graph = pw::dump(runner)?;

    for verbindung in graph.offene_links(&[in_l.clone(), in_r.clone()]) {
        let _ = runner.run("pw-link", &["-d", &verbindung.von, &verbindung.nach]);
    }

    let quelle = source.map(kanonisch).filter(|s| !s.is_empty());
    if let Some(src) = quelle.as_deref() {
        link_source(runner, &graph, src, &in_l, &in_r)?;
    }
    save_mapping_mit(pfad, strip, quelle.as_deref());
    Ok(())
}

// ---------------------------------------------------------------------------
// Zustand für die Oberfläche
// ---------------------------------------------------------------------------

/// Zustand als JSON: Quellen (mit echten Ports), Zuordnung der Streifen und die
/// tatsächlich bestehenden Verbindungen an den Streifenports.
pub fn status_json(runner: &dyn CommandRunner, mit_monitoren: bool) -> String {
    status_json_mit(runner, &mapping_path(), mit_monitoren)
}

/// Wie [`status_json`], aber mit eigener Merkliste.
pub fn status_json_mit(
    runner: &dyn CommandRunner,
    pfad: &std::path::Path,
    mit_monitoren: bool,
) -> String {
    let graph = pw::dump(runner);
    let (quellen, fehler) = match &graph {
        Ok(g) => (quellen(g, mit_monitoren), Value::Null),
        Err(e) => (Vec::new(), Value::str(e.clone())),
    };
    let m = mapping_mit(pfad);

    let quellen_json: Vec<Value> = quellen
        .iter()
        .map(|q| {
            let ports = match &graph {
                Ok(g) => source_ports(runner, g, &q.name),
                Err(_) => Vec::new(),
            };
            Value::obj(vec![
                ("id", Value::num(q.id as f64)),
                ("name", Value::str(q.name.clone())),
                ("desc", Value::str(q.desc.clone())),
                ("monitor", Value::Bool(q.is_monitor)),
                ("ports", Value::Arr(ports.into_iter().map(Value::Str).collect())),
            ])
        })
        .collect();

    let zuordnung: Vec<Value> = m
        .iter()
        .enumerate()
        .filter_map(|(i, eintrag)| {
            let name = eintrag.as_ref()?;
            let desc = graph
                .as_ref()
                .ok()
                .and_then(|g| g.knoten(name))
                .map(|n| n.anzeige().to_string())
                .unwrap_or_default();
            Some(Value::obj(vec![
                ("strip", Value::num((i + 1) as f64)),
                ("source", Value::str(name.clone())),
                ("desc", Value::str(desc)),
            ]))
        })
        .collect();

    // Echte Verbindungen an den Streifenports – Ist-Zustand statt Merkliste.
    let streifen: Vec<String> = (1..=MAX_HW_STRIPS)
        .flat_map(|i| [format!("crosspoint:strip{i}_L"), format!("crosspoint:strip{i}_R")])
        .collect();
    let links: Vec<Value> = graph
        .as_ref()
        .map(|g| {
            g.offene_links(&streifen)
                .into_iter()
                .map(|v| {
                    Value::obj(vec![
                        ("von", Value::str(v.von)),
                        ("nach", Value::str(v.nach)),
                        ("zustand", Value::str(v.zustand)),
                    ])
                })
                .collect()
        })
        .unwrap_or_default();

    Value::obj(vec![
        ("sources", Value::Arr(quellen_json)),
        ("assigned", Value::Arr(zuordnung)),
        ("links", Value::Arr(links)),
        ("strips", Value::num(MAX_HW_STRIPS as f64)),
        ("error", fehler),
    ])
    .to_json()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pw::tests::DUMP;

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("crosspoint-inputs-{}-{tag}.conf", std::process::id()))
    }

    fn fake(text: &str) -> FakeRunner {
        FakeRunner::new().with_output("pw-dump", text)
    }

    const MONO: &str =
        "alsa_input.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.mono-fallback";

    #[test]
    fn quellen_kommen_aus_dem_graphen() {
        let q = list_sources(&fake(DUMP)).unwrap();
        assert_eq!(q.len(), 1, "ohne Monitore nur das Mikrofon: {q:?}");
        assert_eq!(q[0].name, MONO);
        assert_eq!(q[0].desc, "Corsair HS80 RGB USB Gaming Headset Mono");
        assert!(!q[0].is_monitor);
        assert_eq!(q[0].id, 269);
    }

    #[test]
    fn monitore_sind_ausgeblendet_aber_zuschaltbar() {
        let ohne = quellen(&Graph::parse(DUMP).unwrap(), false);
        assert!(ohne.iter().all(|q| !q.is_monitor));

        let mit = quellen(&Graph::parse(DUMP).unwrap(), true);
        let monitore: Vec<&Source> = mit.iter().filter(|q| q.is_monitor).collect();
        // Corsair und HDMI haben Monitorports; crosspoint_out_B1 auch – ist aber eigen.
        assert_eq!(monitore.len(), 2, "{mit:?}");
        assert_eq!(
            monitore[0].desc,
            "Corsair HS80 RGB USB Gaming Headset Analoges Stereo (Monitor)"
        );
        assert_eq!(
            monitore[0].name,
            "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo"
        );
        assert!(monitore.iter().all(|m| !ist_eigen(&m.name)));
        // Monitore stehen hinter den echten Geräten.
        assert!(!mit[0].is_monitor);
        assert!(mit.last().unwrap().is_monitor);
        assert_eq!(list_sources_mit(&fake(DUMP), true).unwrap().len(), 3);
    }

    #[test]
    fn eigene_knoten_und_streams_werden_ausgeblendet() {
        let graph = Graph::parse(DUMP).unwrap();
        let mit = quellen(&graph, true);
        assert!(mit.iter().all(|q| !q.name.starts_with("crosspoint")), "{mit:?}");
        assert!(mit.iter().all(|q| q.name != "Chromium"), "Stream ist keine Quelle");
        assert!(ist_eigen("crosspoint"));
        assert!(ist_eigen("crosspoint_in_1"));
        assert!(ist_eigen("crosspoint:strip1_L"));
        assert!(!ist_eigen("alsa_input.usb"));
    }

    #[test]
    fn portnamen_unterscheiden_quelle_und_monitor() {
        assert_eq!(
            ports_for("alsa_input.usb-mic"),
            ("alsa_input.usb-mic:capture_FL".into(), "alsa_input.usb-mic:capture_FR".into())
        );
        assert_eq!(
            ports_for("alsa_output.hdmi.monitor"),
            ("alsa_output.hdmi:monitor_FL".into(), "alsa_output.hdmi:monitor_FR".into())
        );
        assert_eq!(kanonisch("alsa_output.hdmi.monitor"), "alsa_output.hdmi");
        assert_eq!(kanonisch(" alsa_input.usb "), "alsa_input.usb");

        let graph = Graph::parse(DUMP).unwrap();
        assert_eq!(
            source_ports(&FakeRunner::new(), &graph, MONO),
            [format!("{MONO}:capture_MONO")]
        );
        // Monitor: der Sink liefert seine monitor_*-Ports, ohne dass jemand
        // einen Portnamen raten muss.
        let hdmi = "alsa_output.pci-0000_03_00.1.hdmi-stereo-extra2";
        assert_eq!(
            source_ports(&FakeRunner::new(), &graph, hdmi),
            [format!("{hdmi}:monitor_FL"), format!("{hdmi}:monitor_FR")]
        );
    }

    #[test]
    fn zulaessige_streifen() {
        assert!(strip_ports(1).is_ok());
        assert!(strip_ports(5).is_ok());
        assert!(strip_ports(0).is_err());
        assert!(strip_ports(6).is_err());
    }

    #[test]
    fn assign_legt_mono_auf_beide_seiten_und_loest_alte_links() {
        let pfad = tmp_path("assign");
        let _ = std::fs::remove_file(&pfad);
        let f = fake(DUMP);

        assign_mit(&f, &pfad, 1, Some(MONO)).unwrap();
        let aufrufe = f.lines();
        // Im Graphen hängt das Mikrofon schon an strip1_L (Link 501) – erst lösen.
        assert_eq!(aufrufe[0], "pw-dump");
        assert_eq!(aufrufe[1], format!("pw-link -d {MONO}:capture_MONO crosspoint:strip1_L"));
        assert_eq!(aufrufe[2], format!("pw-link {MONO}:capture_MONO crosspoint:strip1_L"));
        assert_eq!(aufrufe[3], format!("pw-link {MONO}:capture_MONO crosspoint:strip1_R"));
        assert_eq!(std::fs::read_to_string(&pfad).unwrap(), format!("1={MONO}\n"));
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn assign_trennt_auch_fremde_verbindungen() {
        let pfad = tmp_path("fremd");
        let _ = std::fs::remove_file(&pfad);
        // Ein VBAN-Port hängt an strip2_L – der muss weichen.
        let f = fake(
            r#"[
              {"id": 1, "type": "PipeWire:Interface:Node",
               "info": {"props": {"media.class": "Audio/Source", "node.name": "alsa_input.usb-mic",
                                  "node.description": "USB-Mikrofon"}}},
              {"id": 2, "type": "PipeWire:Interface:Node",
               "info": {"props": {"node.name": "crosspoint"}}},
              {"id": 3, "type": "PipeWire:Interface:Port",
               "info": {"direction": "output", "props": {"node.id": 1, "port.name": "capture_FL"}}},
              {"id": 4, "type": "PipeWire:Interface:Port",
               "info": {"direction": "output", "props": {"node.id": 1, "port.name": "capture_FR"}}},
              {"id": 5, "type": "PipeWire:Interface:Port",
               "info": {"direction": "input", "props": {"node.id": 2, "port.name": "strip2_L"}}},
              {"id": 6, "type": "PipeWire:Interface:Port",
               "info": {"direction": "input", "props": {"node.id": 2, "port.name": "strip2_R"}}},
              {"id": 7, "type": "PipeWire:Interface:Port",
               "info": {"direction": "output", "props": {"node.id": 2, "port.name": "vban_L"}}},
              {"id": 8, "type": "PipeWire:Interface:Link",
               "info": {"output-node-id": 2, "output-port-id": 7,
                        "input-node-id": 2, "input-port-id": 5, "state": "active"}}
            ]"#,
        );
        assign_mit(&f, &pfad, 2, Some("alsa_input.usb-mic")).unwrap();
        let zeilen = f.lines();
        assert_eq!(zeilen[1], "pw-link -d crosspoint:vban_L crosspoint:strip2_L", "{zeilen:?}");
        assert!(zeilen.contains(&"pw-link alsa_input.usb-mic:capture_FL crosspoint:strip2_L".to_string()));
        assert!(zeilen.contains(&"pw-link alsa_input.usb-mic:capture_FR crosspoint:strip2_R".to_string()));
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn assign_ohne_quelle_loest_nur_und_leert_die_datei() {
        let pfad = tmp_path("ohne");
        let _ = std::fs::remove_file(&pfad);
        assign_mit(&fake(DUMP), &pfad, 3, Some(MONO)).unwrap();
        assert!(std::fs::read_to_string(&pfad).unwrap().contains(MONO));

        let f = fake(DUMP);
        assign_mit(&f, &pfad, 3, None).unwrap();
        let aufrufe = f.lines();
        assert!(!aufrufe.iter().any(|z| z.contains("pw-link crosspoint")), "{aufrufe:?}");
        assert!(std::fs::read_to_string(&pfad).unwrap().is_empty(), "Eintrag entfernt");
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn zuordnung_speichert_nur_den_namen_und_liest_alte_monitornamen() {
        let pfad = tmp_path("mapping");
        let _ = std::fs::remove_file(&pfad);
        std::fs::write(&pfad, "1=alsa_output.hdmi.monitor\n7=zu_viel\n2=\n").unwrap();
        let m = mapping_mit(&pfad);
        assert_eq!(m[0].as_deref(), Some("alsa_output.hdmi"));
        assert!(m[1].is_none(), "leer bleibt leer");
        assert!(m.iter().skip(1).all(|e| e.is_none()), "unbekannte Streifen ignoriert");
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn status_json_zeigt_quellen_ports_und_echte_links() {
        let pfad = tmp_path("status");
        let _ = std::fs::remove_file(&pfad);
        assign_mit(&fake(DUMP), &pfad, 1, Some(MONO)).unwrap();

        let wert = crate::json::parse(&status_json_mit(&fake(DUMP), &pfad, false)).unwrap();
        assert_eq!(wert.get("error").unwrap(), &Value::Null);
        let quellen = wert.get("sources").unwrap().as_arr().unwrap();
        assert_eq!(quellen.len(), 1);
        assert_eq!(quellen[0].get("name").unwrap(), &Value::str(MONO));
        assert_eq!(quellen[0].get("monitor").unwrap(), &Value::Bool(false));
        assert_eq!(
            quellen[0].get("ports").unwrap().as_arr().unwrap(),
            &vec![Value::str(format!("{MONO}:capture_MONO"))]
        );

        let zuordnung = wert.get("assigned").unwrap().as_arr().unwrap();
        assert_eq!(zuordnung[0].get("strip").unwrap().as_f64(), Some(1.0));
        assert_eq!(zuordnung[0].get("source").unwrap(), &Value::str(MONO));
        assert_eq!(
            zuordnung[0].get("desc").unwrap(),
            &Value::str("Corsair HS80 RGB USB Gaming Headset Mono")
        );

        let links = wert.get("links").unwrap().as_arr().unwrap();
        assert_eq!(links.len(), 1, "{links:?}");
        assert_eq!(links[0].get("von").unwrap(), &Value::str(format!("{MONO}:capture_MONO")));
        assert_eq!(links[0].get("nach").unwrap(), &Value::str("crosspoint:strip1_L"));

        // Ohne pw-dump bleibt das JSON gültig und meldet den Fehler.
        let kaputt = FakeRunner::new().with_failure("pw-dump");
        let wert = crate::json::parse(&status_json_mit(&kaputt, &pfad, false)).unwrap();
        assert!(wert.get("error").unwrap() != &Value::Null);
        assert_eq!(wert.get("sources").unwrap().as_arr().unwrap().len(), 0);
        assert_eq!(wert.get("strips").unwrap().as_f64(), Some(5.0));
        let _ = std::fs::remove_file(&pfad);
    }
}
