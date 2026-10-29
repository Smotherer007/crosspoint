//! Betreute Effekt-Inserts für Streifen und Busse.
//!
//! Crosspoint hostet **keine** Plugins selbst. Stattdessen wird ein fertiger,
//! stabiler Host betreut:
//!
//! * **LV2** über [`mod-host`](https://github.com/moddevices/mod-host),
//!   gesteuert über dessen TCP-Socket (Vorgabe-Port [`STANDARD_PORT`]).
//! * **VST2/VST3** optional über Carla (nur wenn installiert) – Carla wird
//!   **nicht** automatisch gestartet, wir melden nur Status und Startbefehl.
//!
//! # Warum ein fremder Host
//!
//! Ein eigener LV2-Host hieße: `liblilv`, `libsuil`, Worker-Threads,
//! State-Restore, Plugin-Crashes im Audio-Callback. `mod-host` kann das seit
//! Jahren und läuft als eigener JACK-Client neben der Engine; ein Absturz
//! eines Plugins reißt damit nicht den Mixer mit. Crosspoint bleibt Herr über
//! Pegel, Routing und Presets und schickt nur Befehle.
//!
//! # Wenn `mod-host` fehlt
//!
//! Nichts davon ist Pflicht: ist `mod-host` nicht installiert oder kommt keine
//! Verbindung zustande, bleibt [`status`] auf „inaktiv“, die Insert-Ketten
//! werden trotzdem geladen und gespeichert, und es gibt genau **eine** Logzeile
//! statt eines Fehlers. Kein Panic, kein Abbruch, kein Warten im Tonseil –
//! Start und Wiederverbinden laufen in einem eigenen Thread.
//!
//! # Protokoll (belegt an mod-host, `src/protocol.c`, `src/socket.c`)
//!
//! * Befehle sind eine Zeile, Argumente durch Leerzeichen getrennt,
//!   **ohne** Anführungszeichen (URIs und Symbole dürfen deshalb keine
//!   Leerzeichen enthalten – das prüfen wir vorher).
//! * Antworten enden mit `\0` (nicht mit Zeilenumbruch!) und lauten
//!   `resp <code>` bzw. `resp <code> <wert>` bei `param_get`.
//!   Negative Codes sind Fehler (`resp -1`). Protokollfehler kommen als
//!   Klartext, z. B. `error: unknown command`.
//! * Der Host wird mit `-n -p <port>` gestartet (kein Fork, damit der
//!   Kindprozess der Host selbst ist und sich sauber beenden lässt).
//! * Bypass-Bedeutung wie bei mod-host: `1` = Effekt umgangen.
//!
//! # Instanznummern
//!
//! mod-host adressiert Instanzen über Zahlen 0…9990. Damit sich Streifen und
//! Busse nicht ins Gehege kommen, ist die Nummer aus Kanal und Platz
//! abgeleitet, siehe [`Ziel::instanz`]: Streifen beginnen bei 100, Busse
//! bei 500, je Kanal [`MAX_INSERTS`] Plätze.
//!
//! # Zustand
//!
//! Je Streifen und Bus eine Kette aus [`Insert`] (URI, Bypass, Parameter),
//! gespeichert in `~/.config/crosspoint/fx.json` (über [`crate::pfade::datei`],
//! überschreibbar mit `CROSSPOINT_FX_FILE`). Aufbau wie die API-Ausgabe, nur
//! mit `version` davor; Indizes sind **1-basiert** wie in der Oberfläche
//! (Streifen 1…8, Busse 1…8 = A1…B5/B1…B3).
//!
//! # Verdrahtung im Graphen
//!
//! mod-host legt je Instanz einen JACK-Client `effect_<nummer>` an, dessen
//! Portnamen die **Symbolnamen des Plugins** sind (`in_l`, `out_l`, `gain` …)
//! – die kennt nur das jeweilige Plugin. Deshalb verkettet dieses Modul die
//! Audio-Ports nicht selbst: es liefert [`Verbindung::connect`] und
//! [`Verbindung::disconnect`] und die Portnamen der Engine über
//! [`Ziel::ports`] (`crosspoint:strip<n>_L/R`, `crosspoint:busA1_L/R`).
//! Wer eine Kette in den Signalweg hängen will, verbindet also
//! `crosspoint:strip1_L` → `effect_100:in_l` → … → `crosspoint:strip1_L`.
//! Automatisch zu raten wäre schlimmer als es zu unterlassen.
//!
//! # Umgebungsvariablen
//!
//! | Variable | Wirkung |
//! |---|---|
//! | `CROSSPOINT_MODHOST_PORT` | Socket-Port (Vorgabe [`STANDARD_PORT`] = 5556) |
//! | `CROSSPOINT_FX_FILE` | anderer Pfad für `fx.json` |
//! | `CROSSPOINT_FX_TIMEOUT_MS` | Zeitgrenze für Verbinden/Antwort (Vorgabe 800 ms) |
//! | `CROSSPOINT_NO_FX` | schaltet den Wächter-Thread ab |

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::engine::Params;
use crate::json::{self, Value};
use crate::{BUSES, BUS_NAMES, STRIPS, STRIP_NAMES};

// ---------------------------------------------------------------- Konstanten

/// Programmname des LV2-Hosts, wie er im `PATH` liegt.
pub const MODHOST: &str = "mod-host";

/// Socket-Port von mod-host, wenn `CROSSPOINT_MODHOST_PORT` nichts sagt.
///
/// 5556 statt der mod-host-Vorgabe 5555: die 5555 ist auf Entwicklungsrechnern
/// oft schon von einer MOD-Oberfläche belegt, und ein Fehlgriff dort würde
/// fremde Instanzen löschen.
pub const STANDARD_PORT: u16 = 5556;

/// Umgebungsvariable für den Port.
pub const PORT_VAR: &str = "CROSSPOINT_MODHOST_PORT";

/// Umgebungsvariable für einen anderen Ablageort der Ketten.
pub const DATEI_VAR: &str = "CROSSPOINT_FX_FILE";

/// Carla mit JACK-Unterstützung (`carla-jack-multi`), die VST-Brücke.
pub const CARLA: &str = "carla-jack-multi";

/// Flatpak-Kennung von Carla (KXStudio-Repository).
pub const CARLA_FLATPAK: &str = "studio.kx.carla";

/// So startet man Carla von Hand, wenn es installiert ist.
pub const CARLA_START: &str = "flatpak run studio.kx.carla";

/// Carla als klassisches JACK-Programm starten.
pub const CARLA_START_JACK: &str = "carla-jack-multi -J JACK";

/// Plätze je Kanal. Begrenzt durch die Instanznummern von mod-host (0…9990).
pub const MAX_INSERTS: usize = 16;

/// Vorgabe-Zeitgrenze für Verbindungsaufbau und Antwort.
pub const ZEITGRENZE_MS: u64 = 800;

/// Obergrenze für eine Antwortzeile (Schutz gegen unendliches Lesen).
const ANTWORT_MAX: usize = 4096;

// --------------------------------------------------------------------- Ziel

/// Ein Kanal, an dem eine Insert-Kette hängt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ziel {
    /// Streifen 0…7 (in der Oberfläche 1…8).
    Streifen(usize),
    /// Bus 0…7 (A1…A5, B1…B3).
    Bus(usize),
}

impl Ziel {
    /// Aus einem **1-basierten** Index, wie ihn die Oberfläche und `fx.json`
    /// benutzen.
    pub fn aus_index(bus: bool, index: usize) -> Option<Ziel> {
        let ziel = if bus {
            Ziel::Bus(index.checked_sub(1)?)
        } else {
            Ziel::Streifen(index.checked_sub(1)?)
        };
        ziel.ist_gueltig().then_some(ziel)
    }

    /// Aus Text: `streifen:1`, `strip:1`, `bus:A1`, `bus:2`.
    pub fn aus_text(text: &str) -> Option<Ziel> {
        let (art, rest) = text.trim().split_once(':')?;
        let rest = rest.trim();
        match art.trim().to_ascii_lowercase().as_str() {
            "streifen" | "strip" | "s" => {
                let n: usize = rest.parse().ok()?;
                Ziel::aus_index(false, n)
            }
            "bus" | "busse" | "b" => {
                if let Ok(n) = rest.parse::<usize>() {
                    return Ziel::aus_index(true, n);
                }
                let gross = rest.to_ascii_uppercase();
                BUS_NAMES
                    .iter()
                    .position(|name| *name == gross)
                    .map(Ziel::Bus)
            }
            _ => None,
        }
    }

    /// Nullbasierter Index im jeweiligen Feld.
    pub fn index(&self) -> usize {
        match self {
            Ziel::Streifen(i) => *i,
            Ziel::Bus(i) => *i,
        }
    }

    /// Ist der Kanal im Pult vorhanden?
    pub fn ist_gueltig(&self) -> bool {
        match self {
            Ziel::Streifen(i) => *i < STRIPS,
            Ziel::Bus(i) => *i < BUSES,
        }
    }

