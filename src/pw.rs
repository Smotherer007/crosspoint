//! PipeWire-Graph über die offiziellen Bindings (`pipewire`-Crate).
//!
//! Früher wurden `pw-dump` und `pw-link` aufgerufen und ihre Textausgaben
//! gelesen. Jetzt läuft im Prozess ein eigener PipeWire-Client:
//!
//! ```text
//! Registry-Thread (eigene tokio-Runtime)
//!   ├─ liest die Registry: Knoten, Ports, Links samt Properties
//!   ├─ hält den Zustand in einem Bestand (RwLock)
//!   └─ führt Verbinden/Trennen aus (link-factory, registry.destroy)
//!
//! HTTP-Schicht (synchron)
//!   └─ liest nur den Snapshot, blockiert nie auf PipeWire
//! ```
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
//! die echten Ports eines Knotens und [`Graph::verbindungen`] die echten
//! Links – die Oberfläche muss nichts mehr annehmen.
//!
//! **Testbarkeit:** alles, was nach außen geht, läuft über [`Zugriff`]. Im
//! Betrieb ist das [`PipeWire`], in Tests der `Testzugriff` aus
//! [`tests`], der nur mitschreibt.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::types::ObjectType;

/// Knotenklassen, die als Gerät bzw. Strom in Frage kommen.
pub const GERAETEKLASSEN: [&str; 3] = ["Audio/Sink", "Audio/Source", "Stream/Output/Audio"];

/// Wie lange die HTTP-Schicht höchstens auf eine Antwort des Registry-Threads
/// wartet. Muss größer sein als die Fristen im Thread selbst
/// ([`SPERRE`] + [`SETTLE`]), sonst meldet der Aufrufer einen Fehler, während
/// der Link in Wahrheit noch angelegt wird.
const ANTWORT: Duration = Duration::from_millis(2500);

/// Frist für einen Sync-Durchlauf („alle bisherigen Ereignisse zugestellt“).
const SPERRE: Duration = Duration::from_millis(1500);

/// Wie lange nach dem Anlegen auf den ausgehandelten Link-Zustand gewartet wird.
const SETTLE: Duration = Duration::from_millis(300);

/// Takt des Ereignis-Loops im Registry-Thread.
const TAKT: Duration = Duration::from_millis(5);

/// Wartezeit zwischen zwei Verbindungsversuchen.
const NEUVERSUCH: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Datenmodell
// ---------------------------------------------------------------------------

/// Datenrichtung eines Ports (`port.direction`).
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
            "in" | "input" => Some(Richtung::In),
            "out" | "output" => Some(Richtung::Out),
            _ => None,
        }
    }
}

/// Ein PipeWire-Knoten aus einer der [`GERAETEKLASSEN`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Objekt-ID im Graphen – stabil nur bis zum Neustart.
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

/// Ein Port eines Knotens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Port {
    pub id: u32,
    /// Knoten, an dem der Port hängt (`node.id`).
    pub node_id: u32,
    /// Kurzname (`port.name`), z. B. `playback_FL` oder `capture_MONO`.
    pub name: String,
    pub richtung: Richtung,
}

/// Eine Verbindung zwischen zwei Ports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub id: u32,
    pub ausgang_node: u32,
    pub ausgang_port: u32,
    pub eingang_node: u32,
    pub eingang_port: u32,
    /// `active`, `paused`, `error`, …
    pub zustand: String,
}

/// Eine Verbindung in lesbarer Form: `"crosspoint:busA1_L" -> "alsa_output…:playback_FL"`.
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
    pub fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Knoten nach `node.name`. Ein altes `….monitor` aus PulseAudio-Zeiten
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

    /// `"<node.name>:<port.name>"` – die Schreibweise, in der Ports überall
    /// sonst adressiert werden.
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

    /// Alle Links in lesbarer Form. Links zu Knoten, die nicht im Graphen
    /// stehen, werden übersprungen (sie wären ohnehin nicht adressierbar).
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

    /// Sortiert die Links nach ihren Portpfaden.
    ///
    /// Die Objekt-IDs vergibt PipeWire bei jedem Start neu; eine Liste in
    /// ID-Reihenfolge wäre also von Lauf zu Lauf verschieden. Die Oberfläche
    /// und die API sortieren deshalb nach Namen.
    fn sortiere_links(&mut self) {
        let links = std::mem::take(&mut self.links);
        let mut paare: Vec<(String, String, Link)> = links
            .into_iter()
            .map(|l| {
                (
                    self.pfad(l.ausgang_port).unwrap_or_default(),
                    self.pfad(l.eingang_port).unwrap_or_default(),
                    l,
                )
            })
            .collect();
        paare.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        self.links = paare.into_iter().map(|(_, _, l)| l).collect();
    }

    /// Haben die Ports des Sinks Monitor-Ausgänge? Nur dann lohnt es, ihn als
    /// Monitor-Quelle anzubieten.
    pub fn hat_monitor(&self, node_id: u32) -> bool {
        self.ports_in(node_id, Richtung::Out)
            .iter()
            .any(|p| p.name.starts_with("monitor_"))
    }
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
// Zugriff: das, was früher `pw-dump` und `pw-link` waren
// ---------------------------------------------------------------------------

/// Alles, was devices/inputs vom PipeWire-Graphen brauchen.
///
/// Zwei Umsetzungen: [`PipeWire`] im Betrieb, `Testzugriff` in Tests.
pub trait Zugriff {
    /// Der aktuelle Graph. Ist PipeWire nicht erreichbar, kommt ein Fehler –
    /// die Aufrufer machen daraus ein leeres Geräteangebot plus Meldung.
    fn graph(&self) -> Result<Graph, String>;

    /// Der Graph, oder ein leerer, wenn PipeWire nicht erreichbar ist.
    fn snapshot(&self) -> Graph {
        self.graph().unwrap_or_default()
    }

    /// Verbindet zwei Ports (`"node:port"`), Ausgang auf Eingang.
    fn verbinden(&self, von: &str, nach: &str) -> Result<(), String>;

    /// Löst die Verbindung zwischen zwei Ports (`"node:port"`). Gibt es keine,
    /// passiert nichts (idempotent).
    fn trennen(&self, von: &str, nach: &str) -> Result<(), String>;
}

/// Der echte PipeWire-Zugriff über die Crate (ein Client je Prozess).
#[derive(Clone, Copy, Debug, Default)]
pub struct PipeWire;

