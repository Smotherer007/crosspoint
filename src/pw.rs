//! PipeWire-Graph, direkt gelesen statt aus `pactl`-Text geraten.
//!
//! `pw-dump` liefert den vollständigen Graphen als JSON: Knoten (`Node`) mit
//! `info.props`, Ports (`Port`) mit Richtung und `node.id`, Verbindungen
//! (`Link`) mit Ausgangs- und Eingangsport. Damit sind Geräte Name, Anzeigename
//! (`node.description`) und Kanalzahl bekannt, ohne Beschreibungen an
//! Leerzeichen zu zerlegen oder Monitore zu raten.
//!
//! Aufgenommene Knotenklassen ([`GERAETEKLASSEN`]):
//!
//! | `media.class`          | Bedeutung                                   |
//! |------------------------|---------------------------------------------|
//! | `Audio/Sink`           | Ausgabegerät (Lautsprecher, Kopfhörer, HDMI) |
//! | `Audio/Source`         | Aufnahmegerät (Mikrofon, Line-In)            |
//! | `Stream/Output/Audio`  | Abspielstrom einer Anwendung                |
//!
//! Monitore sind in PipeWire **keine eigenen Knoten**, sondern Ausgangsports
//! (`monitor_FL`/`monitor_FR`) eines Sinks. Deshalb liefert [`Graph::ports`]
//! die echten Ports eines Knotens, und [`Graph::verbindungen`] die echten
//! Links – die Oberfläche muss nichts mehr annehmen.
//!
//! **Testbarkeit:** jeder Systemaufruf läuft über [`CommandRunner`]. Im Betrieb
//! ist das [`RealRunner`], in Tests der [`FakeRunner`], der nur mitschreibt.

use std::cell::RefCell;
use std::process::Command;

use crate::json::{self, Value};

/// Knotenklassen, die als Gerät bzw. Strom in Frage kommen.
pub const GERAETEKLASSEN: [&str; 3] = ["Audio/Sink", "Audio/Source", "Stream/Output/Audio"];

/// Programm, das den Graphen als JSON ausgibt.
pub const DUMP: &str = "pw-dump";

// ---------------------------------------------------------------------------
// Kommando-Ausführung
// ---------------------------------------------------------------------------

/// Führt ein Programm aus und liefert dessen Standardausgabe.
///
/// Nur so lässt sich die Geräteerkennung ohne Eingriff in den laufenden
/// PipeWire-Graphen testen.
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String>;
}

/// Echter Aufruf über `std::process::Command`.
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let out = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| format!("{program} nicht ausführbar: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "{program} {} -> Status {:?}: {}",
                args.join(" "),
                out.status.code(),
                stderr.trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Ein aufgezeichneter Aufruf, in Tests als eine Zeile lesbar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub program: String,
    pub args: Vec<String>,
}

impl Call {
    pub fn line(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }
}

/// Test-Doppel: schreibt Aufrufe mit und gibt vorbereitete Ausgaben zurück.
///
/// `outputs` liefert die Standardausgabe je Programm (erste Übereinstimmung),
/// `failures` enthält Teilstrings; passt einer auf `"<programm> <args…>"`,
/// schlägt der Aufruf fehl (so lässt sich prüfen, dass `pw-link -d`-Fehler
/// ignoriert werden).
#[derive(Default)]
pub struct FakeRunner {
    outputs: RefCell<Vec<(String, String)>>,
    failures: RefCell<Vec<String>>,
    calls: RefCell<Vec<Call>>,
}

impl FakeRunner {
    pub fn new() -> FakeRunner {
        FakeRunner::default()
    }

    /// Standardausgabe für ein Programm festlegen (Kette: erste Übereinstimmung).
    pub fn with_output(self, program: &str, stdout: &str) -> FakeRunner {
        self.outputs
            .borrow_mut()
            .push((program.to_string(), stdout.to_string()));
        self
    }

    /// Aufrufe, deren Zeile diesen Teilstring enthält, schlagen fehl.
    pub fn with_failure(self, pattern: &str) -> FakeRunner {
        self.failures.borrow_mut().push(pattern.to_string());
        self
    }

    /// Alle aufgezeichneten Aufrufe in Reihenfolge.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    /// Aufgezeichnete Aufrufe als Zeilen, z. B. `pw-link -d vm8:busA1_L …`.
    pub fn lines(&self) -> Vec<String> {
        self.calls.borrow().iter().map(Call::line).collect()
    }