    /// Name für Log und Oberfläche.
    pub fn titel(&self) -> String {
        match self {
            Ziel::Streifen(i) => match STRIP_NAMES.get(*i) {
                Some(name) => format!("Streifen {} ({name})", i + 1),
                None => format!("Streifen {}", i + 1),
            },
            Ziel::Bus(i) => match BUS_NAMES.get(*i) {
                Some(name) => format!("Bus {name}"),
                None => format!("Bus {}", i + 1),
            },
        }
    }

    /// Instanznummer des Platzes `slot` in mod-host.
    ///
    /// Streifen bei 100, Busse bei 500, je Kanal [`MAX_INSERTS`] Plätze;
    /// bleibt unter der Obergrenze 9990 von mod-host.
    pub fn instanz(&self, slot: usize) -> u32 {
        let basis = match self {
            Ziel::Streifen(_) => 100,
            Ziel::Bus(_) => 500,
        };
        basis + (self.index() as u32) * (MAX_INSERTS as u32) + (slot as u32)
    }

    /// Portnamen der Engine für diesen Kanal (L, R).
    pub fn ports(&self) -> (String, String) {
        match self {
            Ziel::Streifen(i) => (
                format!("crosspoint:strip{}_L", i + 1),
                format!("crosspoint:strip{}_R", i + 1),
            ),
            Ziel::Bus(i) => {
                let name = BUS_NAMES.get(*i).copied().unwrap_or("A1");
                (
                    format!("crosspoint:bus{name}_L"),
                    format!("crosspoint:bus{name}_R"),
                )
            }
        }
    }
}

fn pruefe_ziel(ziel: Ziel) -> Result<(), String> {
    if ziel.ist_gueltig() {
        Ok(())
    } else {
        Err(format!("{}: diesen Kanal gibt es nicht", ziel.titel()))
    }
}

// ------------------------------------------------------------ Insert, Ketten

/// Ein Plugin in einer Insert-Kette.
#[derive(Clone, Debug, PartialEq)]
pub struct Insert {
    /// LV2-URI, z. B. `http://calf.sourceforge.net/plugins/Compressor`.
    pub uri: String,
    /// `true` = umgangen (`bypass 1` bei mod-host).
    pub bypass: bool,
    /// Steuerports in der Reihenfolge, in der sie gesetzt wurden.
    pub params: Vec<(String, f32)>,
}

impl Insert {
    /// Leerer Insert, ohne Bypass und Parameter.
    pub fn neu(uri: impl Into<String>) -> Insert {
        Insert {
            uri: uri.into(),
            bypass: false,
            params: Vec::new(),
        }
    }

    /// Insert mit einem gesetzten Parameter (Builder).
    pub fn mit_param(mut self, symbol: impl Into<String>, wert: f32) -> Insert {
        self.setze_param(symbol.into(), wert);
        self
    }

    /// Wert eines Parameters, falls gesetzt.
    pub fn param(&self, symbol: &str) -> Option<f32> {
        self.params
            .iter()
            .find(|(s, _)| s == symbol)
            .map(|(_, w)| *w)
    }

    /// Setzt einen Parameter; ein vorhandener Wert wird überschrieben, damit
    /// eine Kette nie zwei Werte für denselben Port hat.
    pub fn setze_param(&mut self, symbol: String, wert: f32) {
        if let Some(stelle) = self.params.iter_mut().find(|(s, _)| *s == symbol) {
            stelle.1 = wert;
        } else {
            self.params.push((symbol, wert));
        }
    }

    /// JSON-Form `{uri, bypass, params:{…}}`.
    pub fn als_json(&self) -> Value {
        let params = self
            .params
            .iter()
            .map(|(symbol, wert)| (symbol.clone(), Value::Num(*wert as f64)))
            .collect();
        Value::Obj(vec![
            ("uri".to_string(), Value::Str(self.uri.clone())),
            ("bypass".to_string(), Value::Bool(self.bypass)),
            ("params".to_string(), Value::Obj(params)),
        ])
    }
}

/// Der gesamte Insert-Zustand: je Streifen und Bus eine Kette.
#[derive(Clone, Debug, PartialEq)]
pub struct FxZustand {
    /// 8 Ketten, Index 0 = Streifen 1.
    pub streifen: Vec<Vec<Insert>>,
    /// 8 Ketten, Index 0 = Bus A1.
    pub busse: Vec<Vec<Insert>>,
}

impl Default for FxZustand {
    fn default() -> FxZustand {
        FxZustand::neu()
    }
}

impl FxZustand {
    /// Leerer Zustand mit allen Kanälen.
    pub fn neu() -> FxZustand {
        FxZustand {
            streifen: vec![Vec::new(); STRIPS],
            busse: vec![Vec::new(); BUSES],
        }
    }

    /// Kette eines Kanals (leer, wenn der Kanal nicht existiert).
    pub fn kette(&self, ziel: Ziel) -> &[Insert] {
        match ziel {
            Ziel::Streifen(i) => self.streifen.get(i).map(Vec::as_slice).unwrap_or(&[]),
            Ziel::Bus(i) => self.busse.get(i).map(Vec::as_slice).unwrap_or(&[]),
        }
    }

    /// Veränderbare Kette eines Kanals.
    pub fn kette_mut(&mut self, ziel: Ziel) -> Option<&mut Vec<Insert>> {
        match ziel {
            Ziel::Streifen(i) => self.streifen.get_mut(i),
            Ziel::Bus(i) => self.busse.get_mut(i),
        }
    }

    /// Speicherform (`fx.json`): `{version, streifen:[…], busse:[…]}`.
    pub fn als_json(&self) -> Value {
        Value::Obj(vec![
            ("version".to_string(), Value::Num(1.0)),
            ("streifen".to_string(), ketten_json(&self.streifen)),
            ("busse".to_string(), ketten_json(&self.busse)),
        ])
    }
}

fn ketten_json(ketten: &[Vec<Insert>]) -> Value {
    let eintraege = ketten
        .iter()
        .enumerate()
        .map(|(i, kette)| {
            Value::Obj(vec![
                ("index".to_string(), Value::Num((i + 1) as f64)),
                (
                    "kette".to_string(),
                    Value::Arr(kette.iter().map(Insert::als_json).collect()),
                ),
            ])
        })
        .collect();
    Value::Arr(eintraege)
}

// ------------------------------------------------------------- Prüfungen

fn pruefe_uri(uri: &str) -> Result<(), String> {
    if uri.is_empty() {
        return Err("URI fehlt".into());
    }
    if uri.len() > 512 {
        return Err("URI zu lang".into());
    }
    if uri.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("URI enthält Leerraum: {uri:?}"));
    }
    if !uri.contains(':') {
        return Err(format!("{uri:?} sieht nicht wie eine URI aus"));
    }
    Ok(())
}

fn pruefe_symbol(symbol: &str) -> Result<(), String> {
    if symbol.is_empty() {
        return Err("Parametername fehlt".into());
    }
    if symbol.len() > 128 {
        return Err("Parametername zu lang".into());
    }
    if !symbol
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        return Err(format!("Parametername unzulässig: {symbol:?}"));
    }
    Ok(())
}

fn pruefe_wert(wert: f32) -> Result<(), String> {
    if !wert.is_finite() {
        return Err(format!("Parameterwert unendlich: {wert}"));
    }
    Ok(())
}

// ----------------------------------------------------------- Befehlssatz

/// Zahl so schreiben, wie mod-host sie mit `atof` versteht: ohne Exponent,
/// ohne überflüssige Nullen.
fn zahl_text(wert: f32) -> String {
    if wert.fract() == 0.0 && wert.abs() < 1e7 {
        format!("{}", wert as i64)
    } else {
        format!("{wert}")
    }
}

/// `add <uri> <instanz>`
pub fn cmd_add(uri: &str, instanz: u32) -> String {
    format!("add {uri} {instanz}")
}

/// `remove <instanz>`
pub fn cmd_remove(instanz: u32) -> String {
    format!("remove {instanz}")
}

/// `param_set <instanz> <symbol> <wert>`
pub fn cmd_param_set(instanz: u32, symbol: &str, wert: f32) -> String {
    format!("param_set {instanz} {symbol} {}", zahl_text(wert))
}

/// `param_get <instanz> <symbol>`
pub fn cmd_param_get(instanz: u32, symbol: &str) -> String {
    format!("param_get {instanz} {symbol}")
}

/// `bypass <instanz> <0|1>` – `1` umgeht den Effekt.
pub fn cmd_bypass(instanz: u32, umgehen: bool) -> String {
    format!("bypass {instanz} {}", u8::from(umgehen))
}

/// `connect <quelle> <ziel>`
pub fn cmd_connect(von: &str, nach: &str) -> String {
    format!("connect {von} {nach}")
}

/// `disconnect <quelle> <ziel>`
pub fn cmd_disconnect(von: &str, nach: &str) -> String {
    format!("disconnect {von} {nach}")
}

