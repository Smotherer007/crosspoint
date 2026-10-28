//! Oberfläche der VoiceMeeter-Remote-API (`VBVMR_*`) als reine Rust-Funktionen.
//!
//! Das Original stellt eine C-DLL mit Prozess-globalem Zustand bereit
//! (`VBVMR_Login`, `VBVMR_SetParameterFloat`, …). Hier liegt derselbe
//! Funktionsumfang als Methoden auf [`RemoteApi`], das ein `Arc<Params>`
//! hält und den API-eigenen Zustand (Login-Zähler, Running-Flag, Dirty-Flag,
//! Macro-Button-Tabelle) in Atomen führt — damit ist die API `Send + Sync` und
//! kann vom HTTP-Thread, von einem MIDI-Thread und vom Audio-Thread zugleich
//! benutzt werden.
//!
//! Abbildung Original → hier:
//!
//! | VBVMR_*                          | Methode                                  |
//! |----------------------------------|------------------------------------------|
//! | `VBVMR_Login` / `VBVMR_Logout`    | [`RemoteApi::login`] / [`RemoteApi::logout`] |
//! | `VBVMR_RunVoicemeeter`            | [`RemoteApi::run_voicemeeter`]           |
//! | `VBVMR_GetVoicemeeterType`        | [`RemoteApi::get_voicemeeter_type`]      |
//! | `VBVMR_GetVoicemeeterVersion`     | [`RemoteApi::get_voicemeeter_version`]   |
//! | `VBVMR_SetParameterFloat`         | [`RemoteApi::set_parameter_float`]       |
//! | `VBVMR_GetParameterFloat`         | [`RemoteApi::get_parameter_float`]       |
//! | `VBVMR_SetParameterStringW`       | [`RemoteApi::set_parameter_string`]      |
//! | `VBVMR_GetParameterStringW`       | [`RemoteApi::get_parameter_string`]      |
//! | `VBVMR_SetParameters`             | [`RemoteApi::set_parameters`]            |
//! | `VBVMR_IsParametersDirty`         | [`RemoteApi::is_parameters_dirty`]       |
//! | `VBVMR_GetLevel`                  | [`RemoteApi::get_level`]                 |
//! | `VBVMR_GetMidiMessage`            | [`RemoteApi::get_midi_message`]          |
//! | `VBVMR_MacroButton_SetStatus`     | [`RemoteApi::macro_button_set_status`]   |
//! | `VBVMR_MacroButton_GetStatus`     | [`RemoteApi::macro_button_get_status`]   |
//! | `VBVMR_MacroButton_IsDirty`       | [`RemoteApi::macro_button_is_dirty`]     |
//!
//! Unterschiede zum Original, bewusst:
//!   * Es gibt keine globale Instanz und keinen Rückgabecode `VBVMR_RESULT_*`;
//!     Fehler kommen als [`RemoteError`] zurück.
//!   * Die API schreibt direkt in [`Params`]; es gibt keinen zweiten,
//!     gespiegelten Parametersatz, der auseinanderlaufen könnte.
//!   * Als Gerätetyp meldet sich das Pult als **Potato** (8 Streifen, 8 Busse)
//!     — das ist die Konfiguration, die `vm8` tatsächlich abbildet.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::Arc;

use crate::engine::{BusParam, Params, StripParam};
use crate::json::Value;
use crate::{BUSES, EQ_CELLS, STRIPS};

/// Anzahl der Macro-Button-Slots wie im Original (`VBVMR_MACROBUTTON_NUMBER`).
pub const MACRO_BUTTONS: usize = 80;

/// Version, die als VoiceMeeter-Version gemeldet wird (3.1.3.0).
pub const VERSION_MAJOR: u32 = 3;
pub const VERSION_MINOR: u32 = 1;
pub const VERSION_PATCH: u32 = 3;
pub const VERSION_BUILD: u32 = 0;

/// Gerätetyp wie `VBVMR_DEVICE_*`: 1 = Standard (3 Streifen), 2 = Banana
/// (5 Streifen), 3 = Potato (8 Streifen).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoicemeeterType {
    Standard = 1,
    Banana = 2,
    Potato = 3,
}

