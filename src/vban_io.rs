//! VBAN-Ausgabe für Bus A1.
//!
//! Der Audio-Thread schiebt seine Stereo-Blöcke über den SPSC-Ring aus
//! [`crate::tape_io::Ring`] (1024 Frames pro Block) in Richtung Netzwerk; ein
//! eigener Thread zieht sie heraus, baut daraus je einen float32-[`AudioFrame`]
//! und sendet ihn über [`vban::Sender`]. Im Echtzeitpfad liegen damit **kein
//! Lock, keine Allokation, kein Syscall** – nur zwei atomare Indizes.
//!
//! Kontrolle (ein/aus, Ziel, Streamname, Fehlertext) läuft über
//! [`VbanControl`] und wird von der Weboberfläche über [`VbanControl::status_json`]
//! bzw. [`VbanControl::set`] bedient.
//!
//! ## Festlegungen (bewusst, nicht geraten)
//!
//! 1. **Blockgröße 1024 Stereo-Frames** je VBAN-Paket. Das sind 8192 Byte
//!    Nutzdaten (float32, 2 Kanäle), mit 28-Byte-Header 8220 Byte pro Datagramm.
//!    Das liegt über der üblichen Ethernet-MTU (1500 Byte) und wird vom
//!    IP-Stack fragmentiert. Folge, die man kennen muss: ein Empfänger mit
//!    kleinem Puffer schneidet das Paket ab – `vban::Receiver` hat
//!    standardmäßig 2048 Byte und muss auf `with_buffer_size(8220..)` gehoben
//!    werden (der Test unten tut genau das). Wer Fragmentierung ganz vermeiden
//!    will, senkt `BLOCK_FRAMES` (z. B. 256 → 2076 Byte). Der Wert steht hier
//!    zentral, damit Ring und Sender immer dieselbe Blockgröße nutzen.
//! 2. **Port 1024..=65535.** Ports unter 1024 sind privilegiert; ein
//!    versehentlicher Wert wie 0 oder 80 soll nicht als "gültig" durchgehen.
//! 3. **Streamname: 1..=15 Zeichen druckbares ASCII ohne Leerzeichen
//!    (0x21..=0x7E).** Das VBAN-Crate selbst erlaubt 0x20..=0x7E; Leerzeichen
//!    sind hier zusätzlich verboten, weil der Name im Header NUL-terminiert
//!    liegt und Gegenstellen ihn häufig trimmen – ein Name mit Leerzeichen ist
//!    damit nicht eindeutig adressierbar. 15 Zeichen ist die harte Grenze des
//!    16-Byte-Headerfelds (NUL-Abschluss).
//! 4. **Host wird syntaktisch geprüft, nicht aufgelöst.** `set()` läuft im
//!    Webserver-Kontext; ein DNS-Timeout dort würde die Antwort blockieren.
//!    Auflösungsfehler landen stattdessen in `letzter_fehler` und der
//!    Sender-Thread versucht es beim nächsten Durchlauf erneut.
//! 5. **`gesendete_frames` zählt nur erfolgreich gesendete Pakete** (u64,
//!    Wrap-around ist gewollt). Der VBAN-Frame-Zähler im Header ist der
//!    niederwertige 32-Bit-Anteil davon, läuft also modulo 2^32 – genau wie es
//!    die Spezifikation für den Header vorsieht.
//! 6. **Abtastrate:** nur die Raten der VBAN-Tabelle sind übertragbar. Ein
//!    anderer Wert (z. B. 50000 Hz) wird auf 48000 gemappt **und** als Fehler
//!    gemeldet, statt still ein Paket mit falscher Rate zu senden. 44100 Hz ist
//!    über den Rate-Index 16 direkt übertragbar.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vban::{AudioFormat, AudioFrame, SampleRate, Sender};

use crate::json::Value;
use crate::tape_io::Ring;

/// Stereo-Frames je VBAN-Paket. Muss zur Blockgröße des Rings passen.
pub const BLOCK_FRAMES: usize = 1024;

/// Kleinster erlaubter Zielport.
pub const MIN_PORT: u16 = 1024;

/// Größter erlaubter Zielport.
pub const MAX_PORT: u16 = u16::MAX;

/// Länge des Streamnamens im VBAN-Header (ohne NUL).
pub const MAX_STREAM: usize = vban::MAX_STREAM_NAME;

/// Ziel und Streamname der VBAN-Ausgabe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VbanKonfig {
    /// Zielhost (IPv4/IPv6-Literal oder Hostname).
    pub host: String,
    /// UDP-Zielport, 1024..=65535.
    pub port: u16,
    /// VBAN-Streamname, 1..=15 Zeichen druckbares ASCII ohne Leerzeichen.
    pub stream: String,
}

impl Default for VbanKonfig {
    fn default() -> Self {
        VbanKonfig {
            host: "127.0.0.1".to_string(),
            port: vban::DEFAULT_PORT,
            stream: "crosspointbusA1".to_string(),
        }
    }
}

impl VbanKonfig {
    /// Baut eine Konfiguration, ohne sie zu prüfen (Prüfung: [`VbanKonfig::pruefe`]).
    pub fn neu(host: impl Into<String>, port: u16, stream: impl Into<String>) -> VbanKonfig {
        VbanKonfig {
            host: host.into(),
            port,
            stream: stream.into(),
        }
    }

    /// Prüft alle drei Felder; der erste Fehler wird im Klartext zurückgegeben.
    pub fn pruefe(&self) -> Result<(), String> {
        pruefe_host(&self.host)?;
        pruefe_port(self.port)?;
        pruefe_stream(&self.stream)?;
        Ok(())
    }

    /// Anzeigeform `host:port/stream` (nur für Status/Log).
    pub fn anzeige(&self) -> String {
        format!("{}:{}/{}", self.host, self.port, self.stream)
    }

    /// Zieladresse auflösen. Blockiert (DNS), daher nur im Sender-Thread.
    pub fn aufloesen(&self) -> Result<SocketAddr, String> {
        if let Ok(ip) = self.host.parse::<std::net::IpAddr>() {
            return Ok(SocketAddr::new(ip, self.port));
        }
        (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| format!("host '{}' nicht auflösbar: {e}", self.host))?
            .next()
            .ok_or_else(|| format!("host '{}' liefert keine Adresse", self.host))
    }
}

/// Portprüfung auf dem Rohwert, wie ihn die API liefert (`u32`).
///
/// Der Umweg über `u32` ist nötig, damit ein Wert wie 70000 überhaupt
/// ankommt und eine Meldung erzeugen kann – in einem `u16` wäre er schon beim
/// Parsen ein Überlauf und die Prüfung liefe ins Leere.
pub fn pruefe_port_wert(port: u32) -> Result<u16, String> {
    if (u32::from(MIN_PORT)..=u32::from(MAX_PORT)).contains(&port) {
        Ok(port as u16)
    } else {
        Err(format!(
            "port {port} ungültig: erlaubt sind {MIN_PORT}..={MAX_PORT}"
        ))
    }
}

/// Portprüfung: 1024..=65535.
pub fn pruefe_port(port: u16) -> Result<(), String> {
    pruefe_port_wert(u32::from(port)).map(|_| ())
}

/// Streamnamenprüfung: 1..=15 Zeichen druckbares ASCII, kein Leerzeichen.
pub fn pruefe_stream(stream: &str) -> Result<(), String> {
    let n = stream.chars().count();
    if n == 0 || n > MAX_STREAM {
        return Err(format!(
            "stream '{stream}' hat {n} Zeichen: erlaubt sind 1..={MAX_STREAM}"
        ));
    }
    if let Some(c) = stream.chars().find(|c| !c.is_ascii_graphic()) {
        return Err(format!(
            "stream '{stream}' enthält '{c}' (U+{:04X}): erlaubt sind nur druckbare ASCII-Zeichen 0x21..0x7E ohne Leerzeichen",
            c as u32
        ));
    }
    Ok(())
}

/// Hostprüfung: nicht leer, keine Leerraumzeichen, max. 255 Zeichen.
pub fn pruefe_host(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("host ist leer".to_string());
    }
    if host.len() > 255 {
        return Err(format!("host zu lang ({} Zeichen, max. 255)", host.len()));
    }
    if let Some(c) = host.chars().find(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "host '{host}' enthält '{c}': Leerraum und Steuerzeichen sind nicht erlaubt"
        ));
    }
    Ok(())
}

/// Steuerung der VBAN-Ausgabe aus der Weboberfläche/API.
///
/// `enabled` und `gesendete_frames` sind atomar, weil der Audio-/Sender-Thread
/// sie ohne Lock lesen bzw. schreiben muss. `letzter_fehler` und `konfig`
/// liegen hinter einem `Mutex` – zugegriffen wird nur aus dem Sender-Thread und
/// aus HTTP-Handlern, nie aus dem Audio-Callback.
#[derive(Debug)]
pub struct VbanControl {
    enabled: AtomicBool,
    gesendete_frames: AtomicU64,
    /// Zaehler der gemeldeten Fehler (fuer die Spalte „Errors“ im Dialog).
    fehler_anzahl: AtomicU64,
    letzter_fehler: Mutex<String>,
    konfig: Mutex<VbanKonfig>,
}

impl Default for VbanControl {
    fn default() -> Self {
        VbanControl::new()
    }
}

impl VbanControl {
    /// Aus (Standard), Ziel 127.0.0.1:6980, Stream `crosspointbusA1`, kein Fehler.
    pub fn new() -> VbanControl {
        VbanControl::mit_konfig(VbanKonfig::default())
    }

    /// Wie [`VbanControl::new`], aber mit vorgegebener Konfiguration.
    pub fn mit_konfig(konfig: VbanKonfig) -> VbanControl {
        VbanControl {
            enabled: AtomicBool::new(false),
            gesendete_frames: AtomicU64::new(0),
            fehler_anzahl: AtomicU64::new(0),
            letzter_fehler: Mutex::new(String::new()),
            konfig: Mutex::new(konfig),
        }
    }

    /// Ist die Ausgabe eingeschaltet?
    pub fn enabled(&self) -> bool {
        self.enabled.load(Relaxed)
    }

    /// Ein-/ausschalten. Beim Ausschalten bleibt der Zähler stehen.
    pub fn set_enabled(&self, an: bool) {
        self.enabled.store(an, Relaxed);
    }

    /// Anzahl erfolgreich gesendeter Pakete.
    pub fn gesendete_frames(&self) -> u64 {
        self.gesendete_frames.load(Relaxed)
    }