    /// Nur die Aufrufe eines Programms.
    pub fn calls_of(&self, program: &str) -> Vec<Call> {
        self.calls
            .borrow()
            .iter()
            .filter(|c| c.program == program)
            .cloned()
            .collect()
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let call = Call {
            program: program.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
        };
        let line = call.line();
        self.calls.borrow_mut().push(call);
        if self.failures.borrow().iter().any(|p| line.contains(p.as_str())) {
            return Err(format!("{line} -> simulierter Fehler"));
        }
        let outputs = self.outputs.borrow();
        Ok(outputs
            .iter()
            .find(|(prog, _)| prog == program)
            .map(|(_, out)| out.clone())
            .unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Datenmodell
// ---------------------------------------------------------------------------

/// Datenrichtung eines Ports, so wie `pw-dump` sie nennt (`info.direction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Richtung {
    /// Daten fließen in den Knoten hinein (Sink-Eingänge, Mixer-Eingänge).
    In,
    /// Daten fließen aus dem Knoten heraus (Quell-Ausgänge, Monitor-Ports).
    Out,
}

impl Richtung {
    fn aus_text(text: &str) -> Option<Richtung> {
        match text {
            "input" => Some(Richtung::In),
            "output" => Some(Richtung::Out),
            _ => None,
        }
    }
}

/// Ein PipeWire-Knoten aus einer der [`GERAETEKLASSEN`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Objekt-ID im Graphen (`id` im Dump) – stabil nur bis zum Neustart.
    pub id: u32,
    /// Technischer Name (`node.name`), z. B. `alsa_output.usb-Corsair_…`.
    /// Nur dieser Name wird gespeichert.
    pub name: String,
    /// Anzeigename (`node.description`), vollständig mit Leerzeichen.
    pub description: String,
    /// Kurzname (`node.nick`), oft leer.
    pub nick: String,
    /// `media.class`, z. B. `Audio/Sink`.
    pub media_class: String,
    /// Monitor eines Sinks (Name endet auf `.monitor` oder `stream.monitor`).
    pub ist_monitor: bool,
}

impl Node {
    /// Beschriftung für die Oberfläche: Beschreibung, sonst Kurzname, sonst Name.
    pub fn anzeige(&self) -> &str {
        if !self.description.is_empty() {
            &self.description
        } else if !self.nick.is_empty() {
            &self.nick
        } else {
            &self.name
        }
    }

    pub fn ist_ausgabegeraet(&self) -> bool {
        self.media_class == "Audio/Sink"
    }

    pub fn ist_eingabegeraet(&self) -> bool {
        self.media_class == "Audio/Source"
    }

    /// Abspielstrom einer Anwendung (`Stream/Output/Audio`), kein Gerät.
    pub fn ist_strom(&self) -> bool {
        self.media_class == "Stream/Output/Audio"
    }

    /// Gehört der Knoten zu einer der [`GERAETEKLASSEN`]?
    pub fn ist_geraet(&self) -> bool {
        GERAETEKLASSEN.contains(&self.media_class.as_str())
    }
}

/// Ein Port eines Knotens (`PipeWire:Interface:Port`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Port {
    pub id: u32,
    /// Knoten, an dem der Port hängt (`props.node.id`).
    pub node_id: u32,
    /// Kurzname (`props.port.name`), z. B. `playback_FL` oder `capture_MONO`.
    pub name: String,
    pub richtung: Richtung,
}

/// Eine Verbindung zwischen zwei Ports (`PipeWire:Interface:Link`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub id: u32,
    pub ausgang_node: u32,
    pub ausgang_port: u32,
    pub eingang_node: u32,
    pub eingang_port: u32,
    /// `active`, `passive`, `error`, …
    pub zustand: String,
}

/// Eine Verbindung in lesbarer Form: `"vm8:busA1_L" -> "alsa_output…:playback_FL"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verbindung {
    pub von: String,
    pub nach: String,
    pub zustand: String,
}

/// Der aufgenommene Graph: Knoten, Ports, Links.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub ports: Vec<Port>,
    pub links: Vec<Link>,
}