impl VoicemeeterType {
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    /// Anzahl Streifen/Busse des Typs (Standard 3/2, Banana 5/5, Potato 8/8).
    pub fn channels(self) -> (usize, usize) {
        match self {
            VoicemeeterType::Standard => (3, 2),
            VoicemeeterType::Banana => (5, 5),
            VoicemeeterType::Potato => (8, 8),
        }
    }
}

/// Der Typ, den dieses Pult abbildet: 8 Streifen und 8 Busse = Potato.
pub const DEVICE_TYPE: VoicemeeterType = VoicemeeterType::Potato;

/// Fehler der Remote-API. Entspricht den negativen Rückgabecodes des Originals,
/// ist aber als Typ unterscheidbar.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RemoteError {
    /// `VBVMR_Logout` ohne vorherigen `Login`.
    NotLoggedIn,
    /// Ziel unbekannt — es gibt nur `Strip` und `Bus`.
    UnknownTarget,
    /// Index außerhalb 0..8 (Streifen/Busse) bzw. Kanal > 1.
    IndexOutOfRange,
    /// Parameter existiert für dieses Ziel nicht.
    UnknownParameter,
    /// Wert ließ sich nicht als Zahl lesen.
    InvalidValue(String),
    /// Zuweisung nicht lesbar (fehlendes `=`, `[`, `]` oder `.`).
    Syntax(String),
}

impl fmt::Display for RemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemoteError::NotLoggedIn => write!(f, "nicht angemeldet"),
            RemoteError::UnknownTarget => write!(f, "unbekanntes Ziel (nur Strip/Bus)"),
            RemoteError::IndexOutOfRange => write!(f, "Index außerhalb des gültigen Bereichs"),
            RemoteError::UnknownParameter => write!(f, "unbekannter Parameter"),
            RemoteError::InvalidValue(v) => write!(f, "Wert '{v}' ist keine Zahl"),
            RemoteError::Syntax(s) => write!(f, "Zuweisung '{s}' nicht lesbar"),
        }
    }
}

impl std::error::Error for RemoteError {}

pub type RemoteResult<T> = Result<T, RemoteError>;

/// Ziel einer Zuweisung. `Strip` entspricht `Strip[n]`, `Bus` entspricht `Bus[n]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Strip,
    Bus,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Strip => "strip",
            Target::Bus => "bus",
        }
    }

    /// Anzahl gültiger Indizes des Ziels.
    pub fn len(self) -> usize {
        match self {
            Target::Strip => STRIPS,
            Target::Bus => BUSES,
        }
    }
}

/// Eine geparste Zuweisung aus dem `SetParameters`-Text.
#[derive(Clone, PartialEq, Debug)]
pub struct Assignment {
    pub target: Target,
    pub index: usize,
    /// Bereits auf die Namen von [`Params`] normalisiert (klein geschrieben).
    pub param: String,
    pub value: f32,
}

impl Assignment {
    /// Zuweisung auf `Params` anwenden (ohne Dirty-Flag; das macht der Aufrufer).
    pub fn apply(&self, params: &Params) -> bool {
        params.set(self.target.as_str(), self.index, &self.param, self.value)
    }
}

/// Ein Macro-Button-Slot: Status wie im Original (0/1) plus eigenes Dirty-Flag.
struct MacroSlot {
    status: AtomicU32,
    dirty: AtomicBool,
}

impl MacroSlot {
    fn new() -> MacroSlot {
        MacroSlot { status: AtomicU32::new(0), dirty: AtomicBool::new(false) }
    }
}

/// Die Remote-API über einem geteilten Parametersatz.
pub struct RemoteApi {
    params: Arc<Params>,
    /// Referenzzähler wie im Original: jedes `Login` zählt hoch, `Logout` runter.
    logins: AtomicU32,
    running: AtomicBool,
    /// `IsParametersDirty`: wird bei jeder Änderung gesetzt, beim Abfragen gelöscht.
    dirty: AtomicBool,
    macros: [MacroSlot; MACRO_BUTTONS],
}