    /// Nächste Paketnummer (Wrap-around bei `u64::MAX` → 0).
    pub fn naechste_nummer(&self) -> u64 {
        self.gesendete_frames.load(Relaxed).wrapping_add(1)
    }

    /// Zähler setzen (Tests/Wrap-around, sonst nur der Sender-Thread).
    pub fn setze_frames(&self, n: u64) {
        self.gesendete_frames.store(n, Relaxed);
    }

    /// Letzter Fehlertext; leer, wenn keiner vorliegt.
    pub fn letzter_fehler(&self) -> String {
        self.letzter_fehler
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    /// Fehlertext setzen (leerer String löscht ihn).
    ///
    /// Jeder **neue** Fehlertext zählt mit – nicht jeder Aufruf, damit ein
    /// stehenbleibender Text die Spalte „Errors“ nicht inflationär füllt.
    pub fn set_fehler(&self, s: impl Into<String>) {
        let s = s.into();
        match self.letzter_fehler.lock() {
            Ok(mut g) => {
                if !s.is_empty() && *g != s {
                    self.fehler_anzahl.fetch_add(1, Relaxed);
                }
                *g = s;
            }
            Err(e) => {
                let mut g = e.into_inner();
                if !s.is_empty() && *g != s {
                    self.fehler_anzahl.fetch_add(1, Relaxed);
                }
                *g = s;
            }
        }
    }

    /// Anzahl gemeldeter Fehler (Spalte „Errors“ der Ausgangstabelle).
    pub fn fehler(&self) -> u64 {
        self.fehler_anzahl.load(Relaxed)
    }

    /// Fehlerzähler setzen (nur Tests).
    pub fn setze_fehler_anzahl(&self, n: u64) {
        self.fehler_anzahl.store(n, Relaxed);
    }

    /// Aktuelle Konfiguration (Kopie).
    pub fn konfig(&self) -> VbanKonfig {
        match self.konfig.lock() {
            Ok(g) => g.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    /// Ziel setzen, mit Prüfung von Host, Port und Streamname.
    ///
    /// Bei einem Fehler bleibt die alte Konfiguration unverändert stehen und der
    /// Text landet zusätzlich in `letzter_fehler` – so sieht die Oberfläche,
    /// warum die Änderung nicht übernommen wurde.
    pub fn set(&self, host: &str, port: u32, stream: &str) -> Result<(), String> {
        let geprueft = match pruefe_port_wert(port) {
            Ok(p) => VbanKonfig::neu(host, p, stream),
            Err(e) => {
                // Auch abgelehnte Werte müssen sichtbar werden, nicht nur
                // zurückgegeben: die Oberfläche zeigt `letzter_fehler`.
                self.set_fehler(format!("konfig: {e}"));
                return Err(e);
            }
        };
        if let Err(e) = geprueft.pruefe() {
            self.set_fehler(format!("konfig: {e}"));
            return Err(e);
        }
        match self.konfig.lock() {
            Ok(mut g) => *g = geprueft,
            Err(e) => *e.into_inner() = geprueft,
        }
        self.set_fehler(String::new());
        Ok(())
    }

    /// Status für die API als JSON-Objekt (Pflichtfelder: `enabled`, `host`,
    /// `port`, `stream`, `gesendete_frames`, `letzter_fehler`).
    pub fn status_json(&self) -> String {
        let k = self.konfig();
        Value::obj(vec![
            ("enabled", Value::Bool(self.enabled())),
            ("host", Value::Str(k.host)),
            ("port", Value::Num(f64::from(k.port))),
            ("stream", Value::Str(k.stream)),
            (
                "gesendete_frames",
                Value::Num(self.gesendete_frames() as f64),
            ),
            ("letzter_fehler", Value::Str(self.letzter_fehler())),
        ])
        .to_json()
    }
}

/// 64-Bit-Paketnummer auf den 32-Bit-Headerzähler abbilden (modulo 2^32).
pub fn als_zaehler(n: u64) -> u32 {
    n as u32
}

/// Baut aus einem interleaved Stereo-Block einen float32-VBAN-Frame.
///
/// `block` muss eine gerade Zahl von Samples enthalten (2 pro Frame); die
/// Prüfung übernimmt das VBAN-Crate. Reine Funktion ohne Zustand – im
/// Sender-Thread wird sie für jeden aus dem Ring gezogenen Block aufgerufen.
pub fn rahmen_aus_block(
    stream: &str,
    rate: SampleRate,
    zaehler: u32,
    block: &[f32],
) -> Result<AudioFrame, String> {
    if block.len() % 2 != 0 {
        return Err(format!(
            "block mit {} Samples ist nicht stereo (gerade Anzahl nötig)",
            block.len()
        ));
    }
    let mut frame = AudioFrame::float32(stream, rate, 2).map_err(|e| format!("frame: {e}"))?;
    frame
        .push_f32(block)
        .map_err(|e| format!("nutzdaten: {e}"))?;
    frame.set_frame_counter(zaehler);
    Ok(frame)
}

/// Abtastrate auf die VBAN-Raten abbilden. Unbekannte Raten werden auf 48 kHz
/// gemappt und als Hinweis zurückgegeben, statt still falsch zu senden.
pub fn rate_fuer(hz: u32) -> (SampleRate, Option<String>) {
    match SampleRate::from_hz(hz) {
        Ok(r) => (r, None),
        Err(e) => (
            SampleRate::Hz48000,
            Some(format!(
                "sample_rate {hz} Hz nicht VBAN-fähig ({e}), sende mit 48000 Hz"
            )),
        ),
    }
}

/// Startet den Sender-Thread.
///
/// Der Thread liest die Konfiguration bei jedem Durchlauf (billig, ein Lock im
/// Nicht-Echtzeitpfad), baut bei geändertem Ziel einen neuen [`Sender`] und
/// überträgt sonst alle wartenden Ringblöcke. Ist die Ausgabe aus, wird der Ring
/// weiter geleert, damit der Audio-Thread nie blockiert.
pub fn spawn_sender(
    ctl: Arc<VbanControl>,
    ring: Arc<Ring>,
    sample_rate: u32,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (rate, hinweis) = rate_fuer(sample_rate);
        if let Some(h) = hinweis {
            ctl.set_fehler(h);
        }
        let mut buf = vec![0.0f32; BLOCK_FRAMES * 2];
        let mut ziel: Option<(String, u16)> = None;
        let mut sender: Option<Sender> = None;

        loop {
            let k = ctl.konfig();
            let key = (k.host.clone(), k.port);
            if ziel.as_ref() != Some(&key) {
                match verbinde(&k) {
                    Ok(s) => {
                        sender = Some(s);
                        ziel = Some(key);
                        ctl.set_fehler(String::new());
                    }
                    Err(e) => {
                        ctl.set_fehler(format!("vban {}: {e}", k.anzeige()));
                        sender = None;
                        ziel = None;
                        // Kein Busy-Loop bei kaputtem Ziel.
                        thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                }
            }

            if !ctl.enabled() {
                while ring.pop(&mut buf).is_some() {}
                thread::sleep(Duration::from_millis(4));
                continue;
            }

            let mut gesendet = 0usize;
            while let Some(frames) = ring.pop(&mut buf) {
                let n = (frames * 2).min(buf.len());
                let nr = ctl.naechste_nummer();
                let frame = match rahmen_aus_block(&k.stream, rate, als_zaehler(nr), &buf[..n]) {
                    Ok(f) => f,
                    Err(e) => {
                        ctl.set_fehler(format!("vban {}: {e}", k.anzeige()));
                        continue;
                    }
                };
                match sender.as_ref() {
                    Some(s) => match s.send_frame(&frame) {
                        Ok(_) => {
                            ctl.setze_frames(nr);
                            gesendet += 1;
                        }
                        Err(e) => ctl.set_fehler(format!("vban senden: {e}")),
                    },
                    None => {}
                }
            }

            if gesendet == 0 {
                thread::sleep(Duration::from_millis(4));
            }
        }
    })
}

/// Baut den UDP-Sender für eine Konfiguration (Auflösung + Socket).
fn verbinde(k: &VbanKonfig) -> Result<Sender, String> {
    let addr = k.aufloesen()?;
    Sender::new(addr).map_err(|e| format!("socket: {e}"))
}

// ---------------------------------------------------------------------------
// Empfangsweg (VBAN -> SPSC-Ring -> JACK-Ports crosspoint:vban_L/R)
// ---------------------------------------------------------------------------
//
// Ein eigener Thread liest ein nicht blockierendes `UdpSocket`, prüft den
// Header und legt die Nutzdaten als Stereo-Samples in einen [`Ring`]. Der
// JACK-Callback zieht von dort und schreibt in die beiden Zusatzports
// `vban_L`/`vban_R`; diese lassen sich wie ein Hardware-Eingang per
// `pw-link crosspoint:vban_L crosspoint:strip1_L` auf einen Streifen legen.
//
// Festlegungen:
//
// 1. **Nur Format FLOAT32** (Format-Code 4). Andere Formate werden verworfen
//    und gezählt, nicht konvertiert – eine stille Fehlinterpretation von
//    Integer-Bytes als Float wäre schlimmer als ein verworfener Frame.
// 2. **Nur 1 oder 2 Kanäle.** Bei einem Kanal wird das Sample auf beide Seiten
//    dupliziert; mehr als zwei Kanäle werden verworfen (kein Downmix, weil
//    dessen Pegelregel hier nicht belegt ist).
// 3. **Ringblöcke fester Länge** ([`RECV_BLOCK_FRAMES`]). Ankommende Pakete
//    dürfen beliebig lang sein (bis [`RECV_BUFFER`]); der Empfänger-Thread
//    sammelt sie in [`Sammler`] und legt nur volle Blöcke ab. Damit stimmt die
//    Blocklänge im Ring mit der überein, die `Ring::pop` zurückmeldet.
// 4. **Streamfilter:** leer = alle Streams, sonst exakter Namensvergleich
//    (Groß-/Kleinschreibung zählt, so wie VBAN Namen behandelt).
// 5. **Bindet an 0.0.0.0**, damit auch Sender aus dem LAN ankommen.
// 6. **Ports 1..=65535** (0 wäre "vom System vergeben" und nicht adressierbar).

/// Standardport des Empfängers (VBAN-Standard).
pub const RECV_PORT_STANDARD: u16 = 6980;

/// Stereo-Frames je Ringblock des Empfangswegs.
pub const RECV_BLOCK_FRAMES: usize = BLOCK_FRAMES;

/// Größter Datagrammpuffer: Header plus ein voller Ringblock.
pub const RECV_BUFFER: usize = vban::HEADER_LEN + RECV_BLOCK_FRAMES * 2 * 4;

/// Warum ein Datagramm verworfen wurde.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvFehler {
    /// Kürzer als der 28-Byte-Header.
    ZuKurz { got: usize, need: usize },
    /// Erste vier Bytes sind nicht `VBAN`.
    FalschesMagic([u8; 4]),
    /// Sub-Protokoll ist nicht Audio (0x00) – z. B. MIDI, Text, RT-Packet.
    KeinAudio { sub: u8 },
    /// Codec ist nicht PCM (0x00).
    FalscherCodec { codec: u8 },
    /// Format-Code ist nicht 4 (32-Bit-Float).
    FalschesFormat { code: u8 },
    /// Kein gültiger Samplerate-Code.
    UnbekannteRate { code: u8 },
    /// Kanalzahl ist weder 1 noch 2.
    FalscheKanaele { kanaele: u16 },
    /// Nutzdaten passen nicht ganzzahlig auf Kanäle × 4 Byte.
    FalscheNutzdaten { len: usize, block: usize },
    /// Streamname passt nicht zum eingestellten Filter.
    FremderStream { stream: String },
}

impl std::fmt::Display for RecvFehler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecvFehler::ZuKurz { got, need } => {
                write!(f, "zu kurz: {got} Byte, mindestens {need} nötig")
            }
            RecvFehler::FalschesMagic(m) => {
                write!(f, "kein VBAN-Magic: {:?}", String::from_utf8_lossy(m))
            }
            RecvFehler::KeinAudio { sub } => write!(f, "Sub-Protokoll 0x{sub:02X} ist kein Audio"),
            RecvFehler::FalscherCodec { codec } => write!(f, "Codec 0x{codec:02X} ist nicht PCM"),
            RecvFehler::FalschesFormat { code } => {
                write!(f, "Format-Code {code} ist nicht FLOAT32 (4)")
            }
            RecvFehler::UnbekannteRate { code } => write!(f, "Samplerate-Code {code} unbekannt"),
            RecvFehler::FalscheKanaele { kanaele } => {
                write!(f, "{kanaele} Kanäle: erlaubt sind 1 oder 2")
            }
            RecvFehler::FalscheNutzdaten { len, block } => {
                write!(f, "{len} Byte Nutzdaten sind kein Vielfaches von {block}")
            }
            RecvFehler::FremderStream { stream } => {
                write!(f, "Stream '{stream}' passt nicht zum Filter")
            }
        }
    }
}