impl Graph {
    /// Liest einen `pw-dump`-Auszug. Objekte anderer Typen (Module, Clients,
    /// Devices, Metadata) werden übergangen; Knoten aller Klassen bleiben
    /// erhalten, damit sich auch Links des Mixers (`vm8`, ohne `media.class`)
    /// auflösen lassen.
    pub fn parse(text: &str) -> Result<Graph, String> {
        let wert = json::parse(text)?;
        let objekte = wert
            .as_arr()
            .ok_or_else(|| "pw-dump liefert kein JSON-Array".to_string())?;

        let mut graph = Graph::default();
        for obj in objekte {
            match text_wert(obj.get("type")).as_str() {
                "PipeWire:Interface:Node" => {
                    if let Some(n) = knoten_aus(obj) {
                        graph.nodes.push(n);
                    }
                }
                "PipeWire:Interface:Port" => {
                    if let Some(p) = port_aus(obj) {
                        graph.ports.push(p);
                    }
                }
                "PipeWire:Interface:Link" => {
                    if let Some(l) = link_aus(obj) {
                        graph.links.push(l);
                    }
                }
                _ => {}
            }
        }
        Ok(graph)
    }

    pub fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Knoten nach `node.name`. Ein altes `…​.monitor` aus PulseAudio-Zeiten
    /// wird auf den Sink davor abgebildet (Monitor ist ein Port, kein Knoten).
    pub fn knoten(&self, name: &str) -> Option<&Node> {
        let name = name.trim();
        if let Some(n) = self.node_by_name(name) {
            return Some(n);
        }
        self.node_by_name(name.strip_suffix(".monitor").unwrap_or(name))
    }

    pub fn node_by_name(&self, name: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.name == name)
    }

    /// Alle Knoten der [`GERAETEKLASSEN`], alphabetisch – also das, was die
    /// Oberfläche als Geräte bzw. Ströme anbietet.
    pub fn geraete(&self) -> Vec<&Node> {
        self.nach_anzeige(|n| n.ist_geraet())
    }

    /// Ausgabegeräte (`Audio/Sink`) ohne Monitor-Knoten, alphabetisch.
    pub fn sinks(&self) -> Vec<&Node> {
        self.nach_anzeige(|n| n.ist_ausgabegeraet() && !n.ist_monitor)
    }

    /// Aufnahmegeräte (`Audio/Source`) ohne Monitor-Knoten, alphabetisch.
    pub fn sources(&self) -> Vec<&Node> {
        self.nach_anzeige(|n| n.ist_eingabegeraet() && !n.ist_monitor)
    }

    /// Abspielströme von Anwendungen, alphabetisch.
    pub fn streams(&self) -> Vec<&Node> {
        self.nach_anzeige(|n| n.ist_strom())
    }

    fn nach_anzeige(&self, passt: impl Fn(&Node) -> bool) -> Vec<&Node> {
        let mut liste: Vec<&Node> = self.nodes.iter().filter(|n| passt(n)).collect();
        liste.sort_by(|a, b| a.anzeige().to_lowercase().cmp(&b.anzeige().to_lowercase()));
        liste
    }

    pub fn port(&self, id: u32) -> Option<&Port> {
        self.ports.iter().find(|p| p.id == id)
    }

    /// Alle Ports eines Knotens, nach Name sortiert (dadurch steht `_FL` vor `_FR`).
    pub fn ports(&self, node_id: u32) -> Vec<&Port> {
        let mut liste: Vec<&Port> = self.ports.iter().filter(|p| p.node_id == node_id).collect();
        liste.sort_by(|a, b| a.name.cmp(&b.name));
        liste
    }

    /// Ports eines Knotens in einer Richtung, nach Name sortiert.
    pub fn ports_in(&self, node_id: u32, richtung: Richtung) -> Vec<&Port> {
        self.ports(node_id)
            .into_iter()
            .filter(|p| p.richtung == richtung)
            .collect()
    }

    /// `"<node.name>:<port.name>"` – genau die Schreibweise, die `pw-link` will.
    pub fn pfad(&self, port_id: u32) -> Option<String> {
        let port = self.port(port_id)?;
        let node = self.node(port.node_id)?;
        Some(format!("{}:{}", node.name, port.name))
    }

    /// Pfade aller Ports eines Knotens in einer Richtung.
    pub fn pfade(&self, node_id: u32, richtung: Richtung) -> Vec<String> {
        let Some(node) = self.node(node_id) else {
            return Vec::new();
        };
        self.ports_in(node_id, richtung)
            .into_iter()
            .map(|p| format!("{}:{}", node.name, p.name))
            .collect()
    }

    /// Alle Links in lesbarer Form. Links zu Knoten, die nicht im Dump stehen,
    /// werden übersprungen (sie wären ohnehin nicht adressierbar).
    pub fn verbindungen(&self) -> Vec<Verbindung> {
        self.links
            .iter()
            .filter_map(|l| {
                Some(Verbindung {
                    von: self.pfad(l.ausgang_port)?,
                    nach: self.pfad(l.eingang_port)?,
                    zustand: l.zustand.clone(),
                })
            })
            .collect()
    }

    /// Ist `von` gerade mit `nach` verbunden? Beide als `"node:port"`.
    pub fn ist_verbunden(&self, von: &str, nach: &str) -> bool {
        self.verbindungen()
            .iter()
            .any(|v| v.von == von && v.nach == nach)
    }

    /// Alle Verbindungen, an denen einer der Ports hängt – in beiden Richtungen.
    pub fn offene_links(&self, ports: &[String]) -> Vec<Verbindung> {
        self.verbindungen()
            .into_iter()
            .filter(|v| ports.iter().any(|p| *p == v.von || *p == v.nach))
            .collect()
    }

    /// Haben die Ports des Sinks Monitor-Ausgänge? Nur dann lohnt es, ihn als
    /// Monitor-Quelle anzubieten.
    pub fn hat_monitor(&self, node_id: u32) -> bool {
        self.ports_in(node_id, Richtung::Out)
            .iter()
            .any(|p| p.name.starts_with("monitor_"))
    }
}

