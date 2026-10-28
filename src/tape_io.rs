//! Tape-Anbindung: SPSC-Ring zwischen Audio-Thread und Schreiber-Thread.
//!
//! Der Audio-Callback schiebt Blöcke allocationsfrei in den Ring; ein eigener
//! Thread zieht sie heraus und schreibt sie über `tape::Tape` als WAV. Damit
//! liegt kein Lock und keine Allokation im Echtzeitpfad.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Acquire, Ordering::Release, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tape::{Format, Tape};

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

/// Steuerung des Tape-Recorders aus der Weboberfläche.
pub struct TapeControl {
    pub ring: Arc<Ring>,
    pub frames: usize,
    pub recording: AtomicBool,
    pub status: Mutex<String>,
}

impl TapeControl {
    pub fn new(ring: Arc<Ring>, frames: usize) -> TapeControl {
        TapeControl {
            ring,
            frames,
            recording: AtomicBool::new(false),
            status: Mutex::new("idle".to_string()),
        }
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

/// Schreiber-Thread: schreibt Bus A1 (Stereo) als WAV, solange `recording` gilt.
pub fn spawn_writer(ctl: Arc<TapeControl>, base: std::path::PathBuf, sample_rate: u32) {
    thread::spawn(move || {
        let mut buf = vec![0.0f32; ctl.frames * 2];
        let mut tape = Tape::new();
        let mut active = false;
        loop {
            let want = ctl.recording.load(Relaxed);
            if want && !active {
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let path = base.join(format!("vm8-tape-{ts}.wav"));
                match tape.start(&path, sample_rate, 2, Format::Pcm24) {
                    Ok(()) => {
                        active = true;
                        ctl.set_status(format!("rec {}", path.display()));
                    }
                    Err(e) => {
                        ctl.set_status(format!("fehler: {e}"));
                        ctl.recording.store(false, Relaxed);
                        continue;
                    }
                }
            }
            if !want && active {
                match tape.stop() {
                    Ok(meta) => ctl.set_status(format!(
                        "gestoppt: {} Frames, Peak {:.1} dBFS, {:.1} s",
                        meta.frames,
                        20.0 * meta.peak.max(1e-7).log10(),
                        meta.dauer_s()
                    )),
                    Err(e) => ctl.set_status(format!("stop-fehler: {e}")),
                }
                active = false;
            }
            if want && active {
                while let Some(frames) = ctl.ring.pop(&mut buf) {
                    let n = (frames * 2).min(buf.len());
                    if let Err(e) = tape.write_block(&buf[..n]) {
                        ctl.set_status(format!("schreibfehler: {e}"));
                        ctl.recording.store(false, Relaxed);
                        break;
                    }
                }
            }
            thread::sleep(Duration::from_millis(4));
        }
    });
}

