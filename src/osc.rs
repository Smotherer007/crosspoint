//! OSC-Eingang über UDP: Fader, Tasten und Presets von aussen steuern.
//!
//! Warum OSC und nicht MIDI: MIDI selbst zu sprechen hiesse, ALSA-Sequencer,
//! SysEx, 14-Bit-NRPN und Gerätequirks zu implementieren. Für die Übersetzung
//! von MIDI (und ArtNet, sACN, ...) auf OSC gibt es MIDIMonster; die Engine
//! braucht dann nur **einen** schmalen Eingang. `data/midimonster.cfg` ist die
//! fertige Zuordnung für ein nanoKONTROL2.
//!
//! Adressen (die Zahl ist der Index 0..7, wie in `/api/set`):
//!
//! ```text
//! /strip/<n>/<param>  <wert>     z. B. /strip/0/gain -6.0
//! /bus/<n>/<param>    <wert>     z. B. /bus/0/gain 3.0
//! /preset/save                   schreibt preset.json
//! /preset/load                   liest preset.json
//! ```
//!
//! Angewendet wird über [`Params::set`] – dieselbe Funktion wie `/api/set`,
//! also mit denselben Grenzen (Gain wird auf -60..+12 dB geklemmt), mit der
//! Markierung für den Autosave und ohne Lock im Audio-Pfad.
//!
//! Argumenttypen: `f` (dB bzw. 0/1 für Schalter), `i` und `s` (Zahl als Text).
//! Ein **ganzzahliges** Argument auf einem Pegel-Parameter (`gain`, `trim`,
//! `send0..3`) ist ein roher MIDI-CC 0..127 und wird über [`cc_zu_db`]
//! umgerechnet; ein `f`-Argument ist immer schon dB. `s` versteht ausserdem
//! `on`/`off` (`true`/`false`).
//!
//! Adresse: Vorgabe `127.0.0.1:9000`, änderbar mit `CROSSPOINT_OSC_ADDR`
//! (`host:port`). Rückmeldung: mit `CROSSPOINT_OSC_ECHO=1` geht der
//! tatsächlich übernommene Wert als `/strip/<n>/<param> <f>` an den Absender
//! zurück – das ist der Rückweg für Motorfader und Tasten-LEDs.

use std::fmt;
use std::net::{ToSocketAddrs, UdpSocket};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::engine::Params;

/// Vorgabe-Adresse des Eingangs.
pub const VORGABE_ADDR: &str = "127.0.0.1:9000";
/// Umgebungsvariable für die Adresse (`host:port`).
pub const ENV_ADDR: &str = "CROSSPOINT_OSC_ADDR";
/// Umgebungsvariable für die Rückmeldung (`1` = an).
pub const ENV_ECHO: &str = "CROSSPOINT_OSC_ECHO";

/// Grösstes UDP-Paket, das gelesen wird (IPv4-Grenze).
pub const MAX_PAKET: usize = 65_507;

/// Höchster Wert eines MIDI-Controllers.
pub const CC_MAX: f32 = 127.0;
/// Unteres Ende des Faders in dB.
pub const DB_MIN: f32 = -60.0;
/// Oberes Ende des Faders in dB.
pub const DB_MAX: f32 = 12.0;

/* ==================================================================
Wertebereich
------------------------------------------------------------------ */

/// Rohwert eines MIDI-Controllers (0..127) auf dB abbilden.
///
/// Linear in dB: 0 → -60 dB, 127 → +12 dB, 63.5 → -24 dB. Genau diese
/// Umrechnung macht auch `data/midimonster.cfg` über die OSC-Angabe
/// `f -60.0 12.0`; beide Wege liefern denselben Wert. Werte ausserhalb
/// 0..127 werden geklemmt, `NaN`/`inf` ergibt den unteren Anschlag.
pub fn cc_zu_db(cc: f32) -> f32 {
    if !cc.is_finite() {
        return DB_MIN;
    }
    DB_MIN + (cc / CC_MAX).clamp(0.0, 1.0) * (DB_MAX - DB_MIN)
}

/// Umkehrung von [`cc_zu_db`]: dB → CC 0..127 (für die Rückmeldung).
pub fn db_zu_cc(db: f32) -> f32 {
    if !db.is_finite() {
        return 0.0;
    }
    ((db - DB_MIN) / (DB_MAX - DB_MIN)).clamp(0.0, 1.0) * CC_MAX
}

/* ==================================================================
Nachrichten
------------------------------------------------------------------ */

/// Ein dekodiertes OSC-Argument.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    /// 32-Bit-Gleitkomma (`f`) – in dB bzw. 0/1.
    Float(f32),
    /// 32-Bit-Ganzzahl (`i`) – auf Pegeln ein roher MIDI-CC.
    Int(i32),
    /// OSC-String (`s`) – Zahl als Text oder `on`/`off`.
    Text(String),
}

