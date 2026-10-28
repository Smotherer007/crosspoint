//! Metadaten einer Aufnahme.
//!
//! Beim `stop()` liefert der Recorder diese Struktur. Sie beschreibt die
//! gesamte Aufnahme (Summe ueber alle rotierten Dateien) und jede Einzeldatei.
//! Optional wird sie zusaetzlich als JSON neben die Aufnahme geschrieben
//! (`Config::metadata_sidecar`), damit spaetere Werkzeuge sie ohne Parcer
//! fuer dieses Crate lesen koennen.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::timefmt;
use crate::Format;

/// Daten einer einzelnen WAV-Datei (ein Segment der Aufnahme).
#[derive(Debug, Clone, PartialEq)]
pub struct FileMeta {
    pub pfad: PathBuf,
    /// Anzahl geschriebener Frames (ein Frame = ein Sample je Kanal).
    pub frames: u64,
    /// Nutzlast des data-Chunks in Byte.
    pub data_bytes: u64,
    /// Betragsmaximum der in dieser Datei geschriebenen Samples.
    pub peak: f32,
}

impl FileMeta {
    /// Dauer in Sekunden bei gegebener Samplerate.
    pub fn dauer_s(&self, sample_rate: u32) -> f64 {
        if sample_rate == 0 {
            0.0
        } else {
            self.frames as f64 / sample_rate as f64
        }
    }
}

/// Gesamtmetadaten einer Aufnahme (Idle -> Recording -> Stopped).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingMeta {
    pub sample_rate: u32,
    pub channels: u16,
    pub format: Format,
    /// Startzeit als UNIX-Sekunden (UTC).
    pub start_epoch_s: u64,
    /// Startzeit als ISO-8601-String (UTC), z. B. "2026-10-28T14:03:12Z".
    pub start_time_iso: String,
    /// Frames ueber alle Dateien.
    pub frames: u64,
    /// Betragsmaximum ueber alle Dateien (1.0 = Vollaussteuerung).
    pub peak: f32,
    pub dateien: Vec<FileMeta>,
}

impl RecordingMeta {
    /// Gesamtdauer in Sekunden.
    pub fn dauer_s(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames as f64 / self.sample_rate as f64
        }
    }

    /// JSON von Hand geschrieben (kein serde, keine Abhaengigkeit).
    pub fn to_json(&self) -> String {
        let mut s = String::with_capacity(256 + 128 * self.dateien.len());
        let _ = write!(
            s,
            "{{\"samplerate\":{},\"kanaele\":{},\"format\":\"{}\",\
             \"startzeit\":\"{}\",\"start_epoch_s\":{},\"frames\":{},\
             \"dauer_s\":{:.3},\"peak\":{:.6},\"dateien\":[",
            self.sample_rate,
            self.channels,
            self.format.name(),
            self.start_time_iso,
            self.start_epoch_s,
            self.frames,
            self.dauer_s(),
            self.peak
        );
        for (i, f) in self.dateien.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            let _ = write!(
                s,
                "{{\"pfad\":\"{}\",\"frames\":{},\"data_bytes\":{},\"peak\":{:.6}}}",
                escape(&f.pfad.to_string_lossy()),
                f.frames,
                f.data_bytes,
                f.peak
            );
        }
        s.push_str("]}");
        s
    }

    /// Schreibt die Metadaten als JSON nach `pfad`.
    pub fn write_json(&self, pfad: &Path) -> io::Result<()> {
        fs::write(pfad, self.to_json())
    }

    /// Neben-Datei zu einer Aufnahme: gleicher Stamm, Endung ".json".
    pub fn sidecar_path(wav_pfad: &Path) -> PathBuf {
        wav_pfad.with_extension("json")
    }
}

/// Minimales Escaping fuer JSON-Strings (Pfade enthalten selten Sonderzeichen).
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Baut die Metadaten aus den Einzeldateien.
pub(crate) fn build(
    sample_rate: u32,
    channels: u16,
    format: Format,
    start_epoch_s: u64,
    dateien: Vec<FileMeta>,
) -> RecordingMeta {
    let frames = dateien.iter().map(|f| f.frames).sum();
    let peak = dateien.iter().fold(0.0f32, |a, f| a.max(f.peak));
    RecordingMeta {
        sample_rate,
        channels,
        format,
        start_epoch_s,
        start_time_iso: timefmt::iso_from_epoch(start_epoch_s),
        frames,
        peak,
        dateien,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> RecordingMeta {
        build(
            48_000,
            2,
            Format::Pcm24,
            1_709_210_096,
            vec![
                FileMeta {
                    pfad: PathBuf::from("/tmp/a.wav"),
                    frames: 100,
                    data_bytes: 600,
                    peak: 0.5,
                },
                FileMeta {
                    pfad: PathBuf::from("/tmp/b.wav"),
                    frames: 50,
                    data_bytes: 300,
                    peak: 0.75,
                },
            ],
        )
    }

    #[test]
    fn summiert_frames_und_peak() {
        let m = meta();
        assert_eq!(m.frames, 150);
        assert_eq!(m.peak, 0.75);
        assert_eq!(m.start_time_iso, "2024-02-29T12:34:56Z");
        assert!((m.dauer_s() - 150.0 / 48_000.0).abs() < 1e-9);
        assert_eq!(m.dateien.len(), 2);
    }

    #[test]
    fn json_enthaelt_pflichtfelder() {
        let j = meta().to_json();
        for feld in [
            "\"samplerate\":48000",
            "\"kanaele\":2",
            "\"format\":\"pcm24\"",
            "\"startzeit\":\"2024-02-29T12:34:56Z\"",
            "\"frames\":150",
            "\"peak\":0.750000",
            "\"dateien\":[",
        ] {
            assert!(j.contains(feld), "fehlt {feld} in {j}");
        }
        assert_eq!(j.matches("pfad").count(), 2);
    }

    #[test]
    fn sidecar_hat_json_endung() {
        assert_eq!(
            RecordingMeta::sidecar_path(Path::new("/x/take-1.wav")),
            PathBuf::from("/x/take-1.json")
        );
    }
}
