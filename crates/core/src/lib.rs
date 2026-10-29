//! crosspoint — Signalkette eines 8×8-Mixers in Rust.
//!
//! Dieses Modul ist das **Parametermodell**: es bildet 1:1 die Attributnamen ab,
//! die das Referenzprogramm (3.1.3.0) in seiner Konfigurationsdatei schreibt (siehe
//! `docs/02-signalmodell.md`, belegt aus `<>`-Formatstrings des Originals).
//!
//! Aufbau, wie im Original belegt:
//!   Streifen 1..8  ->  Gate -> Denoiser -> 6-Band-EQ -> Compressor -> Pitch
//!                  ->  Sends (R, D, FX1, FX2) + Routing-Matrix auf 8 Busse
//!   Busse A1..A5 (Hardware) / B1..B3 (virtuell)
//!                  ->  EQ -> Trim/Delay pro Kanal -> Limiter -> Ausgang
//!
//! Kein Code, keine Ressourcen und keine Strings aus dem Original.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod autostart;
pub mod devices;
pub mod dsp;
pub mod engine;
/// Effekt-Host (mod-host/Carla als betreute Kindprozesse).
pub mod fx;
pub mod http;
pub mod inputs;
pub mod jack;
pub mod json;
/// Start des ganzen Prozesses (Engine, Adapter, Aufpasser) an einer Stelle.
pub mod lauf;
pub mod macros_io;
/// OSC-Eingang (`/strip/<n>/<param>`) für MIDIMonster und Fader.
pub mod osc;
/// Pegelbank: der Audio-Thread schreibt, die Oberfläche liest.
pub mod pegel;
pub mod pfade;
/// PipeWire-Graph (`pw-dump`) als Grundlage der Geräteerkennung.
pub mod pw;
pub mod remote_api;
/// Zustand beim Start wiederherstellen, Änderungen selbsttätig sichern.
pub mod restore;
pub mod tape_io;
pub mod vban_io;

pub const STRIPS: usize = 8;
pub const BUSES: usize = 8;
pub const EQ_CELLS: usize = 6;
pub const LAYERS: usize = 8;

/// Streifennamen wie im Original: 5 Hardware-Eingänge, 3 virtuelle.
pub const STRIP_NAMES: [&str; STRIPS] = [
    "HW In 1",
    "HW In 2",
    "HW In 3",
    "HW In 4",
    "HW In 5",
    "Virt In 1",
    "Virt In 2",
    "Virt In 3",
];

/// Busse: A1..A5 sind Hardware-Ausgänge, B1..B3 virtuelle (VAIO).
pub const BUS_NAMES: [&str; BUSES] = ["A1", "A2", "A3", "A4", "A5", "B1", "B2", "B3"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BusKind {
    /// Ausgang auf ein reales Gerät (im Original A1..A5).
    Hardware,
    /// Virtueller Ausgang (VAIO, im Original B1..B3).
    Virtual,
}

pub fn bus_kind(index: usize) -> BusKind {
    if index < 5 {
        BusKind::Hardware
    } else {
        BusKind::Virtual
    }
}

/// EQ-Filtertypen. Die Namen folgen den im Original gespeicherten `EQtype`-Werten
/// (Zählung dort 0..n); die Zuordnung Typ->Koeffizienten ist **noch nicht** aus dem
/// Binary belegt und hier als offene Stelle markiert.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FilterType {
    #[default]
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    BandPass,
    Notch,
}

/// Eine EQ-Zelle: Frequenz, Güte und Verstärkung in dB.
#[derive(Clone, Copy, Debug)]
pub struct EqCell {
    pub enabled: bool,
    pub kind: FilterType,
    pub freq_hz: f32,
    pub q: f32,
    pub gain_db: f32,
}

impl Default for EqCell {
    fn default() -> Self {
        EqCell {
            enabled: false,
            kind: FilterType::Peak,
            freq_hz: 1000.0,
            q: 0.7,
            gain_db: 0.0,
        }
    }
}

/// Kompressor nach den Originalfeldern `gainin, attack, release, knee,
/// comprate, threshold, automakeup, gainout`.
#[derive(Clone, Copy, Debug)]
pub struct Compressor {
    pub enabled: bool,
    pub gain_in_db: f32,
    pub threshold_db: f32,
    pub ratio: f32, // "comprate"
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub auto_makeup: bool,
    pub gain_out_db: f32,
}

impl Default for Compressor {
    fn default() -> Self {
        Compressor {
            enabled: false,
            gain_in_db: 0.0,
            threshold_db: -18.0,
            ratio: 1.0,
            attack_ms: 20.0,
            release_ms: 200.0,
            knee_db: 0.0,
            auto_makeup: false,
            gain_out_db: 0.0,
        }
    }
}

/// Gate nach `thresin, damping, bpsidechain, attack, hold, release`.
#[derive(Clone, Copy, Debug)]
pub struct Gate {
    pub enabled: bool,
    pub threshold_db: f32,
    pub damping: f32,
    pub sidechain_bp_hz: f32,
    pub attack_ms: f32,
    pub hold_ms: f32,
    pub release_ms: f32,
}

