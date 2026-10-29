//! Der eigentliche Recorder: Zustandsmaschine, Rotation, Finalisierung.
//!
//! Entwurf:
//! * Geschrieben wird über [`hound::WavWriter`] in einen `BufWriter`. `hound`
//!   legt den RIFF-Kopf an und patcht beim `finalize` die Größen – der
//!   Eigenbau-Kopf (44 Byte, `seek(0)` + `write_all`) ist damit entfallen.
//! * Bytes pro Datei werden mitgezaehlt; sobald `Config::max_file_bytes` erreicht
//!   ist, wird die Datei finalisiert und die naechste geoeffnet.
//! * Im Hotpath (`write_block`) wird weiterhin nichts allokiert: Samples gehen
//!   direkt aus dem Eingabepuffer in den gepufferten Schreiber.

use std::fs::{self, File, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::meta::{self, FileMeta, RecordingMeta};
use crate::timefmt;
use crate::wav;
use crate::{Config, Format, State, TapeError};

/// Obergrenze fuer Kanaele; begrenzt den Carry-Puffer (kein Heap noetig).
pub const MAX_CHANNELS: usize = 32;

/// Ein Segment der Aufnahme: eine WAV-Datei in Arbeit.
struct Active {
    path: PathBuf,
    writer: hound::WavWriter<BufWriter<File>>,
    /// Fertige Frames (ohne angefangenes Restframe in `Tape::carry`).
    frames: u64,
    /// Nutzlast im data-Chunk.
    data_bytes: u64,
    peak: f32,
}

/// Bandmaschine: schreibt interleaved f32-Bloecke als WAV.
pub struct Tape {
    cfg: Config,
    state: State,
    sample_rate: u32,
    channels: u16,
    format: Format,
    /// Angefangenes Frame, wenn ein Block nicht frame-aligned endet.
    carry: [f32; MAX_CHANNELS],
    carry_len: usize,
    active: Option<Active>,
    base_dir: PathBuf,
    stem: String,
    /// Zeitstempel des Aufnahmestarts ("YYYYMMDD-HHMMSS"), Teil jedes Dateinamens.
    stamp: String,
    start_epoch_s: u64,
    next_index: u32,
    peak: f32,
    dateien: Vec<FileMeta>,
    meta: Option<RecordingMeta>,
}

impl Default for Tape {
    fn default() -> Self {
        Self::new()
    }
}

/// Kurzfassung statt des (grossen) Schreibpuffers.
impl std::fmt::Debug for Tape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tape")
            .field("state", &self.state)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("format", &self.format)
            .field("frames", &self.frames())
            .field("peak", &self.peak)
            .field("dateien", &self.dateien.len())
            .field("current", &self.current_path())
            .finish()
    }
}

impl Tape {
    /// Neuer Recorder mit Standardkonfiguration (512 MiB pro Datei).
    pub fn new() -> Self {
        Self::with_config(Config::default())
    }

    /// Neuer Recorder mit eigener Konfiguration.
    pub fn with_config(cfg: Config) -> Self {
        Tape {
            cfg,
            state: State::Idle,
            sample_rate: 0,
            channels: 0,
            format: Format::Pcm24,
            carry: [0.0; MAX_CHANNELS],
            carry_len: 0,
            active: None,
            base_dir: PathBuf::new(),
            stem: String::new(),
            stamp: String::new(),
            start_epoch_s: 0,
            next_index: 1,
            peak: 0.0,
            dateien: Vec::new(),
            meta: None,
        }
    }

    /// Aktueller Zustand der Zustandsmaschine.
    pub fn state(&self) -> State {
        self.state
    }

    /// Konfiguration (lesend).
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Bisher geschriebene Frames (ueber alle Dateien).
    pub fn frames(&self) -> u64 {
        self.dateien.iter().map(|f| f.frames).sum::<u64>()
            + self.active.as_ref().map(|a| a.frames).unwrap_or(0)
    }

    /// Bisher beobachteter Betrags-Peak (1.0 = Vollaussteuerung).
    pub fn peak(&self) -> f32 {
        self.peak
    }

    /// Pfad der Datei, in die gerade geschrieben wird.
    pub fn current_path(&self) -> Option<&Path> {
        self.active.as_ref().map(|a| a.path.as_path())
    }

    /// Metadaten der letzten abgeschlossenen Aufnahme.
    pub fn meta(&self) -> Option<&RecordingMeta> {
        self.meta.as_ref()
    }