// ---------------------------------------------------------------------------
// Systemzugriff
// ---------------------------------------------------------------------------

/// Liest den Graphen über `pw-dump`.
pub fn dump(runner: &dyn CommandRunner) -> Result<Graph, String> {
    let text = runner
        .run(DUMP, &[])
        .map_err(|e| format!("{DUMP} fehlgeschlagen: {e}"))?;
    Graph::parse(&text).map_err(|e| format!("{DUMP} nicht lesbar: {e}"))
}

/// Ports eines Knotens über `pw-link -o` (Ausgänge) bzw. `-i` (Eingänge).
///
/// Rückfall, wenn im Dump keine Ports stehen (z. B. weil `pw-dump` nur einen
/// Teil des Graphen sieht) oder für Zusatzprüfungen.
pub fn ports_ueber_pw_link(
    runner: &dyn CommandRunner,
    node: &str,
    richtung: Richtung,
) -> Vec<String> {
    let arg = match richtung {
        Richtung::In => "-i",
        Richtung::Out => "-o",
    };
    let text = runner.run("pw-link", &[arg]).unwrap_or_default();
    let prefix = format!("{}:", node.trim_end_matches(".monitor"));
    let mut ports: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|zeile| zeile.starts_with(&prefix))
        .map(|zeile| zeile[prefix.len()..].to_string())
        .filter(|p| !p.is_empty())
        .collect();
    ports.sort();
    ports.dedup();
    ports
}

/// Ports eines Knotens als `"node:port"`: erst aus dem Graphen, sonst über
/// `pw-link -o/-i`.
pub fn ports(
    runner: &dyn CommandRunner,
    graph: &Graph,
    node: &str,
    richtung: Richtung,
) -> Vec<String> {
    if let Some(n) = graph.knoten(node) {
        let aus_graph = graph.pfade(n.id, richtung);
        if !aus_graph.is_empty() {
            return aus_graph;
        }
    }
    let basis = node.trim_end_matches(".monitor");
    ports_ueber_pw_link(runner, basis, richtung)
        .into_iter()
        .map(|p| format!("{basis}:{p}"))
        .collect()
}