impl PipeWire {
    pub fn new() -> PipeWire {
        PipeWire
    }

    /// Der Graph-Client, beim ersten Aufruf gestartet.
    pub fn client() -> &'static PwClient {
        PwClient::global()
    }
}

impl Zugriff for PipeWire {
    fn graph(&self) -> Result<Graph, String> {
        PwClient::global().graph()
    }

    fn snapshot(&self) -> Graph {
        PwClient::global().snapshot()
    }

    fn verbinden(&self, von: &str, nach: &str) -> Result<(), String> {
        PwClient::global().verbinden(von, nach)
    }

    fn trennen(&self, von: &str, nach: &str) -> Result<(), String> {
        PwClient::global().trennen(von, nach)
    }
}

// ---------------------------------------------------------------------------
// Registry-Bestand
// ---------------------------------------------------------------------------

/// Objektart, soweit sie uns interessiert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Art {
    Node,
    Port,
    Link,
}

fn art_aus(typ: &ObjectType) -> Option<Art> {
    match typ {
        ObjectType::Node => Some(Art::Node),
        ObjectType::Port => Some(Art::Port),
        ObjectType::Link => Some(Art::Link),
        _ => None,
    }
}

/// Ein Registry-Objekt mit den Properties, die PipeWire dazu liefert.
#[derive(Clone, Debug)]
struct Roh {
    art: Art,
    props: BTreeMap<String, String>,
}

impl Roh {
    fn text(&self, schluessel: &str) -> String {
        self.props.get(schluessel).cloned().unwrap_or_default()
    }

    fn zahl(&self, schluessel: &str) -> Option<u32> {
        self.props.get(schluessel)?.parse().ok()
    }

    fn flagge(&self, schluessel: &str) -> bool {
        matches!(self.props.get(schluessel).map(String::as_str), Some("true" | "1"))
    }
}

/// Alles, was der Registry-Client gerade sieht.
#[derive(Default)]
struct Bestand {
    objekte: BTreeMap<u32, Roh>,
    /// Zustand je Link (`active`, `paused`, …). Steht nur in den Link-Infos,
    /// nicht in den Properties – deshalb ein eigener Speicher.
    link_zustand: BTreeMap<u32, String>,
}

/// Ein Port, wie ihn [`Zugriff::verbinden`] braucht.
#[derive(Clone, Copy, Debug)]
struct Ziel {
    node: u32,
    port: u32,
    richtung: Richtung,
}

impl Bestand {
    fn leeren(&mut self) {
        self.objekte.clear();
        self.link_zustand.clear();
    }

    fn einfuegen(&mut self, id: u32, art: Art, props: BTreeMap<String, String>) {
        self.objekte.insert(id, Roh { art, props });
    }

    fn entfernen(&mut self, id: u32) {
        self.objekte.remove(&id);
        self.link_zustand.remove(&id);
    }

    fn setze_link_zustand(&mut self, id: u32, zustand: &str) {
        self.link_zustand.insert(id, zustand.to_string());
    }

    /// Baut den [`Graph`] aus dem Bestand – Knoten, Ports und Links, jeweils
    /// mit den Properties, die PipeWire selbst nennt.
    fn graph(&self) -> Graph {
        let mut graph = Graph::default();
        for (id, roh) in &self.objekte {
            match roh.art {
                Art::Node => {
                    if let Some(n) = knoten_aus(*id, roh) {
                        graph.nodes.push(n);
                    }
                }
                Art::Port => {
                    if let Some(p) = port_aus(*id, roh) {
                        graph.ports.push(p);
                    }
                }
                Art::Link => {
                    if let Some(l) = link_aus(*id, roh, self.link_zustand.get(id)) {
                        graph.links.push(l);
                    }
                }
            }
        }
        graph.sortiere_links();
        graph
    }

    /// Zerlegt `"node:port"` in Knoten-ID und Portname.
    ///
    /// Der längste passende Knotenname gewinnt, denn Knotennamen dürfen selbst
    /// Doppelpunkte enthalten (z. B. `Midi Through` mit Ports wie
    /// `Midi Through: Port-0 (playback)`).
    fn zerlege<'a>(&self, pfad: &'a str) -> Option<(u32, &'a str)> {
        let mut beste: Option<(usize, u32)> = None;
        for (id, roh) in &self.objekte {
            if roh.art != Art::Node {
                continue;
            }
            let Some(name) = roh.props.get("node.name") else {
                continue;
            };
            let Some(rest) = pfad.strip_prefix(name.as_str()).and_then(|r| r.strip_prefix(':')) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            if beste.map_or(true, |(laenge, _)| name.len() > laenge) {
                beste = Some((name.len(), *id));
            }
        }
        let (laenge, id) = beste?;
        Some((id, &pfad[laenge + 1..]))
    }

    /// Port hinter `"node:port"`, mit Richtung.
    fn port_pfad(&self, pfad: &str) -> Option<Ziel> {
        let (node, portname) = self.zerlege(pfad)?;
        self.objekte.iter().find_map(|(id, roh)| {
            if roh.art != Art::Port || roh.zahl("node.id") != Some(node) {
                return None;
            }
            if roh.props.get("port.name").map(String::as_str) != Some(portname) {
                return None;
            }
            Some(Ziel {
                node,
                port: *id,
                richtung: Richtung::aus_text(&roh.text("port.direction"))?,
            })
        })
    }

    /// Hat der Link zwischen zwei Portpfaden einen ausgehandelten Zustand?
    ///
    /// Frisch angelegte Links sind kurz `init`/`negotiating`/`allocating`.
    /// Erst `active`, `paused`, `error` oder `unlinked` ist der Stand, den auch
    /// ein `pw-dump` zeigen würde.
    fn hat_link_zustand(&self, von: &str, nach: &str) -> bool {
        match self.link_id(von, nach) {
            Some(id) => match self.link_zustand.get(&id).map(String::as_str) {
                Some("") | None | Some("init" | "negotiating" | "allocating") => false,
                Some(_) => true,
            },
            None => false,
        }
    }

    /// ID des Links zwischen zwei Portpfaden, falls es ihn gibt.
    fn link_id(&self, von: &str, nach: &str) -> Option<u32> {
        let aus = self.port_pfad(von)?;
        let ein = self.port_pfad(nach)?;
        self.objekte
            .iter()
            .find(|(_, roh)| {
                roh.art == Art::Link
                    && roh.zahl("link.output.node") == Some(aus.node)
                    && roh.zahl("link.output.port") == Some(aus.port)
                    && roh.zahl("link.input.node") == Some(ein.node)
                    && roh.zahl("link.input.port") == Some(ein.port)
            })
            .map(|(id, _)| *id)
    }
}