/// Deutet eine Antwort von mod-host.
///
/// `Ok(None)` = `resp 0` ohne Wert, `Ok(Some(w))` = `resp 0 <wert>`,
/// `Err` = Fehlercode (`resp -1`), Protokollfehler oder leere Antwort.
pub fn antwort_deuten(antwort: &str) -> Result<Option<f32>, String> {
    let text = antwort.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    if text.is_empty() {
        return Err("leere Antwort vom Effekt-Host".into());
    }
    let mut teile = text.split_whitespace();
    if teile.next() != Some("resp") {
        return Err(format!("Effekt-Host: {text}"));
    }
    let code_text = teile.next().ok_or("Antwort ohne Code")?;
    let code: i32 = code_text
        .parse()
        .map_err(|_| format!("Codewert unlesbar: {text}"))?;
    if code < 0 {
        return Err(format!("Effekt-Host meldet Fehler {code}"));
    }
    match teile.next() {
        None => Ok(None),
        Some(wert) => wert
            .parse::<f32>()
            .map(Some)
            .map_err(|_| format!("Wert unlesbar: {text}")),
    }
}

// -------------------------------------------------------------- Leitung

/// Leitung, über die Befehle gehen – getrennt vom Befehlssatz, damit Tests
/// ohne echten `mod-host` prüfen können.
pub trait Leitung: Send {
    /// Schickt einen Befehl und liefert die Rohantwort.
    fn frage(&mut self, befehl: &str) -> Result<String, String>;
}