impl RemoteApi {
    pub fn new(params: Arc<Params>) -> RemoteApi {
        RemoteApi {
            params,
            logins: AtomicU32::new(0),
            running: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            macros: std::array::from_fn(|_| MacroSlot::new()),
        }
    }

    pub fn params(&self) -> &Arc<Params> {
        &self.params
    }

    // ---------- Sitzung ----------

    /// `VBVMR_Login`: zählt die Anmeldungen hoch und gibt den neuen Stand zurück.
    /// Mehrfaches Anmelden ist erlaubt (Referenzzähler wie im Original).
    pub fn login(&self) -> u32 {
        self.logins.fetch_add(1, Relaxed) + 1
    }

    /// `VBVMR_Logout`: zählt herunter. Ohne offene Anmeldung ein Fehler.
    pub fn logout(&self) -> RemoteResult<u32> {
        loop {
            let cur = self.logins.load(Relaxed);
            if cur == 0 {
                return Err(RemoteError::NotLoggedIn);
            }
            if self
                .logins
                .compare_exchange_weak(cur, cur - 1, Relaxed, Relaxed)
                .is_ok()
            {
                return Ok(cur - 1);
            }
        }
    }

    /// Aktueller Stand des Login-Zählers.
    pub fn login_count(&self) -> u32 {
        self.logins.load(Relaxed)
    }