/// Ein geprüftes Empfangspaket: höchstens die ersten zwei Kanäle, interleaved.
#[derive(Debug, Clone, PartialEq)]
pub struct Empfangsframe {
    /// Streamname aus dem Header.
    pub stream: String,
    /// Abtastrate aus dem Header.
    pub sample_rate: SampleRate,
    /// Kanalzahl im Paket (1 oder 2).
    pub kanaele: u16,
    /// Stereo-Frames (Samples je Kanal nach der Duplizierung).
    pub frames: usize,
    /// Interleaved Stereo-Samples, `frames * 2` Stück.
    pub samples: Vec<f32>,
}

/// Prüft ein Datagramm und wandelt es in Stereo-Samples.
///
/// Reine Funktion ohne Zustand – damit im Empfänger-Thread und in den Tests
/// derselbe Code läuft.
pub fn parse_empfang(bytes: &[u8]) -> Result<Empfangsframe, RecvFehler> {
    if bytes.len() < vban::HEADER_LEN {
        return Err(RecvFehler::ZuKurz {
            got: bytes.len(),
            need: vban::HEADER_LEN,
        });
    }
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&bytes[0..4]);
    if magic != vban::MAGIC {
        return Err(RecvFehler::FalschesMagic(magic));
    }
    // Layout nach VBAN-Spezifikation (28 Byte Header):
    //   Byte 4: Bits 0..4 = Sample-Rate-Index, Bits 5..7 = Sub-Protokoll
    //   Byte 5: Samples pro Frame minus 1
    //   Byte 6: Kanaele minus 1
    //   Byte 7: Bits 0..2 = Datentyp (4 = FLOAT32), Bits 4..7 = Codec
    //   Bytes 8..24: Stream-Name (16 Zeichen), Bytes 24..28: Frame-Zaehler
    let sub = bytes[4] & 0xE0;
    if sub != 0 {
        return Err(RecvFehler::KeinAudio { sub });
    }
    let daten_typ = bytes[7] & 0x07;
    if daten_typ != AudioFormat::F32.code() {
        return Err(RecvFehler::FalschesFormat { code: daten_typ });
    }
    if bytes[7] & 0xF0 != 0 {
        return Err(RecvFehler::FalscherCodec {
            codec: bytes[7] & 0xF0,
        });
    }
    let rate_code = bytes[4] & 0x1F;
    let sample_rate = match SampleRate::from_code(rate_code) {
        Ok(r) => r,
        Err(_) => return Err(RecvFehler::UnbekannteRate { code: rate_code }),
    };
    // Byte 6: Kanalzahl minus eins.
    let kanaele = u16::from(bytes[6]) + 1;
    if kanaele != 1 && kanaele != 2 {
        return Err(RecvFehler::FalscheKanaele { kanaele });
    }
    let name_end = bytes[8..24].iter().position(|&b| b == 0).unwrap_or(16);
    let stream = String::from_utf8_lossy(&bytes[8..8 + name_end]).to_string();

    let rest = &bytes[vban::HEADER_LEN..];
    let block = 4 * kanaele as usize;
    if rest.len() % block != 0 {
        return Err(RecvFehler::FalscheNutzdaten {
            len: rest.len(),
            block,
        });
    }
    let einzel: Vec<f32> = rest
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let samples = if kanaele == 1 {
        // Mono auf beide Seiten duplizieren.
        let mut v = Vec::with_capacity(einzel.len() * 2);
        for s in einzel {
            v.push(s);
            v.push(s);
        }
        v
    } else {
        einzel
    };
    Ok(Empfangsframe {
        stream,
        sample_rate,
        kanaele,
        frames: samples.len() / 2,
        samples,
    })
}

/// Streamfilter: leerer Filter nimmt alles, sonst exakter Namensvergleich.
pub fn passt_stream(filter: &str, stream: &str) -> bool {
    filter.is_empty() || filter == stream
}

/// Sammelt ankommende Stereo-Samples zu Blöcken fester Länge für den Ring.
///
/// Ohne diesen Puffer würde `Ring::pop` immer die volle Slot-Länge melden,
/// auch wenn ein Paket kürzer war – der Callback bekäme dann stille Füllung
/// mitten im Signal.
pub struct Sammler {
    puffer: Vec<f32>,
    frames: usize,
    n: usize,
}

impl Sammler {
    /// `frames` Stereo-Frames pro Block.
    pub fn new(frames: usize) -> Sammler {
        let frames = frames.max(1);
        Sammler {
            puffer: vec![0.0f32; frames * 2],
            frames,
            n: 0,
        }
    }

    /// Hängt `samples` (interleaved stereo) an und legt volle Blöcke ab.
    ///
    /// Rückgabe: `(abgelegt, verworfen)`; `verworfen` zählt Blöcke, die der
    /// Ring nicht mehr aufnehmen konnte (Überlauf).
    pub fn schiebe(&mut self, ring: &Ring, samples: &[f32]) -> (usize, usize) {
        let mut abgelegt = 0;
        let mut verloren = 0;
        let total = samples.len() - samples.len() % 2;
        let mut i = 0;
        while i + 2 <= total {
            let frei = self.frames - self.n;
            let nehm = ((total - i) / 2).min(frei);
            if nehm == 0 {
                break;
            }
            let n = nehm * 2;
            let dst = self.n * 2;
            self.puffer[dst..dst + n].copy_from_slice(&samples[i..i + n]);
            self.n += nehm;
            i += n;
            if self.n == self.frames {
                if ring.push(&self.puffer[..self.frames * 2]) {
                    abgelegt += 1;
                } else {
                    verloren += 1;
                }
                self.n = 0;
            }
        }
        (abgelegt, verloren)
    }

    /// Wie viele Frames liegen noch im unfertigen Block?
    pub fn rest(&self) -> usize {
        self.n
    }
}

impl std::fmt::Debug for Sammler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sammler")
            .field("frames", &self.frames)
            .field("rest", &self.n)
            .finish()
    }
}

/// Linearen Pegel in dB umrechnen, mit −120 dB als Untergrenze.
fn lin_db(x: f32) -> f32 {
    let db = if x <= 1e-6 { -120.0 } else { 20.0 * x.log10() };
    ((db * 10.0).round() / 10.0).max(-120.0)
}

/// Eine Gegenstelle, von der Pakete ankommen (Zeile der Eingangstabelle).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Quelle {
    /// Streamname aus dem VBAN-Header.
    pub stream: String,
    /// Quell-IP des Absenders.
    pub ip: String,
    /// Abtastrate aus dem Header.
    pub sample_rate: u32,
    /// Kanalzahl im Paket.
    pub kanaele: u16,
    /// Angenommene Pakete dieses Absenders.
    pub packets: u64,
    /// Für diese Gegenstelle verworfene Pakete.
    pub errors: u64,
}

/// Steuerung und Zähler des Empfangswegs.
///
/// Alles, was der Empfänger-Thread und der HTTP-Handler gemeinsam brauchen,
/// liegt in Atomen; nur Streamname und Fehlertext sind hinter einem `Mutex`
/// (Zugriff ausschließlich außerhalb des Audio-Callbacks).
#[derive(Debug)]
pub struct VbanRecvControl {
    enabled: AtomicBool,
    port: AtomicU32,
    packets: AtomicU64,
    dropped: AtomicU64,
    level: [AtomicU32; 2],
    stream: Mutex<String>,
    fehler: Mutex<String>,
    /// Zuletzt gesehene Absender, höchstens [`MAX_QUELLEN`] Einträge.
    quellen: Mutex<Vec<Quelle>>,
}

/// So viele Absender zeigt die Eingangstabelle höchstens.
pub const MAX_QUELLEN: usize = 8;

impl Default for VbanRecvControl {
    fn default() -> Self {
        VbanRecvControl::new()
    }
}

