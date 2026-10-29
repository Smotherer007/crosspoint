//! Beleg: der Audio-Callback allokiert nichts.
//!
//! Ein zaehlender Allokator umhuellt das ganze Testprogramm. Gemessen wird
//! nur das Fenster um `EngineDsp::process` – Aufbau, Presets und Puffer
//! entstehen vorher. Im Fenster sind Lowcut, TK (mit Lookahead-Ring), Gate,
//! NR (mit FFT) und der
//! einfache Denoiser aktiv: alle Puffer liegen im DSP-Zustand, `process`
//! baut kein Vec und ruft kein `format!`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use crosspoint::engine::{EngineDsp, Io, Params};
use crosspoint::{BUSES, STRIPS};

struct Zaehler;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Zaehler {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(l.size(), Ordering::Relaxed);
        System.alloc(l)
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(l.size(), Ordering::Relaxed);
        System.alloc_zeroed(l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, neu: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(neu, Ordering::Relaxed);
        System.realloc(p, l, neu)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
}

#[global_allocator]
static SPION: Zaehler = Zaehler;

#[test]
fn process_allokiert_nichts() {
    const N: usize = 4096;
    const BLOCK: usize = 1024;

    let p = std::sync::Arc::new(Params::new(48000.0, BLOCK as u32));
    // Alles an, was Puffer brauchen koennte.
    for (k, v) in [
        ("lowcut", 120.0),
        ("tk", 1.0),
        ("tk_schwelle", 6.0),
        ("tk_tiefe", 15.0),
        ("gate", 1.0),
        ("gate_schwelle", -45.0),
        ("nr", 0.8),
        ("nr_schwelle", -40.0),
        ("nr_gate", 1.0),
        ("denoiser", 1.0),
        ("eq", 1.0),
        ("comp", 1.0),
    ] {
        assert!(p.set("strip", 0, k, v), "Parameter {k}");
    }

    // Der DSP-Zustand ist gross; er entsteht auf einem eigenen Stapel.
    let mut dsp = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| Box::new(EngineDsp::new(BLOCK)))
        .unwrap()
        .join()
        .unwrap();

    let mut ins: Vec<Vec<f32>> = vec![vec![0.0f32; N]; STRIPS * 2];
    let mut outs: Vec<Vec<f32>> = vec![vec![0.0f32; N]; BUSES * 2];
    // Signal: Sinus + Rauschen, damit jede Stufe wirklich rechnet.
    let mut s = 12345u32;
    let mut signal = Vec::with_capacity(N);
    for i in 0..N {
        let mut sum = 0.0;
        for _ in 0..3 {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            sum += (s >> 8) as f32 / 8_388_608.0 - 1.0;
        }
        signal.push(
            0.1 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48000.0).sin()
                + 0.03 * sum * 0.577,
        );
    }
    ins[0][..N].copy_from_slice(&signal);
    ins[1][..N].copy_from_slice(&signal);

    let mut io = Io {
        ins: [[std::ptr::null(); 2]; STRIPS],
        outs: [[std::ptr::null_mut(); 2]; BUSES],
    };
    for k in 0..STRIPS {
        for c in 0..2 {
            io.ins[k][c] = ins[k * 2 + c].as_ptr();
        }
    }
    for k in 0..BUSES {
        for c in 0..2 {
            io.outs[k][c] = outs[k * 2 + c].as_mut_ptr();
        }
    }

    // Ein Block zum Aufwaermen (Koeffizienten, Rauschschaetzung startet).
    dsp.process(&p, &io, BLOCK);

    let vorher = ALLOCS.load(Ordering::Relaxed);
    let bytes_vorher = BYTES.load(Ordering::Relaxed);
    for _ in 0..4 {
        dsp.process(&p, &io, BLOCK);
    }
    let nachher = ALLOCS.load(Ordering::Relaxed);
    let bytes_nachher = BYTES.load(Ordering::Relaxed);

    println!(
        "ALLOC: {} Aufrufe und {} Bytes in vier Bloecken ({} Frames) mit LC+Gate+NR+EQ+Comp",
        nachher - vorher,
        bytes_nachher - bytes_vorher,
        4 * BLOCK
    );
    assert_eq!(
        nachher - vorher,
        0,
        "process hat {} mal allokiert ({} Bytes)",
        nachher - vorher,
        bytes_nachher - bytes_vorher
    );
}
