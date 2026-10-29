//! Integrationstests fuer das Crate `tape`.
//!
//! Geprueft wird, was der Mixer spaeter wirklich braucht: gueltiger RIFF-Kopf
//! (Groessen, Format-Tag, Block-Align), exakte Frame-Zahl, Rotation,
//! Header-Gueltigkeit nach `stop` (zusaetzlich mit `file(1)`, falls vorhanden)
//! und dass im Schreib-Hotpath nichts allokiert wird.
//!
//! Die Dateien landen unter `CARGO_TARGET_TMPDIR`, nie im System-Temp.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tape::{Config, Format, State, Tape, TapeError};

// ---------------------------------------------------------------------------
// Allokations-Zaehler (thread-lokal, damit parallele Tests sich nicht stoeren)
// ---------------------------------------------------------------------------

thread_local! {
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAlloc;

fn zaehle() {
    let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        zaehle();
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        zaehle();
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        zaehle();
        System.realloc(p, l, n)
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

// ---------------------------------------------------------------------------
// Hilfen
// ---------------------------------------------------------------------------

fn tmp(name: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("tape-test-{name}"));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).expect("Testverzeichnis");
    p
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Geparster WAV-Kopf plus Nutzlast.
///
/// `fmt_tag` ist der *wirksame* Format-Tag: bei `WAVE_FORMAT_EXTENSIBLE`
/// (0xFFFE) steht das echte Format im SubFormat-GUID; hier wird es
/// aufgeloest, damit Tests weiter `1` (PCM) bzw. `3` (IEEE float) pruefen.
#[derive(Debug)]
struct Wav {
    riff_size: u32,
    fmt_size: u32,
    fmt_tag: u16,
    extensible: bool,
    channels: u16,
    sample_rate: u32,
    byte_rate: u32,
    block_align: u16,
    bits: u16,
    data_len: u32,
    data_off: usize,
    data: Vec<u8>,
    file_len: u64,
    pad: bool,
}

impl Wav {
    fn frames(&self) -> u64 {
        self.data.len() as u64 / self.block_align as u64
    }
    /// i32-Wert eines 24-bit-Samples an Frame-Position `frame`, Kanal `ch`.
    fn pcm24(&self, frame: usize, ch: usize) -> i32 {
        let o = frame * self.block_align as usize + ch * 3;
        let v = i32::from_le_bytes([self.data[o], self.data[o + 1], self.data[o + 2], 0]);
        // Vorzeichenbit 23 erweitern
        if v & 0x0080_0000 != 0 {
            v | !0x00FF_FFFF
        } else {
            v
        }
    }
    fn f32_at(&self, frame: usize, ch: usize) -> f32 {
        let o = frame * self.block_align as usize + ch * 4;
        f32::from_le_bytes([
            self.data[o],
            self.data[o + 1],
            self.data[o + 2],
            self.data[o + 3],
        ])
    }
}

/// Liest die Datei und prueft die strukturelle Konsistenz des Kopfes.
///
/// Der Kopf wird ueber die Chunk-Kette gelesen, nicht ueber feste Offsets:
/// `hound` schreibt fuer mehr als 16 bit oder mehr als zwei Kanaele
/// `WAVE_FORMAT_EXTENSIBLE` (40-Byte-`fmt`-Chunk), sonst das klassische
/// 16-Byte-Layout.
fn parse_wav(path: &Path) -> Wav {
    let raw = fs::read(path).unwrap_or_else(|e| panic!("{} nicht lesbar: {e}", path.display()));
    assert!(raw.len() >= 44, "Datei kuerzer als ein WAV-Kopf");
    assert_eq!(&raw[0..4], b"RIFF", "RIFF-Signatur fehlt");
    assert_eq!(&raw[8..12], b"WAVE", "WAVE-Signatur fehlt");
    assert_eq!(&raw[12..16], b"fmt ", "fmt-Chunk fehlt an Offset 12");

    let fmt_size = u32_at(&raw, 16);
    assert!(
        fmt_size == 16 || fmt_size == 40,
        "unerwartete fmt-Chunk-Groesse {fmt_size}"
    );
    let roh_tag = u16_at(&raw, 20);
    let extensible = roh_tag == 0xFFFE;
    // WAVEFORMATEXTENSIBLE: SubFormat-GUID ab Datei-Offset 44, die ersten
    // beiden Byte sind der Format-Tag (1 = PCM, 3 = IEEE float).
    let fmt_tag = if extensible {
        u16_at(&raw, 44)
    } else {
        roh_tag
    };

    // data-Chunk ueber die Kette suchen.
    let mut i = 20 + fmt_size as usize;
    let mut data_off = None;
    while i + 8 <= raw.len() {
        let len = u32_at(&raw, i + 4) as usize;
        if &raw[i..i + 4] == b"data" {
            data_off = Some((i + 8, len));
            break;
        }
        i += 8 + len + (len & 1);
    }
    let (data_off, data_len) = data_off.expect("kein data-Chunk");
    let pad = data_len % 2 == 1;
    let w = Wav {
        riff_size: u32_at(&raw, 4),
        fmt_size,
        fmt_tag,
        extensible,
        channels: u16_at(&raw, 22),
        sample_rate: u32_at(&raw, 24),
        byte_rate: u32_at(&raw, 28),
        block_align: u16_at(&raw, 32),
        bits: u16_at(&raw, 34),
        data_len: data_len as u32,
        data_off,
        data: raw[data_off..data_off + data_len].to_vec(),
        file_len: raw.len() as u64,
        pad,
    };

    // Konsistenz: alle Groessenfelder muessen zusammenpassen.
    assert!(
        w.extensible == (w.bits > 16 || w.channels > 2),
        "EXTENSIBLE genau bei >16 bit oder >2 Kanaelen (bits {}, kanaele {})",
        w.bits,
        w.channels
    );
    assert_eq!(
        w.byte_rate,
        w.sample_rate as u32 * w.block_align as u32,
        "byte_rate != samplerate * block_align"
    );
    assert_eq!(
        w.block_align,
        w.channels * w.bits / 8,
        "block_align != kanaele * bits/8"
    );
    assert_eq!(
        w.riff_size as u64,
        w.file_len - 8,
        "riff_size muss Dateigroesse - 8 sein"
    );
    // `hound` schreibt kein Fuellbyte hinter eine ungerade Nutzlast, die Datei
    // endet also direkt nach dem data-Chunk.
    assert_eq!(
        w.file_len,
        w.data_off as u64 + w.data_len as u64,
        "Dateigroesse passt nicht zu data-Offset und data_len"
    );
    assert_eq!(
        w.riff_size as u64,
        w.file_len - 8,
        "riff_size = Dateigroesse - 8"
    );
    assert_eq!(
        w.data_len % w.block_align as u32,
        0,
        "data_len keine ganze Frame-Zahl"
    );
    w
}

fn wav_dateien(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .expect("Verzeichnis lesbar")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "wav").unwrap_or(false))
        .collect();
    v.sort();
    v
}