impl Default for Gate {
    fn default() -> Self {
        Gate {
            enabled: false,
            threshold_db: -40.0,
            damping: 0.0,
            sidechain_bp_hz: 0.0,
            attack_ms: 5.0,
            hold_ms: 100.0,
            release_ms: 200.0,
        }
    }
}

/// Denoiser (im Original "Audibility"-Modul).
#[derive(Clone, Copy, Debug, Default)]
pub struct Denoiser {
    pub bypass: bool,
    pub learn: bool,
    pub gain: f32,
    pub audibility_c: f32,
    pub audibility_g: f32,
}

/// Pitch/Formant (`pitchon, drywet, pitchvalue, formantlo/med/high`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Pitch {
    pub enabled: bool,
    pub dry_wet: f32,
    pub semitones: f32,
    pub formant_low: f32,
    pub formant_mid: f32,
    pub formant_high: f32,
}

/// Bus-Sendepegel + Post-Fader-Schalter (`sendR/postR`, `sendD/postD`, `sendFx1/2`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Send {
    pub level: f32,
    pub post: bool,
}

/// Mischmodi eines Busses (`BusMode`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BusMode {
    #[default]
    Normal,
    DownmixA,
    DownmixB,
    StereoRepeat,
    Composite,
    TvMix,
    Upmix21,
    Upmix41,
    Upmix61,
}

/// Kanal-Routing eines Busses (L/R/C/LFE/Rear).
#[derive(Clone, Copy, Debug)]
pub struct SpeakerRoute {
    pub left: bool,
    pub right: bool,
    pub center: bool,
    pub lfe: bool,
    pub rear: bool,
}

impl Default for SpeakerRoute {
    fn default() -> Self {
        SpeakerRoute {
            left: true,
            right: true,
            center: false,
            lfe: false,
            rear: false,
        }
    }
}

/// Ein Streifen (Kanalzug).
pub struct Strip {
    pub name: &'static str,
    /// `dblevel` — Fader in dB.
    pub gain_db: f32,
    /// `dblimit` — Anschlag des Faders in dB.
    pub limit_db: f32,
    pub mute: bool,
    pub solo: bool,
    pub mono: bool,
    /// `muc` — Mute-Center (nur Center-Anteil stumm).
    pub mute_center: bool,
    pub karaoke: bool,
    /// `greenmode` — reduzierter Rechenpfad.
    pub green_mode: bool,
    /// Routing-Matrix: `busa, busa2..busa5, busb, busb2, busb3`.
    pub routing: [bool; BUSES],
    /// Pro Kanal 6 EQ-Zellen (`Strip index=… cell=…`).
    pub eq: [[EqCell; EQ_CELLS]; 2],
    pub eq_enabled: bool,
    pub comp: Compressor,
    pub gate: Gate,
    pub denoiser: Denoiser,
    pub pitch: Pitch,
    /// Sends: 0 = Reverb (`R`), 1 = Delay (`D`), 2 = FX1, 3 = FX2.
    pub sends: [Send; 4],
    /// `layer1..layer8` — acht Pegel-Ebenen.
    pub layers: [f32; LAYERS],
    /// `stripmode` — 0 normal, 1 Post-Fader-Meter usw. (Bedeutung offen).
    pub strip_mode: u32,
}

impl Strip {
    fn new(name: &'static str) -> Strip {
        let mut routing = [false; BUSES];
        routing[0] = true; // Vorgabe im Original: Streifen geht auf A1
        Strip {
            name,
            gain_db: 0.0,
            limit_db: 12.0,
            mute: false,
            solo: false,
            mono: false,
            mute_center: false,
            karaoke: false,
            green_mode: false,
            routing,
            eq: [[EqCell::default(); EQ_CELLS]; 2],
            eq_enabled: false,
            comp: Compressor::default(),
            gate: Gate::default(),
            denoiser: Denoiser::default(),
            pitch: Pitch::default(),
            sends: [Send::default(); 4],
            layers: [0.0; LAYERS],
            strip_mode: 0,
        }
    }
}

/// Ein Bus (Summenkanal).
pub struct Bus {
    pub name: &'static str,
    pub kind: BusKind,
    pub gain_db: f32,
    pub limit_db: f32,
    pub mute: bool,
    pub mono: bool,
    /// `cross` — Kreuzmischung der Kanäle.
    pub cross: bool,
    pub bus_mode: BusMode,
    pub eq_enabled: bool,
    pub eq: [[EqCell; EQ_CELLS]; 2],
    /// `trim` und `delay` je Kanal (Verzögerung in ms).
    pub trim_db: [f32; 2],
    pub delay_ms: [f32; 2],
    pub speaker: SpeakerRoute,
    /// `SEL`, `monitor`, `greenmode`.
    pub sel: bool,
    pub monitor: bool,
    pub green_mode: bool,
    /// Returns: Reverb, Delay, FX1, FX2.
    pub returns: [f32; 4],
    pub limiter_enabled: bool,
}