impl Arg {
    /// Wert als Zahl. Text wird als Zahl gelesen (`"-6.0"`, `"1"`), sonst als
    /// Schalter (`on`/`off`, `true`/`false`).
    pub fn als_zahl(&self) -> Option<f32> {
        match self {
            Arg::Float(v) => v.is_finite().then_some(*v),
            Arg::Int(v) => Some(*v as f32),
            Arg::Text(t) => {
                let t = t.trim();
                if let Ok(v) = t.parse::<f32>() {
                    return v.is_finite().then_some(v);
                }
                match t.to_ascii_lowercase().as_str() {
                    "on" | "true" | "yes" => Some(1.0),
                    "off" | "false" | "no" => Some(0.0),
                    _ => None,
                }
            }
        }
    }
}

/// Was mit einer Adresse geschehen soll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auftrag {
    /// `/strip/<n>/<param>` oder `/bus/<n>/<param>`.
    Setzen {
        /// `"strip"` oder `"bus"` – wie im Rumpf von `/api/set`.
        target: &'static str,
        index: usize,
        param: String,
    },
    /// `/preset/save` – Zustand in die Preset-Datei schreiben.
    PresetSpeichern,
    /// `/preset/load` – Preset-Datei lesen und anwenden.
    PresetLaden,
}

/// Eine dekodierte OSC-Nachricht.
#[derive(Clone, Debug, PartialEq)]
pub struct Nachricht {
    /// OSC-Pfad, z. B. `/strip/0/gain`.
    pub adresse: String,
    /// Argumente in der Reihenfolge der Typ-Tags.
    pub args: Vec<Arg>,
}

/// Warum ein Paket nicht verarbeitet wurde.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OscFehler {
    /// Paket endet mitten in einem Feld.
    ZuKurz,
    /// Adresse ohne abschliessendes NUL.
    AdresseNichtTerminiert,
    /// Adresse ist leer.
    AdresseLeer,
    /// Adresse ohne führenden `/` oder mit unbrauchbarem Index.
    UngueltigeAdresse,
    /// Formal gültig, aber hier nicht bekannt.
    UnbekannteAdresse,
    /// Nach der Adresse fehlen die Typ-Tags.
    FehlendeTypTags,
    /// Typ-Tag, den OSC hier nicht kennt (alles ausser `f`, `i`, `s`).
    UnbekannterTyp(char),
    /// Argument endet mitten im Paket.
    ArgumentZuKurz,
    /// Text-Argument ohne abschliessendes NUL.
    ArgumentNichtTerminiert,
    /// Text-Argument ist kein gültiges UTF-8.
    UngueltigerText,
    /// Zahlen-Argument ist `NaN` oder unendlich.
    UngueltigerWert,
}

impl fmt::Display for OscFehler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OscFehler::ZuKurz => write!(f, "Paket endet zu früh"),
            OscFehler::AdresseNichtTerminiert => write!(f, "Adresse ohne Abschluss-NUL"),
            OscFehler::AdresseLeer => write!(f, "leere Adresse"),
            OscFehler::UngueltigeAdresse => write!(f, "ungültige Adresse"),
            OscFehler::UnbekannteAdresse => write!(f, "unbekannte Adresse"),
            OscFehler::FehlendeTypTags => write!(f, "Typ-Tags fehlen"),
            OscFehler::UnbekannterTyp(c) => write!(f, "unbekannter Typ-Tag {c:?}"),
            OscFehler::ArgumentZuKurz => write!(f, "Argument endet zu früh"),
            OscFehler::ArgumentNichtTerminiert => write!(f, "Text ohne Abschluss-NUL"),
            OscFehler::UngueltigerText => write!(f, "Text ist kein UTF-8"),
            OscFehler::UngueltigerWert => write!(f, "Wert ist keine endliche Zahl"),
        }
    }
}

impl std::error::Error for OscFehler {}

/* ==================================================================
Kodierung
------------------------------------------------------------------ */

/// Länge einer OSC-Zeichenkette samt Füll-NUL, auf 4 Byte aufgerundet.
fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// OSC-String anhängen: Text, Abschluss-NUL, Auffüllen auf 4 Byte.
fn schreibe_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// Nachricht als OSC-Paket kodieren (`f`, `i`, `s`).
///
/// Ein Paket ohne Argumente bekommt trotzdem Typ-Tags – der leere Satz `,`,
/// wie es OSC 1.0 verlangt.
pub fn kodiere(adresse: &str, args: &[Arg]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + args.len() * 8);
    schreibe_string(&mut out, adresse);
    let mut tags = String::from(",");
    for a in args {
        tags.push(match a {
            Arg::Float(_) => 'f',
            Arg::Int(_) => 'i',
            Arg::Text(_) => 's',
        });
    }
    schreibe_string(&mut out, &tags);
    for a in args {
        match a {
            Arg::Float(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
            Arg::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
            Arg::Text(t) => schreibe_string(&mut out, t),
        }
    }
    out
}