    /// `VBVMR_RunVoicemeeter`: Modus 0 stoppt die Engine, alles andere startet sie.
    /// Gibt den Running-Zustand nach dem Aufruf zurück.
    pub fn run_voicemeeter(&self, mode: i32) -> bool {
        let run = mode != 0;
        self.running.store(run, Relaxed);
        run
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Relaxed)
    }

    // ---------- Kennung ----------

    /// `VBVMR_GetVoicemeeterType`: 1 = Standard, 2 = Banana, 3 = Potato.
    pub fn get_voicemeeter_type(&self) -> VoicemeeterType {
        DEVICE_TYPE
    }

    /// `VBVMR_GetVoicemeeterVersion`: `Major << 24 | Minor << 16 | Patch << 8 | Build`.
    pub fn get_voicemeeter_version(&self) -> u32 {
        make_version(VERSION_MAJOR, VERSION_MINOR, VERSION_PATCH, VERSION_BUILD)
    }

    // ---------- Parameter ----------

    /// `VBVMR_SetParameterFloat`.
    pub fn set_parameter_float(
        &self,
        target: &str,
        index: usize,
        param: &str,
        value: f32,
    ) -> RemoteResult<()> {
        let target = parse_target(target)?;
        if index >= target.len() {
            return Err(RemoteError::IndexOutOfRange);
        }
        let param = normalize_param(param);
        if !self.params.set(target.as_str(), index, &param, value) {
            return Err(RemoteError::UnknownParameter);
        }
        self.dirty.store(true, Relaxed);
        Ok(())
    }

    /// `VBVMR_GetParameterFloat`.
    pub fn get_parameter_float(&self, target: &str, index: usize, param: &str) -> RemoteResult<f32> {
        let target = parse_target(target)?;
        let param = normalize_param(param);
        let value = match target {
            Target::Strip => match self.params.strips.get(index) {
                Some(s) => strip_get(s, &param),
                None => return Err(RemoteError::IndexOutOfRange),
            },
            Target::Bus => match self.params.buses.get(index) {
                Some(b) => bus_get(b, &param),
                None => return Err(RemoteError::IndexOutOfRange),
            },
        };
        value.ok_or(RemoteError::UnknownParameter)
    }

    /// `VBVMR_SetParameterStringW`: dieselben Parameter, Zahl als Text
    /// (`"-6.0"`, `"1"`, `"on"`/`"off"`).
    pub fn set_parameter_string(
        &self,
        target: &str,
        index: usize,
        param: &str,
        value: &str,
    ) -> RemoteResult<()> {
        let v = parse_value(value).ok_or_else(|| RemoteError::InvalidValue(value.to_string()))?;
        self.set_parameter_float(target, index, param, v)
    }

    /// `VBVMR_GetParameterStringW`: Zahl als Text. Ganzzahlige Werte mit einer
    /// Nachkommastelle (`"-6.0"`), sonst so kurz wie möglich.
    pub fn get_parameter_string(&self, target: &str, index: usize, param: &str) -> RemoteResult<String> {
        Ok(format_number(self.get_parameter_float(target, index, param)?))
    }

    /// `VBVMR_SetParameters`: Text im Originalformat, mehrere Zuweisungen durch
    /// `;` (oder Zeilenumbruch) getrennt, z. B.
    /// `"Strip[0].Gain=-6.0;Bus[1].Mute=1;"`.
    ///
    /// Ziel- und Parameternamen sind unabhängig von der Schreibweise. Gibt die
    /// Anzahl der angewandten Zuweisungen zurück; beim ersten Fehler bricht der
    /// Aufruf ab (die davor angewandten bleiben stehen, wie im Original).
    pub fn set_parameters(&self, text: &str) -> RemoteResult<usize> {
        let mut count = 0;
        for part in text.split([';', '\n']) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let a = parse_assignment(part)?;
            if a.index >= a.target.len() {
                return Err(RemoteError::IndexOutOfRange);
            }
            if !a.apply(&self.params) {
                return Err(RemoteError::UnknownParameter);
            }
            self.dirty.store(true, Relaxed);
            count += 1;
        }
        Ok(count)
    }

    /// `VBVMR_IsParametersDirty`: meldet, ob sich seit der letzten Abfrage etwas
    /// geändert hat, und löscht dabei das Flag (Zustand wird konsumiert).
    pub fn is_parameters_dirty(&self) -> bool {
        self.dirty.swap(false, Relaxed)
    }

    /// `VBVMR_GetLevel`: Pegel eines Bus-Kanals in dB. Ohne Signal -140 dB.
    /// Nur die Kanäle 0 und 1 existieren (Stereo-Summe, beide gleich).
    pub fn get_level(&self, bus: usize, channel: usize) -> RemoteResult<f32> {
        let b = self.params.buses.get(bus).ok_or(RemoteError::IndexOutOfRange)?;
        if channel > 1 {
            return Err(RemoteError::IndexOutOfRange);
        }
        Ok(crate::dsp::lin_to_db(fget(&b.level)))
    }

    /// `VBVMR_GetMidiMessage`: **nicht implementiert**, liefert immer 0.
    ///
    /// Das Original schleust hier MIDI-CC/Mackie-Control-Meldungen in die
    /// Konsole; vm8 hat keinen MIDI-Eingang. 0 bedeutet dort „keine Nachricht".
    pub fn get_midi_message(&self) -> i32 {
        0
    }

    // ---------- Macro-Buttons ----------

    /// `VBVMR_MacroButton_SetStatus`: setzt den Status eines Slots (0/1) und
    /// markiert genau diesen Slot als geändert.
    pub fn macro_button_set_status(&self, index: usize, status: u32) -> RemoteResult<()> {
        let slot = self.macros.get(index).ok_or(RemoteError::IndexOutOfRange)?;
        if slot.status.swap(status, Relaxed) != status {
            slot.dirty.store(true, Relaxed);
        }
        Ok(())
    }

    /// `VBVMR_MacroButton_GetStatus`.
    pub fn macro_button_get_status(&self, index: usize) -> RemoteResult<u32> {
        let slot = self.macros.get(index).ok_or(RemoteError::IndexOutOfRange)?;
        Ok(slot.status.load(Relaxed))
    }

    /// `VBVMR_MacroButton_IsDirty`: wird beim Abfragen gelöscht.
    pub fn macro_button_is_dirty(&self, index: usize) -> RemoteResult<bool> {
        let slot = self.macros.get(index).ok_or(RemoteError::IndexOutOfRange)?;
        Ok(slot.dirty.swap(false, Relaxed))
    }

    /// Zustand der API als JSON (für eine Statusanzeige). Das Parameter-Dirty-Flag
    /// wird dabei **nicht** gelöscht, nur gelesen.
    pub fn state_json(&self) -> Value {
        Value::obj(vec![
            ("type", Value::num(self.get_voicemeeter_type().as_i32() as f64)),
            ("version", Value::num(self.get_voicemeeter_version() as f64)),
            ("logins", Value::num(self.login_count() as f64)),
            ("running", Value::Bool(self.is_running())),
            ("parameters_dirty", Value::Bool(self.dirty.load(Relaxed))),
            (
                "macro_status",
                Value::Arr(
                    self.macros
                        .iter()
                        .map(|m| Value::num(m.status.load(Relaxed) as f64))
                        .collect(),
                ),
            ),
        ])
    }
}