fn knoten_aus(id: u32, roh: &Roh) -> Option<Node> {
    let name = roh.text("node.name");
    if name.is_empty() {
        return None;
    }
    Some(Node {
        id,
        ist_monitor: name.ends_with(".monitor") || roh.flagge("stream.monitor"),
        name,
        description: roh.text("node.description"),
        nick: roh.text("node.nick"),
        media_class: roh.text("media.class"),
    })
}

fn port_aus(id: u32, roh: &Roh) -> Option<Port> {
    let name = roh.text("port.name");
    if name.is_empty() {
        return None;
    }
    Some(Port {
        id,
        node_id: roh.zahl("node.id")?,
        name,
        richtung: Richtung::aus_text(&roh.text("port.direction"))?,
    })
}

fn link_aus(id: u32, roh: &Roh, zustand: Option<&String>) -> Option<Link> {
    Some(Link {
        id,
        ausgang_node: roh.zahl("link.output.node")?,
        ausgang_port: roh.zahl("link.output.port")?,
        eingang_node: roh.zahl("link.input.node")?,
        eingang_port: roh.zahl("link.input.port")?,
        zustand: zustand.cloned().unwrap_or_default(),
    })
}

/// PipeWire-Zustand eines Links als Text, wie ihn `pw-dump` schreibt.
fn zustand_text(zustand: pw::link::LinkState<'_>) -> &'static str {
    use pw::link::LinkState;
    match zustand {
        LinkState::Error(_) => "error",
        LinkState::Unlinked => "unlinked",
        LinkState::Init => "init",
        LinkState::Negotiating => "negotiating",
        LinkState::Allocating => "allocating",
        LinkState::Paused => "paused",
        LinkState::Active => "active",
    }
}

// ---------------------------------------------------------------------------
// Client mit eigener tokio-Runtime
// ---------------------------------------------------------------------------

/// Ein Auftrag an den Registry-Thread.
enum Befehl {
    Verbinden {
        von: String,
        nach: String,
        antwort: SyncSender<Result<(), String>>,
    },
    Trennen {
        von: String,
        nach: String,
        antwort: SyncSender<Result<(), String>>,
    },
    Ende,
}

/// Hält einen gebundenen Link-Proxy samt Listener am Leben.
///
/// Der Proxy ist nur zum Lesen des Zustands da (`info`-Ereignis). Solange er
/// lebt, meldet PipeWire Änderungen; fällt er weg, endet die Meldung.
struct LinkHalte {
    _proxy: pw::link::Link,
    _listener: pw::link::LinkListener,
}

/// Der Graph-Client: ein Thread, ein PipeWire-Client, ein Snapshot.
pub struct PwClient {
    daten: Arc<RwLock<Bestand>>,
    verbunden: Arc<AtomicBool>,
    grund: Arc<RwLock<Option<String>>>,
    stopp: Arc<AtomicBool>,
    befehle: Sender<Befehl>,
}

static GLOBAL: OnceLock<PwClient> = OnceLock::new();

impl PwClient {
    /// Der Client des Prozesses (beim ersten Aufruf gestartet).
    pub fn global() -> &'static PwClient {
        GLOBAL.get_or_init(|| PwClient::neu(None))
    }

    /// Startet den Registry-Thread gegen den Socket `remote`
    /// (`None` = Standard-Socket). Kehrt zurück, sobald der erste
    /// Verbindungsversuch durch ist – spätestens nach [`ANTWORT`].
    pub fn neu(remote: Option<&str>) -> PwClient {
        let daten = Arc::new(RwLock::new(Bestand::default()));
        let verbunden = Arc::new(AtomicBool::new(false));
        let grund = Arc::new(RwLock::new(None));
        let stopp = Arc::new(AtomicBool::new(false));
        let (befehle, eingang) = std::sync::mpsc::channel();
        let (start_tx, start_rx) = std::sync::mpsc::channel::<()>();
        let remote = remote.map(str::to_string);

        let gestartet = {
            let daten = daten.clone();
            let verbunden = verbunden.clone();
            let grund = grund.clone();
            let stopp = stopp.clone();
            std::thread::Builder::new()
                .name("pipewire-graph".into())
                .spawn(move || {
                    let mut start = Some(start_tx);
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build();
                    match runtime {
                        Ok(runtime) => runtime.block_on(schleife(
                            daten,
                            verbunden,
                            grund,
                            stopp,
                            eingang,
                            remote,
                            &mut start,
                        )),
                        Err(e) => grund_setzen(&grund, Some(format!("tokio-Runtime: {e}"))),
                    }
                    if let Some(start) = start.take() {
                        let _ = start.send(());
                    }
                })
        };
        if let Err(e) = gestartet {
            grund_setzen(&grund, Some(format!("Registry-Thread nicht startbar: {e}")));
        }

        let _ = start_rx.recv_timeout(ANTWORT * 2);
        PwClient {
            daten,
            verbunden,
            grund,
            stopp,
            befehle,
        }
    }

    /// Steht die Verbindung zum PipeWire-Daemon?
    pub fn verbunden(&self) -> bool {
        self.verbunden.load(Ordering::SeqCst)
    }

    /// Warum keine Verbindung besteht (letzter Fehler).
    pub fn grund(&self) -> Option<String> {
        self.grund.read().ok().and_then(|g| g.clone())
    }

    /// Der Snapshot – auch ohne Verbindung (dann leer, ohne Panik).
    pub fn snapshot(&self) -> Graph {
        match self.daten.read() {
            Ok(bestand) => bestand.graph(),
            Err(_) => Graph::default(),
        }
    }

    /// Der Snapshot, oder der Grund, warum es keinen gibt.
    pub fn graph(&self) -> Result<Graph, String> {
        if !self.verbunden() {
            return Err(self
                .grund()
                .unwrap_or_else(|| "keine Verbindung zu PipeWire".to_string()));
        }
        self.daten
            .read()
            .map(|bestand| bestand.graph())
            .map_err(|_| "PipeWire-Zustand gesperrt".to_string())
    }

    pub fn verbinden(&self, von: &str, nach: &str) -> Result<(), String> {
        self.anfrage(von, nach, true)
    }

    pub fn trennen(&self, von: &str, nach: &str) -> Result<(), String> {
        self.anfrage(von, nach, false)
    }

    fn anfrage(&self, von: &str, nach: &str, verbinden: bool) -> Result<(), String> {
        if !self.verbunden() {
            return Err(self
                .grund()
                .unwrap_or_else(|| "keine Verbindung zu PipeWire".to_string()));
        }
        let (antwort, empfangen) = sync_channel(1);
        let befehl = if verbinden {
            Befehl::Verbinden {
                von: von.to_string(),
                nach: nach.to_string(),
                antwort,
            }
        } else {
            Befehl::Trennen {
                von: von.to_string(),
                nach: nach.to_string(),
                antwort,
            }
        };
        self.befehle
            .send(befehl)
            .map_err(|_| "PipeWire-Thread läuft nicht mehr".to_string())?;
        empfangen
            .recv_timeout(ANTWORT)
            .unwrap_or_else(|_| Err("PipeWire antwortet nicht".to_string()))
    }
}