/// Zeitgrenze für Verbindung und Antwort.
pub fn zeitgrenze() -> Duration {
    let ms = std::env::var("CROSSPOINT_FX_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|ms| *ms >= 50)
        .unwrap_or(ZEITGRENZE_MS);
    Duration::from_millis(ms)
}

/// TCP-Leitung zum Socket von mod-host.
pub struct TcpLeitung {
    strom: TcpStream,
    grenze: Duration,
}

impl TcpLeitung {
    /// Verbindet nach `127.0.0.1:<port>`; scheitert innerhalb `grenze`.
    pub fn verbinde(port: u16, grenze: Duration) -> Result<TcpLeitung, String> {
        let adresse = SocketAddr::from(([127, 0, 0, 1], port));
        let strom = TcpStream::connect_timeout(&adresse, grenze)
            .map_err(|e| format!("Port {port}: {e}"))?;
        // Kleine Befehle, keine Verzögerung nötig.
        let _ = strom.set_nodelay(true);
        let _ = strom.set_read_timeout(Some(grenze));
        let _ = strom.set_write_timeout(Some(grenze));
        Ok(TcpLeitung { strom, grenze })
    }
}

impl Leitung for TcpLeitung {
    fn frage(&mut self, befehl: &str) -> Result<String, String> {
        // Kein Zeilenumbruch: mod-host verarbeitet den empfangenen Puffer als
        // Ganzes, ein angehängtes `\n` landete im letzten Argument.
        self.strom
            .write_all(befehl.as_bytes())
            .map_err(|e| format!("Senden: {e}"))?;
        self.strom.flush().map_err(|e| format!("Senden: {e}"))?;

        let mut roh = Vec::new();
        let mut puffer = [0u8; 256];
        loop {
            match self.strom.read(&mut puffer) {
                Ok(0) => break, // Gegenstelle hat geschlossen
                Ok(n) => {
                    for byte in &puffer[..n] {
                        // mod-host beendet Antworten mit \0; \n schadet nicht.
                        if *byte == 0 || *byte == b'\n' {
                            return Ok(String::from_utf8_lossy(&roh).into_owned());
                        }
                        roh.push(*byte);
                    }
                    if roh.len() > ANTWORT_MAX {
                        return Err("Antwort zu lang".into());
                    }
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Err(format!(
                        "keine Antwort binnen {} ms",
                        self.grenze.as_millis()
                    ))
                }
                Err(e) => return Err(format!("Empfangen: {e}")),
            }
        }
        if roh.is_empty() {
            return Err("Effekt-Host hat die Verbindung geschlossen".into());
        }
        Ok(String::from_utf8_lossy(&roh).into_owned())
    }
}

// ------------------------------------------------------------ Verbindung

/// Befehlssatz auf einer Leitung. Jede Methode schickt genau einen Befehl und
/// wertet die Antwort aus.
pub struct Verbindung {
    leitung: Box<dyn Leitung>,
}

impl Verbindung {
    /// Neue Verbindung über eine beliebige Leitung.
    pub fn neu(leitung: impl Leitung + 'static) -> Verbindung {
        Verbindung {
            leitung: Box::new(leitung),
        }
    }

    /// Verbindung zum laufenden Host an `port`.
    pub fn tcp(port: u16, grenze: Duration) -> Result<Verbindung, String> {
        Ok(Verbindung::neu(TcpLeitung::verbinde(port, grenze)?))
    }

    /// Schickt einen Befehl und liefert den optionalen Zahlenwert.
    pub fn roh(&mut self, befehl: &str) -> Result<Option<f32>, String> {
        let antwort = self.leitung.frage(befehl)?;
        antwort_deuten(&antwort)
    }

    /// Befehl, der nur „ok“ liefern darf.
    fn nur_ok(&mut self, befehl: &str) -> Result<(), String> {
        self.roh(befehl).map(|_| ())
    }

    /// Plugin-Instanz anlegen.
    pub fn add(&mut self, uri: &str, instanz: u32) -> Result<(), String> {
        pruefe_uri(uri)?;
        self.nur_ok(&cmd_add(uri, instanz))
    }

    /// Plugin-Instanz entfernen. Nicht vorhandene Instanzen sind kein Fehler
    /// (mod-host meldet dann `resp -1`).
    pub fn remove(&mut self, instanz: u32) -> Result<(), String> {
        self.nur_ok(&cmd_remove(instanz))
    }

    /// Steuerport setzen.
    pub fn param_set(&mut self, instanz: u32, symbol: &str, wert: f32) -> Result<(), String> {
        pruefe_symbol(symbol)?;
        pruefe_wert(wert)?;
        self.nur_ok(&cmd_param_set(instanz, symbol, wert))
    }

    /// Steuerport lesen.
    pub fn param_get(&mut self, instanz: u32, symbol: &str) -> Result<f32, String> {
        pruefe_symbol(symbol)?;
        self.roh(&cmd_param_get(instanz, symbol))?
            .ok_or_else(|| format!("param_get {instanz} {symbol}: kein Wert"))
    }

    /// Bypass schalten (`true` = Effekt umgangen).
    pub fn bypass(&mut self, instanz: u32, umgehen: bool) -> Result<(), String> {
        self.nur_ok(&cmd_bypass(instanz, umgehen))
    }

    /// Zwei JACK-Ports verbinden.
    pub fn connect(&mut self, von: &str, nach: &str) -> Result<(), String> {
        self.nur_ok(&cmd_connect(von, nach))
    }

    /// Zwei JACK-Ports trennen.
    pub fn disconnect(&mut self, von: &str, nach: &str) -> Result<(), String> {
        self.nur_ok(&cmd_disconnect(von, nach))
    }

    /// Lebenszeichen: CPU-Last des Hosts.
    pub fn cpu_load(&mut self) -> Result<f32, String> {
        self.roh("cpu_load")?
            .ok_or_else(|| "cpu_load: kein Wert".to_string())
    }

    /// Ist der Host noch da? Schickt einen billigen Befehl.
    pub fn lebendig(&mut self) -> Result<(), String> {
        self.cpu_load().map(|_| ())
    }
}

// ------------------------------------------------------------ Erkennung

/// Wie ein Programm im `PATH` gesucht wird. Als Funktionszeiger, damit Tests
/// ein „gefaktes which“ einsetzen können, ohne `PATH` zu verbiegen.
pub type Suche = fn(&str) -> Option<PathBuf>;

/// Programmsuche über `which` (mit Rückfall auf `PATH`).
pub fn welche(name: &str) -> Option<PathBuf> {
    if !name.is_empty() && !name.contains('/') {
        if let Ok(aus) = Command::new("which")
            .arg(name)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
        {
            if aus.status.success() {
                let pfad = String::from_utf8_lossy(&aus.stdout)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !pfad.is_empty() {
                    return Some(PathBuf::from(pfad));
                }
            }
        }
    }
    match std::env::var_os("PATH") {
        Some(pfade) => pfad_suche_in(&pfade, name),
        None => None,
    }
}

/// Sucht `name` in einem `PATH`-Wert (`:`-getrennt). Getrennt von der
/// Umgebungsvariable, damit es prüfbar ist.
pub fn pfad_suche_in(pfade: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('/') {
        return None;
    }
    for verzeichnis in std::env::split_paths(pfade) {
        let kandidat = verzeichnis.join(name);
        if kandidat.is_file() {
            return Some(kandidat);
        }
    }
    None
}

/// Programmsuche mit austauschbarer „which“-Funktion.
#[derive(Clone, Copy)]
pub struct Erkennung {
    suche: Suche,
    flatpak: bool,
}

impl Erkennung {
    /// Die echte Umgebung.
    pub fn echt() -> Erkennung {
        Erkennung {
            suche: welche,
            flatpak: true,
        }
    }

    /// Erkennung mit eigener Suchfunktion (Tests, eingeschränkte Umgebungen).
    pub fn mit_suche(suche: Suche) -> Erkennung {
        Erkennung {
            suche,
            flatpak: true,
        }
    }

    /// Flatpak nicht befragen – nötig in Tests, die kein Flatpak starten
    /// wollen.
    pub fn ohne_flatpak(mut self) -> Erkennung {
        self.flatpak = false;
        self
    }

    /// Findet ein Programm.
    pub fn finde(&self, name: &str) -> Option<PathBuf> {
        (self.suche)(name)
    }

    /// Ist Carla als Flatpak installiert? Fragt `flatpak info` – ohne den
    /// Host zu starten.
    pub fn flatpak_carla(&self) -> bool {
        if !self.flatpak {
            return false;
        }
        if (self.suche)("flatpak").is_none() {
            return false;
        }
        Command::new("flatpak")
            .args(["info", CARLA_FLATPAK])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

/// Ergebnis der Carla-Erkennung.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Carla {
    /// Nicht installiert – VST bleibt außen vor.
    #[default]
    NichtInstalliert,
    /// `carla-jack-multi` liegt im `PATH`.
    JackMulti(PathBuf),
    /// Carla ist als Flatpak `studio.kx.carla` installiert.
    Flatpak,
}

impl Carla {
    /// Ist eine VST-Brücke verfügbar?
    pub fn aktiv(&self) -> bool {
        !matches!(self, Carla::NichtInstalliert)
    }

    /// Kurzstatus für Log und JSON.
    pub fn status(&self) -> String {
        match self {
            Carla::NichtInstalliert => "nicht installiert".to_string(),
            Carla::JackMulti(pfad) => format!("installiert ({})", pfad.display()),
            Carla::Flatpak => format!("installiert (flatpak {CARLA_FLATPAK})"),
        }
    }

    /// Befehl, mit dem man Carla von Hand startet.
    pub fn startbefehl(&self) -> String {
        match self {
            Carla::JackMulti(pfad) => {
                let name = pfad
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| CARLA.to_string());
                format!("{name} -J JACK")
            }
            Carla::NichtInstalliert | Carla::Flatpak => CARLA_START.to_string(),
        }
    }

    /// JSON-Form `{aktiv, status, startbefehl, hinweis}`.
    pub fn als_json(&self) -> Value {
        let mut felder = vec![
            ("aktiv".to_string(), Value::Bool(self.aktiv())),
            ("status".to_string(), Value::Str(self.status())),
            ("startbefehl".to_string(), Value::Str(self.startbefehl())),
        ];
        if self.aktiv() {
            felder.push((
                "hinweis".to_string(),
                Value::Str(format!(
                    "VST2/VST3 nur über Carla: nicht automatisch gestartet, \
                     Start von Hand mit: {}. Danach Carla-VST-Rack auf die \
                     Crosspoint-Ports legen.",
                    self.startbefehl()
                )),
            ));
        }
        Value::Obj(felder)
    }
}

/// Erkennt Carla: erst das Programm, dann das Flatpak. Startet nichts.
pub fn carla_erkennen(erkennung: &Erkennung) -> Carla {
    if let Some(pfad) = erkennung.finde(CARLA) {
        return Carla::JackMulti(pfad);
    }
    if erkennung.flatpak_carla() {
        return Carla::Flatpak;
    }
    Carla::NichtInstalliert
}

// ------------------------------------------------------------ Ablage

/// Ablageort der Ketten.
pub fn pfad() -> PathBuf {
    if let Some(eigen) = std::env::var_os(DATEI_VAR).filter(|p| !p.is_empty()) {
        return PathBuf::from(eigen);
    }
    crate::pfade::datei("fx.json")
}

/// Port aus Text; alles Unbrauchbare fällt auf [`STANDARD_PORT`] zurück.
pub fn port_aus(wert: Option<&str>) -> u16 {
    wert.and_then(|s| s.trim().parse::<u16>().ok())
        .filter(|port| *port > 0)
        .unwrap_or(STANDARD_PORT)
}

/// Port für den Host.
pub fn port() -> u16 {
    port_aus(std::env::var(PORT_VAR).ok().as_deref())
}

/// Liest die Ketten. Fehlende oder kaputte Datei ⇒ leerer Zustand, kein Fehler.
pub fn zustand_laden(pfad: &Path) -> FxZustand {
    let text = match std::fs::read_to_string(pfad) {
        Ok(text) => text,
        Err(_) => return FxZustand::neu(),
    };
    match json::parse(&text) {
        Ok(wert) => zustand_aus_json(&wert),
        Err(fehler) => {
            eprintln!(
                "fx: {} unlesbar ({fehler}) - Ketten bleiben leer",
                pfad.display()
            );
            FxZustand::neu()
        }
    }
}

/// Baut den Zustand aus JSON; unbekannte Felder und Indizes werden
/// übersprungen, statt den Start zu verhindern.
pub fn zustand_aus_json(wert: &Value) -> FxZustand {
    let mut zustand = FxZustand::neu();
    for (schluessel, bus) in [("streifen", false), ("busse", true)] {
        let Some(liste) = wert.get(schluessel).and_then(Value::as_arr) else {
            continue;
        };
        for eintrag in liste {
            let index = eintrag
                .get("index")
                .and_then(|v| v.as_f64())
                .map(|n| n as usize)
                .unwrap_or(0);
            let Some(ziel) = Ziel::aus_index(bus, index) else {
                continue;
            };
            let kette: Vec<Insert> = eintrag
                .get("kette")
                .and_then(Value::as_arr)
                .map(|liste| liste.iter().filter_map(insert_aus_json).collect())
                .unwrap_or_default();
            if let Some(ziel_kette) = zustand.kette_mut(ziel) {
                *ziel_kette = kette;
            }
        }
    }
    zustand
}

fn insert_aus_json(wert: &Value) -> Option<Insert> {
    let uri = match wert.get("uri") {
        Some(Value::Str(s)) if !s.is_empty() => s.clone(),
        _ => return None,
    };
    let bypass = wert
        .get("bypass")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut params = Vec::new();
    if let Some(Value::Obj(paare)) = wert.get("params") {
        for (symbol, wert) in paare {
            if let Some(zahl) = wert.as_f64() {
                params.push((symbol.clone(), zahl as f32));
            }
        }
    }
    Some(Insert {
        uri,
        bypass,
        params,
    })
}

/// Schreibt die Ketten (erst daneben, dann umbenennen – nie halbe Dateien).
pub fn zustand_sichern(zustand: &FxZustand, pfad: &Path) -> Result<(), String> {
    if let Some(ordner) = pfad.parent().filter(|o| !o.as_os_str().is_empty()) {
        std::fs::create_dir_all(ordner)
            .map_err(|e| format!("{} anlegen: {e}", ordner.display()))?;
    }
    let text = zustand.als_json().to_json();
    let zwischen = pfad.with_extension("json.tmp");
    std::fs::write(&zwischen, text.as_bytes())
        .map_err(|e| format!("{} schreiben: {e}", zwischen.display()))?;
    std::fs::rename(&zwischen, pfad).map_err(|e| format!("{} ersetzen: {e}", pfad.display()))?;
    Ok(())
}

// ------------------------------------------------- Ketten-Operationen (pur)

/// Ersetzt die Kette eines Kanals. Prüft URIs, Symbole und Werte **vor** der
/// Änderung – ein abgelehnter Insert lässt die Kette unangetastet.
pub fn kette_setzen(zustand: &mut FxZustand, ziel: Ziel, kette: Vec<Insert>) -> Result<(), String> {
    pruefe_ziel(ziel)?;
    if kette.len() > MAX_INSERTS {
        return Err(format!(
            "{}: höchstens {MAX_INSERTS} Inserts, {} übergeben",
            ziel.titel(),
            kette.len()
        ));
    }
    for insert in &kette {
        pruefe_uri(&insert.uri)?;
        for (symbol, wert) in &insert.params {
            pruefe_symbol(symbol)?;
            pruefe_wert(*wert)?;
        }
    }
    match zustand.kette_mut(ziel) {
        Some(platz) => {
            *platz = kette;
            Ok(())
        }
        None => Err(format!("{}: diesen Kanal gibt es nicht", ziel.titel())),
    }
}

/// Hängt einen Insert hinten an und liefert seinen Platz.
pub fn kette_hinzufuegen(
    zustand: &mut FxZustand,
    ziel: Ziel,
    insert: Insert,
) -> Result<usize, String> {
    pruefe_ziel(ziel)?;
    let mut neue = zustand.kette(ziel).to_vec();
    if neue.len() >= MAX_INSERTS {
        return Err(format!("{}: höchstens {MAX_INSERTS} Inserts", ziel.titel()));
    }
    pruefe_uri(&insert.uri)?;
    neue.push(insert);
    let platz = neue.len() - 1;
    kette_setzen(zustand, ziel, neue)?;
    Ok(platz)
}

/// Entfernt den Insert an `platz` und liefert ihn zurück.
pub fn kette_entfernen(
    zustand: &mut FxZustand,
    ziel: Ziel,
    platz: usize,
) -> Result<Insert, String> {
    pruefe_ziel(ziel)?;
    let mut neue = zustand.kette(ziel).to_vec();
    if platz >= neue.len() {
        return Err(format!(
            "{}: Platz {platz} gibt es nicht ({} Inserts)",
            ziel.titel(),
            neue.len()
        ));
    }
    let insert = neue.remove(platz);
    kette_setzen(zustand, ziel, neue)?;
    Ok(insert)
}

/// Schaltet den Bypass eines Inserts und liefert den neuen Wert.
pub fn bypass_setzen(
    zustand: &mut FxZustand,
    ziel: Ziel,
    platz: usize,
    umgehen: bool,
) -> Result<bool, String> {
    pruefe_ziel(ziel)?;
    let kette = zustand
        .kette_mut(ziel)
        .ok_or_else(|| format!("{}: diesen Kanal gibt es nicht", ziel.titel()))?;
    let insert = kette
        .get_mut(platz)
        .ok_or_else(|| format!("{}: Platz {platz} gibt es nicht", ziel.titel()))?;
    insert.bypass = umgehen;
    Ok(umgehen)
}

/// Setzt einen Parameter eines Inserts.
pub fn param_setzen(
    zustand: &mut FxZustand,
    ziel: Ziel,
    platz: usize,
    symbol: &str,
    wert: f32,
) -> Result<(), String> {
    pruefe_ziel(ziel)?;
    pruefe_symbol(symbol)?;
    pruefe_wert(wert)?;
    let kette = zustand
        .kette_mut(ziel)
        .ok_or_else(|| format!("{}: diesen Kanal gibt es nicht", ziel.titel()))?;
    let insert = kette
        .get_mut(platz)
        .ok_or_else(|| format!("{}: Platz {platz} gibt es nicht", ziel.titel()))?;
    insert.setze_param(symbol.to_string(), wert);
    Ok(())
}

// --------------------------------------------------------------- Host-Status

/// Wer den Host betreibt.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Prozess {
    /// Kein Host, keine Verbindung.
    #[default]
    Keiner,
    /// Von Crosspoint gestartet (PID).
    Selbst(u32),
    /// Lief schon, bevor Crosspoint startete.
    Fremd,
}