/// Version wie `VBVMR_GetVoicemeeterVersion` packen.
pub fn make_version(major: u32, minor: u32, patch: u32, build: u32) -> u32 {
    ((major & 0xff) << 24) | ((minor & 0xff) << 16) | ((patch & 0xff) << 8) | (build & 0xff)
}

/// Zuweisung im Originalformat parsen.
pub fn parse_assignment(text: &str) -> RemoteResult<Assignment> {
    let text = text.trim();
    let (lhs, rhs) = text
        .split_once('=')
        .ok_or_else(|| RemoteError::Syntax(text.to_string()))?;
    let lhs = lhs.trim();
    let open = lhs.find('[').ok_or_else(|| RemoteError::Syntax(text.to_string()))?;
    let close = lhs.find(']').ok_or_else(|| RemoteError::Syntax(text.to_string()))?;
    if close < open {
        return Err(RemoteError::Syntax(text.to_string()));
    }
    let target = parse_target(lhs[..open].trim())?;
    let index: usize = lhs[open + 1..close]
        .trim()
        .parse()
        .map_err(|_| RemoteError::Syntax(text.to_string()))?;
    let rest = lhs[close + 1..].trim();
    let param = rest
        .strip_prefix('.')
        .ok_or_else(|| RemoteError::Syntax(text.to_string()))?
        .trim();
    if param.is_empty() {
        return Err(RemoteError::Syntax(text.to_string()));
    }
    let raw = rhs.trim();
    let value = parse_value(raw).ok_or_else(|| RemoteError::InvalidValue(raw.to_string()))?;
    Ok(Assignment { target, index, param: normalize_param(param), value })
}

/// Zielnamen tolerant lesen: `Strip`, `strip`, `STRIPS`, `Bus`, `BUS`.
pub fn parse_target(name: &str) -> RemoteResult<Target> {
    match name.trim().to_ascii_lowercase().as_str() {
        "strip" | "strips" => Ok(Target::Strip),
        "bus" | "buses" | "buss" => Ok(Target::Bus),
        _ => Err(RemoteError::UnknownTarget),
    }
}

/// Parameternamen auf die Namen von [`Params`] normalisieren: Schreibweise egal,
/// dazu die Original-Aliase (`dBLevel`, `A1`..`B3` für das Routing).
pub fn normalize_param(param: &str) -> String {
    let p = param.trim().to_ascii_lowercase();
    match p.as_str() {
        "dblevel" => "gain".to_string(),
        "audibility" => "denoiser".to_string(),
        _ => {
            let b = p.as_bytes();
            // Routing-Matrix wie im Original: Strip[0].A1 = 1 … A5 = 1, B1..B3.
            if b.len() == 2 && b[1].is_ascii_digit() {
                let n = (b[1] - b'0') as usize;
                if b[0] == b'a' && (1..=5).contains(&n) {
                    return format!("send{}", n - 1);
                }
                if b[0] == b'b' && (1..=3).contains(&n) {
                    return format!("send{}", 4 + n);
                }
            }
            p
        }
    }
}