impl Drop for PwClient {
    fn drop(&mut self) {
        self.stopp.store(true, Ordering::SeqCst);
        let _ = self.befehle.send(Befehl::Ende);
    }
}

/// Äußerer Ablauf: verbinden, laufen, bei Abbruch neu versuchen.
async fn schleife(
    daten: Arc<RwLock<Bestand>>,
    verbunden: Arc<AtomicBool>,
    grund: Arc<RwLock<Option<String>>>,
    stopp: Arc<AtomicBool>,
    befehle: Receiver<Befehl>,
    remote: Option<String>,
    start: &mut Option<Sender<()>>,
) {
    loop {
        if stopp.load(Ordering::SeqCst) {
            break;
        }
        let ende = lauf(&daten, &verbunden, &grund, &stopp, &befehle, remote.as_deref(), start).await;
        // Erster Verbindungsversuch ist durch – auch wenn er scheiterte.
        if let Some(start) = start.take() {
            let _ = start.send(());
        }
        if stopp.load(Ordering::SeqCst) {
            break;
        }
        if let Some(meldung) = ende {
            grund_setzen(&grund, Some(meldung));
        }
        // Ohne Verbindung: Aufträge sofort abweisen, statt sie hängen zu lassen.
        warte(&befehle, &stopp, NEUVERSUCH).await;
    }
}

/// Schreibt den Grund in den gemeinsamen Zustand (auch bei vergiftetem Lock).
fn grund_setzen(grund: &Arc<RwLock<Option<String>>>, text: Option<String>) {
    match grund.write() {
        Ok(mut g) => *g = text,
        Err(vergiftet) => *vergiftet.into_inner() = text,
    }
}

