//! Geräteauswahl für die Hardware-Busse A1..A5.
//!
//! VoiceMeeter hängt jeden Hardware-Bus (A1..A5) an ein Ausgabegerät. Auf Linux
//! ist das ein PipeWire-Sink; die Verbindung stellt `pw-link` her:
//!
//! ```text
//! vm8:busA1_L  ->  <sink>:playback_FL
//! vm8:busA1_R  ->  <sink>:playback_FR
//! ```
//!
//! Der Mixer heißt in der Graph-Sicht `vm8` (siehe `engine`/`jack`); die
//! Sink-Ports heißen je Gerät `<sink>:playback_FL`/`_FR`.
//!
//! Die Auswahl wird in `~/.config/vm8-linux/devices.json` gemerkt, damit sie
//! einen Neustart überlebt:
//!
//! ```json
//! {"A1":{"name":"alsa_output.usb-…","desc":"Kopfhörer (USB)"}}
//! ```
//!
//! **Testbarkeit:** jeder Systemaufruf läuft über [`CommandRunner`]. Im Betrieb
//! ist das [`RealRunner`] (`std::process::Command`), in Tests der [`FakeRunner`],
//! der nur mitschreibt und nichts am laufenden PipeWire-Graphen ändert.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use crate::json::Value;

/// Hardware-Busse des Originals. B1..B3 sind virtuell und haben kein Gerät.
pub const HARDWARE_BUSES: [&str; 5] = ["A1", "A2", "A3", "A4", "A5"];

/// Client-Name des Mixers in der PipeWire-Graph-Sicht.
pub const CLIENT: &str = "vm8";

/// Kanalpaar eines Busses: Port-Suffix am Mixer -> Port-Suffix am Sink.
const CHANNELS: [(&str, &str); 2] = [("L", "playback_FL"), ("R", "playback_FR")];

/// Unterverzeichnis + Dateiname der gespeicherten Zuordnung.
const CONFIG_DIR: &str = "vm8-linux";
const CONFIG_FILE: &str = "devices.json";

// ---------------------------------------------------------------------------
// Kommando-Ausführung
// ---------------------------------------------------------------------------

/// Führt ein Programm aus und liefert dessen Standardausgabe.
///
/// Nur so lässt sich die Geräteauswahl ohne Eingriff in den laufenden
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
/// `outputs` liefert die Standardausgabe je Programm, `failures` enthält
/// Teilstrings; passt einer auf `"<programm> <args…>"`, schlägt der Aufruf fehl
/// (so lässt sich prüfen, dass `pw-link -d`-Fehler ignoriert werden).
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
// Sink-Liste aus `pactl`
// ---------------------------------------------------------------------------

/// Ein Ausgabegerät, wie es `pactl list sinks` beschreibt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sink {
    /// Technischer Name, z. B. `alsa_output.pci-0000_03_00.1.hdmi-stereo`.
    /// Wird für die `pw-link`-Ports gebraucht.
    pub name: String,
    /// Anzeigename aus der Oberfläche, lokalisiert.
    pub desc: String,
}

impl Sink {
    pub fn new(name: impl Into<String>, desc: impl Into<String>) -> Sink {
        Sink { name: name.into(), desc: desc.into() }
    }

    /// Port am Sink für einen Kanal (`L` -> `playback_FL`).
    pub fn port(&self, sink_suffix: &str) -> String {
        format!("{}:{sink_suffix}", self.name)
    }
}

/// Ist die Zeile der Kopf eines Sink-Blocks? `pactl` schreibt ihn lokalisiert
/// (`Sink #50` / `Ziel #50` / `Destination #50`).
fn ist_blockkopf(zeile: &str) -> bool {
    zeile.contains('#')
        && (zeile.starts_with("Sink")
            || zeile.starts_with("Ziel")
            || zeile.starts_with("Destination"))
}

/// Zerlegt eine `Schlüssel: Wert`-Zeile. Der Wert darf Doppelpunkte enthalten,
/// deshalb wird nur am ersten getrennt.
fn schluessel_wert(zeile: &str) -> Option<(&str, &str)> {
    let (k, v) = zeile.split_once(':')?;
    Some((k.trim(), v.trim()))
}