impl VbanRecvControl {
    /// Aus, Port 6980, Streamfilter leer (alle).
    pub fn new() -> VbanRecvControl {
        VbanRecvControl {
            enabled: AtomicBool::new(false),
            port: AtomicU32::new(u32::from(RECV_PORT_STANDARD)),
            packets: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            level: [
                AtomicU32::new(0f32.to_bits()),
                AtomicU32::new(0f32.to_bits()),
            ],
            stream: Mutex::new(String::new()),
            fehler: Mutex::new(String::new()),
            quellen: Mutex::new(Vec::new()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Relaxed)
    }

    pub fn set_enabled(&self, an: bool) {
        self.enabled.store(an, Relaxed);
    }

    pub fn port(&self) -> u16 {
        self.port.load(Relaxed) as u16
    }

    /// Port setzen; 1..=65535. Bei Fehler bleibt der alte Wert stehen.
    pub fn set_port(&self, port: u32) -> Result<(), String> {
        if port == 0 || port > u32::from(u16::MAX) {
            let e = format!("empfangs-port {port} ungültig: erlaubt sind 1..=65535");
            self.set_fehler(e.clone());
            return Err(e);
        }
        self.port.store(port, Relaxed);
        Ok(())
    }

    pub fn stream(&self) -> String {
        match self.stream.lock() {
            Ok(g) => g.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    /// Streamfilter setzen; leerer String = alle Streams.
    pub fn set_stream(&self, s: &str) -> Result<(), String> {
        if s.len() > MAX_STREAM {
            let e = format!(
                "empfangs-stream '{s}' hat {} Zeichen: erlaubt sind 0..={MAX_STREAM}",
                s.len()
            );
            self.set_fehler(e.clone());
            return Err(e);
        }
        if let Some(c) = s.chars().find(|c| !c.is_ascii_graphic()) {
            let e = format!("empfangs-stream '{s}' enthält '{c}': nur druckbares ASCII");
            self.set_fehler(e.clone());
            return Err(e);
        }
        match self.stream.lock() {
            Ok(mut g) => *g = s.to_string(),
            Err(e) => *e.into_inner() = s.to_string(),
        }
        Ok(())
    }

    /// Angenommene Pakete.
    pub fn packets(&self) -> u64 {
        self.packets.load(Relaxed)
    }

    /// Verworfene Pakete (falscher Header, fremder Stream, voller Ring).
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Relaxed)
    }

    /// Zuletzt gemessener Spitzenpegel je Kanal, linear (0.0 …).
    pub fn level_linear(&self) -> [f32; 2] {
        [
            f32::from_bits(self.level[0].load(Relaxed)),
            f32::from_bits(self.level[1].load(Relaxed)),
        ]
    }

    /// Dasselbe in dB (−120 = Stille).
    pub fn level_db(&self) -> [f32; 2] {
        let l = self.level_linear();
        [lin_db(l[0]), lin_db(l[1])]
    }

    pub fn letzter_fehler(&self) -> String {
        match self.fehler.lock() {
            Ok(g) => g.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    pub fn set_fehler(&self, s: impl Into<String>) {
        match self.fehler.lock() {
            Ok(mut g) => *g = s.into(),
            Err(e) => *e.into_inner() = s.into(),
        }
    }

    /// Zählerstand setzen (nur Tests).
    pub fn setze_zaehler(&self, packets: u64, dropped: u64) {
        self.packets.store(packets, Relaxed);
        self.dropped.store(dropped, Relaxed);
    }

    /// Einen Fehler einer Gegenstelle zuordnen (die Errors-Spalte je Zeile).
    fn notiere_fehler(&self, ip: &str) {
        if ip.is_empty() {
            return;
        }
        let Ok(mut liste) = self.quellen.lock() else {
            return;
        };
        if let Some(q) = liste.iter_mut().find(|q| q.ip == ip) {
            q.errors += 1;
            return;
        }
        if liste.len() >= MAX_QUELLEN {
            liste.remove(0);
        }
        liste.push(Quelle {
            stream: "?".into(),
            ip: ip.to_string(),
            sample_rate: 0,
            kanaele: 0,
            packets: 0,
            errors: 1,
        });
    }

    fn zaehle_paket(&self) {
        self.packets.fetch_add(1, Relaxed);
    }

    fn zaehle_verworfen(&self) {
        self.dropped.fetch_add(1, Relaxed);
    }

    /// Pegel aus einem Paket fortschreiben (Spitze, Abfall Faktor 0,85).
    fn melde_pegel(&self, samples: &[f32]) {
        let mut peak = [0.0f32; 2];
        for f in samples.chunks_exact(2) {
            let l = f[0].abs();
            let r = f[1].abs();
            if l > peak[0] {
                peak[0] = l;
            }
            if r > peak[1] {
                peak[1] = r;
            }
        }
        for c in 0..2 {
            let old = f32::from_bits(self.level[c].load(Relaxed));
            let neu = peak[c].max(old * 0.85);
            self.level[c].store(neu.to_bits(), Relaxed);
        }
    }

    /// Pegel abklingen lassen (im Leerlauf des Empfänger-Threads).
    fn altere_pegel(&self) {
        for c in 0..2 {
            let old = f32::from_bits(self.level[c].load(Relaxed));
            self.level[c].store((old * 0.9).to_bits(), Relaxed);
        }
    }

    /// Ein Datagramm verarbeiten: prüfen, filtern, zählen, in den Ring legen.
    ///
    /// Ohne Socket – so ist der ganze Weg (Header-Prüfung, Filter, Zähler,
    /// Ring) im Unit-Test prüfbar. Die Quell-IP bleibt hier leer.
    pub fn verarbeite(
        &self,
        sammler: &mut Sammler,
        ring: &Ring,
        bytes: &[u8],
    ) -> Result<Empfangsframe, RecvFehler> {
        self.verarbeite_von(sammler, ring, bytes, "")
    }

    /// Wie [`VbanRecvControl::verarbeite`], merkt sich aber zusätzlich die
    /// Gegenstelle (`ip`) für die Eingangstabelle des Dialogs.
    pub fn verarbeite_von(
        &self,
        sammler: &mut Sammler,
        ring: &Ring,
        bytes: &[u8],
        ip: &str,
    ) -> Result<Empfangsframe, RecvFehler> {
        let frame = match parse_empfang(bytes) {
            Ok(f) => f,
            Err(e) => {
                self.zaehle_verworfen();
                self.notiere_fehler(ip);
                self.set_fehler(format!("verworfen: {e}"));
                return Err(e);
            }
        };
        let filter = self.stream();
        if !passt_stream(&filter, &frame.stream) {
            let e = RecvFehler::FremderStream {
                stream: frame.stream.clone(),
            };
            self.zaehle_verworfen();
            self.notiere_fehler(ip);
            self.set_fehler(format!("verworfen: '{filter}' != '{}'", frame.stream));
            return Err(e);
        }
        self.zaehle_paket();
        self.melde_pegel(&frame.samples);
        self.notiere_quelle(ip, &frame);
        let (_, verloren) = sammler.schiebe(ring, &frame.samples);
        for _ in 0..verloren {
            self.zaehle_verworfen();
        }
        Ok(frame)
    }

    /// Gegenstelle fortschreiben: neue Zeile oder Zähler der bekannten erhöhen.
    fn notiere_quelle(&self, ip: &str, frame: &Empfangsframe) {
        if ip.is_empty() {
            return;
        }
        let Ok(mut liste) = self.quellen.lock() else {
            return;
        };
        let rate = frame.sample_rate.hz();
        if let Some(q) = liste
            .iter_mut()
            .find(|q| q.ip == ip && q.stream == frame.stream)
        {
            q.packets += 1;
            q.sample_rate = rate;
            q.kanaele = frame.kanaele;
            return;
        }
        if liste.len() >= MAX_QUELLEN {
            liste.remove(0);
        }
        liste.push(Quelle {
            stream: frame.stream.clone(),
            ip: ip.to_string(),
            sample_rate: rate,
            kanaele: frame.kanaele,
            packets: 1,
            errors: 0,
        });
    }

    /// Zuletzt gesehene Gegenstellen (für `GET /api/vban/config`).
    pub fn quellen(&self) -> Vec<Quelle> {
        self.quellen
            .lock()
            .map(|q| q.clone())
            .unwrap_or_else(|e| e.into_inner().clone())
    }

    /// JSON für `GET /api/vban/recv`.
    pub fn status_json(&self) -> String {
        let l = self.level_db();
        Value::obj(vec![
            ("enabled", Value::Bool(self.enabled())),
            ("port", Value::Num(f64::from(self.port()))),
            ("stream", Value::Str(self.stream())),
            ("packets", Value::Num(self.packets() as f64)),
            ("dropped", Value::Num(self.dropped() as f64)),
            ("level_left", Value::Num(f64::from(l[0]))),
            ("level_right", Value::Num(f64::from(l[1]))),
            ("letzter_fehler", Value::Str(self.letzter_fehler())),
        ])
        .to_json()
    }
}

/// Bindet den Empfangssocket an `0.0.0.0:port`, nicht blockierend.
fn binde_empfang(port: u16) -> Result<UdpSocket, String> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port);
    let sock = UdpSocket::bind(addr).map_err(|e| format!("bind {addr}: {e}"))?;
    sock.set_nonblocking(true)
        .map_err(|e| format!("nonblocking auf {addr}: {e}"))?;
    Ok(sock)
}

/// Startet den Empfänger-Thread.
///
/// Der Thread ist ein Aufseher: er liest die gewünschte Konfiguration aus der
/// [`VbanRecvControl`] und hält den Socket passend dazu.
///
/// * aus → Socket wird geschlossen, 20 ms Pause,
/// * (Port, Stream) geändert → Socket wird neu gebunden,
/// * sonst → alle wartenden Datagramme werden abgearbeitet; ist keins da,
///   schläft der Thread 2 ms (kein Busy-Loop).
///
/// Bindefehler (Port belegt, Port < 1024 ohne Rechte) landen in
/// `letzter_fehler`; der Thread versucht es alle 250 ms erneut.
pub fn spawn_empfaenger(ctl: Arc<VbanRecvControl>, ring: Arc<Ring>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = vec![0u8; RECV_BUFFER];
        let mut sammler = Sammler::new(RECV_BLOCK_FRAMES);
        let mut sock: Option<UdpSocket> = None;
        let mut aktiv: Option<(u16, String)> = None;
        let mut letzte_alterung = Instant::now();

        loop {
            if !ctl.enabled() {
                sock = None;
                aktiv = None;
                if !ctl.letzter_fehler().is_empty() {
                    ctl.set_fehler(String::new());
                }
                sammler = Sammler::new(RECV_BLOCK_FRAMES);
                thread::sleep(Duration::from_millis(20));
                continue;
            }

            let gewuenscht = (ctl.port(), ctl.stream());
            if aktiv.as_ref() != Some(&gewuenscht) {
                sock = None;
                aktiv = None;
                match binde_empfang(gewuenscht.0) {
                    Ok(s) => {
                        sock = Some(s);
                        aktiv = Some(gewuenscht);
                        ctl.set_fehler(String::new());
                    }
                    Err(e) => {
                        ctl.set_fehler(format!("vban recv {}: {e}", gewuenscht.0));
                        thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                }
            }

            let mut empfangen = 0usize;
            if let Some(s) = sock.as_ref() {
                loop {
                    match s.recv_from(&mut buf) {
                        Ok((n, src)) => {
                            empfangen += 1;
                            let ip = src.ip().to_string();
                            let _ = ctl.verarbeite_von(&mut sammler, &ring, &buf[..n], &ip);
                        }
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                || e.kind() == std::io::ErrorKind::TimedOut =>
                        {
                            break
                        }
                        Err(e) => {
                            ctl.set_fehler(format!("vban recv: {e}"));
                            break;
                        }
                    }
                }
            }

            if empfangen == 0 {
                if letzte_alterung.elapsed() >= Duration::from_millis(50) {
                    ctl.altere_pegel();
                    letzte_alterung = Instant::now();
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
    })
}

/* ==================================================================
Konfiguration für den Dialog „IP Audio Network Configuration“
================================================================== */

/// Ziel der empfangenen Streams im Pult. Die Engine legt empfangenes
/// VBAN-Audio auf die beiden Zusatzports; von dort verdrahtet der Nutzer es
/// per `pw-link` auf einen Streifen.
pub const ZIEL_EINGANG: &str = "vban_L/R";

/// Anteil in Prozent für die Spalte „Quality“.
///
/// Ohne Verkehr gibt es keine Aussage: dann steht ein Gedankenstrich, keine
/// erfundene 100 %.
fn prozent(gut: u64, schlecht: u64) -> String {
    let gesamt = gut.saturating_add(schlecht);
    if gesamt == 0 {
        return "–".to_string();
    }
    let p = (gut as f64 * 100.0 / gesamt as f64).round() as u32;
    format!("{p}%")
}

/// Alles, was der VBAN-Dialog zeigt: Sende- und Empfangsweg, Incoming- und
/// Outgoing-Tabelle.
pub fn konfig_json(send: &VbanControl, recv: &VbanRecvControl, sample_rate: u32) -> String {
    let k = send.konfig();
    let senden = Value::obj(vec![
        ("an", Value::Bool(send.enabled())),
        ("host", Value::Str(k.host.clone())),
        ("port", Value::Num(f64::from(k.port))),
        ("stream", Value::Str(k.stream.clone())),
        ("frames", Value::Num(send.gesendete_frames() as f64)),
        ("fehler", Value::Num(send.fehler() as f64)),
        ("letzter_fehler", Value::Str(send.letzter_fehler())),
    ]);
    let empfangen = Value::obj(vec![
        ("an", Value::Bool(recv.enabled())),
        ("port", Value::Num(f64::from(recv.port()))),
        ("stream", Value::Str(recv.stream())),
        ("pakete", Value::Num(recv.packets() as f64)),
        ("verworfen", Value::Num(recv.dropped() as f64)),
        ("letzter_fehler", Value::Str(recv.letzter_fehler())),
    ]);
    let incoming: Vec<Value> = recv
        .quellen()
        .into_iter()
        .map(|q| {
            Value::obj(vec![
                ("stream", Value::Str(q.stream)),
                ("ip", Value::Str(q.ip)),
                ("sr", Value::Num(f64::from(q.sample_rate))),
                ("ch", Value::Num(f64::from(q.kanaele))),
                ("format", Value::Str("FLOAT32".into())),
                ("quality", Value::Str(prozent(q.packets, q.errors))),
                ("ziel", Value::Str(ZIEL_EINGANG.into())),
                ("errors", Value::Num(q.errors as f64)),
            ])
        })
        .collect();
    // Die Ausgangszeile steht immer: sie zeigt das eingestellte Ziel, auch
    // wenn gerade nichts gesendet wird.
    let outgoing = vec![Value::obj(vec![
        ("source", Value::Str("Bus A1".into())),
        ("stream", Value::Str(k.stream.clone())),
        ("ip", Value::Str(k.host.clone())),
        ("sr", Value::Num(f64::from(sample_rate))),
        ("ch", Value::Num(2.0)),
        ("format", Value::Str("FLOAT32".into())),
        (
            "quality",
            Value::Str(prozent(send.gesendete_frames(), send.fehler())),
        ),
        ("errors", Value::Num(send.fehler() as f64)),
    ])];
    Value::obj(vec![
        ("senden", senden),
        ("empfangen", empfangen),
        ("samplerate", Value::Num(f64::from(sample_rate))),
        ("incoming", Value::Arr(incoming)),
        ("outgoing", Value::Arr(outgoing)),
    ])
    .to_json()
}

/// Sende- und Empfangsweg aus einem JSON setzen.
///
/// Erst prüfen, dann setzen: ein ungültiger Port lässt die übrigen Felder
/// unangetastet, statt halb übernommen zu werden.
pub fn wende_konfig_an(
    send: &VbanControl,
    recv: &VbanRecvControl,
    v: &Value,
) -> Result<(), String> {
    let text = |o: &Value, k: &str| {
        o.get(k).and_then(|x| match x {
            Value::Str(s) => Some(s.clone()),
            _ => None,
        })
    };
    let zahl = |o: &Value, k: &str| o.get(k).and_then(|x| x.as_f64());
    let an = |o: &Value, k: &str| o.get(k).and_then(|x| x.as_bool());

    if let Some(sd) = v.get("senden") {
        let k = send.konfig();
        let host = text(sd, "host").unwrap_or(k.host);
        let port = zahl(sd, "port")
            .map(|p| p as u32)
            .unwrap_or(u32::from(k.port));
        let stream = text(sd, "stream").unwrap_or(k.stream);
        let port = pruefe_port_wert(port)?;
        send.set(&host, u32::from(port), &stream)?;
        if let Some(w) = an(sd, "an") {
            send.set_enabled(w);
        }
    }
    if let Some(ed) = v.get("empfangen") {
        let port = zahl(ed, "port")
            .map(|p| p as u32)
            .unwrap_or(u32::from(recv.port()));
        let port = pruefe_port_wert(port)?;
        if let Some(stream) = text(ed, "stream") {
            recv.set_stream(&stream)?;
        }
        recv.set_port(u32::from(port))?;
        if let Some(w) = an(ed, "an") {
            recv.set_enabled(w);
        }
    }
    Ok(())
}

/// Standardwerte: aus, 127.0.0.1:6980, Stream `crosspointbusA1`, Empfang aus.
pub fn setze_standard(send: &VbanControl, recv: &VbanRecvControl) {
    send.set_enabled(false);
    let _ = send.set(
        &VbanKonfig::default().host,
        u32::from(VbanKonfig::default().port),
        &VbanKonfig::default().stream,
    );
    recv.set_enabled(false);
    let _ = recv.set_stream("");
    let _ = recv.set_port(u32::from(RECV_PORT_STANDARD));
}

/// Konfigurationsdatei des Dialogs (`~/.config/crosspoint/vban.json`).
pub fn konfig_pfad() -> std::path::PathBuf {
    crate::pfade::datei("vban.json")
}

/// Sende- und Empfangsweg sichern.
pub fn speichere_konfig(send: &VbanControl, recv: &VbanRecvControl) -> Result<(), String> {
    speichere_konfig_nach(send, recv, &konfig_pfad())
}

/// Wie [`speichere_konfig`], aber an einem beliebigen Pfad (Tests).
pub fn speichere_konfig_nach(
    send: &VbanControl,
    recv: &VbanRecvControl,
    pfad: &std::path::Path,
) -> Result<(), String> {
    if let Some(dir) = pfad.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("{} nicht anlegbar: {e}", dir.display()))?;
    }
    let k = send.konfig();
    let datei = Value::obj(vec![
        (
            "senden",
            Value::obj(vec![
                ("an", Value::Bool(send.enabled())),
                ("host", Value::Str(k.host)),
                ("port", Value::Num(f64::from(k.port))),
                ("stream", Value::Str(k.stream)),
            ]),
        ),
        (
            "empfangen",
            Value::obj(vec![
                ("an", Value::Bool(recv.enabled())),
                ("port", Value::Num(f64::from(recv.port()))),
                ("stream", Value::Str(recv.stream())),
            ]),
        ),
    ]);
    std::fs::write(pfad, datei.to_json())
        .map_err(|e| format!("{} nicht schreibbar: {e}", pfad.display()))
}

/// Gesicherte Konfiguration laden und anwenden.
pub fn lade_konfig(send: &VbanControl, recv: &VbanRecvControl) -> Result<(), String> {
    lade_konfig_von(send, recv, &konfig_pfad())
}

/// Wie [`lade_konfig`], aber von einem beliebigen Pfad (Tests).
pub fn lade_konfig_von(
    send: &VbanControl,
    recv: &VbanRecvControl,
    pfad: &std::path::Path,
) -> Result<(), String> {
    let text = std::fs::read_to_string(pfad)
        .map_err(|e| format!("{} nicht lesbar: {e}", pfad.display()))?;
    let v =
        crate::json::parse(&text).map_err(|e| format!("{} ist kein JSON: {e}", pfad.display()))?;
    wende_konfig_an(send, recv, &v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vban::AudioFrame;

    fn ctl() -> VbanControl {
        VbanControl::new()
    }

    #[test]
    fn standardkonfig_ist_loopback_6980() {
        let k = VbanKonfig::default();
        assert_eq!(k.host, "127.0.0.1");
        assert_eq!(k.port, 6980);
        assert_eq!(k.stream, "crosspointbusA1");
        assert!(k.pruefe().is_ok(), "Standardkonfig muss gültig sein");
        assert_eq!(k.anzeige(), "127.0.0.1:6980/crosspointbusA1");
    }

    #[test]
    fn port_null_und_70000_werden_abgelehnt() {
        assert!(pruefe_port(0).is_err(), "Port 0 muss abgelehnt werden");
        assert!(
            pruefe_port_wert(70_000).is_err(),
            "Port 70000 muss abgelehnt werden"
        );
        assert_eq!(pruefe_port_wert(7000).unwrap(), 7000);
        assert!(
            pruefe_port(1023).is_err(),
            "Port 1023 (privilegiert) muss abgelehnt werden"
        );
        assert!(pruefe_port(1024).is_ok());
        assert!(pruefe_port(65535).is_ok());

        let c = ctl();
        let alt = c.konfig();
        assert!(c.set("127.0.0.1", 0, "crosspointbusA1").is_err());
        assert!(!c.letzter_fehler().is_empty(), "Fehler muss abgelegt sein");
        assert!(c.letzter_fehler().contains("0"), "Meldung nennt den Port");
        assert_eq!(c.konfig(), alt, "alte Konfig bleibt bei Fehler stehen");

        assert!(c.set("127.0.0.1", 70000, "crosspointbusA1").is_err());
        assert_eq!(c.konfig(), alt);
    }

    #[test]
    fn stream_zu_lang_und_mit_leerzeichen_wird_abgelehnt() {
        // Ein Zeichen über der Grenze des Headerfelds (Streamnamen sind 15 lang).
        let zu_lang = "crosspointbusA11";
        assert_eq!(zu_lang.len(), MAX_STREAM + 1);
        assert!(pruefe_stream(zu_lang).is_err());
        assert!(pruefe_stream("").is_err(), "leerer Stream ist ungültig");
        assert!(
            pruefe_stream("crosspoint bus").is_err(),
            "Leerzeichen ist verboten"
        );
        assert!(
            pruefe_stream("crosspointbusA1\t").is_err(),
            "Tab ist verboten"
        );
        assert!(
            pruefe_stream("crosspointbusä1").is_err(),
            "Nicht-ASCII ist verboten"
        );
        assert!(pruefe_stream("crosspointbusA1").is_ok());
        // Genau 15 Zeichen ist die Obergrenze.
        assert!(pruefe_stream("abcdefghijklmno").is_ok());
        assert_eq!("abcdefghijklmno".len(), MAX_STREAM);

        let c = ctl();
        let alt = c.konfig();
        let err = c.set("127.0.0.1", 6980, zu_lang).unwrap_err();
        assert!(err.contains("16"), "Meldung nennt die Länge: {err}");
        assert_eq!(c.konfig(), alt);
        assert!(c.set("127.0.0.1", 6980, "crosspoint bus").is_err());
        assert_eq!(c.konfig(), alt);
    }

    #[test]
    fn gueltiges_set_uebernimmt_und_loescht_fehler() {
        let c = ctl();
        c.set_fehler("alter fehler");
        c.set("10.1.2.3", 7000, "BusA1").expect("gültig");
        let k = c.konfig();
        assert_eq!(k.host, "10.1.2.3");
        assert_eq!(k.port, 7000);
        assert_eq!(k.stream, "BusA1");
        assert_eq!(c.letzter_fehler(), "");
    }

    #[test]
    fn status_json_hat_pflichtfelder() {
        let c = ctl();
        c.set_enabled(true);
        c.setze_frames(4711);
        c.set_fehler("testfehler");
        let j = c.status_json();
        assert!(j.starts_with('{') && j.ends_with('}'), "kein Objekt: {j}");
        for key in [
            "enabled",
            "host",
            "port",
            "stream",
            "gesendete_frames",
            "letzter_fehler",
        ] {
            let muster = format!("\"{key}\":");
            assert!(j.contains(&muster), "Feld {key} fehlt in {j}");
        }
        assert!(j.contains("\"enabled\":true"), "{j}");
        assert!(j.contains("\"port\":6980"), "{j}");
        assert!(j.contains("\"stream\":\"crosspointbusA1\""), "{j}");
        assert!(j.contains("\"gesendete_frames\":4711"), "{j}");
        assert!(j.contains("\"letzter_fehler\":\"testfehler\""), "{j}");

        // Auch im Ruhezustand muss das JSON vollständig sein.
        let leer = VbanControl::new().status_json();
        assert!(leer.contains("\"enabled\":false"), "{leer}");
        assert!(leer.contains("\"letzter_fehler\":\"\""), "{leer}");
    }

    #[test]
    fn ring_nach_frame_ergibt_erwartete_sample_zahl() {
        let ring = Ring::new(4, BLOCK_FRAMES);
        let mut block = vec![0.0f32; BLOCK_FRAMES * 2];
        for (i, s) in block.iter_mut().enumerate() {
            *s = i as f32 * 0.5;
        }
        assert!(ring.push(&block), "erster Block passt in den Ring");
        assert!(ring.push(&block), "zweiter Block passt in den Ring");

        let mut out = vec![0.0f32; BLOCK_FRAMES * 2];
        let frames = ring.pop(&mut out).expect("Block im Ring");
        assert_eq!(frames, BLOCK_FRAMES, "Ring liefert 1024 Stereo-Frames");

        let frame = rahmen_aus_block(
            "crosspointbusA1",
            SampleRate::Hz48000,
            7,
            &out[..frames * 2],
        )
        .expect("Frame");
        assert_eq!(frame.sample_frames(), 1024, "1024 Frames je Kanal");
        assert_eq!(frame.sample_count(), 2048, "2048 Einzelwerte (stereo)");
        assert_eq!(frame.frame_counter(), 7);
        assert_eq!(frame.sample_count() * 4, frame.payload().len());

        // Rundlauf über die Leitung: Samples und Header bleiben unverändert.
        let back = AudioFrame::parse(&frame.encode().expect("encode")).expect("parse");
        assert_eq!(back, frame);
        assert_eq!(back.samples_f32().expect("f32")[..4], out[..4]);

        // Der zweite Block ist noch da.
        let wieder = ring.pop(&mut out).expect("zweiter Block");
        assert_eq!(wieder, BLOCK_FRAMES);
        assert!(ring.pop(&mut out).is_none(), "Ring ist dann leer");
    }

    #[test]
    fn ungerader_block_wird_abgelehnt() {
        let block = vec![0.0f32; 3];
        assert!(rahmen_aus_block("crosspointbusA1", SampleRate::Hz48000, 0, &block).is_err());
    }

    #[test]
    fn zaehler_wrap_bei_u64_max_und_u32_grenze() {
        let c = ctl();
        c.setze_frames(u64::MAX);
        assert_eq!(c.gesendete_frames(), u64::MAX);
        assert_eq!(c.naechste_nummer(), 0, "u64-Wrap: MAX + 1 == 0");
        c.setze_frames(0);
        assert_eq!(c.naechste_nummer(), 1);

        // Header-Zähler ist der 32-Bit-Anteil, läuft modulo 2^32.
        assert_eq!(als_zaehler(0), 0);
        assert_eq!(als_zaehler(u64::from(u32::MAX)), u32::MAX);
        assert_eq!(als_zaehler(u64::from(u32::MAX) + 1), 0);
        assert_eq!(als_zaehler(u64::MAX), u32::MAX);
        assert_eq!(als_zaehler(u64::MAX.wrapping_add(2)), 1);

        // Der Frame trägt genau diesen Zähler.
        let block = vec![0.0f32; 4];
        let f = rahmen_aus_block(
            "crosspointbusA1",
            SampleRate::Hz48000,
            als_zaehler(u64::MAX),
            &block,
        )
        .expect("Frame");
        assert_eq!(f.frame_counter(), u32::MAX);
        let back = AudioFrame::parse(&f.encode().expect("encode")).expect("parse");
        assert_eq!(back.frame_counter(), u32::MAX);

        // Anzeige bleibt bei großen Werten stabil (kein Absturz/Panik).
        c.setze_frames(1 << 40);
        assert!(c
            .status_json()
            .contains("\"gesendete_frames\":1099511627776"));
    }

    #[test]
    fn unbekannte_rate_wird_auf_48k_gemappt_und_gemeldet() {
        // 44100 Hz hat in der VBAN-Tabelle einen eigenen Index (16) und ist
        // damit ohne Hinweis übertragbar.
        let (r44, h44) = rate_fuer(44100);
        assert_eq!(r44, SampleRate::Hz44100);
        assert!(h44.is_none(), "44100 Hz ist VBAN-fähig: {h44:?}");

        // 50000 Hz steht nicht in der Tabelle.
        let (r, hinweis) = rate_fuer(50000);
        assert_eq!(r, SampleRate::Hz48000);
        let h = hinweis.expect("Hinweis bei 50000");
        assert!(h.contains("50000"), "{h}");

        let (r2, h2) = rate_fuer(48000);
        assert_eq!(r2, SampleRate::Hz48000);
        assert!(h2.is_none());

        assert_eq!(rate_fuer(96000).0.hz(), 96000);
    }

    #[test]
    fn hostpruefung_lehnt_leerraum_und_leeres_ab() {
        assert!(pruefe_host("").is_err());
        assert!(pruefe_host("127.0.0.1 ").is_err());
        assert!(pruefe_host("local host").is_err());
        assert!(pruefe_host("127.0.0.1").is_ok());
        assert!(pruefe_host("::1").is_ok());
        assert!(pruefe_host("mein-nas.local").is_ok());
    }

    #[test]
    fn sender_thread_schickt_block_ueber_udp() {
        use std::time::Duration;

        // Puffer muss ein volles 8220-Byte-Paket aufnehmen: der Standard des
        // Empfängers (2048 Byte) schneidet den Block sonst ab.
        let mut rx = vban::Receiver::bind("127.0.0.1:0".parse().expect("addr"))
            .expect("bind")
            .with_buffer_size(vban::HEADER_LEN + BLOCK_FRAMES * 2 * 4);
        rx.set_read_timeout(Some(Duration::from_millis(2000)))
            .expect("timeout");
        let port = rx.local_addr().expect("local_addr").port();

        let c = Arc::new(VbanControl::new());
        c.set("127.0.0.1", u32::from(port), "crosspointbusA1")
            .expect("konfig");
        let ring = Arc::new(Ring::new(8, BLOCK_FRAMES));

        // Thread starten, dann einschalten, dann einen Block ablegen: so kann
        // der Block nicht im Leerlauf (Aus) aus dem Ring geleert werden.
        let _sender = spawn_sender(Arc::clone(&c), Arc::clone(&ring), 48000);
        c.set_enabled(true);

        let mut block = vec![0.0f32; BLOCK_FRAMES * 2];
        block[0] = 0.25;
        block[1] = -0.25;
        assert!(ring.push(&block), "Block passt in den Ring");

        let (frame, _src) = rx
            .recv_frame()
            .expect("empfangen")
            .expect("Paket innerhalb von 2 s");
        assert_eq!(frame.header().stream_name, "crosspointbusA1");
        assert_eq!(frame.sample_frames(), BLOCK_FRAMES);
        assert_eq!(frame.samples_f32().expect("f32")[..2], [0.25f32, -0.25f32]);
        // Der Empfänger kann das Datagramm bereits haben, während der
        // Sender-Thread den Zähler noch nicht hochgesetzt hat: `send_frame`
        // kehrt zurück, bevor `setze_frames` läuft. Deshalb pollen statt
        // sofort prüfen – sonst ist der Test ein Timing-Test.
        let start = std::time::Instant::now();
        while c.gesendete_frames() == 0 {
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "Zähler wurde erhöht"
            );
            thread::sleep(Duration::from_millis(2));
        }
        assert!(c.gesendete_frames() >= 1, "Zähler wurde erhöht");
        assert_eq!(c.letzter_fehler(), "", "kein Fehler beim Senden");
        c.set_enabled(false);
    }

    #[test]
    fn loopback_literal_loest_ohne_dns_auf() {
        let k = VbanKonfig::neu("127.0.0.1", 6980, "crosspointbusA1");
        let a = k.aufloesen().expect("Literal");
        assert_eq!(a.port(), 6980);
        assert!(a.ip().is_loopback());
    }

    // -----------------------------------------------------------------------
    // Empfangsweg
    // -----------------------------------------------------------------------

    /// Baut ein gültiges VBAN-FLOAT32-Paket.
    fn paket(stream: &str, kanaele: u16, rate: SampleRate, samples: &[f32]) -> Vec<u8> {
        let mut f = AudioFrame::float32(stream, rate, kanaele).expect("Frame");
        f.push_f32(samples).expect("push");
        f.encode().expect("encode")
    }

    /// Kostenloser Port für den Socket-Test (binden, Nummer merken, freigeben).
    fn freier_port() -> u16 {
        let s = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let p = s.local_addr().expect("addr").port();
        drop(s);
        p
    }

    #[test]
    fn empfang_parst_gueltigen_float32_frame() {
        let roh = paket("test", 2, SampleRate::Hz48000, &[0.25, -0.25, 0.5, -0.5]);
        let f = parse_empfang(&roh).expect("gültiges Paket");
        assert_eq!(f.stream, "test");
        assert_eq!(f.kanaele, 2);
        assert_eq!(f.frames, 2);
        assert_eq!(f.sample_rate, SampleRate::Hz48000);
        assert_eq!(f.samples, vec![0.25, -0.25, 0.5, -0.5]);
    }

    #[test]
    fn empfang_lehnt_falsches_magic_ab() {
        let mut roh = paket("test", 2, SampleRate::Hz48000, &[0.0, 0.0]);
        roh[0..4].copy_from_slice(b"VBAM");
        match parse_empfang(&roh) {
            Err(RecvFehler::FalschesMagic(m)) => assert_eq!(&m, b"VBAM"),
            other => panic!("falsches Ergebnis: {other:?}"),
        }
    }

    #[test]
    fn empfang_lehnt_falsches_format_ab() {
        // Format-Code 1 = 16 Bit, nicht FLOAT32.
        let mut f = AudioFrame::pcm16("test", SampleRate::Hz48000, 2).expect("Frame");
        f.push_i16(&[1, 2]).expect("push");
        match parse_empfang(&f.encode().expect("encode")) {
            Err(RecvFehler::FalschesFormat { code }) => assert_eq!(code, 1),
            other => panic!("falsches Ergebnis: {other:?}"),
        }
    }

    #[test]
    fn empfang_lehnt_zu_kurzes_paket_ab() {
        let roh = paket("test", 2, SampleRate::Hz48000, &[0.0, 0.0]);
        match parse_empfang(&roh[..vban::HEADER_LEN - 1]) {
            Err(RecvFehler::ZuKurz { got, need }) => {
                assert_eq!(got, vban::HEADER_LEN - 1);
                assert_eq!(need, vban::HEADER_LEN);
            }
            other => panic!("falsches Ergebnis: {other:?}"),
        }
        assert!(matches!(
            parse_empfang(b""),
            Err(RecvFehler::ZuKurz { got: 0, .. })
        ));
    }

    #[test]
    fn empfang_dupliziert_einkanal_frame_auf_beide_seiten() {
        let roh = paket("mono", 1, SampleRate::Hz48000, &[0.1, 0.2, 0.3]);
        let f = parse_empfang(&roh).expect("Mono ist erlaubt");
        assert_eq!(f.kanaele, 1);
        assert_eq!(f.frames, 3);
        assert_eq!(
            f.samples,
            vec![0.1, 0.1, 0.2, 0.2, 0.3, 0.3],
            "Mono muss auf L und R liegen"
        );
    }

    #[test]
    fn empfang_lehnt_mehr_als_zwei_kanaele_ab() {
        let roh = paket("acht", 4, SampleRate::Hz48000, &[0.0; 8]);
        assert!(matches!(
            parse_empfang(&roh),
            Err(RecvFehler::FalscheKanaele { kanaele: 4 })
        ));
    }

    #[test]
    fn streamfilter_leer_nimmt_alles_sonst_exakt() {
        assert!(passt_stream("", "beliebig"));
        assert!(passt_stream("test", "test"));
        assert!(!passt_stream("test", "Test"), "Groß-/Kleinschreibung zählt");
        assert!(!passt_stream("test", "test2"));

        let c = VbanRecvControl::new();
        assert_eq!(c.stream(), "", "Vorgabe: alle Streams");
        assert!(c.set_stream("test").is_ok());
        assert_eq!(c.stream(), "test");
        assert!(c.set_stream("").is_ok(), "leerer Filter ist erlaubt");
        assert!(c.set_stream("viel_zu_langer_name").is_err());
        assert!(c.set_stream("mit leer").is_err());
    }

    #[test]
    fn zaehler_zaehlt_gueltige_und_verworfene_pakete() {
        let c = VbanRecvControl::new();
        let ring = Ring::new(4, RECV_BLOCK_FRAMES);
        let mut sammler = Sammler::new(RECV_BLOCK_FRAMES);

        let gut = paket("test", 2, SampleRate::Hz48000, &[0.1, -0.1]);
        let mut falsches_magic = gut.clone();
        falsches_magic[0] = b'X';
        let falsches_format = {
            let mut f = AudioFrame::pcm16("test", SampleRate::Hz48000, 2).expect("Frame");
            f.push_i16(&[1, 2]).expect("push");
            f.encode().expect("encode")
        };

        assert!(c.verarbeite(&mut sammler, &ring, &gut).is_ok());
        assert!(c.verarbeite(&mut sammler, &ring, &falsches_magic).is_err());
        assert!(c.verarbeite(&mut sammler, &ring, &falsches_format).is_err());
        assert!(c.verarbeite(&mut sammler, &ring, b"zu kurz").is_err());
        assert_eq!(c.packets(), 1, "nur das gültige Paket zählt");
        assert_eq!(c.dropped(), 3, "drei Pakete verworfen");
        assert!(
            c.letzter_fehler().contains("zu kurz"),
            "letzter Fehler: {}",
            c.letzter_fehler()
        );

        // Fremder Stream wird gefiltert und als verworfen gezählt.
        c.set_stream("test").expect("Filter");
        let fremd = paket("anderer", 2, SampleRate::Hz48000, &[0.1, 0.1]);
        assert!(matches!(
            c.verarbeite(&mut sammler, &ring, &fremd),
            Err(RecvFehler::FremderStream { .. })
        ));
        assert_eq!(c.packets(), 1);
        assert_eq!(c.dropped(), 4);
    }

    #[test]
    fn ring_rundlauf_ueber_den_sammler() {
        // Vier Stereo-Frames je Ringblock; zwei Pakete füllen zwei Blöcke.
        let ring = Ring::new(4, 4);
        let mut sammler = Sammler::new(4);

        let mut samples = Vec::new();
        for i in 0..4 {
            samples.push(i as f32 * 0.1);
            samples.push(-(i as f32) * 0.1);
        }
        let a = paket("test", 2, SampleRate::Hz48000, &samples);
        let (bloecke, verloren) = sammler.schiebe(&ring, &parse_empfang(&a).unwrap().samples);
        assert_eq!((bloecke, verloren), (1, 0));
        assert_eq!(sammler.rest(), 0);

        // Kürzeres Paket: bleibt als Rest liegen, bis der Block voll ist.
        let b = paket(
            "test",
            2,
            SampleRate::Hz48000,
            &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
        );
        let (bloecke, _) = sammler.schiebe(&ring, &parse_empfang(&b).unwrap().samples);
        assert_eq!(bloecke, 0, "drei Frames füllen keinen Viererblock");
        assert_eq!(sammler.rest(), 3);

        // Ein Frame nachlegen: Block wird voll und abgelegt.
        let c = paket("test", 2, SampleRate::Hz48000, &[7.0, 8.0]);
        let (bloecke, _) = sammler.schiebe(&ring, &parse_empfang(&c).unwrap().samples);
        assert_eq!(bloecke, 1);
        assert_eq!(sammler.rest(), 0);

        // Beide Blöcke in Reihenfolge aus dem Ring.
        let mut out = vec![0.0f32; 8];
        assert_eq!(ring.pop(&mut out), Some(4));
        assert_eq!(out, samples, "erster Block unverändert");
        assert_eq!(ring.pop(&mut out), Some(4));
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        assert!(ring.pop(&mut out).is_none(), "Ring ist dann leer");
    }

    #[test]
    fn empfangs_status_json_hat_pflichtfelder() {
        let c = VbanRecvControl::new();
        c.set_enabled(true);
        c.set_stream("test").expect("Filter");
        c.setze_zaehler(12, 3);
        let j = c.status_json();
        for key in [
            "enabled",
            "port",
            "stream",
            "packets",
            "dropped",
            "level_left",
            "level_right",
        ] {
            assert!(
                j.contains(&format!("\"{key}\":")),
                "Feld {key} fehlt in {j}"
            );
        }
        assert!(j.contains("\"enabled\":true"), "{j}");
        assert!(j.contains("\"port\":6980"), "{j}");
        assert!(j.contains("\"stream\":\"test\""), "{j}");
        assert!(j.contains("\"packets\":12"), "{j}");
        assert!(j.contains("\"dropped\":3"), "{j}");
        assert!(j.contains("\"level_left\":-120"), "Stille ist -120 dB: {j}");
    }

    #[test]
    fn empfaenger_thread_empfaengt_ueber_udp() {
        let port = freier_port();
        let c = Arc::new(VbanRecvControl::new());
        c.set_port(u32::from(port)).expect("Port");
        c.set_stream("test").expect("Stream");
        let ring = Arc::new(Ring::new(8, RECV_BLOCK_FRAMES));

        let _t = spawn_empfaenger(Arc::clone(&c), Arc::clone(&ring));
        c.set_enabled(true);

        let tx = UdpSocket::bind("127.0.0.1:0").expect("tx");
        let ziel: SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");
        let roh = paket("test", 2, SampleRate::Hz48000, &[0.5, -0.5, 0.5, -0.5]);

        // UDP wiederholt nicht: solange der Thread noch bindet, gehen Pakete
        // verloren. Deshalb senden, bis die erwartete Zahl angekommen ist.
        let mut versuche = 0;
        while c.packets() < 5 && versuche < 200 {
            let _ = tx.send_to(&roh, ziel);
            thread::sleep(Duration::from_millis(10));
            versuche += 1;
        }
        assert!(c.packets() >= 5, "nur {} Pakete angekommen", c.packets());
        assert_eq!(c.dropped(), 0);

        // 0.5 entspricht -6,0 dB (Pegel wird im Leerlauf gedämpft, daher Toleranz).
        let [l, r] = c.level_db();
        assert!(
            (l - (-6.0)).abs() < 1.5,
            "linker Pegel war {l} dB, erwartet ≈ -6,0"
        );
        assert!((r - (-6.0)).abs() < 1.5, "rechter Pegel war {r} dB");
        c.set_enabled(false);
    }

    #[test]
    fn empfaenger_wechselt_port_und_haelt_an() {
        let port_a = freier_port();
        let port_b = freier_port();
        let c = Arc::new(VbanRecvControl::new());
        c.set_port(u32::from(port_a)).expect("Port A");
        let ring = Arc::new(Ring::new(8, RECV_BLOCK_FRAMES));
        let _t = spawn_empfaenger(Arc::clone(&c), Arc::clone(&ring));
        c.set_enabled(true);

        let tx = UdpSocket::bind("127.0.0.1:0").expect("tx");
        let roh = paket("test", 2, SampleRate::Hz48000, &[0.1, 0.1]);
        while c.packets() < 3 {
            let _ = tx.send_to(
                &roh,
                format!("127.0.0.1:{port_a}").parse::<SocketAddr>().unwrap(),
            );
            thread::sleep(Duration::from_millis(20));
        }

        // Portwechsel: der Aufseher bindet neu, auf dem alten Port kommt nichts mehr.
        c.set_port(u32::from(port_b)).expect("Port B");
        thread::sleep(Duration::from_millis(100));
        let vorher = c.packets();
        for _ in 0..5 {
            let _ = tx.send_to(
                &roh,
                format!("127.0.0.1:{port_a}").parse::<SocketAddr>().unwrap(),
            );
        }
        thread::sleep(Duration::from_millis(100));
        assert_eq!(c.packets(), vorher, "alter Port darf nicht mehr zählen");

        for _ in 0..5 {
            let _ = tx.send_to(
                &roh,
                format!("127.0.0.1:{port_b}").parse::<SocketAddr>().unwrap(),
            );
        }
        let mut versuche = 0;
        while c.packets() <= vorher && versuche < 100 {
            thread::sleep(Duration::from_millis(10));
            versuche += 1;
        }
        assert!(c.packets() > vorher, "neuer Port empfängt");

        // Abschalten: Socket wird geschlossen, Zähler bleibt stehen.
        c.set_enabled(false);
        thread::sleep(Duration::from_millis(100));
        let stand = c.packets();
        for _ in 0..5 {
            let _ = tx.send_to(
                &roh,
                format!("127.0.0.1:{port_b}").parse::<SocketAddr>().unwrap(),
            );
        }
        thread::sleep(Duration::from_millis(100));
        assert_eq!(c.packets(), stand, "ausgeschaltet zählt nichts mehr");
    }

    /* ---------- Dialog-Konfiguration ---------- */

    fn testpfad(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("crosspoint-vban-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("vban.json")
    }

    #[test]
    fn konfig_json_hat_beide_tabellen_und_alle_spalten() {
        let send = VbanControl::new();
        let recv = VbanRecvControl::new();
        let j = konfig_json(&send, &recv, 48_000);
        let v = crate::json::parse(&j).expect("JSON");
        assert!(v.get("senden").is_some(), "senden fehlt");
        assert!(v.get("empfangen").is_some(), "empfangen fehlt");
        assert_eq!(v.get("samplerate").and_then(|x| x.as_f64()), Some(48_000.0));

        // Ausgangszeile: alle Spalten der Original-Tabelle.
        let out = v
            .get("outgoing")
            .and_then(|x| x.as_arr())
            .expect("outgoing");
        assert_eq!(out.len(), 1);
        for key in [
            "source", "stream", "ip", "sr", "ch", "format", "quality", "errors",
        ] {
            assert!(out[0].get(key).is_some(), "Spalte {key} fehlt: {j}");
        }
        assert_eq!(
            out[0].get("source").and_then(|x| match x {
                Value::Str(s) => Some(s.as_str()),
                _ => None,
            }),
            Some("Bus A1")
        );
        assert_eq!(out[0].get("sr").and_then(|x| x.as_f64()), Some(48_000.0));

        // Ohne Empfang ist die Eingangstabelle leer, nicht erfunden.
        assert_eq!(v.get("incoming").and_then(|x| x.as_arr()).unwrap().len(), 0);
    }

    #[test]
    fn konfig_rundlauf_setzen_und_lesen() {
        let send = VbanControl::new();
        let recv = VbanRecvControl::new();
        let v = crate::json::parse(
            r#"{"senden":{"an":true,"host":"192.168.1.255","port":6981,"stream":"TESTSTREAM"},
                "empfangen":{"an":true,"port":6982,"stream":"INCOMING"}}"#,
        )
        .unwrap();
        wende_konfig_an(&send, &recv, &v).expect("gültig");
        assert!(send.enabled() && recv.enabled());
        assert_eq!(send.konfig().host, "192.168.1.255");
        assert_eq!(send.konfig().port, 6981);
        assert_eq!(send.konfig().stream, "TESTSTREAM");
        assert_eq!(recv.port(), 6982);
        assert_eq!(recv.stream(), "INCOMING");

        // Zurückgelesen steht alles im JSON.
        let j = konfig_json(&send, &recv, 48_000);
        assert!(j.contains("\"host\":\"192.168.1.255\""), "{j}");
        assert!(j.contains("\"port\":6981"), "{j}");
        assert!(j.contains("\"stream\":\"TESTSTREAM\""), "{j}");
        assert!(j.contains("\"an\":true"), "{j}");
    }

    #[test]
    fn ungueltige_konfig_wird_abgelehnt_und_aendert_nichts() {
        let send = VbanControl::new();
        let recv = VbanRecvControl::new();
        let vorher = send.konfig();
        let v = crate::json::parse(r#"{"senden":{"port":80,"stream":"X"}}"#).unwrap();
        assert!(
            wende_konfig_an(&send, &recv, &v).is_err(),
            "Port 80 ist gesperrt"
        );
        assert_eq!(send.konfig(), vorher, "alte Konfiguration bleibt stehen");

        let v = crate::json::parse(r#"{"senden":{"stream":"mit leerzeichen"}}"#).unwrap();
        assert!(wende_konfig_an(&send, &recv, &v).is_err());
        assert_eq!(send.konfig(), vorher);
    }

    #[test]
    fn konfig_datei_rundlauf() {
        let pfad = testpfad("datei");
        let send = VbanControl::new();
        let recv = VbanRecvControl::new();
        wende_konfig_an(
            &send,
            &recv,
            &crate::json::parse(
                r#"{"senden":{"an":true,"host":"10.0.0.9","port":7000,"stream":"ABC"},
                    "empfangen":{"an":true,"port":7001,"stream":"DEF"}}"#,
            )
            .unwrap(),
        )
        .unwrap();
        speichere_konfig_nach(&send, &recv, &pfad).unwrap();

        let send2 = VbanControl::new();
        let recv2 = VbanRecvControl::new();
        lade_konfig_von(&send2, &recv2, &pfad).unwrap();
        assert_eq!(send2.konfig(), send.konfig());
        assert!(send2.enabled() && recv2.enabled());
        assert_eq!(recv2.port(), 7001);
        assert_eq!(recv2.stream(), "DEF");
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn standard_setzt_alles_zurueck() {
        let send = VbanControl::new();
        let recv = VbanRecvControl::new();
        wende_konfig_an(
            &send,
            &recv,
            &crate::json::parse(
                r#"{"senden":{"an":true,"host":"10.1.1.1","port":7100,"stream":"XYZ"},
                    "empfangen":{"an":true,"port":7101}}"#,
            )
            .unwrap(),
        )
        .unwrap();
        setze_standard(&send, &recv);
        assert!(!send.enabled() && !recv.enabled());
        assert_eq!(send.konfig(), VbanKonfig::default());
        assert_eq!(recv.port(), RECV_PORT_STANDARD);
        assert_eq!(recv.stream(), "");
    }

    #[test]
    fn gegenstelle_erscheint_in_der_eingangstabelle() {
        let c = VbanRecvControl::new();
        let ring = Ring::new(4, RECV_BLOCK_FRAMES);
        let mut sammler = Sammler::new(RECV_BLOCK_FRAMES);
        let roh = paket("FUNK", 2, SampleRate::Hz48000, &[0.1, -0.1, 0.2, -0.2]);
        c.verarbeite_von(&mut sammler, &ring, &roh, "192.168.0.7")
            .expect("Paket");
        c.verarbeite_von(&mut sammler, &ring, &roh, "192.168.0.7")
            .expect("Paket");
        let q = c.quellen();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].stream, "FUNK");
        assert_eq!(q[0].ip, "192.168.0.7");
        assert_eq!(q[0].packets, 2);
        assert_eq!(q[0].kanaele, 2);
        assert_eq!(q[0].sample_rate, 48_000);

        // Ein kaputtes Paket derselben Gegenstelle zählt als Fehler.
        let _ = c.verarbeite_von(&mut sammler, &ring, b"kaputt", "192.168.0.7");
        let q = c.quellen();
        assert_eq!(q[0].errors, 1);
        assert!(konfig_json(&VbanControl::new(), &c, 48_000).contains("\"ip\":\"192.168.0.7\""));
    }
}