/// `file(1)`-Pruefung; liefert None, wenn das Werkzeug fehlt.
fn file_beschreibung(p: &Path) -> Option<String> {
    let out = Command::new("file").arg("-b").arg(p).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn konstante_frames(n: usize, ch: usize, wert: f32) -> Vec<f32> {
    vec![wert; n * ch]
}

// ---------------------------------------------------------------------------
// Tests: Kopf und Frames
// ---------------------------------------------------------------------------

#[test]
fn pcm24_kopf_und_framezahl() {
    let dir = tmp("pcm24");
    let mut tape = Tape::new();
    tape.start(dir.join("take"), 48_000, 2, Format::Pcm24)
        .unwrap();
    assert_eq!(tape.state(), State::Recording);
    assert!(
        tape.current_path().unwrap().exists(),
        "Datei bei start angelegt"
    );

    // 1500 Frames in zwei Bloecken a 500 Frames (1000 Samples stereo)
    tape.write_block(&konstante_frames(500, 2, 0.25)).unwrap();
    tape.write_block(&konstante_frames(500, 2, -0.5)).unwrap();
    tape.write_block(&konstante_frames(500, 2, 0.0)).unwrap();
    assert_eq!(tape.frames(), 1500, "laufender Zaehler");

    let meta = tape.stop().unwrap();
    assert_eq!(tape.state(), State::Stopped);

    let dateien = wav_dateien(&dir);
    assert_eq!(dateien.len(), 1, "ohne Rotation genau eine Datei");
    let w = parse_wav(&dateien[0]);

    assert_eq!(w.fmt_tag, 1, "PCM muss Format-Tag 1 haben");
    assert!(w.extensible, "24 bit liegt als WAVE_FORMAT_EXTENSIBLE vor");
    assert_eq!(w.fmt_size, 40, "40-Byte-fmt-Chunk bei EXTENSIBLE");
    assert_eq!(w.channels, 2);
    assert_eq!(w.sample_rate, 48_000);
    assert_eq!(w.bits, 24);
    assert_eq!(w.block_align, 6, "Block-Align bei 2x24 bit");
    assert_eq!(w.byte_rate, 48_000 * 6);
    assert_eq!(w.frames(), 1500, "geschriebene Frames == erwartet");
    assert_eq!(w.data_len, 1500 * 6);

    // Stichproben: Kodierung der drei Bloecke
    let q25 = w.pcm24(0, 0);
    assert!((q25 - 2_097_152).abs() <= 1, "0.25 -> {q25}");
    let qn5 = w.pcm24(700, 1);
    assert!((qn5 + 4_194_304).abs() <= 1, "-0.5 -> {qn5}");
    assert_eq!(w.pcm24(1499, 1), 0, "letzter Frame still");

    // Metadaten
    assert_eq!(meta.frames, 1500);
    assert_eq!(meta.channels, 2);
    assert_eq!(meta.sample_rate, 48_000);
    assert_eq!(meta.format, Format::Pcm24);
    assert!((meta.peak - 0.5).abs() < 1e-6, "Peak {}", meta.peak);
    assert_eq!(meta.dateien.len(), 1);
    assert_eq!(meta.dateien[0].data_bytes, 9000);
    assert!(
        meta.start_time_iso.ends_with('Z'),
        "{}",
        meta.start_time_iso
    );
    assert!(meta.start_epoch_s > 1_600_000_000);
}

#[test]
fn float32_kopf_und_werte() {
    let dir = tmp("float32");
    let mut tape = Tape::new();
    tape.start(dir.join("take"), 44_100, 2, Format::Float32)
        .unwrap();
    tape.write_block(&[0.5, -0.5, 1.0, -1.0, 0.125, 0.0])
        .unwrap(); // 3 Frames
    let meta = tape.stop().unwrap();

    let dateien = wav_dateien(&dir);
    assert_eq!(dateien.len(), 1);
    let w = parse_wav(&dateien[0]);

    assert_eq!(w.fmt_tag, 3, "IEEE float muss Format-Tag 3 haben");
    assert!(w.extensible, "32 bit liegt als WAVE_FORMAT_EXTENSIBLE vor");
    assert_eq!(w.bits, 32);
    assert_eq!(w.block_align, 8, "Block-Align bei 2x32 bit");
    assert_eq!(w.sample_rate, 44_100);
    assert_eq!(w.frames(), 3);
    assert_eq!(w.data_len, 24);

    assert_eq!(w.f32_at(0, 0), 0.5);
    assert_eq!(w.f32_at(0, 1), -0.5);
    assert_eq!(w.f32_at(1, 0), 1.0);
    assert_eq!(w.f32_at(1, 1), -1.0);
    assert_eq!(w.f32_at(2, 0), 0.125);
    assert_eq!(w.f32_at(2, 1), 0.0);
    assert!((meta.peak - 1.0).abs() < 1e-6);
}

#[test]
fn unvollstaendige_frames_werden_gepuffert() {
    let dir = tmp("carry");
    let mut tape = Tape::new();
    tape.start(dir.join("take"), 16_000, 2, Format::Pcm24)
        .unwrap();
    // 3 Samples, dann 3 Samples -> zusammen 3 Frames stereo
    tape.write_block(&[0.1, 0.2, 0.3]).unwrap();
    assert_eq!(tape.frames(), 1, "1 Frame komplett, 1 Sample im Carry");
    tape.write_block(&[0.4, 0.5, 0.6]).unwrap();
    assert_eq!(tape.frames(), 3);
    tape.stop().unwrap();

    let w = parse_wav(&wav_dateien(&dir)[0]);
    assert_eq!(w.frames(), 3);
    assert_eq!(w.data_len, 18);
}

/// `hound` schreibt kein Fuellbyte hinter eine ungerade Nutzlast (RIFF sieht
/// Wortausrichtung vor, `hound` laesst sie aus). Fuer den Mixer ist das
/// folgenlos: aufgenommen wird immer mit geraden Kanalzahlen (2 * Busse), und
/// 24 bit je Sample ergibt damit immer eine gerade Nutzlast. Der Test haelt
/// das Verhalten fest, damit ein Wechsel des Schreibers auffaellt.
#[test]
fn ungerade_nutzlast_bleibt_ohne_pad_byte() {
    let dir = tmp("pad");
    let mut tape = Tape::new();
    tape.start(dir.join("take"), 16_000, 1, Format::Pcm24)
        .unwrap();
    tape.write_block(&[0.1, 0.2, 0.3, 0.4, 0.5]).unwrap(); // 5 Frames mono = 15 Byte
    tape.stop().unwrap();

    let p = &wav_dateien(&dir)[0];
    let w = parse_wav(p);
    assert_eq!(w.data_len, 15);
    assert!(w.pad, "15 Byte sind ungerade");
    assert_eq!(w.frames(), 5);
    assert_eq!(w.data_off, 68, "EXTENSIBLE-Kopf bei 24 bit");
    assert_eq!(fs::metadata(p).unwrap().len(), 68 + 15, "kein Pad-Byte");
    assert_eq!(w.riff_size as u64, w.file_len - 8);
}

// ---------------------------------------------------------------------------
// Tests: Rotation
// ---------------------------------------------------------------------------

#[test]
fn rotation_erzeugt_zweite_datei() {
    let dir = tmp("rotation");
    let cfg = Config {
        max_file_bytes: 4096,
        ..Config::default()
    };
    let mut tape = Tape::with_config(cfg);
    tape.start(dir.join("take"), 48_000, 2, Format::Pcm24)
        .unwrap();

    // 6 Byte/Frame: 4096/6 = 682 Frames pro Datei
    let block = konstante_frames(100, 2, 0.5);
    for _ in 0..20 {
        tape.write_block(&block).unwrap(); // 2000 Frames
    }
    let meta = tape.stop().unwrap();

    let dateien = wav_dateien(&dir);
    assert_eq!(dateien.len(), 3, "2000 Frames / 682 = 3 Segmente");
    assert!(
        dateien[1].to_string_lossy().contains("-002"),
        "{:?}",
        dateien
    );
    assert!(
        dateien[2].to_string_lossy().contains("-003"),
        "{:?}",
        dateien
    );

    let mut summe = 0u64;
    for p in &dateien {
        let w = parse_wav(p); // jeder Kopf muss gueltig sein
        assert_eq!(w.fmt_tag, 1);
        assert!(w.extensible);
        assert_eq!(w.block_align, 6);
        assert!(
            w.data_len as u64 <= 4096,
            "Segment {} Byte > 4096",
            w.data_len
        );
        assert!(w.frames() > 0, "keine leeren Segmente");
        summe += w.frames();
    }
    assert_eq!(summe, 2000, "Rotationsverlust: {summe} != 2000");
    assert_eq!(meta.frames, 2000);
    assert_eq!(meta.dateien.len(), 3);
    assert_eq!(
        meta.dateien.iter().map(|f| f.frames).sum::<u64>(),
        2000,
        "Metadaten muessen die Rotation abbilden"
    );
    assert!((meta.peak - 0.5).abs() < 1e-6);
}

#[test]
fn rotation_mit_krummer_blockgroesse_verliert_keine_frames() {
    let dir = tmp("rotation-krumm");
    let cfg = Config {
        max_file_bytes: 4096,
        buffer_frames: 64,
        ..Config::default()
    };
    let mut tape = Tape::with_config(cfg);
    tape.start(dir.join("take"), 48_000, 2, Format::Pcm24)
        .unwrap();
    // Blauecke zu 77 Frames, die Grenze (682) liegt nicht auf einem Vielfachen
    let block = konstante_frames(77, 2, 0.1);
    for _ in 0..13 {
        tape.write_block(&block).unwrap(); // 1001 Frames
    }
    let meta = tape.stop().unwrap();

    let summe: u64 = wav_dateien(&dir)
        .iter()
        .map(|p| parse_wav(p).frames())
        .sum();
    assert_eq!(summe, 1001);
    assert_eq!(meta.frames, 1001);
    assert!(
        wav_dateien(&dir).len() >= 2,
        "Rotation muss stattgefunden haben"
    );
}

// ---------------------------------------------------------------------------
// Tests: Header nach stop gegen `file(1)`
// ---------------------------------------------------------------------------

#[test]
fn header_nach_stop_ist_gueltig_laut_file() {
    let dir = tmp("file-cmd");

    // 24-bit mono, 16 kHz
    let mut tape = Tape::new();
    tape.start(dir.join("pcm"), 16_000, 1, Format::Pcm24)
        .unwrap();
    tape.write_block(&konstante_frames(1000, 1, 0.25)).unwrap();
    tape.stop().unwrap();
    let pcm = &wav_dateien(&dir)[0];

    // float32 stereo, 48 kHz
    let mut tape = Tape::new();
    tape.start(dir.join("float"), 48_000, 2, Format::Float32)
        .unwrap();
    tape.write_block(&konstante_frames(500, 2, 0.1)).unwrap();
    tape.stop().unwrap();

    let mut dateien = wav_dateien(&dir);
    dateien.sort();
    let float = dateien
        .iter()
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("float")
        })
        .unwrap();

    let Some(beschreibung_pcm) = file_beschreibung(pcm) else {
        eprintln!("file(1) nicht vorhanden - Pruefung des Kopfes nur intern");
        return;
    };
    let beschreibung_float = file_beschreibung(float).unwrap();
    eprintln!("file: {beschreibung_pcm}\nfile: {beschreibung_float}");

    // `file(1)` nennt bei WAVE_FORMAT_EXTENSIBLE nur Format, Rate und
    // Kanalzahl, nicht die Bittiefe - die prueft der zweite Werkzeugweg.
    let a = beschreibung_pcm.to_lowercase();
    assert!(a.contains("wave audio"), "file erkennt kein WAV: {a}");
    assert!(a.contains("16000 hz"), "Samplerate nicht erkannt: {a}");
    assert!(a.contains("mono"), "Kanalzahl nicht erkannt: {a}");

    let b = beschreibung_float.to_lowercase();
    assert!(b.contains("wave audio"), "file erkennt kein WAV: {b}");
    assert!(b.contains("48000 hz"), "Samplerate nicht erkannt: {b}");
    assert!(b.contains("stereo"), "Kanalzahl nicht erkannt: {b}");

    // Zweiter Weg: ffprobe liest Bittiefe und Sample-Format aus dem
    // EXTENSIBLE-Kopf. Fehlt ffprobe, bleibt es bei der internen Pruefung.
    let probe = |p: &Path| -> Option<String> {
        let out = Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=codec_name,channels"])
            .arg(p)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).to_lowercase())
    };
    match probe(pcm) {
        Some(text) => assert!(text.contains("pcm_s24le"), "ffprobe: {text}"),
        None => eprintln!("ffprobe nicht vorhanden - Bittiefe nur intern geprueft"),
    }
    match probe(float) {
        Some(text) => assert!(text.contains("pcm_f32le"), "ffprobe: {text}"),
        None => eprintln!("ffprobe nicht vorhanden - Format nur intern geprueft"),
    }
    // Bittiefe und Sample-Format stehen im EXTENSIBLE-Kopf; geprueft werden
    // sie von parse_wav und oben von ffprobe.
}