/// Wählt aus einer Portliste die beiden Kanalpfade für links/rechts.
///
/// Vorrang hat ein Paar `_FL`/`_FR` (so heißen `capture_*`, `playback_*` und
/// `monitor_*`). Gibt es nur einen Port – etwa `capture_MONO` bei einem
/// Mono-Mikrofon –, wird dieser Pfad einmal zurückgegeben und vom Aufrufer auf
/// beide Seiten gelegt. Sonst gelten die ersten beiden Ports als L und R.
pub fn kanalpaar(ports: &[String]) -> Vec<String> {
    let finde = |suffix: &str| ports.iter().find(|p| p.ends_with(suffix)).cloned();
    if let (Some(fl), Some(fr)) = (finde("_FL"), finde("_FR")) {
        return vec![fl, fr];
    }
    match ports.len() {
        0 => Vec::new(),
        1 => vec![ports[0].clone()],
        _ => vec![ports[0].clone(), ports[1].clone()],
    }
}

// ---------------------------------------------------------------------------
// JSON-Zugriff
// ---------------------------------------------------------------------------

fn text_wert(wert: Option<&Value>) -> String {
    match wert {
        Some(Value::Str(s)) => s.clone(),
        _ => String::new(),
    }
}

fn zahl_wert(wert: Option<&Value>) -> Option<u32> {
    wert.and_then(|v| v.as_f64()).map(|f| f as u32)
}

fn knoten_aus(obj: &Value) -> Option<Node> {
    let props = obj.get("info")?.get("props")?;
    let name = text_wert(props.get("node.name"));
    if name.is_empty() {
        return None;
    }
    let media_class = text_wert(props.get("media.class"));
    let ist_monitor = name.ends_with(".monitor")
        || props
            .get("stream.monitor")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    Some(Node {
        id: zahl_wert(obj.get("id"))?,
        description: text_wert(props.get("node.description")),
        nick: text_wert(props.get("node.nick")),
        name,
        media_class,
        ist_monitor,
    })
}

fn port_aus(obj: &Value) -> Option<Port> {
    let info = obj.get("info")?;
    let props = info.get("props")?;
    let name = text_wert(props.get("port.name"));
    if name.is_empty() {
        return None;
    }
    Some(Port {
        id: zahl_wert(obj.get("id"))?,
        node_id: zahl_wert(props.get("node.id"))?,
        name,
        richtung: Richtung::aus_text(&text_wert(info.get("direction")))?,
    })
}