impl Bus {
    fn new(index: usize) -> Bus {
        Bus {
            name: BUS_NAMES[index],
            kind: bus_kind(index),
            gain_db: 0.0,
            limit_db: 12.0,
            mute: false,
            mono: false,
            cross: false,
            bus_mode: BusMode::Normal,
            eq_enabled: false,
            eq: [[EqCell::default(); EQ_CELLS]; 2],
            trim_db: [0.0; 2],
            delay_ms: [0.0; 2],
            speaker: SpeakerRoute::default(),
            sel: false,
            monitor: false,
            green_mode: false,
            returns: [0.0; 4],
            limiter_enabled: false,
        }
    }
}

/// Laufzeit-Einstellungen der Engine. Grenzen aus dem Original belegt:
/// 32 kHz … 192 kHz, Blockgröße ≤ 2048 Frames, 2–8 Kanäle.
#[derive(Clone, Copy, Debug)]
pub struct EngineConfig {
    pub sample_rate: u32,
    pub block_frames: u32,
    pub channels: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            sample_rate: 48_000,
            block_frames: 512,
            channels: 8,
        }
    }
}

/// Belegte Sampleraten des Originals (Vergleichskette in `FUN_1400f4080`).
pub const SUPPORTED_SAMPLE_RATES: [u32; 7] =
    [32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

pub const MAX_BLOCK_FRAMES: u32 = 2048;

impl EngineConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !SUPPORTED_SAMPLE_RATES.contains(&self.sample_rate) {
            return Err(format!("Samplerate {} nicht unterstützt", self.sample_rate));
        }
        if self.block_frames == 0 || self.block_frames > MAX_BLOCK_FRAMES {
            return Err(format!(
                "Blockgröße {} unzulässig (1..={MAX_BLOCK_FRAMES})",
                self.block_frames
            ));
        }
        if !(2..=8).contains(&self.channels) {
            return Err(format!("Kanäle {} unzulässig (2..=8)", self.channels));
        }
        Ok(())
    }
}

/// Das gesamte Pult.
pub struct Mixer {
    pub strips: [Strip; STRIPS],
    pub buses: [Bus; BUSES],
    pub config: EngineConfig,
}

impl Mixer {
    pub fn new(config: EngineConfig) -> Mixer {
        Mixer {
            strips: std::array::from_fn(|i| Strip::new(STRIP_NAMES[i])),
            buses: std::array::from_fn(Bus::new),
            config,
        }
    }

    /// dB → linearer Faktor.
    pub fn db_to_lin(db: f32) -> f32 {
        10f32.powf(db / 20.0)
    }

    /// Wird ein Streifen hörbar? Solo-Logik wie im Original: sobald ein Streifen
    /// Solo hat, sind alle anderen stumm.
    pub fn strip_audible(&self, index: usize) -> bool {
        let s = &self.strips[index];
        if s.mute {
            return false;
        }
        let any_solo = self.strips.iter().any(|x| x.solo);
        !any_solo || s.solo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acht_und_acht() {
        let m = Mixer::new(EngineConfig::default());
        assert_eq!(m.strips.len(), 8);
        assert_eq!(m.buses.len(), 8);
        assert_eq!(m.strips[7].name, "Virt In 3");
        assert_eq!(m.buses[5].kind, BusKind::Virtual);
        assert_eq!(m.buses[0].kind, BusKind::Hardware);
    }

    #[test]
    fn standard_routing_ist_a1() {
        let m = Mixer::new(EngineConfig::default());
        assert!(m.strips[0].routing[0]);
        assert!(!m.strips[0].routing[1]);
    }

    #[test]
    fn grenzwerte_wie_im_original() {
        let mut c = EngineConfig::default();
        c.sample_rate = 44_100;
        assert!(c.validate().is_ok());
        c.sample_rate = 50_000;
        assert!(c.validate().is_err());
        let mut c = EngineConfig::default();
        c.block_frames = 2049;
        assert!(c.validate().is_err());
        c.block_frames = 2048;
        assert!(c.validate().is_ok());
        c.channels = 9;
        assert!(c.validate().is_err());
    }

    #[test]
    fn solo_verdraengt_andere() {
        let mut m = Mixer::new(EngineConfig::default());
        assert!(m.strip_audible(3));
        m.strips[3].solo = true;
        assert!(m.strip_audible(3));
        assert!(!m.strip_audible(2));
        m.strips[3].solo = false;
        m.strips[2].mute = true;
        assert!(!m.strip_audible(2));
    }

    #[test]
    fn db_umrechnung() {
        assert!((Mixer::db_to_lin(0.0) - 1.0).abs() < 1e-6);
        assert!((Mixer::db_to_lin(-6.0206) - 0.5).abs() < 1e-4);
        assert!((Mixer::db_to_lin(12.0) - 3.981).abs() < 1e-3);
    }
}