    /// Startet die Aufnahme. `path` ist entweder ein Verzeichnis (Dateiname wird
    /// gebildet) oder ein Dateistamm/Pfad mit Endung. Der tatsaechliche Name
    /// bekommt Zeitstempel und Segmentnummer: `<stamm>-<YYYYMMDD-HHMMSS>-001.wav`.
    ///
    /// Zustand: `Idle`/`Stopped` -> `Recording`. Waehrend einer laufenden Aufnahme
    /// ist ein erneuter `start` ein Fehler (erst `stop`).
    pub fn start(
        &mut self,
        path: impl AsRef<Path>,
        sample_rate: u32,
        channels: u16,
        format: Format,
    ) -> Result<(), TapeError> {
        if self.state == State::Recording {
            return Err(TapeError::WrongState {
                erwartet: State::Idle,
                aktuell: self.state,
            });
        }
        if sample_rate == 0 || sample_rate > 768_000 {
            return Err(TapeError::UngueltigeKonfiguration(format!(
                "Samplerate {sample_rate} unplausibel (1..=768000)"
            )));
        }
        if channels == 0 || channels as usize > MAX_CHANNELS {
            return Err(TapeError::UngueltigeKonfiguration(format!(
                "Kanaele {channels} unzulaessig (1..={MAX_CHANNELS})"
            )));
        }
        let bytes_per_frame = channels as usize * format.bytes_per_sample();
        if bytes_per_frame == 0 {
            return Err(TapeError::UngueltigeKonfiguration(
                "Bytes pro Frame sind 0".to_string(),
            ));
        }
        if self.cfg.max_file_bytes < 4096 || !wav::data_len_fits(self.cfg.max_file_bytes) {
            return Err(TapeError::UngueltigeKonfiguration(format!(
                "max_file_bytes {} unzulaessig (4096..={})",
                self.cfg.max_file_bytes,
                u32::MAX as u64 - 36
            )));
        }

        let (dir, stem) = resolve_target(path.as_ref())?;
        let (epoch, stamp) = timefmt::now_epoch_and_stamp();

        // Aufnahmezustand zuruecksetzen (Neustart aus `Stopped` ist erlaubt).
        self.sample_rate = sample_rate;
        self.channels = channels;
        self.format = format;
        self.carry = [0.0; MAX_CHANNELS];
        self.carry_len = 0;
        self.active = None;
        self.base_dir = dir;
        self.stem = stem;
        self.stamp = stamp;
        self.start_epoch_s = epoch;
        self.next_index = 1;
        self.peak = 0.0;
        self.dateien = Vec::new();
        self.meta = None;

        self.open_next()?;
        self.state = State::Recording;
        Ok(())
    }

    /// Schreibt einen interleaved f32-Block. Die Laenge muss kein Vielfaches der
    /// Kanalzahl sein: ein angefangenes Frame wird bis zum naechsten Aufruf
    /// gehalten.
    ///
    /// Im Hotpath wird nicht allokiert; rotiert wird hoechstens einmal pro
    /// `max_file_bytes`.
    pub fn write_block(&mut self, block: &[f32]) -> Result<(), TapeError> {
        if self.state != State::Recording {
            return Err(TapeError::WrongState {
                erwartet: State::Recording,
                aktuell: self.state,
            });
        }
        if block.is_empty() {
            return Ok(());
        }

        let ch = self.channels as usize;
        let mut pos = 0usize;

        // 1) Restframe aus dem vorigen Aufruf vervollstaendigen.
        if self.carry_len > 0 {
            let need = ch - self.carry_len;
            let take = need.min(block.len());
            self.carry[self.carry_len..self.carry_len + take].copy_from_slice(&block[..take]);
            self.carry_len += take;
            pos = take;
            if self.carry_len == ch {
                // Kopie, damit `write_frames(&mut self)` den Carry nicht mehr leiht.
                let mut frame = [0.0f32; MAX_CHANNELS];
                frame[..ch].copy_from_slice(&self.carry[..ch]);
                self.carry_len = 0;
                self.write_frames(&frame[..ch])?;
            }
        }

        // 2) Ganze Frames direkt aus dem Eingabepuffer schreiben.
        let rest = block.len() - pos;
        let whole = rest - rest % ch;
        if whole > 0 {
            self.write_frames(&block[pos..pos + whole])?;
            pos += whole;
        }

        // 3) Unvollstaendiges Frame fuer den naechsten Aufruf merken.
        let rem = block.len() - pos;
        if rem > 0 {
            self.carry[..rem].copy_from_slice(&block[pos..]);
            self.carry_len = rem;
        }
        Ok(())
    }

    /// Beendet die Aufnahme, patcht den Kopf der letzten Datei und liefert die
    /// Metadaten. Zustand: `Recording` -> `Stopped`.
    pub fn stop(&mut self) -> Result<RecordingMeta, TapeError> {
        if self.state != State::Recording {
            return Err(TapeError::WrongState {
                erwartet: State::Recording,
                aktuell: self.state,
            });
        }
        self.finalize_active()?;
        self.carry_len = 0;
        self.state = State::Stopped;

        let m = meta::build(
            self.sample_rate,
            self.channels,
            self.format,
            self.start_epoch_s,
            self.dateien.clone(),
        );
        if self.cfg.metadata_sidecar {
            if let Some(first) = m.dateien.first() {
                let p = RecordingMeta::sidecar_path(&first.pfad);
                m.write_json(&p)?;
            }
        }
        self.meta = Some(m.clone());
        Ok(m)
    }