#[test]
fn rotation_bei_realistischer_dateigroesse() {
    let dir = tmp("rotation-1mib");
    let cfg = Config {
        max_file_bytes: 1 << 20, // 1 MiB, 1048576/6 = 174762 Frames pro Datei
        ..Config::default()
    };
    let mut tape = Tape::with_config(cfg);
    tape.start(dir.join("take"), 48_000, 2, Format::Pcm24)
        .unwrap();
    let block = konstante_frames(1024, 2, 0.4);
    for _ in 0..390 {
        tape.write_block(&block).unwrap(); // 399360 Frames
    }
    tape.write_block(&konstante_frames(640, 2, 0.4)).unwrap(); // 400000
    let meta = tape.stop().unwrap();

    let dateien = wav_dateien(&dir);
    assert_eq!(dateien.len(), 3, "{} Dateien", dateien.len());
    let mut summe = 0u64;
    for p in &dateien {
        let w = parse_wav(p);
        assert!(w.data_len as u64 <= 1 << 20, "{} Byte > 1 MiB", w.data_len);
        summe += w.frames();
    }
    assert_eq!(summe, 400_000);
    assert_eq!(meta.frames, 400_000);
    assert!((meta.dauer_s() - 400_000.0 / 48_000.0).abs() < 1e-9);
}

