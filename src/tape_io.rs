//! Tape-Anbindung: SPSC-Ringe zwischen Audio-Thread und Schreiber-Thread.
//!
//! Der Audio-Callback schiebt Blöcke allocationsfrei in den Ring; ein eigener
//! Thread zieht sie heraus und schreibt sie über `tape::Tape` als WAV. Damit
//! liegt kein Lock und keine Allokation im Echtzeitpfad.
//!
//! Zwei Ringe teilen sich dieses Muster:
//!
//! * [`Ring`] – ein Stereo-Block pro Slot (Bus A1, VBAN, Altverdrahtung).
//! * [`MultiRing`] – bis zu [`MAX_BUSSE`] Stereo-Paare pro Slot, interleaved
//!   `[frame0_ch0..frame0_chN, frame1_ch0..]`. Der Schreiber nimmt daraus alle
//!   in [`TapeControl::busse`] gewählten Busse als eine Datei mit
//!   `2 * busse.len()` Kanälen auf. Ändert sich die Auswahl während der
//!   Aufnahme, wird die laufende Datei finalisiert und eine neue begonnen.

use std::cell::UnsafeCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{
    AtomicBool, AtomicUsize, Ordering::Acquire, Ordering::Relaxed, Ordering::Release,
};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tape::{Format, Tape};

use crate::BUS_NAMES;

/// Höchstzahl aufgenommener Busse (0..7) und damit Stereo-Paare pro Block.
pub const MAX_BUSSE: usize = crate::BUSES;

/// Fester Ring: `slots` Blöcke zu je `frames` Stereo-Frames.
pub struct Ring {
    data: Box<[UnsafeCell<f32>]>,
    frames: usize,
    slots: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// Der Ring ist bewusst als SPSC gebaut: genau ein Produzent (Audio-Thread),
// genau ein Konsument (Schreiber-Thread). Das ist die Bedingung für `Sync`.
unsafe impl Sync for Ring {}
unsafe impl Send for Ring {}

impl Ring {
    pub fn new(slots: usize, frames: usize) -> Ring {
        let n = slots * frames * 2;
        Ring {
            data: (0..n).map(|_| UnsafeCell::new(0.0f32)).collect(),
            frames,
            slots,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    /// Produzent: einen Block ablegen. `false`, wenn der Ring voll ist.
    pub fn push(&self, interleaved: &[f32]) -> bool {
        let head = self.head.load(Relaxed);
        let tail = self.tail.load(Acquire);
        if head.wrapping_sub(tail) >= self.slots {
            return false;
        }
        let slot = head % self.slots;
        let base = slot * self.frames * 2;
        let n = (self.frames * 2).min(interleaved.len());
        unsafe {
            let dst = self.data.as_ptr().add(base) as *mut f32;
            std::ptr::copy_nonoverlapping(interleaved.as_ptr(), dst, n);
        }
        self.head.store(head.wrapping_add(1), Release);
        true
    }

    pub fn is_empty(&self) -> bool {
        self.tail.load(Acquire) == self.head.load(Acquire)
    }

    /// Konsument: einen Block holen. Liefert die Zahl der Stereo-Frames.
    pub fn pop(&self, out: &mut [f32]) -> Option<usize> {
        let tail = self.tail.load(Relaxed);
        let head = self.head.load(Acquire);
        if tail == head {
            return None;
        }
        let slot = tail % self.slots;
        let base = slot * self.frames * 2;
        let n = (self.frames * 2).min(out.len());
        unsafe {
            let src = self.data.as_ptr().add(base) as *const f32;
            std::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), n);
        }
        self.tail.store(tail.wrapping_add(1), Release);
        Some(n / 2)
    }
}

/// SPSC-Ring für mehrere Stereo-Paare.
///
/// Ein Slot faßt `frames` Frames zu je `pairs` Kanalpaaren. Ein Block wird als
/// `[frame0_ch0 .. frame0_chN, frame1_ch0 ..]` abgelegt (N = 2*pairs-1); die
/// Kanäle des jeweiligen Blocks werden pro Slot mitgespeichert, damit ein
/// Ring auch weniger Paare als seine Kapazität transportieren kann.
pub struct MultiRing {
    data: Box<[UnsafeCell<f32>]>,
    /// Kanäle je Slot (0 = noch nicht beschrieben).
    chans: Box<[AtomicUsize]>,
    /// Frames im jeweiligen Slot (ein Block darf kürzer als `frames` sein).
    lens: Box<[AtomicUsize]>,
    frames: usize,
    slots: usize,
    pairs: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
    last_channels: AtomicUsize,
}

// Wie `Ring`: ein Produzent, ein Konsument.
unsafe impl Sync for MultiRing {}
unsafe impl Send for MultiRing {}

impl MultiRing {
    /// `slots` Blöcke zu `frames` Frames, je Slot bis zu `pairs` Kanalpaare
    /// (1..=`MAX_BUSSE`).
    pub fn new(slots: usize, frames: usize, pairs: usize) -> MultiRing {
        let slots = slots.max(1);
        let frames = frames.max(1);
        let pairs = pairs.clamp(1, MAX_BUSSE);
        MultiRing {
            data: (0..slots * frames * pairs * 2)
                .map(|_| UnsafeCell::new(0.0f32))
                .collect(),
            chans: (0..slots).map(|_| AtomicUsize::new(0)).collect(),
            lens: (0..slots).map(|_| AtomicUsize::new(0)).collect(),
            frames,
            slots,
            pairs,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            last_channels: AtomicUsize::new(pairs * 2),
        }
    }