/// Parst die Ausgabe von `pactl list sinks`.
///
/// Erkennt deutsche und englische Beschriftungen (`Beschreibung:` /
/// `Description:`), `Name:` heißt in beiden Sprachen gleich. Doppelte Namen
/// (kommt bei mehrfach geladenen Modulen vor) werden zusammengefasst; der erste
/// Eintrag gewinnt, damit die Oberfläche keine Dubletten zeigt.
pub fn parse_sinks(text: &str) -> Vec<Sink> {
    let mut sinks: Vec<Sink> = Vec::new();
    let mut name: Option<String> = None;
    let mut desc: Option<String> = None;

    for roh in text.lines() {
        let zeile = roh.trim_end_matches('\r').trim();

        if zeile.is_empty() || ist_blockkopf(zeile) {
            // Blockende: vollständigen Eintrag übernehmen.
            if let Some(n) = name.take() {
                let beschreibung = desc.take().unwrap_or_default();
                if !sinks.iter().any(|s| s.name == n) {
                    sinks.push(Sink { name: n, desc: beschreibung });
                }
            }
            desc = None;
            continue;
        }

        // Eigenschaften stehen als `key = value` und werden nicht angefasst.
        if !zeile.contains(':') || zeile.contains(" = ") {
            continue;
        }
        let Some((key, wert)) = schluessel_wert(zeile) else {
            continue;
        };
        match key {
            "Name" if name.is_none() => name = Some(wert.to_string()),
            "Beschreibung" | "Description" if name.is_some() => {
                desc = Some(wert.to_string());
            }
            _ => {}
        }
    }

    if let Some(n) = name.take() {
        if !sinks.iter().any(|s| s.name == n) {
            sinks.push(Sink { name: n, desc: desc.unwrap_or_default() });
        }
    }
    sinks
}

/// Liest die verfügbaren Sinks über `pactl list sinks`.
///
/// Der Aufruf läuft bewusst ohne `LC_ALL`-Eingriff: `parse_sinks` versteht die
/// deutsche wie die englische Ausgabe.
pub fn list_sinks(runner: &dyn CommandRunner) -> Result<Vec<Sink>, String> {
    let out = runner
        .run("pactl", &["list", "sinks"])
        .map_err(|e| format!("pactl list sinks fehlgeschlagen: {e}"))?;
    Ok(parse_sinks(&out))
}

// ---------------------------------------------------------------------------
// Zuordnung Bus -> Sink
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

/// Mixer-Port eines Bus-Kanals, z. B. `vm8:busA1_L`.
pub fn bus_port(bus: &str, kanal: &str) -> String {
    format!("{CLIENT}:bus{bus}_{kanal}")
}

/// Speicherort der Zuordnung: `$XDG_CONFIG_HOME/vm8-linux/devices.json`,
/// sonst `~/.config/vm8-linux/devices.json`.
pub fn default_path() -> PathBuf {
    let basis = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    basis.join(CONFIG_DIR).join(CONFIG_FILE)
}

/// Zuordnung Hardware-Bus -> Ausgabegerät, gespiegelt nach JSON.
pub struct DeviceMap {
    pfad: PathBuf,
    busse: BTreeMap<String, Sink>,
}

impl DeviceMap {
    /// Leere Zuordnung, die nach `pfad` schreibt.
    pub fn new(pfad: impl Into<PathBuf>) -> DeviceMap {
        DeviceMap { pfad: pfad.into(), busse: BTreeMap::new() }
    }