    // ---- intern -----------------------------------------------------------

    /// Schreibt `frames` (Laenge ist Vielfaches von `channels`), rotiert bei Bedarf.
    fn write_frames(&mut self, frames: &[f32]) -> Result<(), TapeError> {
        let ch = self.channels as usize;
        let bytes_per_frame = ch * self.format.bytes_per_sample();
        let total = frames.len() / ch;
        let mut done = 0usize;

        while done < total {
            if self.active.is_none() {
                self.open_next()?;
            }
            let data_bytes = self
                .active
                .as_ref()
                .map(|a| a.data_bytes)
                .ok_or(TapeError::KeinZielpfad)?;
            let frei = self.cfg.max_file_bytes.saturating_sub(data_bytes);
            if frei < bytes_per_frame as u64 {
                // Datei ist voll: finalisieren und naechstes Segment oeffnen.
                self.rotate()?;
                continue;
            }

            let fit_frames = (frei / bytes_per_frame as u64) as usize;
            let n = fit_frames.min(total - done).max(1);

            let a = self.active.as_mut().ok_or(TapeError::KeinZielpfad)?;
            let src = &frames[done * ch..(done + n) * ch];
            // Peak vor der Quantisierung messen: Werte ueber 1.0 zeigen
            // Uebersteuerung an.
            let mut peak = a.peak;
            for &s in src {
                if s.is_finite() {
                    let betrag = s.abs();
                    if betrag > peak {
                        peak = betrag;
                    }
                }
            }
            match self.format {
                Format::Pcm24 => {
                    for &s in src {
                        a.writer.write_sample(wav::pcm24(s))?;
                    }
                }
                Format::Float32 => {
                    for &s in src {
                        a.writer.write_sample(wav::float32(s))?;
                    }
                }
            }
            a.data_bytes += (n * bytes_per_frame) as u64;
            a.frames += n as u64;
            a.peak = peak;
            if peak > self.peak {
                self.peak = peak;
            }
            done += n;
        }
        Ok(())
    }

    /// Finalisiert die aktuelle Datei: `hound` schreibt die echten Groessen.
    fn finalize_active(&mut self) -> Result<(), TapeError> {
        let Some(a) = self.active.take() else {
            return Ok(());
        };
        a.writer.finalize()?;
        self.dateien.push(FileMeta {
            pfad: a.path,
            frames: a.frames,
            data_bytes: a.data_bytes,
            peak: a.peak,
        });
        Ok(())
    }

    /// Aktuelle Datei finalisieren, Segmentnummer erhoehen, neue Datei oeffnen.
    fn rotate(&mut self) -> Result<(), TapeError> {
        self.finalize_active()?;
        self.next_index += 1;
        self.open_next()
    }

    /// Oeffnet die naechste Segmentdatei; `hound` legt den Kopf an.
    fn open_next(&mut self) -> Result<(), TapeError> {
        let name = format!("{}-{}-{:03}.wav", self.stem, self.stamp, self.next_index);
        let path = self.base_dir.join(name);
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        let buf = BufWriter::with_capacity(self.cfg.writer_bytes, file);
        let writer =
            hound::WavWriter::new(buf, wav::spec(self.format, self.sample_rate, self.channels))?;
        self.active = Some(Active {
            path,
            writer,
            frames: 0,
            data_bytes: 0,
            peak: 0.0,
        });
        Ok(())
    }
}

/// Bestimmt (Verzeichnis, Dateistamm) aus dem uebergebenen Zielpfad.
///
/// * existierendes Verzeichnis -> `<dir>/tape`
/// * Pfad mit Dateinamen     -> `<parent>/<file_stem>`
///
/// Das Verzeichnis wird angelegt, falls es fehlt.
fn resolve_target(path: &Path) -> Result<(PathBuf, String), TapeError> {
    if path.as_os_str().is_empty() {
        return Err(TapeError::KeinZielpfad);
    }
    if path.is_dir() {
        fs::create_dir_all(path)?;
        return Ok((path.to_path_buf(), "tape".to_string()));
    }
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "tape".to_string());
    fs::create_dir_all(&dir)?;
    Ok((dir, stem))
}

impl Drop for Tape {
    /// Bricht die Aufnahme ab, ohne den Kopf zu patchen, waere die Datei
    /// unbrauchbar. Deshalb wird hier best effort finalisiert.
    fn drop(&mut self) {
        if self.state == State::Recording {
            let _ = self.finalize_active();
            self.state = State::Stopped;
        }
    }
}