/// Wartet bis zum nächsten Verbindungsversuch und beantwortet Aufträge.
async fn warte(befehle: &Receiver<Befehl>, stopp: &Arc<AtomicBool>, dauer: Duration) {
    let ende = Instant::now() + dauer;
    while Instant::now() < ende {
        if stopp.load(Ordering::SeqCst) {
            return;
        }
        while let Ok(befehl) = befehle.try_recv() {
            match befehl {
                Befehl::Ende => return,
                Befehl::Verbinden { antwort, .. } | Befehl::Trennen { antwort, .. } => {
                    let _ = antwort.send(Err("keine Verbindung zu PipeWire".to_string()));
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Eine Verbindung zum Daemon: alles aufbauen, Ereignisse einsammeln,
/// Aufträge ausführen. Kehrt zurück, wenn die Verbindung endet.
async fn lauf(
    daten: &Arc<RwLock<Bestand>>,
    verbunden: &Arc<AtomicBool>,
    grund: &Arc<RwLock<Option<String>>>,
    stopp: &Arc<AtomicBool>,
    befehle: &Receiver<Befehl>,
    remote: Option<&str>,
    start: &mut Option<Sender<()>>,
) -> Option<String> {
    let main_loop = match pw::main_loop::MainLoopBox::new(None) {
        Ok(loop_) => loop_,
        Err(e) => return Some(format!("PipeWire-MainLoop nicht anlegbar: {e:?}")),
    };
    let context = match pw::context::ContextBox::new(&main_loop.loop_(), None) {
        Ok(context) => context,
        Err(e) => return Some(format!("PipeWire-Context nicht anlegbar: {e:?}")),
    };
    let eigenschaften = remote.map(|r| {
        pw::properties::properties! {
            "remote.name" => r.to_string(),
        }
    });
    let core = match context.connect(eigenschaften) {
        Ok(core) => core,
        Err(e) => return Some(format!("keine Verbindung zu PipeWire: {e:?}")),
    };
    let registry = match core.get_registry() {
        Ok(registry) => registry,
        Err(e) => return Some(format!("PipeWire-Registry nicht lesbar: {e:?}")),
    };
    // Zustand, in den die Callbacks schreiben. Alles bleibt im Loop-Thread.
    let proxies: Rc<RefCell<HashMap<u32, LinkHalte>>> = Rc::new(RefCell::new(HashMap::new()));
    let entsorgt: Rc<RefCell<Vec<LinkHalte>>> = Rc::new(RefCell::new(Vec::new()));
    let fertig_seq = Rc::new(Cell::new(-1i32));
    let letzter_fehler: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let abbruch = Rc::new(Cell::new(false));
    // `Registry::bind` verlangt das `GlobalObject` aus dem Callback; der
    // Callback selbst bekommt aber kein `&Registry` mehr. Über den rohen
    // Zeiger ist derselbe Registry-Durchgriff im Callback erreichbar.
    let registry_zeiger = registry.as_raw_ptr();

    let _registry_listener = {
        let daten = daten.clone();
        let daten_weg = daten.clone();
        let proxies_neu = proxies.clone();
        let proxies_weg = proxies.clone();
        let entsorgt = entsorgt.clone();
        registry
            .add_listener_local()
            .global(move |obj| {
                let Some(art) = art_aus(&obj.type_) else {
                    return;
                };
                let props: BTreeMap<String, String> = match &obj.props {
                    Some(dict) => dict
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                    None => BTreeMap::new(),
                };
                if let Ok(mut bestand) = daten.write() {
                    bestand.einfuegen(obj.id, art, props);
                }
                if art != Art::Link {
                    return;
                }
                // Der Zustand eines Links steht nur im Info-Ereignis, nicht in
                // den Properties – dafür braucht es einen Proxy je Link.
                let registry: &pw::registry::Registry =
                    unsafe { &*(registry_zeiger as *const pw::registry::Registry) };
                if let Ok(link) = registry.bind::<pw::link::Link, _>(obj) {
                    let daten = daten.clone();
                    let id = obj.id;
                    let listener = link
                        .add_listener_local()
                        .info(move |info| {
                            if let Ok(mut bestand) = daten.write() {
                                bestand.setze_link_zustand(id, zustand_text(info.state()));
                            }
                        })
                        .register();
                    proxies_neu.borrow_mut().insert(
                        id,
                        LinkHalte {
                            _proxy: link,
                            _listener: listener,
                        },
                    );
                }
            })
            .global_remove(move |id| {
                if let Ok(mut bestand) = daten_weg.write() {
                    bestand.entfernen(id);
                }
                // Nicht im Callback freigeben: der Proxy würde mitten im
                // Registry-Ereignis abgeräumt. Der Loop leert den Eimer.
                if let Some(halte) = proxies_weg.borrow_mut().remove(&id) {
                    entsorgt.borrow_mut().push(halte);
                }
            })
            .register()
    };

    let _core_listener = {
        let fertig = fertig_seq.clone();
        let fehler = letzter_fehler.clone();
        let abbruch = abbruch.clone();
        core.add_listener_local()
            .done(move |_id, seq| fertig.set(seq.seq()))
            .error(move |id, _seq, res, msg| {
                if id == 0 {
                    // Fehler des Core selbst: Verbindung ist weg.
                    abbruch.set(true);
                }
                *fehler.borrow_mut() = Some(format!("{msg} (Code {res})"));
            })
            .register()
    };

    // Erst wenn die Registry einmal durchgelaufen ist, ist der Snapshot
    // vollständig – vorher meldet der Client „noch nicht verbunden“ und die
    // erste Anfrage der Oberfläche sähe einen leeren Graphen.
    if let Err(e) = sperre(&core, &main_loop, &fertig_seq) {
        return Some(format!("Registry nicht lesbar: {e}"));
    }
    grund_setzen(grund, None);
    verbunden.store(true, Ordering::SeqCst);
    if let Some(start) = start.take() {
        let _ = start.send(());
    }

    let mut ticker = tokio::time::interval(TAKT);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ende = None;

    'aussen: loop {
        if stopp.load(Ordering::SeqCst) {
            break;
        }
        if abbruch.get() {
            ende = letzter_fehler.borrow().clone();
            break;
        }

        // Aufträge der HTTP-Schicht – einer nach dem anderen, damit sich
        // Verbinden und Trennen nie überholen.
        while let Ok(befehl) = befehle.try_recv() {
            match befehl {
                Befehl::Ende => break 'aussen,
                Befehl::Verbinden { von, nach, antwort } => {
                    let ergebnis = verbinden(
                        &core,
                        &main_loop,
                        &fertig_seq,
                        &letzter_fehler,
                        daten,
                        &von,
                        &nach,
                    );
                    let _ = antwort.send(ergebnis);
                }
                Befehl::Trennen { von, nach, antwort } => {
                    let ergebnis = trennen(
                        &registry,
                        &core,
                        &main_loop,
                        &fertig_seq,
                        &letzter_fehler,
                        daten,
                        &von,
                        &nach,
                    );
                    let _ = antwort.send(ergebnis);
                }
            }
        }

        // Ereignisse einsammeln (nicht blockierend) und Abgeräumtes freigeben.
        main_loop.loop_().iterate(pw::loop_::Timeout::None);
        if !entsorgt.borrow().is_empty() {
            entsorgt.borrow_mut().clear();
        }
        ticker.tick().await;
    }

    verbunden.store(false, Ordering::SeqCst);
    if let Ok(mut bestand) = daten.write() {
        bestand.leeren();
    }
    ende
}

/// Wartet, bis PipeWire alle bisherigen Ereignisse zugestellt hat.
fn sperre(
    core: &pw::core::Core,
    main_loop: &pw::main_loop::MainLoopBox,
    fertig: &Cell<i32>,
) -> Result<(), String> {
    sperre_mit(core, main_loop, fertig, SPERRE)
}

/// Wie [`sperre`], aber mit eigener Frist.
fn sperre_mit(
    core: &pw::core::Core,
    main_loop: &pw::main_loop::MainLoopBox,
    fertig: &Cell<i32>,
    frist: Duration,
) -> Result<(), String> {
    let seq = core
        .sync(0)
        .map_err(|e| format!("PipeWire-Sync fehlgeschlagen: {e:?}"))?
        .seq();
    let ende = Instant::now() + frist;
    while fertig.get() < seq && Instant::now() < ende {
        main_loop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(5)));
    }
    if fertig.get() < seq {
        return Err("PipeWire hat nicht geantwortet".to_string());
    }
    Ok(())
}

/// Wartet kurz darauf, dass PipeWire den Zustand eines frisch angelegten
/// Links aushandelt (`init`/`negotiating` → `active`).
fn warte_auf_zustand(
    core: &pw::core::Core,
    main_loop: &pw::main_loop::MainLoopBox,
    fertig: &Cell<i32>,
    daten: &RwLock<Bestand>,
    von: &str,
    nach: &str,
) {
    let ende = Instant::now() + SETTLE;
    let mut naechster_sync = Instant::now();
    while Instant::now() < ende {
        let beruhigt = daten
            .read()
            .map(|bestand| bestand.hat_link_zustand(von, nach))
            .unwrap_or(false);
        if beruhigt {
            return;
        }
        if Instant::now() >= naechster_sync {
            if sperre_mit(core, main_loop, fertig, Duration::from_millis(50)).is_err() {
                return;
            }
            naechster_sync = Instant::now() + Duration::from_millis(20);
        }
        main_loop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(2)));
    }
}