/// Zahlenwert tolerant lesen: `-6.0`, `1`, `on`/`off`, `true`/`false`.
pub fn parse_value(s: &str) -> Option<f32> {
    let s = s.trim();
    match s.to_ascii_lowercase().as_str() {
        "on" | "true" => Some(1.0),
        "off" | "false" => Some(0.0),
        _ => s.parse::<f32>().ok(),
    }
}

/// Zahl als Text für `GetParameterString`.
pub fn format_number(v: f32) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    if v.fract() == 0.0 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

// ---------- Lesen aus Params ----------
//
// Spiegelt genau die Parameter, die `Params::set` annimmt (siehe engine.rs);
// alles andere ist `None` und wird zum Fehler `UnknownParameter`.

#[inline]
fn fget(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Relaxed))
}

#[inline]
fn flag(b: bool) -> f32 {
    if b { 1.0 } else { 0.0 }
}

fn strip_get(s: &StripParam, param: &str) -> Option<f32> {
    Some(match param {
        "gain" => fget(&s.gain),
        "pan" => fget(&s.pan),
        "mute" => flag(s.mute.load(Relaxed)),
        "solo" => flag(s.solo.load(Relaxed)),
        "mono" => flag(s.mono.load(Relaxed)),
        "eq" => flag(s.eq_on.load(Relaxed)),
        "comp" => flag(s.comp.on.load(Relaxed)),
        "gate" => flag(s.gate.on.load(Relaxed)),
        "denoiser" => flag(s.dn_on.load(Relaxed)),
        "comp_threshold" => fget(&s.comp.threshold),
        "comp_ratio" => fget(&s.comp.ratio),
        "comp_attack" => fget(&s.comp.attack),
        "comp_release" => fget(&s.comp.release),
        "comp_knee" => fget(&s.comp.knee),
        "gate_threshold" => fget(&s.gate.threshold),
        "gate_attack" => fget(&s.gate.attack),
        "gate_hold" => fget(&s.gate.hold),
        "gate_release" => fget(&s.gate.release),
        _ => return strip_get_ext(s, param),
    })
}

fn strip_get_ext(s: &StripParam, param: &str) -> Option<f32> {
    if let Some(rest) = param.strip_prefix("send") {
        let bus: usize = rest.parse().ok()?;
        return s.sends.get(bus).map(fget);
    }
    if let Some(rest) = param.strip_prefix("eq") {
        let (cell, field) = rest.split_once('_')?;
        let cell: usize = cell.parse().ok()?;
        if cell >= EQ_CELLS {
            return None;
        }
        return match field {
            "on" => Some(flag(s.eq[0][cell].on.load(Relaxed))),
            "type" => Some(s.eq[0][cell].kind.load(Relaxed) as f32),
            "freq" => Some(fget(&s.eq[0][cell].freq)),
            "q" => Some(fget(&s.eq[0][cell].q)),
            "gain" => Some(fget(&s.eq[0][cell].gain)),
            _ => None,
        };
    }
    None
}

