//! # tape - Tape-Recorder fuer den Mixer
//!
//! Eigenstaendiges Crate (nur `std`, keine externen Abhaengigkeiten), das
//! interleaved f32-Audiobloecke als WAV auf Platte schreibt. Es kennt keine
//! Typen des Haupt-Crates `vm8`; die Anbindung passiert ausschliesslich ueber
//! die unten dokumentierte API mit primitiven Typen.
//!
//! ## Integrations-API (fuer vm8)
//!
//! ```text
//! start(path, sample_rate, channels, format) -> Result<(), TapeError>
//! write_block(&[f32])                        -> Result<(), TapeError>
//! stop()                                     -> Result<RecordingMeta, TapeError>
//! ```
//!
//! * `start` legt das Zielverzeichnis an, bildet den Dateinamen mit Zeitstempel
//!   (`<stamm>-YYYYMMDD-HHMMSS-001.wav`) und oeffnet das erste Segment.
//!   `path` darf ein Verzeichnis (Name wird zu `tape-...`) oder ein Dateistamm
//!   sein. Die Samplerate kommt vom Mixer, `format` ist [`Format::Pcm24`] oder
//!   [`Format::Float32`].
//! * `write_block` nimmt einen Block interleaved f32 (`frames * channels`
//!   Samples, 0.0 = Stille, 1.0 = Vollaussteuerung). Die Laenge muss kein
//!   Vielfaches der Kanalzahl sein; ein angefangenes Frame wird gehalten.
//!   Im Hotpath wird **nicht allokiert**: kodiert wird in einen bei `start`
//!   vorallokierten Byte-Puffer, geschrieben ueber einen [`std::io::BufWriter`].
//! * `stop` patcht den RIFF-Kopf (Groessen, Pad-Byte) und liefert die
//!   [`RecordingMeta`] mit Samplerate, Kanaelen, Startzeit, Frames und Peak.
//!
//! Zustandsmaschine: [`State::Idle`] -> `Recording` -> [`State::Stopped`].
//! `write_block` ausserhalb von `Recording` und `stop` ausserhalb von
//! `Recording` liefern [`TapeError::WrongState`]; aus `Stopped` kann mit
//! `start` eine neue Aufnahme beginnen.
//!
//! Rotation: erreicht der data-Chunk [`Config::max_file_bytes`], wird die Datei
//! finalisiert und das naechste Segment (`...-002.wav`) geoeffnet. Es gehen
//! keine Frames verloren, weil die Blockgroesse auf den freien Rest gekuerzt
//! wird.
//!
//! ## Beispiel
//!
//! ```no_run
//! use tape::{Format, Tape};
//!
//! # fn main() -> Result<(), tape::TapeError> {
//! let mut tape = Tape::new();
//! tape.start("aufnahmen/take", 48_000, 2, Format::Pcm24)?;
//! tape.write_block(&[0.0f32, 0.0, 0.5, -0.5])?; // 2 Frames stereo
//! let meta = tape.stop()?;
//! println!("{} Frames, Peak {}, Dateien: {}", meta.frames, meta.peak, meta.dateien.len());
//! # Ok(())
//! # }
//! ```
//!
//! ## Grenzen
//!
//! WAV (RIFF) adressiert Groessen mit u32, also rund 4 GiB pro Datei. Mit der
//! Standardrotation von 512 MiB bleibt das unkritisch; `Config` verhindert
//! groessere Werte. In dieser Version: zwei Formate (24-bit PCM, IEEE float32),
//! ein Kanal-Layout (interleaved), kein Multiplexing mehrerer Senken.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

mod meta;
mod recorder;
mod timefmt;
mod wav;

pub use meta::{FileMeta, RecordingMeta};
pub use recorder::{Tape, MAX_CHANNELS};
pub use wav::HEADER_LEN;

use std::fmt;
use std::io;

/// Zustand der Bandmaschine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Noch keine Aufnahme gestartet (oder Aufnahme verworfen).
    Idle,
    /// Aufnahme laeuft, `write_block` erlaubt.
    Recording,
    /// Sauber finalisiert, Datei(en) geschlossen.
    Stopped,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Idle => "Idle",
            State::Recording => "Recording",
            State::Stopped => "Stopped",
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Sample-Format der Zieldatei.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// 24-bit PCM, Format-Tag 1, Wertebereich -8388607..=8388607.
    Pcm24,
    /// 32-bit IEEE float, Format-Tag 3, Werte unveraendert uebernommen.
    Float32,
}

impl Format {
    pub const fn bits_per_sample(self) -> u16 {
        match self {
            Format::Pcm24 => 24,
            Format::Float32 => 32,
        }
    }

    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Format::Pcm24 => 3,
            Format::Float32 => 4,
        }
    }

    /// WAVE-Format-Tag: 1 = PCM, 3 = IEEE float.
    pub const fn format_tag(self) -> u16 {
        match self {
            Format::Pcm24 => 1,
            Format::Float32 => 3,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Format::Pcm24 => "pcm24",
            Format::Float32 => "float32",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Konfiguration des Recorders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Maximaler data-Chunk pro Datei, danach Rotation. Standard: 512 MiB.
    pub max_file_bytes: u64,
    /// Frames im vorallokierten Schreibpuffer. Standard: 8192.
    pub buffer_frames: usize,
    /// Kapazitaet des `BufWriter` in Byte. Standard: 256 KiB.
    pub writer_bytes: usize,
    /// Metadaten zusaetzlich als `<name>.json` neben die erste Datei schreiben.
    pub metadata_sidecar: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            max_file_bytes: 512 * 1024 * 1024,
            buffer_frames: 8192,
            writer_bytes: 256 * 1024,
            metadata_sidecar: false,
        }
    }
}

/// Fehler des Recorders.
#[derive(Debug)]
pub enum TapeError {
    /// Dateisystem-Fehler (oeffnen, schreiben, seek, sync).
    Io(io::Error),
    /// Aufruf im falschen Zustand der Zustandsmaschine.
    WrongState { erwartet: State, aktuell: State },
    /// Unzulaessige Parameter (Samplerate, Kanaele, Puffergroessen).
    UngueltigeKonfiguration(String),
    /// Kein verwertbarer Zielpfad uebergeben.
    KeinZielpfad,
}

impl fmt::Display for TapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TapeError::Io(e) => write!(f, "E/A-Fehler: {e}"),
            TapeError::WrongState { erwartet, aktuell } => {
                write!(f, "falscher Zustand: {aktuell}, erwartet {erwartet}")
            }
            TapeError::UngueltigeKonfiguration(s) => write!(f, "ungueltige Konfiguration: {s}"),
            TapeError::KeinZielpfad => f.write_str("kein Zielpfad angegeben"),
        }
    }
}

impl std::error::Error for TapeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TapeError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for TapeError {
    fn from(e: io::Error) -> Self {
        TapeError::Io(e)
    }
}