/* ==================================================================
Dekodierung
------------------------------------------------------------------ */

/// OSC-String ab `*pos` lesen; `*pos` steht danach hinter dem Füll-NUL.
fn lese_string(
    buf: &[u8],
    pos: &mut usize,
    nicht_terminiert: OscFehler,
) -> Result<String, OscFehler> {
    if *pos >= buf.len() {
        return Err(OscFehler::ZuKurz);
    }
    let rest = &buf[*pos..];
    let ende = match rest.iter().position(|b| *b == 0) {
        Some(i) => i,
        None => return Err(nicht_terminiert),
    };
    let text = std::str::from_utf8(&rest[..ende]).map_err(|_| OscFehler::UngueltigerText)?;
    // Das Paket kann vor dem Füll-NUL enden; dann ist es zu kurz, aber der
    // Text selbst war lesbar. Der nächste Lesezugriff meldet das.
    *pos = align4(*pos + ende + 1).min(buf.len());
    Ok(text.to_string())
}

/// 4 Byte als vorzeichenlose Ganzzahl (Netzwerk-Byte-Reihenfolge).
fn lese_u32(buf: &[u8], pos: &mut usize) -> Result<u32, OscFehler> {
    if *pos + 4 > buf.len() {
        return Err(OscFehler::ArgumentZuKurz);
    }
    let v = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    Ok(v)
}

/// OSC-Paket dekodieren.
///
/// Unterstützt die Typ-Tags `f` (f32), `i` (i32) und `s` (String); alles
/// andere wird abgelehnt, statt es stillschweigend zu verwerfen.
pub fn parse(paket: &[u8]) -> Result<Nachricht, OscFehler> {
    let mut pos = 0usize;
    let adresse = lese_string(paket, &mut pos, OscFehler::AdresseNichtTerminiert)?;
    if adresse.is_empty() {
        return Err(OscFehler::AdresseLeer);
    }
    if !adresse.starts_with('/') {
        return Err(OscFehler::UngueltigeAdresse);
    }

    let mut args = Vec::new();
    if pos >= paket.len() {
        // Nachricht ohne Typ-Tags: gültig nur, wenn wirklich nichts folgt.
        return Ok(Nachricht { adresse, args });
    }

    let tags = lese_string(paket, &mut pos, OscFehler::FehlendeTypTags)?;
    let mut zeichen = tags.chars();
    if zeichen.next() != Some(',') {
        return Err(OscFehler::FehlendeTypTags);
    }
    for tag in zeichen {
        match tag {
            'f' => {
                let roh = f32::from_bits(lese_u32(paket, &mut pos)?);
                if !roh.is_finite() {
                    return Err(OscFehler::UngueltigerWert);
                }
                args.push(Arg::Float(roh));
            }
            'i' => args.push(Arg::Int(lese_u32(paket, &mut pos)? as i32)),
            's' => args.push(Arg::Text(lese_string(
                paket,
                &mut pos,
                OscFehler::ArgumentNichtTerminiert,
            )?)),
            other => return Err(OscFehler::UnbekannterTyp(other)),
        }
    }

    Ok(Nachricht { adresse, args })
}

/* ==================================================================
Adresse -> Auftrag
------------------------------------------------------------------ */

/// Pfad in einen Auftrag übersetzen.
///
/// `/preset/<was>` kennt nur `save` und `load`; alles andere ist unbekannt.
pub fn auftrag(adresse: &str) -> Result<Auftrag, OscFehler> {
    let teile: Vec<&str> = adresse.split('/').filter(|t| !t.is_empty()).collect();
    match teile.as_slice() {
        ["preset", "save"] => Ok(Auftrag::PresetSpeichern),
        ["preset", "load"] => Ok(Auftrag::PresetLaden),
        [ziel @ ("strip" | "bus"), index, param] => {
            let index: usize = index.parse().map_err(|_| OscFehler::UngueltigeAdresse)?;
            if param.is_empty() || param.len() > 64 {
                return Err(OscFehler::UngueltigeAdresse);
            }
            Ok(Auftrag::Setzen {
                target: if *ziel == "strip" { "strip" } else { "bus" },
                index,
                param: (*param).to_string(),
            })
        }
        _ => Err(OscFehler::UnbekannteAdresse),
    }
}

/// Parameter, deren Ganzzahl-Argument ein MIDI-CC ist (0..127 → dB).
fn ist_pegel(param: &str) -> bool {
    param == "gain" || param == "trim" || (param.len() == 5 && param.starts_with("send"))
}