fn bus_get(b: &BusParam, param: &str) -> Option<f32> {
    Some(match param {
        "gain" => fget(&b.gain),
        "mute" => flag(b.mute.load(Relaxed)),
        "mono" => flag(b.mono.load(Relaxed)),
        "eq" => flag(b.eq_on.load(Relaxed)),
        "limiter" => flag(b.limiter_on.load(Relaxed)),
        "c5" => flag(b.c5_on.load(Relaxed)),
        "c5_threshold" => fget(&b.c5_threshold),
        "c5_ceiling" => fget(&b.c5_ceiling),
        "trim" => fget(&b.trim[0]),
        "delay" => fget(&b.delay_ms[0]),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api() -> (Arc<Params>, RemoteApi) {
        let p = Arc::new(Params::new(48000.0, 512));
        let a = RemoteApi::new(p.clone());
        (p, a)
    }

    fn gain_db(p: &Params, strip: usize) -> f32 {
        f32::from_bits(p.strips[strip].gain.load(Relaxed))
    }

    fn set_lin(a: &AtomicU32, v: f32) {
        a.store(v.to_bits(), Relaxed);
    }

    #[test]
    fn login_und_logout_zaehlen() {
        let (_p, api) = api();
        assert_eq!(api.login_count(), 0);
        assert_eq!(api.login(), 1);
        assert_eq!(api.login(), 2, "zweiter Login zählt weiter hoch");
        assert_eq!(api.login_count(), 2);
        assert_eq!(api.logout().unwrap(), 1);
        assert_eq!(api.logout().unwrap(), 0);
        assert_eq!(api.logout(), Err(RemoteError::NotLoggedIn));
        assert_eq!(api.login_count(), 0);
    }

    #[test]
    fn set_parameters_setzt_drei_zuweisungen() {
        let (p, api) = api();
        let n = api
            .set_parameters("Strip[0].Gain=-6.0;Bus[1].Mute=1;Strip[2].Mute=1;")
            .unwrap();
        assert_eq!(n, 3);
        // wirklich in Params, nicht nur im Parser
        assert!((gain_db(&p, 0) + 6.0).abs() < 1e-6, "Streifen 1 Gain war {}", gain_db(&p, 0));
        assert!(p.buses[1].mute.load(Relaxed), "Bus 2 soll stumm sein");
        assert!(p.strips[2].mute.load(Relaxed), "Streifen 3 soll stumm sein");

        // Schreibweise egal, Zeilenumbruch statt Semikolon, Original-Routing A1
        let n = api
            .set_parameters("STRIP[3].gain=-3.0\nBus[2].MUTE=on;Strip[0].A1=0")
            .unwrap();
        assert_eq!(n, 3);
        assert!((gain_db(&p, 3) + 3.0).abs() < 1e-6);
        assert!(p.buses[2].mute.load(Relaxed));
        assert!((f32::from_bits(p.strips[0].sends[0].load(Relaxed))).abs() < 1e-6);
    }

    #[test]
    fn dirty_flag_loescht_sich_beim_abfragen() {
        let (_p, api) = api();
        assert!(!api.is_parameters_dirty(), "frisch = sauber");
        api.set_parameter_float("strip", 0, "gain", -6.0).unwrap();
        assert!(api.is_parameters_dirty());
        assert!(!api.is_parameters_dirty(), "zweite Abfrage ist sauber");
        api.set_parameters("Bus[0].Gain=-1.5;").unwrap();
        assert!(api.is_parameters_dirty());
        assert!(!api.is_parameters_dirty());
    }

    #[test]
    fn get_parameter_float_liest_gesetzten_wert() {
        let (_p, api) = api();
        api.set_parameter_float("strip", 1, "Gain", -6.5).unwrap();
        let v = api.get_parameter_float("strip", 1, "gain").unwrap();
        assert!((v + 6.5).abs() < 1e-6, "gelesen wurde {v}");
        // Groß/Klein im Getter ebenso tolerant
        assert!((api.get_parameter_float("STRIP", 1, "gAiN").unwrap() + 6.5).abs() < 1e-6);
        // Bool als 0/1, Bus-Kanal-Delay auf beide Kanäle
        assert_eq!(api.get_parameter_float("strip", 0, "mute").unwrap(), 0.0);
        api.set_parameter_float("bus", 0, "delay", 12.5).unwrap();
        assert!((api.get_parameter_float("bus", 0, "Delay").unwrap() - 12.5).abs() < 1e-4);
        // String-Sicht derselben Parameter
        assert_eq!(api.get_parameter_string("strip", 1, "gain").unwrap(), "-6.5");
        api.set_parameter_string("Bus", 3, "Gain", "-3").unwrap();
        assert_eq!(api.get_parameter_string("bus", 3, "gain").unwrap(), "-3.0");
    }

    #[test]
    fn unbekannter_parameter_ist_fehler() {
        let (_p, api) = api();
        assert_eq!(
            api.set_parameter_float("strip", 0, "gibtsnicht", 1.0),
            Err(RemoteError::UnknownParameter)
        );
        assert_eq!(
            api.get_parameter_float("strip", 0, "gibtsnicht"),
            Err(RemoteError::UnknownParameter)
        );
        assert_eq!(
            api.set_parameter_float("strip", STRIPS, "gain", 0.0),
            Err(RemoteError::IndexOutOfRange)
        );
        assert_eq!(
            api.get_parameter_float("bus", BUSES, "gain"),
            Err(RemoteError::IndexOutOfRange)
        );
        assert_eq!(
            api.set_parameter_float("master", 0, "gain", 0.0),
            Err(RemoteError::UnknownTarget)
        );
        // Parserfehler
        assert!(matches!(api.set_parameters("Strip[0].Gain=abc"), Err(RemoteError::InvalidValue(_))));
        assert!(matches!(api.set_parameters("Foo[0].Gain=1"), Err(RemoteError::UnknownTarget)));
        assert!(matches!(api.set_parameters("Strip[0]Gain=1"), Err(RemoteError::Syntax(_))));
        assert!(matches!(api.set_parameters("Strip[0].Gain:1"), Err(RemoteError::Syntax(_))));
        // Fehlversuche dürfen das Dirty-Flag nicht setzen
        assert!(!api.is_parameters_dirty());
    }

    #[test]
    fn version_und_type_stimmen() {
        let (_p, api) = api();
        assert_eq!(api.get_voicemeeter_type(), VoicemeeterType::Potato);
        assert_eq!(api.get_voicemeeter_type().as_i32(), 3);
        let v = api.get_voicemeeter_version();
        assert_eq!(v, 0x0301_0300);
        assert_eq!(v >> 24, 3);
        assert_eq!((v >> 16) & 0xff, 1);
        assert_eq!((v >> 8) & 0xff, 3);
        assert_eq!(v & 0xff, 0);
        assert_eq!(make_version(3, 1, 3, 0), v);
    }

    #[test]
    fn running_midi_und_level() {
        let (p, api) = api();
        assert!(!api.is_running());
        assert!(api.run_voicemeeter(1));
        assert!(api.is_running());
        assert!(!api.run_voicemeeter(0));
        assert!(!api.is_running());
        assert_eq!(api.get_midi_message(), 0, "MIDI ist nicht implementiert");

        assert_eq!(api.get_level(0, 0).unwrap(), -140.0, "ohne Signal -140 dB");
        set_lin(&p.buses[0].level, 1.0);
        assert!(api.get_level(0, 1).unwrap().abs() < 1e-3);
        assert_eq!(api.get_level(BUSES, 0), Err(RemoteError::IndexOutOfRange));
        assert_eq!(api.get_level(0, 2), Err(RemoteError::IndexOutOfRange));
    }

    #[test]
    fn macro_buttons_haben_80_slots() {
        let (_p, api) = api();
        assert_eq!(MACRO_BUTTONS, 80);
        assert_eq!(api.macro_button_get_status(0).unwrap(), 0);
        assert!(!api.macro_button_is_dirty(0).unwrap());
        api.macro_button_set_status(79, 1).unwrap();
        assert_eq!(api.macro_button_get_status(79).unwrap(), 1);
        assert!(api.macro_button_is_dirty(79).unwrap());
        assert!(!api.macro_button_is_dirty(79).unwrap(), "Abfrage löscht das Flag");
        assert!(!api.macro_button_is_dirty(0).unwrap(), "nur der geänderte Slot ist schmutzig");
        assert_eq!(
            api.macro_button_set_status(MACRO_BUTTONS, 1),
            Err(RemoteError::IndexOutOfRange)
        );
        assert_eq!(
            api.macro_button_get_status(MACRO_BUTTONS + 5),
            Err(RemoteError::IndexOutOfRange)
        );
    }

    #[test]
    fn api_ist_threadsafe() {
        let (_p, api) = api();
        let api = Arc::new(api);
        let mut handles = Vec::new();
        for i in 0..4 {
            let a = api.clone();
            handles.push(std::thread::spawn(move || {
                a.set_parameter_float("strip", i, "gain", -(i as f32)).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(api.is_parameters_dirty());
        assert_eq!(api.get_parameter_float("strip", 3, "gain").unwrap(), -3.0);
    }
}