#[test]
fn drop_ohne_stop_finalisiert_den_kopf() {
    let dir = tmp("drop");
    {
        let mut tape = Tape::new();
        tape.start(dir.join("take"), 48_000, 2, Format::Pcm24)
            .unwrap();
        tape.write_block(&konstante_frames(500, 2, 0.2)).unwrap();
        // bewusst kein stop(): Drop muss den Kopf patchen
    }
    let w = parse_wav(&wav_dateien(&dir)[0]);
    assert_eq!(w.frames(), 500);
    assert_eq!(w.data_len, 3000);
    assert_eq!(w.riff_size as u64, w.file_len - 8);
}

// ---------------------------------------------------------------------------
// Tests: Zustandsmaschine und Fehler
// ---------------------------------------------------------------------------

#[test]
fn zustandsmaschine_und_fehlerfaelle() {
    let dir = tmp("states");
    let mut tape = Tape::new();
    assert_eq!(tape.state(), State::Idle);

    // Idle: kein Schreiben, kein Stop
    assert!(matches!(
        tape.write_block(&[0.0, 0.0]),
        Err(TapeError::WrongState {
            erwartet: State::Recording,
            aktuell: State::Idle
        })
    ));
    assert!(matches!(
        tape.stop(),
        Err(TapeError::WrongState {
            erwartet: State::Recording,
            aktuell: State::Idle
        })
    ));

    // Idle -> Recording
    tape.start(dir.join("take"), 48_000, 2, Format::Pcm24)
        .unwrap();
    assert_eq!(tape.state(), State::Recording);

    // Doppelter Start waehrend der Aufnahme ist ein Fehler
    assert!(matches!(
        tape.start(dir.join("zweit"), 48_000, 2, Format::Pcm24),
        Err(TapeError::WrongState {
            erwartet: State::Idle,
            aktuell: State::Recording
        })
    ));

    // Recording -> Stopped
    tape.write_block(&[0.1, 0.1]).unwrap();
    tape.stop().unwrap();
    assert_eq!(tape.state(), State::Stopped);

    // Nach dem Stop: kein weiteres Schreiben, kein zweiter Stop
    assert!(matches!(
        tape.write_block(&[0.1, 0.1]),
        Err(TapeError::WrongState {
            erwartet: State::Recording,
            aktuell: State::Stopped
        })
    ));
    assert!(matches!(tape.stop(), Err(TapeError::WrongState { .. })));

    // Stopped -> Recording (neue Aufnahme, Zaehler zurueckgesetzt)
    assert_eq!(tape.frames(), 1);
    tape.start(dir.join("take2"), 44_100, 1, Format::Pcm24)
        .unwrap();
    assert_eq!(tape.state(), State::Recording);
    assert_eq!(tape.frames(), 0, "neue Aufnahme startet bei 0");
    tape.stop().unwrap();
    assert_eq!(wav_dateien(&dir).len(), 2);
}