fn verbinden(
    core: &pw::core::Core,
    main_loop: &pw::main_loop::MainLoopBox,
    fertig: &Cell<i32>,
    fehler: &RefCell<Option<String>>,
    daten: &RwLock<Bestand>,
    von: &str,
    nach: &str,
) -> Result<(), String> {
    fehler.borrow_mut().take();
    let (aus, ein) = {
        let bestand = daten.read().map_err(|_| "PipeWire-Zustand gesperrt".to_string())?;
        let aus = bestand
            .port_pfad(von)
            .ok_or_else(|| format!("Port {von} steht nicht im PipeWire-Graph"))?;
        let ein = bestand
            .port_pfad(nach)
            .ok_or_else(|| format!("Port {nach} steht nicht im PipeWire-Graph"))?;
        if aus.richtung != Richtung::Out || ein.richtung != Richtung::In {
            return Err(format!(
                "Link {von} -> {nach}: Ausgang muss ein Ausgangsport sein, Eingang ein Eingangsport"
            ));
        }
        (aus, ein)
    };

    let eigenschaften = pw::properties::properties! {
        "link.output.node" => aus.node.to_string(),
        "link.output.port" => aus.port.to_string(),
        "link.input.node" => ein.node.to_string(),
        "link.input.port" => ein.port.to_string(),
        // Der Link gehört dem Graphen, nicht diesem Client: er überlebt das
        // Schließen des Proxys (wie bei `pw-link`).
        "object.linger" => "true",
    };
    let link: pw::link::Link = core
        .create_object("link-factory", &eigenschaften)
        .map_err(|e| format!("Link {von} -> {nach} nicht anlegbar: {e:?}"))?;
    drop(link);
    sperre(core, main_loop, fertig)?;
    // Der Zustand (`active`, …) kommt mit dem Info-Ereignis des Proxys, also
    // einen Wusch später. Kurz nachfassen, damit die Antwort nicht „leer“
    // meldet, obwohl der Link steht.
    warte_auf_zustand(core, main_loop, fertig, daten, von, nach);
    if let Some(meldung) = fehler.borrow_mut().take() {
        return Err(format!("Link {von} -> {nach}: {meldung}"));
    }
    Ok(())
}