/// Wert eines Arguments für diesen Parameter.
fn wert_aus(arg: &Arg, param: &str) -> Option<f32> {
    match arg {
        // Ganzzahl auf einem Pegel: roher MIDI-CC, nicht dB.
        Arg::Int(cc) if ist_pegel(param) => Some(cc_zu_db(*cc as f32)),
        other => other.als_zahl(),
    }
}

/* ==================================================================
Anwenden
------------------------------------------------------------------ */

/// Nachricht anwenden. Liefert den Auftrag zurück, wenn er ausgeführt wurde.
///
/// `None` heisst: unbekannte Adresse, Index ausserhalb 0..7, Parameter, den
/// `Params::set` nicht kennt, oder fehlender/brauchbarer Wert. Solche Pakete
/// werden still verworfen – ein falsch konfigurierter Controller soll die
/// Engine nicht mit Fehlermeldungen fluten.
pub fn anwende(params: &Arc<Params>, nachricht: &Nachricht) -> Option<Auftrag> {
    let auftrag = auftrag(&nachricht.adresse).ok()?;
    match &auftrag {
        Auftrag::PresetSpeichern => {
            if let Err(e) = speichere_preset(params) {
                eprintln!("OSC: Preset nicht gespeichert: {e}");
            }
            Some(auftrag)
        }
        Auftrag::PresetLaden => {
            if let Err(e) = lade_preset(params) {
                eprintln!("OSC: Preset nicht geladen: {e}");
            }
            Some(auftrag)
        }
        Auftrag::Setzen {
            target,
            index,
            param,
        } => {
            let grenze = if *target == "strip" {
                crate::STRIPS
            } else {
                crate::BUSES
            };
            if *index >= grenze {
                return None;
            }
            let wert = wert_aus(nachricht.args.first()?, param)?;
            if !params.set(target, *index, param, wert) {
                return None;
            }
            // Nur merken, geschrieben wird im Autosave-Thread – wie bei
            // `/api/set`, damit ein Faderzug nicht 100 Dateizugriffe auslöst.
            crate::restore::markiere_schmutzig();
            Some(auftrag)
        }
    }
}

/// Angewendeten Wert zurücklesen (Grundlage der Rückmeldung).
///
/// Nur die Parameter, die ein Fader oder eine Taste braucht; alles andere
/// liefert `None`, dann meldet der Aufrufer den gesendeten Wert zurück.
pub fn lies(params: &Params, target: &str, index: usize, param: &str) -> Option<f32> {
    fn zahl(a: &AtomicU32) -> f32 {
        f32::from_bits(a.load(Ordering::Relaxed))
    }
    fn schalter(a: &AtomicBool) -> f32 {
        if a.load(Ordering::Relaxed) {
            1.0
        } else {
            0.0
        }
    }
    match target {
        "strip" => {
            let s = params.strips.get(index)?;
            Some(match param {
                "gain" => zahl(&s.gain),
                "limit" => zahl(&s.limit),
                "pan" => zahl(&s.pan),
                "mute" => schalter(&s.mute),
                "solo" => schalter(&s.solo),
                "mono" => schalter(&s.mono),
                anderes => {
                    let n = anderes.strip_prefix("send")?.parse::<usize>().ok()?;
                    zahl(s.sends.get(n)?)
                }
            })
        }
        "bus" => {
            let b = params.buses.get(index)?;
            Some(match param {
                "gain" => zahl(&b.gain),
                "limit" => zahl(&b.limit),
                "mute" => schalter(&b.mute),
                "mono" => schalter(&b.mono),
                "trim" => zahl(&b.trim[0]),
                "delay" | "delay_ms" => zahl(&b.delay_ms[0]),
                _ => return None,
            })
        }
        _ => None,
    }
}

/// Ein Paket verarbeiten. Liefert die Rückmeldung, wenn `echo` an ist und es
/// etwas zu melden gibt (nur bei `/strip/...`, `/bus/...`).
pub fn verarbeite_paket(params: &Arc<Params>, paket: &[u8], echo: bool) -> Option<Vec<u8>> {
    let nachricht = parse(paket).ok()?;
    let auftrag = anwende(params, &nachricht)?;
    if !echo {
        return None;
    }
    match auftrag {
        Auftrag::Setzen {
            target,
            index,
            param,
        } => {
            let gesendet = wert_aus(nachricht.args.first()?, &param)?;
            let wert = lies(params, target, index, &param).unwrap_or(gesendet);
            Some(kodiere(
                &format!("/{target}/{index}/{param}"),
                &[Arg::Float(wert)],
            ))
        }
        _ => None,
    }
}