#[test]
fn unzulaessige_parameter_werden_abgelehnt() {
    let dir = tmp("params");
    let mut tape = Tape::new();
    assert!(matches!(
        tape.start(dir.join("a"), 0, 2, Format::Pcm24),
        Err(TapeError::UngueltigeKonfiguration(_))
    ));
    assert!(matches!(
        tape.start(dir.join("a"), 48_000, 0, Format::Pcm24),
        Err(TapeError::UngueltigeKonfiguration(_))
    ));
    assert!(matches!(
        tape.start(dir.join("a"), 48_000, 33, Format::Pcm24),
        Err(TapeError::UngueltigeKonfiguration(_))
    ));
    assert_eq!(
        tape.state(),
        State::Idle,
        "Fehler laesst Zustand unberuehrt"
    );

    let mut zu_klein = Tape::with_config(Config {
        max_file_bytes: 1024,
        ..Config::default()
    });
    assert!(matches!(
        zu_klein.start(dir.join("b"), 48_000, 2, Format::Pcm24),
        Err(TapeError::UngueltigeKonfiguration(_))
    ));
    assert!(wav_dateien(&dir).is_empty(), "keine Datei bei Fehlern");
}

// ---------------------------------------------------------------------------
// Tests: Zielpfad, Metadaten-Sidecar, Hotpath-Allokationen
// ---------------------------------------------------------------------------