impl Prozess {
    fn als_text(&self) -> String {
        match self {
            Prozess::Keiner => "keiner".to_string(),
            Prozess::Selbst(pid) => format!("selbst (pid {pid})"),
            Prozess::Fremd => "fremd".to_string(),
        }
    }
}

/// Zustand des Effekt-Hosts, wie ihn `/api/fx` meldet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HostStatus {
    /// Verbindung steht und Befehle gehen durch.
    pub aktiv: bool,
    /// Socket-Port.
    pub port: u16,
    /// Wer den Prozess betreibt.
    pub prozess: Prozess,
}

impl Default for HostStatus {
    fn default() -> HostStatus {
        HostStatus {
            aktiv: false,
            port: port(),
            prozess: Prozess::Keiner,
        }
    }
}

impl HostStatus {
    fn als_json(&self) -> Value {
        Value::Obj(vec![
            ("aktiv".to_string(), Value::Bool(self.aktiv)),
            ("port".to_string(), Value::Num(self.port as f64)),
            ("prozess".to_string(), Value::Str(self.prozess.als_text())),
        ])
    }
}

/// JSON für `/api/fx`: `{streifen, busse, host, carla}`.
pub fn als_json(zustand: &FxZustand, host: &HostStatus, carla: &Carla) -> Value {
    Value::Obj(vec![
        ("streifen".to_string(), ketten_json(&zustand.streifen)),
        ("busse".to_string(), ketten_json(&zustand.busse)),
        ("host".to_string(), host.als_json()),
        ("carla".to_string(), carla.als_json()),
    ])
}

// ------------------------------------------------------------ Host-Start

/// Ergebnis eines Startversuchs: Status, Verbindung, eigener Kindprozess.
struct Start {
    status: HostStatus,
    verbindung: Option<Verbindung>,
    kind: Option<Child>,
}

/// Sucht oder startet den Host und verbindet sich. Startet **nie** mehr als
/// einen Prozess und gibt bei jedem Problem einen inaktiven Status zurück.
fn host_starten(erkennung: &Erkennung, port: u16, grenze: Duration) -> Start {
    let inaktiv = HostStatus {
        aktiv: false,
        port,
        prozess: Prozess::Keiner,
    };

    // 1. Läuft schon einer? Dann übernehmen.
    if let Ok(verbindung) = Verbindung::tcp(port, grenze) {
        return Start {
            status: HostStatus {
                aktiv: true,
                port,
                prozess: Prozess::Fremd,
            },
            verbindung: Some(verbindung),
            kind: None,
        };
    }

    // 2. Selbst starten – nur wenn das Programm da ist.
    let Some(programm) = erkennung.finde(MODHOST) else {
        return Start {
            status: inaktiv,
            verbindung: None,
            kind: None,
        };
    };
    let mut kind = match Command::new(&programm)
        // -n: nicht forken, damit der Kindprozess der Host selbst ist.
        .args(["-n", "-p", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(kind) => kind,
        Err(fehler) => {
            eprintln!(
                "fx: mod-host nicht startbar ({}): {fehler}",
                programm.display()
            );
            return Start {
                status: inaktiv,
                verbindung: None,
                kind: None,
            };
        }
    };

    // 3. Auf den Socket warten, höchstens ein paar Sekundenbruchteile.
    let ende = Instant::now() + grenze * 8;
    loop {
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(verbindung) = Verbindung::tcp(port, grenze) {
            return Start {
                status: HostStatus {
                    aktiv: true,
                    port,
                    prozess: Prozess::Selbst(kind.id()),
                },
                verbindung: Some(verbindung),
                kind: Some(kind),
            };
        }
        match kind.try_wait() {
            Ok(Some(status)) => {
                eprintln!("fx: mod-host endet sofort (Status {status})");
                return Start {
                    status: inaktiv,
                    verbindung: None,
                    kind: None,
                };
            }
            Ok(None) => {}
            Err(fehler) => {
                eprintln!("fx: mod-host-Status unlesbar: {fehler}");
                return Start {
                    status: inaktiv,
                    verbindung: None,
                    kind: None,
                };
            }
        }
        if Instant::now() >= ende {
            eprintln!("fx: mod-host nimmt Port {port} nicht an - wird beendet");
            let _ = kind.kill();
            let _ = kind.wait();
            return Start {
                status: inaktiv,
                verbindung: None,
                kind: None,
            };
        }
    }
}

/// Schreibt eine ganze Kette in den Host: alte Instanzen weg, neue anlegen,
/// Parameter setzen, Bypass schalten.
fn kette_anwenden(verbindung: &mut Verbindung, ziel: Ziel, kette: &[Insert]) -> Result<(), String> {
    for platz in 0..MAX_INSERTS {
        // Fehlende Instanzen sind normal: mod-host antwortet mit Fehlercode.
        let _ = verbindung.remove(ziel.instanz(platz));
    }
    for (platz, insert) in kette.iter().enumerate() {
        let instanz = ziel.instanz(platz);
        verbindung
            .add(&insert.uri, instanz)
            .map_err(|e| format!("{} Platz {platz}: {e}", ziel.titel()))?;
        for (symbol, wert) in &insert.params {
            verbindung
                .param_set(instanz, symbol, *wert)
                .map_err(|e| format!("{} Platz {platz} {symbol}: {e}", ziel.titel()))?;
        }
        if insert.bypass {
            verbindung
                .bypass(instanz, true)
                .map_err(|e| format!("{} Platz {platz} Bypass: {e}", ziel.titel()))?;
        }
    }
    Ok(())
}

// --------------------------------------------------------- Laufender Zustand

struct Lauf {
    zustand: FxZustand,
    verbindung: Option<Verbindung>,
    host: HostStatus,
    kind: Option<Child>,
    carla: Carla,
    /// Wurde die Abwesenheit des Hosts schon gemeldet? (gegen Log-Spam)
    abwesenheit_gemeldet: bool,
}

impl Lauf {
    fn neu() -> Lauf {
        Lauf {
            zustand: zustand_laden(&pfad()),
            verbindung: None,
            host: HostStatus::default(),
            kind: None,
            carla: carla_erkennen(&Erkennung::echt()),
            abwesenheit_gemeldet: false,
        }
    }
}

static LAUF: Mutex<Option<Lauf>> = Mutex::new(None);

/// Sperrt den laufenden Zustand und legt ihn beim ersten Zugriff an.
/// Ein vergifteter Mutex ist hier harmlos: der Zustand bleibt lesbar.
fn lauf_sperren() -> std::sync::MutexGuard<'static, Option<Lauf>> {
    let mut guard = LAUF
        .lock()
        .unwrap_or_else(|vergiftet| vergiftet.into_inner());
    if guard.is_none() {
        *guard = Some(Lauf::neu());
    }
    guard
}

// -------------------------------------------------------------- Öffentlich