    /// Kanalpaare pro Slot (Kapazität).
    pub fn pairs(&self) -> usize {
        self.pairs
    }

    /// Kanäle pro Slot (Kapazität).
    pub fn channels(&self) -> usize {
        self.pairs * 2
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn is_empty(&self) -> bool {
        self.tail.load(Acquire) == self.head.load(Acquire)
    }

    /// Kanäle des zuletzt entnommenen Blocks.
    pub fn last_channels(&self) -> usize {
        self.last_channels.load(Relaxed)
    }

    /// Produzent: Block mit voller Kanalzahl (`2 * pairs`) ablegen.
    ///
    /// Format: `[frame0_ch0 .. frame0_chN, frame1_ch0 ..]`. Es werden ganze
    /// Frames übernommen, höchstens `frames()` Stück. `false` bei vollem Ring.
    pub fn push_interleaved(&self, interleaved: &[f32]) -> bool {
        self.push_pairs(interleaved, self.pairs)
    }

    /// Produzent: Block mit `pairs` Kanalpaaren ablegen (darf kleiner als die
    /// Kapazität sein, z. B. wenn nur zwei Busse aufgenommen werden).
    pub fn push_pairs(&self, interleaved: &[f32], pairs: usize) -> bool {
        let pairs = pairs.clamp(1, self.pairs);
        let ch = pairs * 2;
        let frames = (interleaved.len() / ch).min(self.frames);
        if frames == 0 {
            return false;
        }
        let head = self.head.load(Relaxed);
        let tail = self.tail.load(Acquire);
        if head.wrapping_sub(tail) >= self.slots {
            return false;
        }
        let slot = head % self.slots;
        let base = slot * self.frames * self.pairs * 2;
        unsafe {
            let dst = self.data.as_ptr().add(base) as *mut f32;
            std::ptr::copy_nonoverlapping(interleaved.as_ptr(), dst, frames * ch);
        }
        self.chans[slot].store(ch, Relaxed);
        self.lens[slot].store(frames, Relaxed);
        self.head.store(head.wrapping_add(1), Release);
        true
    }

    /// Konsument: einen Block holen, `out` bekommt `frames * ch` Samples.
    ///
    /// Liefert die Zahl der Frames; `None` bei leerem Ring oder wenn `out` für
    /// einen Frame des Blocks zu klein ist. Die Kanalzahl des Blocks steht
    /// danach in [`MultiRing::last_channels`].
    pub fn pop_block(&self, out: &mut [f32]) -> Option<usize> {
        let tail = self.tail.load(Relaxed);
        let head = self.head.load(Acquire);
        if tail == head {
            return None;
        }
        let slot = tail % self.slots;
        let ch = self.chans[slot].load(Relaxed).max(2);
        if out.len() < ch {
            return None;
        }
        let frames = self.lens[slot].load(Relaxed).clamp(1, self.frames).min(out.len() / ch);
        let base = slot * self.frames * self.pairs * 2;
        unsafe {
            let src = self.data.as_ptr().add(base) as *const f32;
            std::ptr::copy_nonoverlapping(src, out.as_mut_ptr(), frames * ch);
        }
        self.last_channels.store(ch, Relaxed);
        self.tail.store(tail.wrapping_add(1), Release);
        Some(frames)
    }
}

/// Steuerung des Tape-Recorders aus der Weboberfläche.
pub struct TapeControl {
    /// Stereo-Ring des Audio-Threads (Bus A1 bzw. VBAN-Altverdrahtung).
    pub ring: Arc<Ring>,
    /// Frames pro Block.
    pub frames: usize,
    pub recording: AtomicBool,
    pub status: Mutex<String>,
    /// Welche Busse (0..7) aufgenommen werden, in dieser Reihenfolge.
    pub busse: Mutex<Vec<usize>>,
    /// `true` = Mehrkanal aus [`TapeControl::multi`], `false` = Bus A1 aus `ring`.
    pub mehrspurig: AtomicBool,
    /// Mehrspuriger Ring (bis `MAX_BUSSE` Stereo-Paare pro Block).
    pub multi: Arc<MultiRing>,
    /// Beendet den Schreiber-Thread (Aufnahme wird dabei finalisiert).
    pub shutdown: AtomicBool,
}

impl TapeControl {
    pub fn new(ring: Arc<Ring>, frames: usize) -> TapeControl {
        let f = frames.max(1);
        TapeControl {
            ring,
            frames,
            recording: AtomicBool::new(false),
            status: Mutex::new("idle".to_string()),
            busse: Mutex::new(vec![0]),
            mehrspurig: AtomicBool::new(false),
            multi: Arc::new(MultiRing::new(MAX_BUSSE, f, MAX_BUSSE)),
            shutdown: AtomicBool::new(false),
        }
    }