fn trennen(
    registry: &pw::registry::Registry,
    core: &pw::core::Core,
    main_loop: &pw::main_loop::MainLoopBox,
    fertig: &Cell<i32>,
    fehler: &RefCell<Option<String>>,
    daten: &RwLock<Bestand>,
    von: &str,
    nach: &str,
) -> Result<(), String> {
    fehler.borrow_mut().take();
    let id = {
        let bestand = daten.read().map_err(|_| "PipeWire-Zustand gesperrt".to_string())?;
        bestand.link_id(von, nach)
    };
    let Some(id) = id else {
        // Nichts zu lösen – wie `pw-link -d`, das hier nur meckert.
        return Ok(());
    };
    if let Err(e) = registry.destroy_global(id).into_result() {
        // Kommt vor, wenn ein fremder Client den Link geschuetzt hat; wie
        // `pw-link -d` melden wir das, die Aufrufer uebergehen es.
        return Err(format!("Link {von} -> {nach} nicht loesbar: {e:?}"));
    }
    sperre(core, main_loop, fertig)?;
    if let Some(meldung) = fehler.borrow_mut().take() {
        return Err(format!("Link {von} -> {nach} nicht lösbar: {meldung}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::json::{self, Value};

    /// Auszug aus `pw-dump` in der Form, die der Rechner liefert: zwei Sinks
    /// (einer davon der eigene `crosspoint_out_B1`), ein Mono-Mikrofon, ein
    /// Abspielstrom, ein Monitor-Paar, zwei Links.
    ///
    /// Dient nur noch als Fixture für Tests: der Graph kommt im Betrieb aus der
    /// Registry.
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
       "info": {"props": {"media.class": "Audio/Sink", "node.name": "crosspoint_out_B1",
                          "node.description": "CROSSPOINT-Output-B1"}}},
      {"id": 457, "type": "PipeWire:Interface:Node",
       "info": {"props": {"media.class": "Stream/Output/Audio", "node.name": "Chromium"}}},
      {"id": 297, "type": "PipeWire:Interface:Node",
       "info": {"props": {"node.name": "crosspoint", "node.description": "crosspoint"}}},
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

    /// Test-Doppel für den PipeWire-Zugriff: liefert einen vorbereiteten
    /// Graphen und schreibt mit, was verbunden bzw. getrennt wurde.
    pub struct Testzugriff {
        graph: Result<Graph, String>,
        aktionen: RefCell<Vec<String>>,
        scheitert: RefCell<Vec<String>>,
    }

    impl Testzugriff {
        /// Zugriff auf einen festen Graphen.
        pub fn neu(graph: Graph) -> Testzugriff {
            Testzugriff {
                graph: Ok(graph),
                aktionen: RefCell::new(Vec::new()),
                scheitert: RefCell::new(Vec::new()),
            }
        }

        /// Zugriff aus einem eingefrorenen `pw-dump`-Auszug.
        pub fn aus_dump(text: &str) -> Testzugriff {
            Testzugriff::neu(Graph::parse(text).expect("Dump lesbar"))
        }

        /// Zugriff ohne PipeWire (wie ein Daemon, der nicht läuft).
        pub fn ohne_graph() -> Testzugriff {
            Testzugriff {
                graph: Err("keine Verbindung zu PipeWire".to_string()),
                aktionen: RefCell::new(Vec::new()),
                scheitert: RefCell::new(Vec::new()),
            }
        }

        /// Aufrufe, deren Text diesen Teilstring enthält, schlagen fehl.
        pub fn mit_fehler(self, muster: &str) -> Testzugriff {
            self.scheitert.borrow_mut().push(muster.to_string());
            self
        }

        /// Aufgezeichnete Aktionen, z. B. `verbinden crosspoint:busA1_L …`.
        pub fn aktionen(&self) -> Vec<String> {
            self.aktionen.borrow().clone()
        }

        fn pruefe(&self, aktion: &str) -> Result<(), String> {
            if self
                .scheitert
                .borrow()
                .iter()
                .any(|m| aktion.contains(m.as_str()))
            {
                return Err(format!("{aktion} -> simulierter Fehler"));
            }
            Ok(())
        }
    }

    impl Zugriff for Testzugriff {
        fn graph(&self) -> Result<Graph, String> {
            self.graph.clone()
        }

        fn verbinden(&self, von: &str, nach: &str) -> Result<(), String> {
            let aktion = format!("verbinden {von} {nach}");
            self.aktionen.borrow_mut().push(aktion.clone());
            self.pruefe(&aktion)
        }

        fn trennen(&self, von: &str, nach: &str) -> Result<(), String> {
            let aktion = format!("trennen {von} {nach}");
            self.aktionen.borrow_mut().push(aktion.clone());
            self.pruefe(&aktion)
        }
    }

    impl Graph {
        /// Liest einen `pw-dump`-Auszug (nur für Tests und Fehlersuche).
        ///
        /// Im Betrieb kommt der Graph aus der Registry; dieses Format bleibt,
        /// weil es sich als eingefrorene Fixture bewährt hat.
        pub fn parse(text: &str) -> Result<Graph, String> {
            let wert = json::parse(text)?;
            let objekte = wert
                .as_arr()
                .ok_or_else(|| "pw-dump liefert kein JSON-Array".to_string())?;

            let mut graph = Graph::default();
            for obj in objekte {
                match text_wert(obj.get("type")).as_str() {
                    "PipeWire:Interface:Node" => {
                        if let Some(n) = knoten_aus_dump(obj) {
                            graph.nodes.push(n);
                        }
                    }
                    "PipeWire:Interface:Port" => {
                        if let Some(p) = port_aus_dump(obj) {
                            graph.ports.push(p);
                        }
                    }
                    "PipeWire:Interface:Link" => {
                        if let Some(l) = link_aus_dump(obj) {
                            graph.links.push(l);
                        }
                    }
                    _ => {}
                }
            }
            graph.sortiere_links();
            Ok(graph)
        }
    }

    fn text_wert(wert: Option<&Value>) -> String {
        match wert {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        }
    }

    /// Property-Wert als Text – die Registry liefert alle Werte als Text,
    /// das Fixture mischt Zahlen und Zeichenketten.
    fn wert_text(wert: &Value) -> String {
        match wert {
            Value::Str(s) => s.clone(),
            Value::Num(n) => format!("{}", *n as i64),
            Value::Bool(b) => b.to_string(),
            _ => String::new(),
        }
    }

    fn zahl_wert(wert: Option<&Value>) -> Option<u32> {
        wert.and_then(|v| v.as_f64()).map(|f| f as u32)
    }

    fn knoten_aus_dump(obj: &Value) -> Option<Node> {
        let props = obj.get("info")?.get("props")?;
        let name = text_wert(props.get("node.name"));
        if name.is_empty() {
            return None;
        }
        Some(Node {
            id: zahl_wert(obj.get("id"))?,
            ist_monitor: name.ends_with(".monitor")
                || props
                    .get("stream.monitor")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            name,
            description: text_wert(props.get("node.description")),
            nick: text_wert(props.get("node.nick")),
            media_class: text_wert(props.get("media.class")),
        })
    }

    fn port_aus_dump(obj: &Value) -> Option<Port> {
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

    fn link_aus_dump(obj: &Value) -> Option<Link> {
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

    #[test]
    fn knoten_werden_nach_klasse_gefiltert() {
        let g = graph();
        // Alle Knoten bleiben für die Link-Auflösung erhalten …
        assert_eq!(
            g.nodes.len(),
            7,
            "Driver und crosspoint bleiben: {:?}",
            g.nodes
        );
        // … als Geräte gelten nur die drei Klassen.
        assert_eq!(g.geraete().len(), 5);
        assert_eq!(g.sinks().len(), 3);
        assert_eq!(g.sources().len(), 1);
        assert_eq!(g.streams().len(), 1);
        assert!(g.node_by_name("crosspoint").unwrap().media_class.is_empty());
        assert!(!g.node_by_name("crosspoint").unwrap().ist_geraet());
        assert_eq!(g.streams()[0].name, "Chromium");
        assert!(g.streams()[0].ist_strom());
        assert!(g.sinks()[0].ist_ausgabegeraet());
        assert!(g.sources()[0].ist_eingabegeraet());
    }

    #[test]
    fn beschreibung_bleibt_vollstaendig_mit_leerzeichen_und_umlauten() {
        let g = graph();
        let corsair = g
            .node_by_name(
                "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo",
            )
            .expect("Corsair");
        assert_eq!(
            corsair.description,
            "Corsair HS80 RGB USB Gaming Headset Analoges Stereo"
        );
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
        let corsair = g
            .node_by_name(
                "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo",
            )
            .unwrap();
        let namen: Vec<&str> = g
            .ports(corsair.id)
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(
            namen,
            ["monitor_FL", "monitor_FR", "playback_FL", "playback_FR"]
        );

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
        assert!(g.ist_verbunden("crosspoint:busA1_L", "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo:playback_FL"));
        assert_eq!(v[0].zustand, "active");
        assert!(!g.ist_verbunden(
            "crosspoint:busA1_L",
            "alsa_output.pci-0000_03_00.1.hdmi-stereo-extra2:playback_FL"
        ));

        let offen = g.offene_links(&["crosspoint:busA1_R".to_string()]);
        assert!(offen.is_empty());
        let offen = g.offene_links(&["crosspoint:busA1_L".to_string()]);
        assert_eq!(offen.len(), 1);
        assert_eq!(offen[0].von, "crosspoint:busA1_L");
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
        // Alte Zuordnung `….monitor` findet den Knoten dahinter.
        assert_eq!(
            g.knoten("alsa_output.hdmi.monitor").unwrap().name,
            "alsa_output.hdmi.monitor"
        );
    }

    #[test]
    fn kanalpaar_bevorzugt_fl_fr_dann_mono() {
        let p = |s: &str| s.to_string();
        assert_eq!(
            kanalpaar(&[p("n:playback_FL"), p("n:playback_FR")]),
            [p("n:playback_FL"), p("n:playback_FR")]
        );
        assert_eq!(kanalpaar(&[p("n:capture_MONO")]), [p("n:capture_MONO")]);
        assert_eq!(
            kanalpaar(&[p("n:capture_1"), p("n:capture_2"), p("n:capture_3")]),
            [p("n:capture_1"), p("n:capture_2")]
        );
        assert!(kanalpaar(&[]).is_empty());
    }

    /// Der Graph entsteht aus den Registry-Properties (nicht mehr aus JSON).
    #[test]
    fn graph_entsteht_aus_registry_properties() {
        let mut bestand = Bestand::default();
        let mut props = BTreeMap::new();
        props.insert("node.name".to_string(), "crosspoint".to_string());
        bestand.einfuegen(2, Art::Node, props);
        let mut props = BTreeMap::new();
        props.insert("node.name".to_string(), "sink".to_string());
        bestand.einfuegen(9, Art::Node, props);
        let mut props = BTreeMap::new();
        props.insert("node.id".to_string(), "2".to_string());
        props.insert("port.name".to_string(), "busA1_L".to_string());
        props.insert("port.direction".to_string(), "out".to_string());
        bestand.einfuegen(7, Art::Port, props);
        let mut props = BTreeMap::new();
        props.insert("node.id".to_string(), "9".to_string());
        props.insert("port.name".to_string(), "playback_FL".to_string());
        props.insert("port.direction".to_string(), "in".to_string());
        bestand.einfuegen(11, Art::Port, props);
        let mut props = BTreeMap::new();
        props.insert("link.output.node".to_string(), "2".to_string());
        props.insert("link.output.port".to_string(), "7".to_string());
        props.insert("link.input.node".to_string(), "9".to_string());
        props.insert("link.input.port".to_string(), "11".to_string());
        bestand.einfuegen(3, Art::Link, props);
        // Zustand kommt aus dem Info-Ereignis, nicht aus den Properties.
        bestand.setze_link_zustand(3, "active");

        let g = bestand.graph();
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.ports.len(), 2);
        assert_eq!(g.links.len(), 1);
        assert_eq!(g.verbindungen()[0].von, "crosspoint:busA1_L");
        assert_eq!(g.verbindungen()[0].nach, "sink:playback_FL");
        assert_eq!(g.verbindungen()[0].zustand, "active");
        assert_eq!(g.port(7).unwrap().richtung, Richtung::Out);
        assert_eq!(g.port(11).unwrap().richtung, Richtung::In);

        // Ports ohne Richtung oder ohne Knotenbezug werden übergangen.
        let mut props = BTreeMap::new();
        props.insert("port.name".to_string(), "ohne-alles".to_string());
        bestand.einfuegen(8, Art::Port, props);
        assert_eq!(bestand.graph().ports.len(), 2);
    }

    #[test]
    fn portpfade_und_links_werden_ueber_namen_gefunden() {
        let bestand = aus_dump_bestand(DUMP);
        // Knoten mit Doppelpunkt im Namen darf die Zerlegung nicht stören.
        let mic = bestand
            .port_pfad("alsa_input.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.mono-fallback:capture_MONO")
            .expect("Mono-Port");
        assert_eq!(mic.richtung, Richtung::Out);
        assert_eq!(mic.node, 269);

        let bus = bestand.port_pfad("crosspoint:busA1_L").expect("Busport");
        assert_eq!(bus.node, 297);
        assert!(bestand.port_pfad("crosspoint:busA1_L").is_some());

        // Bestehender Link ist auffindbar, ein erfundener nicht.
        assert!(bestand
            .link_id(
                "crosspoint:busA1_L",
                "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo:playback_FL"
            )
            .is_some());
        assert!(bestand
            .link_id(
                "crosspoint:busA1_R",
                "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo:playback_FR"
            )
            .is_none());
        assert!(bestand.port_pfad("gibtsnicht:port").is_none());
        assert!(bestand.port_pfad("crosspoint").is_none());
    }

    /// Baut den Registry-Bestand aus dem Fixture – dieselben Properties, die
    /// der Client sonst aus den Registry-Ereignissen bekommt.
    fn aus_dump_bestand(text: &str) -> Bestand {
        let mut bestand = Bestand::default();
        let wert = json::parse(text).expect("Fixture");
        for obj in wert.as_arr().expect("Array") {
            let id = zahl_wert(obj.get("id")).expect("id");
            let info = obj.get("info").expect("info");
            let mut props = BTreeMap::new();
            if let Some(Value::Obj(paare)) = info.get("props") {
                for (k, v) in paare {
                    props.insert(k.clone(), wert_text(v));
                }
            }
            let art = match text_wert(obj.get("type")).as_str() {
                "PipeWire:Interface:Node" => Art::Node,
                "PipeWire:Interface:Port" => {
                    props.insert(
                        "port.direction".to_string(),
                        text_wert(info.get("direction")),
                    );
                    Art::Port
                }
                "PipeWire:Interface:Link" => {
                    let feld = |name: &str| -> String {
                        info.get(name).map(wert_text).unwrap_or_default()
                    };
                    props.insert("link.output.node".to_string(), feld("output-node-id"));
                    props.insert("link.output.port".to_string(), feld("output-port-id"));
                    props.insert("link.input.node".to_string(), feld("input-node-id"));
                    props.insert("link.input.port".to_string(), feld("input-port-id"));
                    Art::Link
                }
                _ => continue,
            };
            bestand.einfuegen(id, art, props);
        }
        bestand
    }

    /// Ohne laufenden PipeWire: kein Panik, sondern ein leerer Snapshot.
    #[test]
    fn ohne_pipewire_leerer_snapshot_ohne_panik() {
        let client = PwClient::neu(Some("crosspoint-gibtsnicht"));
        assert!(!client.verbunden(), "Socket gibt es nicht");
        let graph = client.snapshot();
        assert!(graph.nodes.is_empty());
        assert!(graph.ports.is_empty());
        assert!(graph.links.is_empty());
        assert!(graph.geraete().is_empty());
        // Der Grund ist benannt, aber ohne Panik.
        assert!(client.graph().is_err());
        assert!(client.grund().is_some(), "{:?}", client.grund());
        // Aufträge werden sofort abgewiesen statt zu hängen.
        assert!(client.verbinden("a:b", "c:d").is_err());
    }

    /// Der Testzugriff verhält sich wie der echte: erst trennen, dann
    /// verbinden, und Fehler werden gemeldet.
    #[test]
    fn testzugriff_schreibt_aktionen_mit() {
        let zugriff = Testzugriff::aus_dump(DUMP);
        assert_eq!(zugriff.snapshot().nodes.len(), 7);
        zugriff
            .trennen("crosspoint:busA1_L", "x:y")
            .expect("trennen");
        zugriff.verbinden("crosspoint:busA1_L", "x:y").expect("verbinden");
        assert_eq!(
            zugriff.aktionen(),
            vec![
                "trennen crosspoint:busA1_L x:y",
                "verbinden crosspoint:busA1_L x:y"
            ]
        );

        let kaputt = Testzugriff::aus_dump(DUMP).mit_fehler("playback_FR");
        assert!(kaputt
            .verbinden("crosspoint:busA1_R", "sink:playback_FR")
            .is_err());
        assert!(kaputt
            .verbinden("crosspoint:busA1_L", "sink:playback_FL")
            .is_ok());

        let ohne = Testzugriff::ohne_graph();
        assert!(ohne.graph().is_err());
        assert!(ohne.snapshot().nodes.is_empty());
    }
}