/// Startet die Betreuung des Effekt-Hosts (ein eigener Thread).
///
/// Aufruf aus `main` nach dem Verdrahten, z. B.:
///
/// ```ignore
/// crate::fx::spawn(params.clone());
/// ```
///
/// Der Aufruf kehrt sofort zurück. Fehlt `mod-host`, passiert nichts weiter
/// als eine Logzeile; Crosspoint läuft unverändert.
pub fn spawn(params: Arc<Params>) {
    if std::env::var_os("CROSSPOINT_NO_FX").is_some() {
        println!("fx: Inserts abgeschaltet (CROSSPOINT_NO_FX)");
        return;
    }
    // Zustand jetzt laden, damit `/api/fx` sofort antwortet.
    {
        let guard = lauf_sperren();
        if let Some(lauf) = guard.as_ref() {
            let anzahl: usize = lauf.zustand.streifen.iter().map(Vec::len).sum();
            println!(
                "fx: {} Inserts geladen ({} Streifen, {} Busse), mod-host Port {}",
                anzahl,
                params.strips.len(),
                params.buses.len(),
                lauf.host.port
            );
        }
    }

    std::thread::spawn(move || {
        let erkennung = Erkennung::echt();
        loop {
            let aktiv = pflege(&erkennung);
            // Ohne Host selten nachsehen: das spart Prozesse und Logzeilen.
            std::thread::sleep(if aktiv {
                Duration::from_secs(2)
            } else {
                Duration::from_secs(10)
            });
        }
    });
}

/// Ein Pflegedurchlauf: Verbindung prüfen, sonst aufbauen und Ketten anwenden.
/// Liefert, ob der Host danach verbunden ist.
fn pflege(erkennung: &Erkennung) -> bool {
    let mut guard = lauf_sperren();
    let Some(lauf) = guard.as_mut() else {
        return false;
    };

    // Verbindung prüfen.
    if let Some(verbindung) = lauf.verbindung.as_mut() {
        match verbindung.lebendig() {
            Ok(()) => return true,
            Err(fehler) => {
                eprintln!("fx: Verbindung zum Effekt-Host verloren: {fehler}");
                lauf.verbindung = None;
                lauf.host = HostStatus {
                    aktiv: false,
                    port: lauf.host.port,
                    prozess: Prozess::Keiner,
                };
                if let Some(mut kind) = lauf.kind.take() {
                    let _ = kind.kill();
                    let _ = kind.wait();
                }
            }
        }
    }

    // Neu aufbauen.
    let port = port();
    let start = host_starten(erkennung, port, zeitgrenze());
    lauf.host = start.status;
    lauf.verbindung = start.verbindung;
    if lauf.kind.is_none() {
        lauf.kind = start.kind;
    }

    let Some(_) = lauf.verbindung.as_ref() else {
        if !lauf.abwesenheit_gemeldet {
            eprintln!(
                "fx: kein Effekt-Host erreichbar (Port {port}) - Inserts bleiben aus, \
                 der Mixer laeuft weiter"
            );
            lauf.abwesenheit_gemeldet = true;
        }
        return false;
    };
    lauf.abwesenheit_gemeldet = false;
    println!(
        "fx: Effekt-Host bereit (Port {port}, {})",
        lauf.host.prozess.als_text()
    );

    // Gespeicherte Ketten nachziehen (nach einem Neustart des Hosts sind
    // dessen Instanzen weg).
    let mut ketten: Vec<(Ziel, Vec<Insert>)> = Vec::new();
    for i in 0..STRIPS {
        ketten.push((Ziel::Streifen(i), lauf.zustand.streifen[i].clone()));
    }
    for i in 0..BUSES {
        ketten.push((Ziel::Bus(i), lauf.zustand.busse[i].clone()));
    }
    if let Some(verbindung) = lauf.verbindung.as_mut() {
        for (ziel, kette) in ketten {
            if kette.is_empty() {
                continue;
            }
            if let Err(fehler) = kette_anwenden(verbindung, ziel, &kette) {
                eprintln!("fx: {} nicht uebernommen: {fehler}", ziel.titel());
            }
        }
    }
    true
}

/// Zustand des Hosts (für `/api/fx` und die Oberfläche).
pub fn status() -> HostStatus {
    lauf_sperren()
        .as_ref()
        .map(|lauf| lauf.host)
        .unwrap_or_default()
}

/// Erkannte VST-Brücke (Carla), ohne sie zu starten.
pub fn carla() -> Carla {
    lauf_sperren()
        .as_ref()
        .map(|lauf| lauf.carla.clone())
        .unwrap_or_default()
}

/// Aktuelle Ketten (Kopie).
pub fn listen() -> FxZustand {
    lauf_sperren()
        .as_ref()
        .map(|lauf| lauf.zustand.clone())
        .unwrap_or_default()
}

/// JSON für `GET /api/fx`.
pub fn api_json() -> Value {
    let guard = lauf_sperren();
    match guard.as_ref() {
        Some(lauf) => als_json(&lauf.zustand, &lauf.host, &lauf.carla),
        None => als_json(&FxZustand::neu(), &HostStatus::default(), &Carla::default()),
    }
}

/// Ersetzt die Kette eines Kanals: speichern und – wenn der Host läuft –
/// sofort übernehmen.
pub fn set_chain(ziel: Ziel, kette: Vec<Insert>) -> Result<(), String> {
    let mut guard = lauf_sperren();
    let lauf = guard.as_mut().ok_or("fx: Zustand nicht bereit")?;
    kette_setzen(&mut lauf.zustand, ziel, kette)?;
    zustand_sichern(&lauf.zustand, &pfad())?;
    let kette = lauf.zustand.kette(ziel).to_vec();
    if let Some(verbindung) = lauf.verbindung.as_mut() {
        if let Err(fehler) = kette_anwenden(verbindung, ziel, &kette) {
            eprintln!("fx: {} nicht uebernommen: {fehler}", ziel.titel());
        }
    }
    Ok(())
}

/// Schaltet den Bypass eines Inserts.
pub fn set_bypass(ziel: Ziel, platz: usize, umgehen: bool) -> Result<(), String> {
    let mut guard = lauf_sperren();
    let lauf = guard.as_mut().ok_or("fx: Zustand nicht bereit")?;
    bypass_setzen(&mut lauf.zustand, ziel, platz, umgehen)?;
    zustand_sichern(&lauf.zustand, &pfad())?;
    if let Some(verbindung) = lauf.verbindung.as_mut() {
        let instanz = ziel.instanz(platz);
        if let Err(fehler) = verbindung.bypass(instanz, umgehen) {
            eprintln!("fx: Bypass {} Platz {platz}: {fehler}", ziel.titel());
        }
    }
    Ok(())
}

/// Setzt einen Plugin-Parameter.
pub fn set_param(ziel: Ziel, platz: usize, symbol: &str, wert: f32) -> Result<(), String> {
    let mut guard = lauf_sperren();
    let lauf = guard.as_mut().ok_or("fx: Zustand nicht bereit")?;
    param_setzen(&mut lauf.zustand, ziel, platz, symbol, wert)?;
    zustand_sichern(&lauf.zustand, &pfad())?;
    if let Some(verbindung) = lauf.verbindung.as_mut() {
        let instanz = ziel.instanz(platz);
        if let Err(fehler) = verbindung.param_set(instanz, symbol, wert) {
            eprintln!(
                "fx: Parameter {symbol} {} Platz {platz}: {fehler}",
                ziel.titel()
            );
        }
    }
    Ok(())
}

/// Beendet einen selbst gestarteten Host (für das Herunterfahren).
pub fn beenden() {
    let mut guard = lauf_sperren();
    if let Some(lauf) = guard.as_mut() {
        lauf.verbindung = None;
        lauf.host = HostStatus {
            aktiv: false,
            port: lauf.host.port,
            prozess: Prozess::Keiner,
        };
        if let Some(mut kind) = lauf.kind.take() {
            let _ = kind.kill();
            let _ = kind.wait();
        }
    }
}