#[test]
fn zielpfad_bekommt_zeitstempel_und_verzeichnis_wird_angelegt() {
    let dir = tmp("pfad");
    let ziel = dir.join("unter/ordner/take.wav");
    let mut tape = Tape::new();
    tape.start(&ziel, 48_000, 2, Format::Pcm24).unwrap();
    let p = tape.current_path().unwrap().to_path_buf();
    tape.stop().unwrap();

    assert!(p.exists());
    let name = p.file_name().unwrap().to_string_lossy().to_string();
    assert!(name.starts_with("take-"), "{name}");
    assert!(name.ends_with("-001.wav"), "{name}");
    // take-YYYYMMDD-HHMMSS-001.wav
    assert_eq!(name.len(), 4 + 1 + 15 + 1 + 7, "{name}");
    assert_eq!(p.parent().unwrap(), dir.join("unter/ordner"));
}

#[test]
fn metadaten_sidecar_als_json() {
    let dir = tmp("sidecar");
    let cfg = Config {
        metadata_sidecar: true,
        ..Config::default()
    };
    let mut tape = Tape::with_config(cfg);
    tape.start(dir.join("take"), 48_000, 2, Format::Pcm24)
        .unwrap();
    tape.write_block(&[0.75, -0.75]).unwrap();
    let meta = tape.stop().unwrap();

    let wav = &wav_dateien(&dir)[0];
    let json = wav.with_extension("json");
    assert!(json.exists(), "Sidecar fehlt: {}", json.display());
    let text = fs::read_to_string(&json).unwrap();
    for feld in [
        "\"samplerate\":48000",
        "\"kanaele\":2",
        "\"format\":\"pcm24\"",
        "\"frames\":1",
        "\"peak\":0.750000",
        "\"startzeit\":",
    ] {
        assert!(text.contains(feld), "fehlt {feld} in {text}");
    }
    assert_eq!(meta.frames, 1);
    assert!(tape.meta().is_some(), "meta() nach stop gefuellt");
}

#[test]
fn write_block_allokiert_nicht() {
    let dir = tmp("noalloc");
    let cfg = Config {
        buffer_frames: 512,
        ..Config::default()
    };
    let mut tape = Tape::with_config(cfg);
    tape.start(dir.join("take"), 48_000, 2, Format::Pcm24)
        .unwrap();

    let block = konstante_frames(256, 2, 0.3);
    // Warmlauf (erster Aufruf darf intern noch nichts anlegen, ist aber
    // nicht Teil der Messung)
    tape.write_block(&block).unwrap();

    let vorher = ALLOCS.with(|c| c.get());
    for _ in 0..100 {
        tape.write_block(&block).unwrap();
    }
    let nachher = ALLOCS.with(|c| c.get());
    assert_eq!(
        nachher - vorher,
        0,
        "write_block hat {} mal allokiert (Hotpath muss allokationsfrei sein)",
        nachher - vorher
    );

    tape.stop().unwrap();
    assert_eq!(tape.frames(), 256 * 101);
}