fn link_aus(obj: &Value) -> Option<Link> {
    let info = obj.get("info")?;
    Some(Link {
        id: zahl_wert(obj.get("id"))?,
        ausgang_node: zahl_wert(info.get("output-node-id"))?,
        ausgang_port: zahl_wert(info.get("output-port-id"))?,
        eingang_node: zahl_wert(info.get("input-node-id"))?,
        eingang_port: zahl_wert(info.get("input-port-id"))?,
        zustand: text_wert(info.get("state")),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Auszug aus `pw-dump` in der Form, die der Rechner liefert: zwei Sinks
    /// (einer davon der eigene `vm8_out_B1`), ein Mono-Mikrofon, ein
    /// Abspielstrom, ein Monitor-Paar, zwei Links.
    pub const DUMP: &str = r#"[
      {"id": 30, "type": "PipeWire:Interface:Node",
       "info": {"props": {"node.name": "Freewheel-Driver"}}},
      {"id": 443, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Sink",
                          "node.name": "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo",
                          "node.description": "Corsair HS80 RGB USB Gaming Headset Analoges Stereo",
                          "node.nick": "Corsair HS80 RGB USB Gaming Hea"}}},
      {"id": 49, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Sink",
                          "node.name": "alsa_output.pci-0000_03_00.1.hdmi-stereo-extra2",
                          "node.description": "Navi 48 HDMI/DP Audio Controller Digital Stereo (HDMI 3) [AG326UD]"}}},
      {"id": 269, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Source",
                          "node.name": "alsa_input.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.mono-fallback",
                          "node.description": "Corsair HS80 RGB USB Gaming Headset Mono"}}},
      {"id": 239, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Audio/Sink", "node.name": "vm8_out_B1",
                          "node.description": "VM8-Output-B1"}}},
      {"id": 457, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Stream/Output/Audio", "node.name": "Chromium"}}},
      {"id": 297, "type": "PipeWire:Interface:Node",
       "info": {"props": {"node.name": "vm8", "node.description": "vm8"}}},
      {"id": 209, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 443, "port.name": "playback_FL"}}},
      {"id": 226, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 443, "port.name": "playback_FR"}}},
      {"id": 296, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 443, "port.name": "monitor_FL"}}},
      {"id": 74, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 443, "port.name": "monitor_FR"}}},
      {"id": 300, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 49, "port.name": "playback_FL"}}},
      {"id": 301, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 49, "port.name": "playback_FR"}}},
      {"id": 302, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 49, "port.name": "monitor_FL"}}},
      {"id": 303, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 49, "port.name": "monitor_FR"}}},
      {"id": 310, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 239, "port.name": "playback_FL"}}},
      {"id": 311, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 239, "port.name": "playback_FR"}}},
      {"id": 312, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 239, "port.name": "monitor_FL"}}},
      {"id": 313, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 239, "port.name": "monitor_FR"}}},
      {"id": 76, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 269, "port.name": "capture_MONO"}}},
      {"id": 70, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 297, "port.name": "busA1_L"}}},
      {"id": 71, "type": "PipeWire:Interface:Port",
       "info": {"direction": "output", "props": {"node.id": 297, "port.name": "busA1_R"}}},
      {"id": 427, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 297, "port.name": "strip1_L"}}},
      {"id": 271, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 297, "port.name": "strip1_R"}}},
      {"id": 500, "type": "PipeWire:Interface:Link",
       "info": {"output-node-id": 297, "output-port-id": 70,
                "input-node-id": 443, "input-port-id": 209, "state": "active"}},
      {"id": 501, "type": "PipeWire:Interface:Link",
       "info": {"output-node-id": 269, "output-port-id": 76,
                "input-node-id": 297, "input-port-id": 427, "state": "active"}}
    ]"#;

    fn graph() -> Graph {
        Graph::parse(DUMP).expect("Dump lesbar")
    }

    #[test]
    fn knoten_werden_nach_klasse_gefiltert() {
        let g = graph();
        // Alle Knoten bleiben für die Link-Auflösung erhalten …
        assert_eq!(g.nodes.len(), 7, "Driver und vm8 bleiben: {:?}", g.nodes);
        // … als Geräte gelten nur die drei Klassen.
        assert_eq!(g.geraete().len(), 5);
        assert_eq!(g.sinks().len(), 3);
        assert_eq!(g.sources().len(), 1);
        assert_eq!(g.streams().len(), 1);
        assert!(g.node_by_name("vm8").unwrap().media_class.is_empty());
        assert!(!g.node_by_name("vm8").unwrap().ist_geraet());
        assert_eq!(g.streams()[0].name, "Chromium");
        assert!(g.streams()[0].ist_strom());
        assert!(g.sinks()[0].ist_ausgabegeraet());
        assert!(g.sources()[0].ist_eingabegeraet());
    }

    #[test]
    fn beschreibung_bleibt_vollstaendig_mit_leerzeichen_und_umlauten() {
        let g = graph();
        let corsair = g
            .node_by_name("alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo")
            .expect("Corsair");
        assert_eq!(corsair.description, "Corsair HS80 RGB USB Gaming Headset Analoges Stereo");
        assert_eq!(corsair.anzeige(), corsair.description);
        assert_eq!(corsair.nick, "Corsair HS80 RGB USB Gaming Hea");
        assert_eq!(corsair.id, 443);
        assert_eq!(corsair.media_class, "Audio/Sink");
        assert!(!corsair.ist_monitor);

        // Ohne Beschreibung gilt der Kurzname, ohne alles der technische Name.
        let chromium = g.streams()[0];
        assert_eq!(chromium.anzeige(), "Chromium");
    }

    #[test]
    fn ports_richtung_und_pfad() {
        let g = graph();
        let corsair = g.node_by_name("alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo").unwrap();
        let namen: Vec<&str> = g.ports(corsair.id).iter().map(|p| p.name.as_str()).collect();
        assert_eq!(namen, ["monitor_FL", "monitor_FR", "playback_FL", "playback_FR"]);

        assert_eq!(
            g.pfade(corsair.id, Richtung::In),
            [
                "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo:playback_FL",
                "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo:playback_FR",
            ]
        );
        assert!(g.hat_monitor(corsair.id));
        // Ein Mikrofon hat nur einen Ausgang – capture_MONO.
        let mic = g.sources()[0];
        assert_eq!(g.pfade(mic.id, Richtung::Out), ["alsa_input.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.mono-fallback:capture_MONO"]);
        assert!(!g.hat_monitor(mic.id));
        assert_eq!(g.pfad(209).unwrap(), "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo:playback_FL");
        assert!(g.pfad(9999).is_none());
    }

    #[test]
    fn links_werden_als_verbindungen_gelesen() {
        let g = graph();
        let v = g.verbindungen();
        assert_eq!(v.len(), 2);
        assert!(g.ist_verbunden("vm8:busA1_L", "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo:playback_FL"));
        assert_eq!(v[0].zustand, "active");
        assert!(!g.ist_verbunden("vm8:busA1_L", "alsa_output.pci-0000_03_00.1.hdmi-stereo-extra2:playback_FL"));

        let offen = g.offene_links(&["vm8:busA1_R".to_string()]);
        assert!(offen.is_empty());
        let offen = g.offene_links(&["vm8:busA1_L".to_string()]);
        assert_eq!(offen.len(), 1);
        assert_eq!(offen[0].von, "vm8:busA1_L");
    }

    #[test]
    fn monitor_wird_am_namen_und_am_property_erkannt() {
        let text = r#"[
          {"id": 1, "type": "PipeWire:Interface:Node",
           "info": {"props": {"media.class": "Audio/Sink", "node.name": "alsa_output.hdmi.monitor",
                              "node.description": "Monitor"}}},
          {"id": 2, "type": "PipeWire:Interface:Node",
           "info": {"props": {"media.class": "Audio/Sink", "node.name": "alsa_output.usb",
                              "node.description": "Kopfhörer", "stream.monitor": true}}}
        ]"#;
        let g = Graph::parse(text).unwrap();
        assert!(g.nodes.iter().all(|n| n.ist_monitor));
        assert!(g.sinks().is_empty(), "Monitore sind keine Geräte");
        // Alte Zuordnung `…​.monitor` findet den Knoten dahinter.
        assert_eq!(g.knoten("alsa_output.hdmi.monitor").unwrap().name, "alsa_output.hdmi.monitor");
    }

    #[test]
    fn dump_ueber_runner_und_fehlerfall() {
        let fake = FakeRunner::new().with_output("pw-dump", DUMP);
        let g = dump(&fake).unwrap();
        assert_eq!(g, graph());
        assert_eq!(fake.lines(), vec!["pw-dump".to_string()]);

        let kaputt = FakeRunner::new().with_output("pw-dump", "kein json");
        assert!(dump(&kaputt).is_err());

        let fehlt = FakeRunner::new().with_failure("pw-dump");
        assert!(dump(&fehlt).is_err());
    }

    #[test]
    fn ports_kommen_aus_dem_graphen_sonst_aus_pw_link() {
        let g = graph();
        let fake = FakeRunner::new();
        let mic = "alsa_input.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.mono-fallback";
        assert_eq!(ports(&fake, &g, mic, Richtung::Out), [format!("{mic}:capture_MONO")]);
        assert!(fake.calls().is_empty(), "Graph reicht, kein pw-link nötig");

        // Knoten nicht im Dump -> pw-link -o wird gefragt.
        let fake = FakeRunner::new().with_output(
            "pw-link",
            "alsa_input.neu:capture_FL\nalsa_input.neu:capture_FR\nalsa_output.x:playback_FL\n",
        );
        assert_eq!(
            ports(&fake, &g, "alsa_input.neu", Richtung::Out),
            ["alsa_input.neu:capture_FL".to_string(), "alsa_input.neu:capture_FR".to_string()]
        );
        assert_eq!(fake.lines(), vec!["pw-link -o".to_string()]);
        assert_eq!(ports_ueber_pw_link(&fake, "alsa_output.gibtsnicht", Richtung::In), Vec::<String>::new());
    }

    #[test]
    fn kanalpaar_bevorzugt_fl_fr_dann_mono() {
        let p = |s: &str| s.to_string();
        assert_eq!(
            kanalpaar(&[p("n:playback_FL"), p("n:playback_FR")]),
            [p("n:playback_FL"), p("n:playback_FR")]
        );
        assert_eq!(kanalpaar(&[p("n:capture_MONO")]), [p("n:capture_MONO")]);
        assert_eq!(kanalpaar(&[p("n:capture_1"), p("n:capture_2"), p("n:capture_3")]),
                   [p("n:capture_1"), p("n:capture_2")]);
        assert!(kanalpaar(&[]).is_empty());
    }
}
