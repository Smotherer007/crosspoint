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
//! 6. **Abtastrate:** nur die acht VBAN-Raten sind übertragbar. Ein anderer
//!    Wert (z. B. 44100) wird auf 48000 gemappt **und** als Fehler gemeldet,
//!    statt still ein Paket mit falscher Rate zu senden.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use vban::{AudioFrame, SampleRate, Sender};

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
            stream: "vm8busA1".to_string(),
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
    letzter_fehler: Mutex<String>,
    konfig: Mutex<VbanKonfig>,
}

impl Default for VbanControl {
    fn default() -> Self {
        VbanControl::new()
    }
}

impl VbanControl {
    /// Aus (Standard), Ziel 127.0.0.1:6980, Stream `vm8busA1`, kein Fehler.
    pub fn new() -> VbanControl {
        VbanControl::mit_konfig(VbanKonfig::default())
    }

    /// Wie [`VbanControl::new`], aber mit vorgegebener Konfiguration.
    pub fn mit_konfig(konfig: VbanKonfig) -> VbanControl {
        VbanControl {
            enabled: AtomicBool::new(false),
            gesendete_frames: AtomicU64::new(0),
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
    pub fn set_fehler(&self, s: impl Into<String>) {
        match self.letzter_fehler.lock() {
            Ok(mut g) => *g = s.into(),
            Err(e) => *e.into_inner() = s.into(),
        }
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
        assert_eq!(k.stream, "vm8busA1");
        assert!(k.pruefe().is_ok(), "Standardkonfig muss gültig sein");
        assert_eq!(k.anzeige(), "127.0.0.1:6980/vm8busA1");
    }

    #[test]
    fn port_null_und_70000_werden_abgelehnt() {
        assert!(pruefe_port(0).is_err(), "Port 0 muss abgelehnt werden");
        assert!(
            pruefe_port_wert(70_000).is_err(),
            "Port 70000 muss abgelehnt werden"
        );
        assert_eq!(pruefe_port_wert(7000).unwrap(), 7000);
        assert!(pruefe_port(1023).is_err(), "Port 1023 (privilegiert) muss abgelehnt werden");
        assert!(pruefe_port(1024).is_ok());
        assert!(pruefe_port(65535).is_ok());

        let c = ctl();
        let alt = c.konfig();
        assert!(c.set("127.0.0.1", 0, "vm8busA1").is_err());
        assert!(!c.letzter_fehler().is_empty(), "Fehler muss abgelegt sein");
        assert!(c.letzter_fehler().contains("0"), "Meldung nennt den Port");
        assert_eq!(c.konfig(), alt, "alte Konfig bleibt bei Fehler stehen");

        assert!(c.set("127.0.0.1", 70000, "vm8busA1").is_err());
        assert_eq!(c.konfig(), alt);
    }

    #[test]
    fn stream_zu_lang_und_mit_leerzeichen_wird_abgelehnt() {
        // 16 Zeichen: ein Zeichen über der Grenze des Headerfelds.
        assert_eq!("vm8busA1Toolange".len(), MAX_STREAM + 1);
        assert!(pruefe_stream("vm8busA1Toolange").is_err());
        assert!(pruefe_stream("").is_err(), "leerer Stream ist ungültig");
        assert!(pruefe_stream("vm8 bus").is_err(), "Leerzeichen ist verboten");
        assert!(pruefe_stream("vm8busA1\t").is_err(), "Tab ist verboten");
        assert!(pruefe_stream("vm8busä1").is_err(), "Nicht-ASCII ist verboten");
        assert!(pruefe_stream("vm8busA1").is_ok());
        // Genau 15 Zeichen ist die Obergrenze.
        assert!(pruefe_stream("abcdefghijklmno").is_ok());
        assert_eq!("abcdefghijklmno".len(), MAX_STREAM);

        let c = ctl();
        let alt = c.konfig();
        let err = c.set("127.0.0.1", 6980, "vm8busA1Toolange").unwrap_err();
        assert!(err.contains("16"), "Meldung nennt die Länge: {err}");
        assert_eq!(c.konfig(), alt);
        assert!(c.set("127.0.0.1", 6980, "vm8 bus").is_err());
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
        assert!(j.contains("\"stream\":\"vm8busA1\""), "{j}");
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

        let frame = rahmen_aus_block("vm8busA1", SampleRate::Hz48000, 7, &out[..frames * 2])
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
        assert!(rahmen_aus_block("vm8busA1", SampleRate::Hz48000, 0, &block).is_err());
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
        let f = rahmen_aus_block("vm8busA1", SampleRate::Hz48000, als_zaehler(u64::MAX), &block)
            .expect("Frame");
        assert_eq!(f.frame_counter(), u32::MAX);
        let back = AudioFrame::parse(&f.encode().expect("encode")).expect("parse");
        assert_eq!(back.frame_counter(), u32::MAX);

        // Anzeige bleibt bei großen Werten stabil (kein Absturz/Panik).
        c.setze_frames(1 << 40);
        assert!(c.status_json().contains("\"gesendete_frames\":1099511627776"));
    }

    #[test]
    fn unbekannte_rate_wird_auf_48k_gemappt_und_gemeldet() {
        let (r, hinweis) = rate_fuer(44100);
        assert_eq!(r, SampleRate::Hz48000);
        let h = hinweis.expect("Hinweis bei 44100");
        assert!(h.contains("44100"), "{h}");

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
        c.set("127.0.0.1", u32::from(port), "vm8busA1")
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
        assert_eq!(frame.header().stream_name, "vm8busA1");
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
        let k = VbanKonfig::neu("127.0.0.1", 6980, "vm8busA1");
        let a = k.aufloesen().expect("Literal");
        assert_eq!(a.port(), 6980);
        assert!(a.ip().is_loopback());
    }
}