    /// Setzt die Auswahl. Unbekannte Busse fallen raus, Doppelte werden
    /// entfernt (Reihenfolge bleibt = Kanalreihenfolge). Leere Auswahl wird
    /// ignoriert, damit nie eine Datei mit 0 Kanälen entsteht.
    pub fn set_busse(&self, busse: &[usize]) {
        let mut neu: Vec<usize> = Vec::with_capacity(busse.len());
        for &b in busse {
            if b < MAX_BUSSE && !neu.contains(&b) {
                neu.push(b);
            }
        }
        if neu.is_empty() {
            return;
        }
        if let Ok(mut g) = self.busse.lock() {
            *g = neu;
        }
    }

    /// Aktuelle Busauswahl (Kopie, nie leer).
    pub fn busse(&self) -> Vec<usize> {
        match self.busse.lock() {
            Ok(g) if !g.is_empty() => g.clone(),
            _ => vec![0],
        }
    }

    /// Kanäle, die die aktuelle Auswahl ergibt (1 Bus = 2, 2 Busse = 4, ...).
    pub fn kanaele(&self) -> usize {
        self.busse().len() * 2
    }

    pub fn set_status(&self, s: impl Into<String>) {
        if let Ok(mut g) = self.status.lock() {
            *g = s.into();
        }
    }