/* ==================================================================
Preset
------------------------------------------------------------------ */

/// Preset-Datei – dieselbe, die `/api/preset` und der Autosave benutzen.
pub fn preset_pfad() -> std::path::PathBuf {
    crate::pfade::datei("preset.json")
}

/// Zustand als Preset nach `pfad` schreiben.
pub fn speichere_nach(params: &Params, pfad: &Path) -> Result<(), String> {
    if let Some(dir) = pfad.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(pfad, params.to_json().to_json()).map_err(|e| format!("{}: {e}", pfad.display()))
}

/// Preset aus `pfad` lesen und anwenden – über `http::apply_preset`, also
/// genau die Felder, die auch die Oberfläche schreibt.
pub fn lade_aus(params: &Arc<Params>, pfad: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(pfad).map_err(|e| format!("{}: {e}", pfad.display()))?;
    let v = crate::json::parse(&text).map_err(|e| format!("{}: {e}", pfad.display()))?;
    crate::http::apply_preset(params, &v);
    crate::restore::markiere_schmutzig();
    Ok(())
}

/// Preset in die Standarddatei schreiben.
pub fn speichere_preset(params: &Params) -> Result<(), String> {
    speichere_nach(params, &preset_pfad())
}

/// Preset aus der Standarddatei laden.
pub fn lade_preset(params: &Arc<Params>) -> Result<(), String> {
    lade_aus(params, &preset_pfad())
}

/* ==================================================================
Eingang
------------------------------------------------------------------ */

/// Laufzeit-Einstellungen des Eingangs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Konfiguration {
    /// Bind-Adresse `host:port`.
    pub addr: String,
    /// Rückmeldung an den Absender.
    pub echo: bool,
}

impl Default for Konfiguration {
    fn default() -> Self {
        Konfiguration {
            addr: VORGABE_ADDR.to_string(),
            echo: false,
        }
    }
}

/// `1`, `true`, `on`, `yes` (jede Schreibweise) schaltet ein.
fn env_schalter(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Err(_) => false,
    }
}

/// Einstellungen aus der Umgebung; leere oder fehlende Variablen ergeben die
/// Vorgabe.
pub fn konfiguration() -> Konfiguration {
    let addr = std::env::var(ENV_ADDR)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| VORGABE_ADDR.to_string());
    Konfiguration {
        addr,
        echo: env_schalter(ENV_ECHO),
    }
}

/// Socket binden. `addr` ist `host:port`, z. B. `127.0.0.1:9000`.
pub fn binde(addr: &str) -> Result<UdpSocket, String> {
    let ziel = addr
        .to_socket_addrs()
        .map_err(|e| format!("{addr}: {e}"))?
        .next()
        .ok_or_else(|| format!("{addr}: keine Adresse"))?;
    UdpSocket::bind(ziel).map_err(|e| format!("bind {ziel}: {e}"))
}

/// Griff zum Anhalten des Eingangs (und Abfrage, ob er noch läuft).
#[derive(Clone, Debug)]
pub struct Halt(Arc<AtomicBool>);