    /// Lädt die Zuordnung; eine fehlende oder unlesbare Datei ergibt eine leere
    /// Zuordnung (erster Start), statt die Oberfläche scheitern zu lassen.
    pub fn load(pfad: impl Into<PathBuf>) -> DeviceMap {
        let pfad = pfad.into();
        let mut map = DeviceMap { pfad, busse: BTreeMap::new() };
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

    /// Zugeordnetes Gerät eines Hardware-Busses.
    pub fn get(&self, bus: &str) -> Option<&Sink> {
        let bus = normalisiere_bus(bus).ok()?;
        self.busse.get(&bus)
    }

    /// Alle gesetzten Zuordnungen in Bus-Reihenfolge A1..A5.
    pub fn entries(&self) -> Vec<(&str, &Sink)> {
        self.busse.iter().map(|(b, s)| (b.as_str(), s)).collect()
    }

    /// Übernimmt eine JSON-Datei. Unbekannte Busse und Einträge ohne Namen
    /// werden übersprungen, damit eine handgeschriebene Datei nichts kaputt macht.
    fn uebernehme(&mut self, text: &str) {
        let Ok(Value::Obj(pairs)) = crate::json::parse(text) else {
            return;
        };
        for (bus, wert) in pairs {
            let Ok(bus) = normalisiere_bus(&bus) else {
                continue;
            };
            let name = wert
                .get("name")
                .and_then(|v| match v {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let desc = wert
                .get("desc")
                .and_then(|v| match v {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            self.busse.insert(bus, Sink { name, desc });
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

    /// Die Zuordnung als JSON-Objekt `{"A1": {name, desc}, …}`.
    pub fn to_json(&self) -> String {
        Value::Obj(
            self.busse
                .iter()
                .map(|(bus, sink)| (bus.clone(), sink_value(sink)))
                .collect(),
        )
        .to_json()
    }

    /// Verdrahtet `bus` auf `sink`: erst beide alten Verbindungen lösen
    /// (`pw-link -d`, Fehler bewusst ignoriert — es kann sein, dass nichts
    /// verbunden ist), dann L und R neu verbinden.
    ///
    /// Ein Fehler beim Verbinden bricht ab, ohne die Zuordnung zu ändern: der
    /// gemerkte Zustand bleibt dann deckungsgleich mit dem Graph.
    pub fn assign(
        &mut self,
        runner: &dyn CommandRunner,
        bus: &str,
        sink: &Sink,
    ) -> Result<(), String> {
        let bus = normalisiere_bus(bus)?;
        if sink.name.trim().is_empty() {
            return Err("Sink ohne Namen".into());
        }

        // Alte Verbindung: bekannte Zuordnung, sonst das neue Ziel (dann läuft
        // der Disconnect ins Leere und wird verworfen).
        let alt = self
            .busse
            .get(&bus)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| sink.name.clone());
        for (kanal, ziel) in CHANNELS {
            let _ = runner.run("pw-link", &["-d", &bus_port(&bus, kanal), &sink_port(&alt, ziel)]);
        }

        for (kanal, ziel) in CHANNELS {
            runner
                .run("pw-link", &[&bus_port(&bus, kanal), &sink_port(&sink.name, ziel)])
                .map_err(|e| format!("{bus} auf '{}' verbinden fehlgeschlagen: {e}", sink.desc))?;
        }

        self.busse.insert(bus, sink.clone());
        self.save()
    }

    /// Löst die Verbindungen eines Busses und entfernt die Zuordnung.
    /// Ist nichts zugeordnet, passiert nichts (idempotent).
    pub fn clear(&mut self, runner: &dyn CommandRunner, bus: &str) -> Result<(), String> {
        let bus = normalisiere_bus(bus)?;
        let Some(sink) = self.busse.remove(&bus) else {
            return Ok(());
        };
        for (kanal, ziel) in CHANNELS {
            let quell = bus_port(&bus, kanal);
            let ziel = sink_port(&sink.name, ziel);
            let _ = runner.run("pw-link", &["-d", &quell, &ziel]);
        }
        self.save()
    }

    /// Zustand für die Oberfläche/API: verfügbare Sinks plus Zuordnung der
    /// Busse A1..A5. `sink: null` heißt „kein Gerät gewählt“; `error` trägt die
    /// Meldung, falls `pactl` nicht erreichbar war.
    pub fn status_json(&self, runner: &dyn CommandRunner) -> String {
        let (sinks, fehler) = match list_sinks(runner) {
            Ok(s) => (s, Value::Null),
            Err(e) => (Vec::new(), Value::str(e)),
        };

        let busse = HARDWARE_BUSES
            .iter()
            .map(|bus| {
                let mut paare: Vec<(String, Value)> =
                    vec![("bus".into(), Value::str(*bus))];
                match self.busse.get(*bus) {
                    Some(sink) => {
                        paare.push(("sink".into(), Value::str(sink.name.clone())));
                        paare.push(("desc".into(), Value::str(sink.desc.clone())));
                    }
                    None => {
                        paare.push(("sink".into(), Value::Null));
                        paare.push(("desc".into(), Value::Null));
                    }
                }
                Value::Obj(paare)
            })
            .collect();

        Value::obj(vec![
            ("sinks", Value::Arr(sinks.iter().map(sink_value).collect())),
            ("buses", Value::Arr(busse)),
            ("error", fehler),
        ])
        .to_json()
    }
}

/// Port am Sink, z. B. `alsa_output.…:playback_FL`.
fn sink_port(sink_name: &str, sink_suffix: &str) -> String {
    format!("{sink_name}:{sink_suffix}")
}

fn sink_value(sink: &Sink) -> Value {
    Value::obj(vec![
        ("name", Value::str(sink.name.clone())),
        ("desc", Value::str(sink.desc.clone())),
    ])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Deutsche `pactl list sinks`-Ausgabe (so auf diesem Rechner), zwei Geräte,
    /// Umlaute in den Beschreibungen.
    const PACTL_DE: &str = "\
Ziel #50
\tStatus: IDLE
\tName: alsa_output.pci-0000_0f_00.6.iec958-stereo
\tBeschreibung: Ryzen HD Audio Controller Digitales Stereo (IEC958)
\tTreiber: PipeWire
\tAbtastwert-Angabe: s32le 2ch 48000Hz
\tStumm: nein
\tEigenschaften:
\t\tdevice.description = \"Internes Audio\"
\t\talsa.card = \"1\"

Ziel #1642
\tStatus: RUNNING
\tName: alsa_output.usb-046d_Headset
\tBeschreibung: Büro-Kopfhörer (USB) – Grün
\tTreiber: PipeWire
\tAbtastwert-Angabe: float32le 2ch 48000Hz
";

    const PACTL_EN: &str = "\
Sink #0
\tState: SUSPENDED
\tName: alsa_output.pci-0000_03_00.1.hdmi-stereo
\tDescription: HDMI / DisplayPort 2 Output
\tDriver: PipeWire
\tMute: no

Sink #9
\tState: RUNNING
\tName: alsa_output.usb-0d8c_Speaker
\tDescription: Lautsprecher auf dem Schreibtisch
\tDriver: PipeWire
";

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("vm8-devices-{}-{tag}.json", std::process::id()))
    }

    fn fehler_sink() -> Sink {
        Sink::new("alsa_output.usb-046d_Headset", "Büro-Kopfhörer (USB) – Grün")
    }

    #[test]
    fn pactl_deutsch_mit_umlauten() {
        let sinks = parse_sinks(PACTL_DE);
        assert_eq!(sinks.len(), 2, "zwei Sinks erwartet: {sinks:?}");
        assert_eq!(sinks[0].name, "alsa_output.pci-0000_0f_00.6.iec958-stereo");
        assert_eq!(sinks[0].desc, "Ryzen HD Audio Controller Digitales Stereo (IEC958)");
        assert_eq!(sinks[1].name, "alsa_output.usb-046d_Headset");
        assert_eq!(sinks[1].desc, "Büro-Kopfhörer (USB) – Grün");
        // Eigenschaften dürfen die Beschreibung nicht überschreiben.
        assert!(!sinks[0].desc.contains("Internes Audio"));
    }

    #[test]
    fn pactl_englisch_und_list_sinks_ueber_runner() {
        let sinks = parse_sinks(PACTL_EN);
        assert_eq!(sinks.len(), 2);
        assert_eq!(sinks[0].desc, "HDMI / DisplayPort 2 Output");
        assert_eq!(sinks[1].name, "alsa_output.usb-0d8c_Speaker");

        let fake = FakeRunner::new().with_output("pactl", PACTL_DE);
        let via_runner = list_sinks(&fake).unwrap();
        assert_eq!(via_runner, parse_sinks(PACTL_DE));
        assert_eq!(fake.lines(), vec!["pactl list sinks".to_string()]);
    }

    #[test]
    fn doppelte_namen_werden_zusammengefasst() {
        let doppelt = format!("{PACTL_DE}\n{PACTL_DE}");
        assert_eq!(parse_sinks(&doppelt).len(), 2);
    }

    #[test]
    fn assign_loest_zwei_und_verbindet_zwei() {
        let pfad = tmp_path("assign");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = FakeRunner::new().with_output("pactl", PACTL_DE);
        let alt = Sink::new("alsa_output.alt_geraet", "Altes Gerät");
        let neu = fehler_sink();

        map.assign(&fake, "A1", &alt).unwrap();
        let nach_erstem = fake.lines();
        assert_eq!(nach_erstem.len(), 4, "4 Aufrufe: {nach_erstem:?}");

        map.assign(&fake, "A1", &neu).unwrap();
        let alle = fake.lines();
        assert_eq!(alle.len(), 8);
        assert_eq!(
            &alle[4..8],
            &[
                "pw-link -d vm8:busA1_L alsa_output.alt_geraet:playback_FL".to_string(),
                "pw-link -d vm8:busA1_R alsa_output.alt_geraet:playback_FR".to_string(),
                "pw-link vm8:busA1_L alsa_output.usb-046d_Headset:playback_FL".to_string(),
                "pw-link vm8:busA1_R alsa_output.usb-046d_Headset:playback_FR".to_string(),
            ]
        );

        // Genau zwei Disconnects und zwei Connects, keine Fremdprogramme.
        let zweite_runde = &fake.calls()[4..8];
        let disconnects = zweite_runde.iter().filter(|c| c.args[0] == "-d").count();
        let connects = zweite_runde.iter().filter(|c| c.args.len() == 2).count();
        assert_eq!(disconnects, 2);
        assert_eq!(connects, 2);
        assert!(zweite_runde.iter().all(|c| c.program == "pw-link"));

        assert_eq!(map.get("A1").map(|s| s.name.as_str()), Some(neu.name.as_str()));
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn disconnect_fehler_wird_ignoriert() {
        let pfad = tmp_path("ignorieren");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        // Jeder Disconnect scheitert – assign muss trotzdem verbinden.
        let fake = FakeRunner::new().with_failure("pw-link -d");
        map.assign(&fake, "A3", &fehler_sink()).unwrap();
        let zeilen = fake.lines();
        assert_eq!(zeilen.len(), 4);
        assert_eq!(zeilen[2], "pw-link vm8:busA3_L alsa_output.usb-046d_Headset:playback_FL");
        assert_eq!(zeilen[3], "pw-link vm8:busA3_R alsa_output.usb-046d_Headset:playback_FR");
        assert!(map.get("A3").is_some());
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn connect_fehler_laesst_zuordnung_unveraendert() {
        let pfad = tmp_path("connectfehler");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = FakeRunner::new().with_failure("playback_FR");
        assert!(map.assign(&fake, "A2", &fehler_sink()).is_err());
        assert!(map.get("A2").is_none());
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn unbekannter_bus_wird_abgelehnt() {
        let pfad = tmp_path("bus");
        let fake = FakeRunner::new();
        let mut map = DeviceMap::new(&pfad);
        for bus in ["B2", "A6", "A0", "", "AI", "A1 x"] {
            let abgelehnt = |r: Result<(), String>| r.is_err();
            assert!(abgelehnt(map.assign(&fake, bus, &fehler_sink())), "'{bus}' abgelehnt");
            assert!(abgelehnt(map.clear(&fake, bus)), "'{bus}' abgelehnt");
        }
        assert!(fake.calls().is_empty(), "kein Systemaufruf bei ungültigem Bus");
        // Kleinschreibung wird auf die kanonische Form gebracht.
        assert_eq!(normalisiere_bus("a4").unwrap(), "A4");
        assert_eq!(normalisiere_bus(" A5 ").unwrap(), "A5");
        assert!(DeviceMap::new(&pfad).get("B1").is_none());
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn clear_loest_verbindung_und_leert_eintrag() {
        let pfad = tmp_path("clear");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = FakeRunner::new();
        map.assign(&fake, "A1", &fehler_sink()).unwrap();
        map.clear(&fake, "A1").unwrap();
        let zeilen = fake.lines();
        assert_eq!(zeilen.len(), 6);
        assert_eq!(zeilen[4], "pw-link -d vm8:busA1_L alsa_output.usb-046d_Headset:playback_FL");
        assert_eq!(zeilen[5], "pw-link -d vm8:busA1_R alsa_output.usb-046d_Headset:playback_FR");
        assert!(map.get("A1").is_none());
        // Zweites clear ist folgenlos.
        map.clear(&fake, "A1").unwrap();
        assert_eq!(fake.lines().len(), 6);
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn json_rundlauf_ueber_datei() {
        let pfad = tmp_path("rundlauf");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = FakeRunner::new();
        map.assign(&fake, "A1", &fehler_sink()).unwrap();
        map.assign(&fake, "A5", &Sink::new("alsa_output.hdmi", "HDMI / DisplayPort 2 Output"))
            .unwrap();

        let text = std::fs::read_to_string(&pfad).unwrap();
        assert!(text.contains("\"A1\""), "{text}");
        assert!(text.contains("Büro-Kopfhörer (USB) – Grün"), "Umlaute bleiben erhalten: {text}");

        let wieder = DeviceMap::load(&pfad);
        assert_eq!(wieder.get("A1"), Some(&fehler_sink()));
        assert_eq!(
            wieder.get("a1").map(|s| s.name.as_str()),
            Some("alsa_output.usb-046d_Headset")
        );
        assert_eq!(wieder.get("A5").map(|s| s.desc.as_str()), Some("HDMI / DisplayPort 2 Output"));
        assert!(wieder.get("A2").is_none());
        assert_eq!(wieder.entries().len(), 2);
        assert_eq!(wieder.to_json(), map.to_json());

        // Kaputte Datei -> leere Zuordnung, kein Absturz.
        std::fs::write(&pfad, "kein json").unwrap();
        assert!(DeviceMap::load(&pfad).entries().is_empty());
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn status_json_enthaelt_sinks_und_zuordnung() {
        let pfad = tmp_path("status");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = FakeRunner::new().with_output("pactl", PACTL_DE);
        map.assign(&fake, "A1", &fehler_sink()).unwrap();

        let json = map.status_json(&fake);
        let wert = crate::json::parse(&json).unwrap();
        let sinks = wert.get("sinks").unwrap().as_arr().unwrap();
        assert_eq!(sinks.len(), 2);
        assert_eq!(sinks[1].get("desc").unwrap(), &Value::str("Büro-Kopfhörer (USB) – Grün"));

        let busse = wert.get("buses").unwrap().as_arr().unwrap();
        assert_eq!(busse.len(), 5, "immer A1..A5");
        assert_eq!(busse[0].get("bus").unwrap(), &Value::str("A1"));
        assert_eq!(busse[0].get("sink").unwrap(), &Value::str("alsa_output.usb-046d_Headset"));
        assert_eq!(busse[1].get("sink").unwrap(), &Value::Null);
        assert_eq!(wert.get("error").unwrap(), &Value::Null);

        // Ohne erreichbares pactl bleibt das JSON gültig.
        let kaputt = FakeRunner::new().with_failure("pactl");
        let json = map.status_json(&kaputt);
        let wert = crate::json::parse(&json).unwrap();
        assert_eq!(wert.get("sinks").unwrap().as_arr().unwrap().len(), 0);
        assert!(wert.get("error").unwrap() != &Value::Null);
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn ports_und_standardpfad() {
        assert_eq!(bus_port("A1", "L"), "vm8:busA1_L");
        assert_eq!(bus_port("A5", "R"), "vm8:busA5_R");
        assert_eq!(fehler_sink().port("playback_FL"), "alsa_output.usb-046d_Headset:playback_FL");
        let pfad = default_path();
        assert!(pfad.ends_with("vm8-linux/devices.json"), "{}", pfad.display());
    }
}