// ------------------------------------------------------------------- Tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Leitung, die Befehle mitschreibt und vorbereitete Antworten gibt.
    struct Mitschreiber {
        gesendet: Arc<StdMutex<Vec<String>>>,
        antworten: std::collections::VecDeque<String>,
    }

    impl Mitschreiber {
        fn neu(antworten: &[&str]) -> (Mitschreiber, Arc<StdMutex<Vec<String>>>) {
            let gesendet = Arc::new(StdMutex::new(Vec::new()));
            (
                Mitschreiber {
                    gesendet: gesendet.clone(),
                    antworten: antworten.iter().map(|a| a.to_string()).collect(),
                },
                gesendet,
            )
        }
    }

    impl Leitung for Mitschreiber {
        fn frage(&mut self, befehl: &str) -> Result<String, String> {
            self.gesendet
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(befehl.to_string());
            Ok(self
                .antworten
                .pop_front()
                .unwrap_or_else(|| "resp 0".into()))
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let ordner =
            std::env::temp_dir().join(format!("crosspoint-fx-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ordner);
        std::fs::create_dir_all(&ordner).unwrap();
        ordner
    }

    fn beispiel() -> Insert {
        Insert::neu("http://calf.sourceforge.net/plugins/Compressor")
            .mit_param("threshold", -18.0)
            .mit_param("ratio", 3.0)
    }

    // ------------------------------------------------- Befehlssatz-Format

    #[test]
    fn befehlsformat_add_und_param_set() {
        assert_eq!(
            cmd_add("http://calf.sourceforge.net/plugins/Compressor", 100),
            "add http://calf.sourceforge.net/plugins/Compressor 100"
        );
        assert_eq!(
            cmd_param_set(100, "threshold", -18.0),
            "param_set 100 threshold -18"
        );
        assert_eq!(cmd_param_set(101, "ratio", 2.5), "param_set 101 ratio 2.5");
        assert_eq!(cmd_param_get(101, "ratio"), "param_get 101 ratio");
        assert_eq!(cmd_remove(101), "remove 101");
        assert_eq!(cmd_bypass(101, true), "bypass 101 1");
        assert_eq!(cmd_bypass(101, false), "bypass 101 0");
        assert_eq!(
            cmd_connect("crosspoint:strip1_L", "effect_100:in_l"),
            "connect crosspoint:strip1_L effect_100:in_l"
        );
        assert_eq!(
            cmd_disconnect("crosspoint:strip1_L", "effect_100:in_l"),
            "disconnect crosspoint:strip1_L effect_100:in_l"
        );
    }

    #[test]
    fn verbindung_schickt_befehle_in_richtiger_form() {
        let (leitung, gesendet) =
            Mitschreiber::neu(&["resp 0", "resp 0", "resp 0", "resp 0 2.5000"]);
        let mut verbindung = Verbindung::neu(leitung);
        verbindung.add(&beispiel().uri, 100).unwrap();
        verbindung.param_set(100, "threshold", -18.0).unwrap();
        verbindung.bypass(100, true).unwrap();
        assert_eq!(verbindung.param_get(100, "ratio").unwrap(), 2.5);

        let gesendet = gesendet.lock().unwrap_or_else(|p| p.into_inner()).clone();
        assert_eq!(
            gesendet,
            vec![
                "add http://calf.sourceforge.net/plugins/Compressor 100".to_string(),
                "param_set 100 threshold -18".to_string(),
                "bypass 100 1".to_string(),
                "param_get 100 ratio".to_string(),
            ]
        );
    }

    #[test]
    fn antworten_werden_gedeutet() {
        assert_eq!(antwort_deuten("resp 0"), Ok(None));
        assert_eq!(antwort_deuten("resp 0\0"), Ok(None));
        assert_eq!(antwort_deuten("resp 0 2.5000"), Ok(Some(2.5)));
        assert!(antwort_deuten("resp -1").is_err());
        assert!(antwort_deuten("resp -21").is_err());
        assert!(antwort_deuten("error: unknown command").is_err());
        assert!(antwort_deuten("").is_err());
        assert!(antwort_deuten("resp").is_err());
    }

    #[test]
    fn fehler_wird_weitergereicht() {
        let (leitung, _) = Mitschreiber::neu(&["resp -1"]);
        let mut verbindung = Verbindung::neu(leitung);
        let fehler = verbindung
            .add("http://calf.sourceforge.net/plugins/Compressor", 100)
            .unwrap_err();
        assert!(fehler.contains("Fehler -1"), "{fehler}");
    }

    // ------------------------------------------------------ Kettenverwaltung

    #[test]
    fn kette_hinzufuegen_und_persistieren() {
        let ordner = temp("hinzufuegen");
        let datei = ordner.join("fx.json");
        let mut zustand = FxZustand::neu();

        assert_eq!(
            kette_hinzufuegen(&mut zustand, Ziel::Streifen(0), beispiel()).unwrap(),
            0
        );
        let zweiter = Insert::neu("http://plugin.org.uk/swh-plugins/plate");
        assert_eq!(
            kette_hinzufuegen(&mut zustand, Ziel::Streifen(0), zweiter).unwrap(),
            1
        );
        zustand_sichern(&zustand, &datei).unwrap();

        let geladen = zustand_laden(&datei);
        assert_eq!(geladen.kette(Ziel::Streifen(0)).len(), 2);
        assert_eq!(
            geladen.kette(Ziel::Streifen(0))[0].param("threshold"),
            Some(-18.0)
        );
        assert_eq!(
            geladen.kette(Ziel::Streifen(0))[1].uri,
            "http://plugin.org.uk/swh-plugins/plate"
        );
        // Andere Kanäle bleiben leer.
        assert!(geladen.kette(Ziel::Streifen(1)).is_empty());
        assert!(geladen.kette(Ziel::Bus(0)).is_empty());
        let _ = std::fs::remove_dir_all(&ordner);
    }

    #[test]
    fn kette_entfernen_bleibt_gespeichert() {
        let ordner = temp("entfernen");
        let datei = ordner.join("fx.json");
        let mut zustand = FxZustand::neu();
        kette_setzen(
            &mut zustand,
            Ziel::Bus(0),
            vec![
                beispiel(),
                Insert::neu("http://plugin.org.uk/swh-plugins/plate"),
            ],
        )
        .unwrap();
        zustand_sichern(&zustand, &datei).unwrap();

        let mut geladen = zustand_laden(&datei);
        let weg = kette_entfernen(&mut geladen, Ziel::Bus(0), 0).unwrap();
        assert_eq!(weg.uri, "http://calf.sourceforge.net/plugins/Compressor");
        zustand_sichern(&geladen, &datei).unwrap();

        let wieder = zustand_laden(&datei);
        assert_eq!(wieder.kette(Ziel::Bus(0)).len(), 1);
        assert_eq!(
            wieder.kette(Ziel::Bus(0))[0].uri,
            "http://plugin.org.uk/swh-plugins/plate"
        );
        assert!(kette_entfernen(&mut geladen, Ziel::Bus(0), 5).is_err());
        let _ = std::fs::remove_dir_all(&ordner);
    }

    #[test]
    fn bypass_toggeln_wird_gespeichert() {
        let ordner = temp("bypass");
        let datei = ordner.join("fx.json");
        let mut zustand = FxZustand::neu();
        kette_setzen(&mut zustand, Ziel::Streifen(3), vec![beispiel()]).unwrap();

        assert!(bypass_setzen(&mut zustand, Ziel::Streifen(3), 0, true).unwrap());
        zustand_sichern(&zustand, &datei).unwrap();
        assert!(zustand_laden(&datei).kette(Ziel::Streifen(3))[0].bypass);

        assert!(!bypass_setzen(&mut zustand, Ziel::Streifen(3), 0, false).unwrap());
        zustand_sichern(&zustand, &datei).unwrap();
        assert!(!zustand_laden(&datei).kette(Ziel::Streifen(3))[0].bypass);

        assert!(bypass_setzen(&mut zustand, Ziel::Streifen(3), 9, true).is_err());
        let _ = std::fs::remove_dir_all(&ordner);
    }

    #[test]
    fn parameter_setzen_ueberschreibt_statt_zu_haeufen() {
        let ordner = temp("param");
        let datei = ordner.join("fx.json");
        let mut zustand = FxZustand::neu();
        kette_setzen(&mut zustand, Ziel::Streifen(0), vec![beispiel()]).unwrap();

        param_setzen(&mut zustand, Ziel::Streifen(0), 0, "threshold", -6.0).unwrap();
        zustand_sichern(&zustand, &datei).unwrap();

        let geladen = zustand_laden(&datei);
        let insert = &geladen.kette(Ziel::Streifen(0))[0];
        assert_eq!(insert.param("threshold"), Some(-6.0));
        assert_eq!(
            insert
                .params
                .iter()
                .filter(|(s, _)| s == "threshold")
                .count(),
            1
        );
        assert_eq!(insert.param("ratio"), Some(3.0));
        let _ = std::fs::remove_dir_all(&ordner);
    }

    #[test]
    fn ungueltige_ketten_werden_abgelehnt_ohne_schaeden() {
        let mut zustand = FxZustand::neu();
        kette_setzen(&mut zustand, Ziel::Streifen(0), vec![beispiel()]).unwrap();

        // URI mit Leerzeichen würde das Socket-Protokoll zerlegen.
        let kaputt = Insert::neu("http://example.org/ kein-plugin");
        assert!(kette_setzen(&mut zustand, Ziel::Streifen(0), vec![kaputt]).is_err());
        assert_eq!(zustand.kette(Ziel::Streifen(0)).len(), 1);

        assert!(kette_setzen(&mut zustand, Ziel::Streifen(8), vec![]).is_err());
        assert!(kette_setzen(
            &mut zustand,
            Ziel::Streifen(0),
            vec![beispiel(); MAX_INSERTS + 1]
        )
        .is_err());
        assert!(param_setzen(&mut zustand, Ziel::Streifen(0), 0, "a b", 1.0).is_err());
        assert!(param_setzen(&mut zustand, Ziel::Streifen(0), 0, "ratio", f32::NAN).is_err());
    }

    #[test]
    fn ziel_wird_aus_text_erkannt() {
        assert_eq!(Ziel::aus_text("streifen:1"), Some(Ziel::Streifen(0)));
        assert_eq!(Ziel::aus_text("strip:8"), Some(Ziel::Streifen(7)));
        assert_eq!(Ziel::aus_text("bus:A1"), Some(Ziel::Bus(0)));
        assert_eq!(Ziel::aus_text("bus:b3"), Some(Ziel::Bus(7)));
        assert_eq!(Ziel::aus_text("bus:2"), Some(Ziel::Bus(1)));
        assert_eq!(Ziel::aus_text("streifen:9"), None);
        assert_eq!(Ziel::aus_text("bus:C9"), None);
        assert_eq!(Ziel::aus_text("quatsch"), None);
        assert_eq!(Ziel::Streifen(2).instanz(0), 132);
        assert_eq!(Ziel::Bus(0).instanz(3), 503);
        assert_eq!(Ziel::Streifen(7).instanz(15), 227);
        assert!(Ziel::Streifen(7).instanz(15) < 9990);
        assert_eq!(
            Ziel::Bus(0).ports(),
            (
                "crosspoint:busA1_L".to_string(),
                "crosspoint:busA1_R".to_string()
            )
        );
    }

    // ------------------------------------------------------------ Ablage

    #[test]
    fn kaputte_datei_ergibt_leeren_zustand() {
        let ordner = temp("kaputt");
        let datei = ordner.join("fx.json");
        assert_eq!(zustand_laden(&datei), FxZustand::neu()); // fehlt

        std::fs::write(&datei, "{ das ist kein json").unwrap();
        assert_eq!(zustand_laden(&datei), FxZustand::neu());

        std::fs::write(
            &datei,
            r#"{"streifen":[{"index":99,"kette":[{"uri":"x:y"}]},{"index":1,"kette":[{"uri":"http://a:b","bypass":true,"params":{"gain":-3}}]}]}"#,
        )
        .unwrap();
        let zustand = zustand_laden(&datei);
        assert_eq!(zustand.kette(Ziel::Streifen(0)).len(), 1);
        assert!(zustand.kette(Ziel::Streifen(0))[0].bypass);
        assert_eq!(
            zustand.kette(Ziel::Streifen(0))[0].param("gain"),
            Some(-3.0)
        );
        let _ = std::fs::remove_dir_all(&ordner);
    }

    #[test]
    fn speicherform_hat_version_und_alle_kanaele() {
        let zustand = FxZustand::neu();
        let text = zustand.als_json().to_json();
        let wert = json::parse(&text).unwrap();
        assert_eq!(wert.get("version").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(
            wert.get("streifen").unwrap().as_arr().unwrap().len(),
            STRIPS
        );
        assert_eq!(wert.get("busse").unwrap().as_arr().unwrap().len(), BUSES);
    }

    #[test]
    fn port_und_pfad_vorgaben() {
        assert_eq!(port_aus(None), STANDARD_PORT);
        assert_eq!(port_aus(Some("6000")), 6000);
        assert_eq!(port_aus(Some(" 6001 ")), 6001);
        assert_eq!(port_aus(Some("0")), STANDARD_PORT);
        assert_eq!(port_aus(Some("quatsch")), STANDARD_PORT);
        assert!(pfad().ends_with("crosspoint/fx.json") || pfad().ends_with("fx.json"));
    }

    // -------------------------------------------------- Verhalten ohne Host

    #[test]
    fn ohne_mod_host_kein_fehler() {
        // Suchfunktion, die nichts findet – wie ein Rechner ohne mod-host.
        fn nichts(_: &str) -> Option<PathBuf> {
            None
        }
        let erkennung = Erkennung::mit_suche(nichts).ohne_flatpak();
        assert!(erkennung.finde(MODHOST).is_none());

        // Freien Port besorgen und wieder freigeben: dort hört niemand zu.
        let port = {
            let horcher = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            horcher.local_addr().unwrap().port()
        };

        let start = host_starten(&erkennung, port, Duration::from_millis(120));
        assert!(!start.status.aktiv);
        assert_eq!(start.status.prozess, Prozess::Keiner);
        assert_eq!(start.status.port, port);
        assert!(start.verbindung.is_none());
        assert!(start.kind.is_none());

        // Der Zustand bleibt trotzdem nutzbar und speicherbar.
        let ordner = temp("ohne-host");
        let datei = ordner.join("fx.json");
        let mut zustand = FxZustand::neu();
        kette_hinzufuegen(&mut zustand, Ziel::Bus(1), beispiel()).unwrap();
        zustand_sichern(&zustand, &datei).unwrap();
        assert_eq!(zustand_laden(&datei).kette(Ziel::Bus(1)).len(), 1);
        let _ = std::fs::remove_dir_all(&ordner);
    }

    // ---------------------------------------------------------- Carla

    #[test]
    fn carla_erkennung_mit_gefaktem_which() {
        // Gefakt: `which` findet carla-jack-multi, mod-host dagegen nicht.
        fn mit_carla(name: &str) -> Option<PathBuf> {
            if name == CARLA {
                Some(PathBuf::from("/usr/bin/carla-jack-multi"))
            } else {
                None
            }
        }
        let gefunden = carla_erkennen(&Erkennung::mit_suche(mit_carla).ohne_flatpak());
        assert!(gefunden.aktiv());
        assert!(matches!(gefunden, Carla::JackMulti(_)));
        assert!(gefunden.status().contains("carla-jack-multi"));
        assert_eq!(gefunden.startbefehl(), "carla-jack-multi -J JACK");
        // Hinweis, aber kein Start.
        let json = gefunden.als_json().to_json();
        assert!(json.contains("\"aktiv\":true"));
        assert!(json.contains("nicht automatisch gestartet"));

        // Gefakt: nichts da, Flatpak nicht befragt.
        fn nichts(_: &str) -> Option<PathBuf> {
            None
        }
        let keine = carla_erkennen(&Erkennung::mit_suche(nichts).ohne_flatpak());
        assert!(!keine.aktiv());
        assert_eq!(keine, Carla::NichtInstalliert);
        assert_eq!(keine.status(), "nicht installiert");
        assert_eq!(keine.startbefehl(), CARLA_START);
        assert!(!keine.als_json().to_json().contains("hinweis"));
    }

    #[test]
    fn pfadsuche_findet_nur_ausfuehrbare_dateien() {
        use std::os::unix::fs::PermissionsExt;
        let ordner = temp("pfadsuche");
        let programm = ordner.join("carla-jack-multi");
        std::fs::write(&programm, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&programm, std::fs::Permissions::from_mode(0o755)).unwrap();
        let pfade = ordner.as_os_str();

        assert_eq!(
            pfad_suche_in(pfade, "carla-jack-multi"),
            Some(programm.clone())
        );
        assert_eq!(pfad_suche_in(pfade, "gibtesnicht"), None);
        let _ = std::fs::remove_dir_all(&ordner);
    }

    // -------------------------------------------------------- API-Ausgabe

    #[test]
    fn api_json_hat_die_vereinbarte_form() {
        let mut zustand = FxZustand::neu();
        kette_setzen(&mut zustand, Ziel::Streifen(0), vec![beispiel()]).unwrap();
        kette_setzen(
            &mut zustand,
            Ziel::Bus(5),
            vec![Insert::neu("http://plugin.org.uk/swh-plugins/plate")],
        )
        .unwrap();
        let host = HostStatus {
            aktiv: true,
            port: STANDARD_PORT,
            prozess: Prozess::Selbst(4711),
        };

        let wert = als_json(&zustand, &host, &Carla::Flatpak);
        let text = wert.to_json();
        let gelesen = json::parse(&text).unwrap();

        let streifen = gelesen.get("streifen").unwrap().as_arr().unwrap();
        assert_eq!(streifen.len(), STRIPS);
        assert_eq!(streifen[0].get("index").unwrap().as_f64(), Some(1.0));
        let kette = streifen[0].get("kette").unwrap().as_arr().unwrap();
        assert_eq!(kette.len(), 1);
        assert_eq!(
            kette[0].get("uri").unwrap(),
            &Value::Str("http://calf.sourceforge.net/plugins/Compressor".into())
        );
        assert_eq!(kette[0].get("bypass").unwrap().as_bool(), Some(false));
        assert_eq!(
            kette[0]
                .get("params")
                .unwrap()
                .get("threshold")
                .unwrap()
                .as_f64(),
            Some(-18.0)
        );

        let busse = gelesen.get("busse").unwrap().as_arr().unwrap();
        assert_eq!(busse[5].get("index").unwrap().as_f64(), Some(6.0));
        assert_eq!(busse[5].get("kette").unwrap().as_arr().unwrap().len(), 1);

        let host = gelesen.get("host").unwrap();
        assert_eq!(host.get("aktiv").unwrap().as_bool(), Some(true));
        assert_eq!(
            host.get("port").unwrap().as_f64(),
            Some(STANDARD_PORT as f64)
        );
        assert_eq!(
            host.get("prozess").unwrap(),
            &Value::Str("selbst (pid 4711)".into())
        );

        let carla = gelesen.get("carla").unwrap();
        assert_eq!(carla.get("aktiv").unwrap().as_bool(), Some(true));
        assert!(carla
            .get("startbefehl")
            .unwrap()
            .to_json()
            .contains("flatpak run studio.kx.carla"));
    }

    #[test]
    fn api_json_ohne_host_meldet_inaktiv() {
        let wert = als_json(
            &FxZustand::neu(),
            &HostStatus::default(),
            &Carla::NichtInstalliert,
        );
        let gelesen = json::parse(&wert.to_json()).unwrap();
        let host = gelesen.get("host").unwrap();
        assert_eq!(host.get("aktiv").unwrap().as_bool(), Some(false));
        assert_eq!(host.get("prozess").unwrap(), &Value::Str("keiner".into()));
        assert_eq!(
            gelesen
                .get("carla")
                .unwrap()
                .get("aktiv")
                .unwrap()
                .as_bool(),
            Some(false)
        );
    }
}