impl Halt {
    fn neu() -> Halt {
        Halt(Arc::new(AtomicBool::new(false)))
    }
    /// Thread beenden; er hält spätestens nach dem Lesetimeout an.
    pub fn anhalten(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    /// Läuft der Eingang noch?
    pub fn laeuft(&self) -> bool {
        !self.0.load(Ordering::Relaxed)
    }
}

/// Eingang auf einem bereits gebundenen Socket starten.
///
/// Getrennt von [`spawn`], damit Tests (und ein Aufrufer, der den Port selbst
/// bestimmen will) denselben Code benutzen können.
pub fn starte(
    params: Arc<Params>,
    sock: UdpSocket,
    echo: bool,
    halt: Halt,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Kurzer Timeout statt blockierendem Lesen: nur so merkt der Thread,
        // dass er angehalten werden soll.
        if let Err(e) = sock.set_read_timeout(Some(Duration::from_millis(200))) {
            eprintln!("OSC-Eingang: Timeout nicht setzbar: {e}");
        }
        let mut buf = vec![0u8; MAX_PAKET];
        while halt.laeuft() {
            match sock.recv_from(&mut buf) {
                Ok((n, herkunft)) => {
                    if let Some(antwort) = verarbeite_paket(&params, &buf[..n], echo) {
                        // Rückweg für Motorfader/LEDs: an den Absender.
                        let _ = sock.send_to(&antwort, herkunft);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => {
                    eprintln!("OSC-Eingang: {e}");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    })
}

/// Eingang im Hintergrund starten (Adresse und Rückmeldung aus der Umgebung).
///
/// Aufruf in `main.rs` nach dem Anlegen der Parameter:
/// `crosspoint::osc::spawn(params.clone());`
///
/// Ein belegter Port ist kein Grund aufzugeben: die Meldung landet auf stderr,
/// die Engine läuft ohne OSC weiter.
pub fn spawn(params: Arc<Params>) -> Halt {
    let k = konfiguration();
    let halt = Halt::neu();
    let sock = match binde(&k.addr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("OSC-Eingang aus: {e}");
            return halt;
        }
    };
    eprintln!(
        "OSC-Eingang auf {} (Rückmeldung {})",
        k.addr,
        if k.echo { "an" } else { "aus" }
    );
    starte(params, sock, k.echo, halt.clone());
    halt
}

/* ==================================================================
Tests
================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;

    fn params() -> Arc<Params> {
        Arc::new(Params::new(48_000.0, 512))
    }

    fn gain(p: &Params, index: usize) -> f32 {
        f32::from_bits(p.strips[index].gain.load(Relaxed))
    }

    /* ----- Parser ----- */

    #[test]
    fn parser_zerlegt_adresse_und_typtags_f_i_s() {
        let paket = kodiere(
            "/strip/3/gain",
            &[Arg::Float(-6.5), Arg::Int(7), Arg::Text("1".into())],
        );
        let n = parse(&paket).expect("gültiges Paket");
        assert_eq!(n.adresse, "/strip/3/gain");
        assert_eq!(
            n.args,
            vec![Arg::Float(-6.5), Arg::Int(7), Arg::Text("1".into())]
        );

        // Typ-Tags stehen wirklich als ",fis" im Paket.
        assert!(paket.windows(4).any(|w| w == b",fis"), "Tags fehlen");

        // Ein einzelnes Float-Paket, wie es MIDIMonster schickt.
        let n = parse(&kodiere("/bus/0/gain", &[Arg::Float(2.0)])).expect("ein Argument");
        assert_eq!(n.args.len(), 1);
        assert_eq!(n.args[0].als_zahl(), Some(2.0));
    }

    #[test]
    fn parser_lehnt_adresse_ohne_schraegstrich_ab() {
        assert_eq!(
            parse(&kodiere("strip/0/gain", &[Arg::Float(0.0)])),
            Err(OscFehler::UngueltigeAdresse)
        );
        assert_eq!(parse(&kodiere("", &[])), Err(OscFehler::AdresseLeer));
    }

    #[test]
    fn parser_lehnt_kaputte_pakete_ab() {
        // Abgeschnitten: Adresse ohne Abschluss-NUL.
        assert_eq!(
            parse(b"/strip/0/gain"),
            Err(OscFehler::AdresseNichtTerminiert)
        );
        // Typtags vorhanden, Argument fehlt.
        let mut paket = kodiere("/strip/0/gain", &[Arg::Float(1.0)]);
        paket.truncate(paket.len() - 2);
        assert_eq!(parse(&paket), Err(OscFehler::ArgumentZuKurz));
        // Unbekannter Typ-Tag (Blob).
        let paket = {
            let mut p = Vec::new();
            schreibe_string(&mut p, "/strip/0/gain");
            schreibe_string(&mut p, ",b");
            p.extend_from_slice(&[0, 0, 0, 0]);
            p
        };
        assert_eq!(parse(&paket), Err(OscFehler::UnbekannterTyp('b')));
        // Kein Typ-Tag, sondern direkt Daten.
        let paket = {
            let mut p = Vec::new();
            schreibe_string(&mut p, "/strip/0/gain");
            schreibe_string(&mut p, "1234");
            p
        };
        assert_eq!(parse(&paket), Err(OscFehler::FehlendeTypTags));
        assert_eq!(parse(b""), Err(OscFehler::ZuKurz));
    }

    #[test]
    fn adresse_wird_in_auftrag_zerlegt() {
        assert_eq!(
            auftrag("/strip/0/gain"),
            Ok(Auftrag::Setzen {
                target: "strip",
                index: 0,
                param: "gain".into()
            })
        );
        assert_eq!(
            auftrag("/bus/7/mute"),
            Ok(Auftrag::Setzen {
                target: "bus",
                index: 7,
                param: "mute".into()
            })
        );
        assert_eq!(auftrag("/preset/save"), Ok(Auftrag::PresetSpeichern));
        assert_eq!(auftrag("/preset/load"), Ok(Auftrag::PresetLaden));
        // Kein Index, kein Parameter, unbekanntes Ziel.
        assert_eq!(auftrag("/strip/x/gain"), Err(OscFehler::UngueltigeAdresse));
        assert_eq!(auftrag("/strip/0"), Err(OscFehler::UnbekannteAdresse));
        assert_eq!(auftrag("/master/0/gain"), Err(OscFehler::UnbekannteAdresse));
        assert_eq!(auftrag("/preset/reset"), Err(OscFehler::UnbekannteAdresse));
        assert_eq!(auftrag("/"), Err(OscFehler::UnbekannteAdresse));
    }

    /* ----- Wertebereich ----- */

    #[test]
    fn cc_zu_db_trifft_die_endpunkte_und_die_mitte() {
        assert!((cc_zu_db(0.0) - (-60.0)).abs() < 1e-6);
        assert!((cc_zu_db(127.0) - 12.0).abs() < 1e-6);
        assert!(
            (cc_zu_db(63.5) - (-24.0)).abs() < 1e-6,
            "{}",
            cc_zu_db(63.5)
        );
        assert!((cc_zu_db(64.0) - (-23.716_537)).abs() < 1e-4);
        // Klemmung und Sonderfälle.
        assert!((cc_zu_db(-5.0) - (-60.0)).abs() < 1e-6);
        assert!((cc_zu_db(200.0) - 12.0).abs() < 1e-6);
        assert!((cc_zu_db(f32::NAN) - (-60.0)).abs() < 1e-6);
        // Rückweg: dB -> CC -> dB bleibt innerhalb einer CC-Stufe.
        for db in [-60.0, -40.0, -6.0, 0.0, 12.0] {
            let zurueck = cc_zu_db(db_zu_cc(db));
            assert!((zurueck - db).abs() < 0.6, "{db} -> {zurueck}");
        }
    }

    /* ----- Anwenden ----- */

    #[test]
    fn streifen_gain_wird_gesetzt() {
        let p = params();
        let paket = kodiere("/strip/0/gain", &[Arg::Float(-6.0)]);
        assert_eq!(p.set("strip", 0, "gain", 0.0), true, "Vorbedingung");
        assert!(verarbeite_paket(&p, &paket, false).is_none());
        assert!((gain(&p, 0) - (-6.0)).abs() < 1e-6, "{}", gain(&p, 0));
        // Nur der angesprochene Streifen ändert sich.
        assert!((gain(&p, 1)).abs() < 1e-6);
    }

    #[test]
    fn streifen_mute_wird_gesetzt() {
        let p = params();
        assert!(
            verarbeite_paket(&p, &kodiere("/strip/3/mute", &[Arg::Float(1.0)]), false).is_none()
        );
        assert!(p.strips[3].mute.load(Relaxed), "Streifen 3 ist stumm");
        assert!(!p.strips[2].mute.load(Relaxed), "Streifen 2 nicht");
        // Text-Argument als Schalter, wie es manche Sender schicken.
        verarbeite_paket(
            &p,
            &kodiere("/strip/3/mute", &[Arg::Text("off".into())]),
            false,
        );
        assert!(!p.strips[3].mute.load(Relaxed));
    }

    #[test]
    fn ganzzahl_auf_gain_ist_ein_midi_cc() {
        let p = params();
        verarbeite_paket(&p, &kodiere("/strip/1/gain", &[Arg::Int(64)]), false);
        assert!(
            (gain(&p, 1) - cc_zu_db(64.0)).abs() < 1e-6,
            "{}",
            gain(&p, 1)
        );
        // Float bleibt dB – 64.0 dB wird von Params auf den Anschlag geklemmt.
        verarbeite_paket(&p, &kodiere("/bus/0/gain", &[Arg::Float(64.0)]), false);
        let b = f32::from_bits(p.buses[0].gain.load(Relaxed));
        assert!((b - DB_MAX).abs() < 1e-6, "Bus-Gain {b}");
    }

    #[test]
    fn unbekanntes_wird_still_verworfen() {
        let p = params();
        assert!(
            verarbeite_paket(&p, &kodiere("/strip/9/gain", &[Arg::Float(1.0)]), true).is_none()
        );
        assert!(verarbeite_paket(
            &p,
            &kodiere("/strip/0/gibtsnicht", &[Arg::Float(1.0)]),
            true
        )
        .is_none());
        assert!(verarbeite_paket(&p, &kodiere("/strip/0/gain", &[]), true).is_none());
        assert!(verarbeite_paket(&p, &kodiere("/quatsch", &[Arg::Float(1.0)]), true).is_none());
        // Unsinn im Paket darf nichts ändern und nichts antworten.
        assert!(verarbeite_paket(&p, b"\x00\x01\x02", true).is_none());
        assert!((gain(&p, 0)).abs() < 1e-6);
    }

    /* ----- Rückmeldung ----- */

    #[test]
    fn echo_antwort_enthaelt_den_uebernommenen_wert() {
        let p = params();
        let antwort = verarbeite_paket(&p, &kodiere("/strip/2/gain", &[Arg::Float(-6.0)]), true)
            .expect("Rückmeldung");
        let n = parse(&antwort).expect("Antwort parst");
        assert_eq!(n.adresse, "/strip/2/gain");
        assert_eq!(n.args, vec![Arg::Float(-6.0)]);

        // Geklemmt: was wirklich im Pult steht, geht zurück, nicht die Rohzahl.
        let antwort = verarbeite_paket(&p, &kodiere("/strip/2/gain", &[Arg::Float(-100.0)]), true)
            .expect("Rückmeldung");
        assert_eq!(parse(&antwort).unwrap().args, vec![Arg::Float(DB_MIN)]);

        // Taste: 1/0 statt dB.
        let antwort = verarbeite_paket(&p, &kodiere("/strip/5/mute", &[Arg::Float(1.0)]), true)
            .expect("Rückmeldung");
        assert_eq!(parse(&antwort).unwrap().args, vec![Arg::Float(1.0)]);

        // Rückmeldung aus: nichts senden, aber anwenden.
        assert!(
            verarbeite_paket(&p, &kodiere("/strip/6/gain", &[Arg::Float(1.0)]), false).is_none()
        );
        assert!((gain(&p, 6) - 1.0).abs() < 1e-6);
    }

    /* ----- Preset ----- */

    #[test]
    fn preset_wird_geschrieben_und_wieder_gelesen() {
        let p = params();
        let dir = std::env::temp_dir().join(format!("crosspoint-osc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pfad = dir.join("preset.json");

        p.set("strip", 0, "gain", -6.0);
        speichere_nach(&p, &pfad).expect("schreiben");
        assert!(pfad.is_file(), "Datei fehlt: {}", pfad.display());

        p.set("strip", 0, "gain", 3.0);
        lade_aus(&p, &pfad).expect("lesen");
        assert!((gain(&p, 0) - (-6.0)).abs() < 1e-6, "{}", gain(&p, 0));

        // Die Adresse für "speichern" löst auf den Auftrag auf. Bewusst ohne
        // verarbeite_paket: das würde die echte Preset-Datei des Benutzers
        // überschreiben – Tests schreiben nur in ihr eigenes Verzeichnis.
        assert_eq!(auftrag("/preset/save"), Ok(Auftrag::PresetSpeichern));
        assert_eq!(auftrag("/preset/load"), Ok(Auftrag::PresetLaden));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /* ----- echter Socket ----- */

    #[test]
    fn echtes_udp_paket_wird_angewendet_und_beantwortet() {
        let p = params();
        let sock = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let port = sock.local_addr().expect("Adresse").port();
        let halt = Halt::neu();
        let _lauf = starte(p.clone(), sock, true, halt.clone());

        let client = UdpSocket::bind("127.0.0.1:0").expect("Client");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("Timeout");
        let paket = kodiere("/strip/4/gain", &[Arg::Float(-12.0)]);
        client
            .send_to(&paket, format!("127.0.0.1:{port}"))
            .expect("send");

        // Die Antwort kommt erst, wenn das Paket angewendet wurde – damit ist
        // sie zugleich der Beweis, dass der Empfang wirklich über UDP lief.
        let mut buf = [0u8; 256];
        let (n, _) = client.recv_from(&mut buf).expect("Rückmeldung");
        let antwort = parse(&buf[..n]).expect("Antwort parst");
        assert_eq!(antwort.adresse, "/strip/4/gain");
        assert_eq!(antwort.args, vec![Arg::Float(-12.0)]);
        assert!((gain(&p, 4) - (-12.0)).abs() < 1e-6, "{}", gain(&p, 4));

        // Ein zweites Paket ohne Band: Mute über einen CC-Wert als Ganzzahl.
        let paket = kodiere("/strip/4/mute", &[Arg::Int(1)]);
        client
            .send_to(&paket, format!("127.0.0.1:{port}"))
            .expect("send");
        let (n, _) = client.recv_from(&mut buf).expect("Rückmeldung Mute");
        assert_eq!(
            parse(&buf[..n]).unwrap().args,
            vec![Arg::Float(1.0)],
            "Mute meldet 1 zurück"
        );
        assert!(p.strips[4].mute.load(Relaxed));

        halt.anhalten();
        assert!(!halt.laeuft());
    }

    #[test]
    fn konfiguration_kommt_aus_der_umgebung() {
        // Ohne gesetzte Variablen gilt die Vorgabe.
        let k = Konfiguration::default();
        assert_eq!(k.addr, VORGABE_ADDR);
        assert!(!k.echo);
        // Der Schalter ist nicht gesetzt, wenn nichts gesetzt ist.
        assert!(!env_schalter("CROSSPOINT_OSC_TEST_GIBTS_NICHT"));
        assert!(binde("127.0.0.1:0").is_ok());
        assert!(binde("127.0.0.1:keinport").is_err());
    }
}