    pub fn status(&self) -> String {
        self.status.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// "A1" bzw. "A1+A3".
fn bus_label(busse: &[usize]) -> String {
    busse
        .iter()
        .map(|&b| BUS_NAMES.get(b).copied().unwrap_or("?"))
        .collect::<Vec<_>>()
        .join("+")
}

/// Dateistamm; `tape` hängt `<stamm>-<YYYYMMDD-HHMMSS>-001.wav` an, deshalb
/// millisekundengenauer Anteil plus laufende Nummer gegen Kollisionen.
fn stamm_name(sel: &[usize], ch: usize, seq: u32) -> String {
    let label = if sel.len() * 2 == ch {
        bus_label(sel)
    } else {
        format!("mix{ch}ch")
    };
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("vm8-tape-{label}-{ms}-{seq:03}")
}

/// Öffnet die nächste Datei; `false`, wenn das fehlschlägt (Status gesetzt).
fn oeffne(
    tape: &mut Tape,
    ctl: &TapeControl,
    base: &Path,
    sel: &[usize],
    ch: usize,
    sample_rate: u32,
    seq: u32,
) -> bool {
    let stamm = base.join(stamm_name(sel, ch, seq));
    match tape.start(&stamm, sample_rate, ch as u16, Format::Pcm24) {
        Ok(()) => {
            let pfad = tape
                .current_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| stamm.display().to_string());
            ctl.set_status(format!("rec {ch} Kanaele [{}] {pfad}", bus_label(sel)));
            true
        }
        Err(e) => {
            ctl.set_status(format!("fehler: {e}"));
            ctl.recording.store(false, Relaxed);
            false
        }
    }
}

/// Finalisiert die laufende Datei und meldet das Ergebnis im Status.
fn stoppe(tape: &mut Tape, ctl: &TapeControl) {
    match tape.stop() {
        Ok(meta) => ctl.set_status(format!(
            "gestoppt: {} Frames, {} Kanaele, Peak {:.1} dBFS, {:.1} s, {} Datei(en)",
            meta.frames,
            meta.channels,
            20.0 * meta.peak.max(1e-7).log10(),
            meta.dauer_s(),
            meta.dateien.len()
        )),
        Err(e) => ctl.set_status(format!("stop-fehler: {e}")),
    }
}

/// Ein Block aus dem passenden Ring: `(Frames, Kanaele)`.
fn hole(
    ctl: &TapeControl,
    mehrspurig: bool,
    mono: &mut [f32],
    multi: &mut [f32],
) -> Option<(usize, usize)> {
    if mehrspurig {
        let f = ctl.multi.pop_block(multi)?;
        Some((f, ctl.multi.last_channels()))
    } else {
        ctl.ring.pop(mono).map(|f| (f, 2))
    }
}

/// Schreiber-Thread: nimmt die in `busse()` gewählten Busse als eine WAV-Datei
/// mit `2 * busse.len()` Kanälen auf. Ohne `mehrspurig` wird Bus A1 aus dem
/// Stereo-Ring geschrieben (bisheriges Verhalten). Wechselt die Auswahl,
/// während aufgenommen wird, wird die Datei gewechselt.
pub fn spawn_writer(ctl: Arc<TapeControl>, base: PathBuf, sample_rate: u32) {
    let _ = std::fs::create_dir_all(&base);
    thread::spawn(move || {
        let frames = ctl.frames.max(1);
        let mut mono = vec![0.0f32; frames * 2];
        let mut multi = vec![0.0f32; frames * 2 * MAX_BUSSE];
        let mut tape = Tape::new();
        // (Kanäle der offenen Datei, Busauswahl der offenen Datei)
        let mut aktiv: Option<(usize, Vec<usize>)> = None;
        let mut seq: u32 = 0;

        loop {
            if ctl.shutdown.load(Relaxed) {
                if aktiv.is_some() {
                    stoppe(&mut tape, &ctl);
                }
                break;
            }

            let want = ctl.recording.load(Relaxed);
            let mehrspurig = ctl.mehrspurig.load(Relaxed);
            let sel = if mehrspurig { ctl.busse() } else { vec![0] };
            let wunsch_ch = sel.len() * 2;

            // Start, Auswahlwechsel oder Stopp.
            if want {
                let wechsel = match &aktiv {
                    None => true,
                    Some((ch, b)) => *ch != wunsch_ch || *b != sel,
                };
                if wechsel {
                    if aktiv.is_some() {
                        stoppe(&mut tape, &ctl);
                        aktiv = None;
                    }
                    seq += 1;
                    if oeffne(&mut tape, &ctl, &base, &sel, wunsch_ch, sample_rate, seq) {
                        aktiv = Some((wunsch_ch, sel.clone()));
                    }
                }
            } else if aktiv.is_some() {
                stoppe(&mut tape, &ctl);
                aktiv = None;
            }

            // Ring leeren.
            while aktiv.is_some() {
                let Some((f, ch_block)) = hole(&ctl, mehrspurig, &mut mono, &mut multi) else {
                    break;
                };
                if let Some((ch, _)) = aktiv.as_ref() {
                    if *ch != ch_block {
                        // Blöcke einer anderen Kanalzahl (Umschalt-Rest im Ring):
                        // Datei sauber schließen und mit der neuen Zahl weiter.
                        stoppe(&mut tape, &ctl);
                        seq += 1;
                        if !oeffne(&mut tape, &ctl, &base, &sel, ch_block, sample_rate, seq) {
                            aktiv = None;
                            break;
                        }
                        aktiv = Some((ch_block, sel.clone()));
                    }
                }
                let Some((ch, _)) = aktiv.as_ref().map(|a| (a.0, ())) else {
                    break;
                };
                let block = if mehrspurig {
                    &multi[..f * ch]
                } else {
                    &mono[..f * 2]
                };
                if let Err(e) = tape.write_block(block) {
                    ctl.set_status(format!("schreibfehler: {e}"));
                    ctl.recording.store(false, Relaxed);
                    break;
                }
            }

            thread::sleep(Duration::from_millis(4));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn testverzeichnis(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("vm8-tape-{name}-{nanos}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("Testverzeichnis");
        dir
    }

    fn wav_dateien(dir: &Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .expect("Verzeichnis")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "wav").unwrap_or(false))
            .collect();
        v.sort();
        v
    }

    fn header(p: &Path) -> Vec<u8> {
        let b = std::fs::read(p).expect("Datei lesen");
        assert_eq!(&b[0..4], b"RIFF", "kein RIFF-Kopf: {}", p.display());
        assert_eq!(&b[8..12], b"WAVE");
        b
    }

    fn wav_kanaele(p: &Path) -> u16 {
        let b = header(p);
        u16::from_le_bytes([b[22], b[23]])
    }

    fn wav_frames(p: &Path) -> u32 {
        let b = header(p);
        let ch = wav_kanaele(p) as usize;
        let bps = 3usize; // Pcm24
        let mut i = 12usize;
        while i + 8 <= b.len() {
            let len = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
            if &b[i..i + 4] == b"data" {
                assert_eq!(
                    ch * bps * (len / (ch * bps)),
                    len,
                    "data-Chunk nicht framealigned"
                );
                return (len / (ch * bps)) as u32;
            }
            i += 8 + len + (len & 1);
        }
        panic!("kein data-Chunk in {}", p.display());
    }

    fn warte<F: FnMut() -> bool>(mut f: F, ms: u64) -> bool {
        let ende = Duration::from_millis(ms);
        let start = SystemTime::now();
        while start.elapsed().unwrap_or_default() < ende {
            if f() {
                return true;
            }
            thread::sleep(Duration::from_millis(2));
        }
        f()
    }

    fn warte_status(ctl: &TapeControl, pruef: impl Fn(&str) -> bool, ms: u64) -> String {
        let mut s = String::new();
        let ok = warte(
            || {
                s = ctl.status();
                pruef(&s)
            },
            ms,
        );
        assert!(ok, "Status passte nicht: {s:?}");
        s
    }

    /// Füllt `n` Blöcke à 64 Frames mit `pairs` Kanalpaaren, wartet auf Platz.
    fn schiebe(ctl: &TapeControl, pairs: usize, n: usize) {
        let ch = pairs * 2;
        let mut block = vec![0.0f32; 64 * ch];
        for (i, v) in block.iter_mut().enumerate() {
            *v = ((i as f32) * 0.01).sin() * 0.1;
        }
        for _ in 0..n {
            let mut versuche = 0u32;
            while !ctl.multi.push_pairs(&block, pairs) {
                thread::sleep(Duration::from_millis(1));
                versuche += 1;
                assert!(versuche < 5000, "MultiRing nimmt nichts mehr an");
            }
        }
    }

    fn runde(pairs: usize) {
        let ring = MultiRing::new(4, 128, pairs);
        let ch = pairs * 2;
        assert_eq!(ring.channels(), ch);
        let mut block = vec![0.0f32; 128 * ch];
        for (i, v) in block.iter_mut().enumerate() {
            *v = ((i % 97) as f32) * 0.01;
        }
        assert!(ring.push_pairs(&block, pairs));
        assert!(ring.push_pairs(&block[..64 * ch], pairs));
        assert!(ring.push_interleaved(&block));

        let mut out = vec![0.0f32; 128 * ch];
        let f = ring.pop_block(&mut out).expect("Block 1");
        assert_eq!(f, 128);
        assert_eq!(ring.last_channels(), ch);
        assert_eq!(&out[..f * ch], &block[..]);

        let f = ring.pop_block(&mut out).expect("Block 2");
        assert_eq!(f, 64, "halber Block");
        assert_eq!(&out[..f * ch], &block[..64 * ch]);

        let f = ring.pop_block(&mut out).expect("Block 3");
        assert_eq!(f, 128);
        assert_eq!(&out[..f * ch], &block[..]);

        assert!(ring.pop_block(&mut out).is_none());
        assert!(ring.is_empty());

        // Volllaufen: 4 Slots, der fünfte Push scheitert.
        for _ in 0..4 {
            assert!(ring.push_pairs(&block, pairs));
        }
        assert!(!ring.push_pairs(&block, pairs), "Ring lief über");
    }

    #[test]
    fn multi_ring_rundlauf_2_und_8_buspaare() {
        runde(2);
        runde(MAX_BUSSE);
    }

    #[test]
    fn multi_ring_kleinere_bloecke_als_kapazitaet() {
        let ring = MultiRing::new(4, 32, MAX_BUSSE);
        let mut b2 = vec![0.11f32; 32 * 4];
        let mut b6 = vec![-0.22f32; 32 * 12];
        assert!(ring.push_pairs(&b2, 2));
        assert!(ring.push_pairs(&b6, 6));
        let mut out = vec![0.0f32; 32 * MAX_BUSSE * 2];
        assert_eq!(ring.pop_block(&mut out), Some(32));
        assert_eq!(ring.last_channels(), 4);
        assert!(out[..32 * 4].iter().all(|v| (*v - 0.11).abs() < 1e-6));
        assert_eq!(ring.pop_block(&mut out), Some(32));
        assert_eq!(ring.last_channels(), 12);
        assert!(out[..32 * 12].iter().all(|v| (*v + 0.22).abs() < 1e-6));
        b2.fill(0.5);
        b6.fill(0.5);
    }

    #[test]
    fn tape_control_busauswahl_api() {
        let ctl = TapeControl::new(Arc::new(Ring::new(2, 16)), 16);
        assert_eq!(ctl.busse(), vec![0]);
        assert_eq!(ctl.kanaele(), 2);
        ctl.set_busse(&[0, 1, 3, 1, 9]);
        assert_eq!(ctl.busse(), vec![0, 1, 3]);
        assert_eq!(ctl.kanaele(), 6);
        ctl.set_busse(&[]);
        assert_eq!(ctl.busse(), vec![0, 1, 3], "leere Auswahl bleibt ignoriert");
        ctl.set_busse(&[2]);
        assert_eq!(ctl.busse(), vec![2]);
        assert!(!ctl.mehrspurig.load(Relaxed));
    }

    #[test]
    fn kanalzahl_im_header_entspricht_auswahl() {
        let dir = testverzeichnis("header");
        let ring = Arc::new(Ring::new(8, 64));
        let ctl = Arc::new(TapeControl::new(ring, 64));
        ctl.mehrspurig.store(true, Relaxed);
        ctl.set_busse(&[0, 2, 5]);
        spawn_writer(ctl.clone(), dir.clone(), 48_000);
        ctl.recording.store(true, Relaxed);
        let rec = warte_status(&ctl, |s| s.starts_with("rec"), 3000);
        schiebe(&ctl, 3, 10);
        assert!(warte(|| ctl.multi.is_empty(), 3000));
        ctl.recording.store(false, Relaxed);
        warte_status(&ctl, |s| s.starts_with("gestoppt"), 3000);
        ctl.shutdown.store(true, Relaxed);

        let dateien = wav_dateien(&dir);
        assert_eq!(dateien.len(), 1, "genau eine Datei: {dateien:?}");
        assert_eq!(wav_kanaele(&dateien[0]), 6, "3 Busse = 6 Kanäle");
        assert!(wav_frames(&dateien[0]) > 0);
        assert!(rec.contains("A1+A3+B1"), "Status: {rec}");
    }

    #[test]
    fn wechsel_der_busauswahl_startet_neue_datei() {
        let dir = testverzeichnis("wechsel");
        let ctl = Arc::new(TapeControl::new(Arc::new(Ring::new(8, 64)), 64));
        ctl.mehrspurig.store(true, Relaxed);
        ctl.set_busse(&[0]);
        spawn_writer(ctl.clone(), dir.clone(), 48_000);
        ctl.recording.store(true, Relaxed);
        warte_status(&ctl, |s| s.starts_with("rec"), 3000);
        schiebe(&ctl, 1, 12);
        assert!(warte(|| ctl.multi.is_empty(), 3000));

        ctl.set_busse(&[0, 1]);
        warte_status(&ctl, |s| s.starts_with("rec") && s.contains("4 Kanaele"), 3000);
        schiebe(&ctl, 2, 12);
        assert!(warte(|| ctl.multi.is_empty(), 3000));

        ctl.recording.store(false, Relaxed);
        warte_status(&ctl, |s| s.starts_with("gestoppt"), 3000);
        ctl.shutdown.store(true, Relaxed);

        let dateien = wav_dateien(&dir);
        for d in &dateien {
            eprintln!("DEBUG {} ch={} frames={}", d.display(), wav_kanaele(d), wav_frames(d));
        }
        assert_eq!(dateien.len(), 2, "Wechsel = neue Datei: {dateien:?}");
        assert_eq!(wav_kanaele(&dateien[0]), 2);
        assert_eq!(wav_kanaele(&dateien[1]), 4);
        assert!(wav_frames(&dateien[0]) > 0);
        assert!(wav_frames(&dateien[1]) > 0);
    }

    #[test]
    fn stopp_finalisiert_alle_dateien() {
        let dir = testverzeichnis("stopp");
        let ctl = Arc::new(TapeControl::new(Arc::new(Ring::new(8, 64)), 64));
        ctl.mehrspurig.store(true, Relaxed);
        ctl.set_busse(&[0]);
        spawn_writer(ctl.clone(), dir.clone(), 48_000);
        ctl.recording.store(true, Relaxed);
        warte_status(&ctl, |s| s.starts_with("rec"), 3000);
        schiebe(&ctl, 1, 8);
        assert!(warte(|| ctl.multi.is_empty(), 3000));
        ctl.set_busse(&[3, 4]);
        warte_status(&ctl, |s| s.starts_with("rec") && s.contains("4 Kanaele"), 3000);
        schiebe(&ctl, 2, 8);
        assert!(warte(|| ctl.multi.is_empty(), 3000));

        ctl.recording.store(false, Relaxed);
        let s = warte_status(&ctl, |s| s.starts_with("gestoppt"), 3000);
        ctl.shutdown.store(true, Relaxed);

        let dateien = wav_dateien(&dir);
        assert_eq!(dateien.len(), 2, "Dateien: {dateien:?}");
        // Finalisiert = data-Chunk-Größe passt zur Dateilänge (Kopf gepatcht).
        for p in &dateien {
            let b = std::fs::read(p).expect("lesen");
            let len = b.len();
            let data = u32::from_le_bytes([b[40], b[41], b[42], b[43]]) as usize;
            assert_eq!(data + tape::HEADER_LEN, len, "{} nicht finalisiert", p.display());
            assert!(wav_frames(p) > 0);
        }
        assert!(s.contains("gestoppt") && s.contains("4 Kanaele"), "Status: {s}");
    }

    #[test]
    fn legacy_ring_schreibt_stereo_ohne_mehrspur() {
        let dir = testverzeichnis("legacy");
        let ring = Arc::new(Ring::new(8, 64));
        let ctl = Arc::new(TapeControl::new(ring.clone(), 64));
        spawn_writer(ctl.clone(), dir.clone(), 44_100);
        ctl.recording.store(true, Relaxed);
        warte_status(&ctl, |s| s.starts_with("rec"), 3000);
        let block: Vec<f32> = (0..64 * 2).map(|i| (i as f32 * 0.01).sin() * 0.2).collect();
        for _ in 0..6 {
            let mut n = 0;
            while !ring.push(&block) {
                thread::sleep(Duration::from_millis(1));
                n += 1;
                assert!(n < 5000);
            }
        }
        assert!(warte(|| ring.is_empty(), 3000), "Ring nicht geleert");
        ctl.recording.store(false, Relaxed);
        warte_status(&ctl, |s| s.starts_with("gestoppt"), 3000);
        ctl.shutdown.store(true, Relaxed);

        let dateien = wav_dateien(&dir);
        assert_eq!(dateien.len(), 1);
        assert_eq!(wav_kanaele(&dateien[0]), 2);
        assert_eq!(wav_frames(&dateien[0]), 6 * 64);
    }
}
