//! Messungen und Reihenfolge-Belege am Streifen (EngineDsp::process).
//!
//! Alles laeuft im Prozess, ohne JACK/PipeWire: der Weg ist derselbe wie im
//! Audio-Callback (Parallelen zu `tests/`-Skripten gibt es nicht). Werte
//! werden mit `--nocapture` ausgegeben.

use std::sync::Arc;

use crosspoint::engine::{EngineDsp, Io, Params};
use crosspoint::{BUSES, STRIPS};

const SR: f32 = 48000.0;
const BLOCK: usize = 1024;

/// Ein Pult mit einem Streifen: Signal in Streifen 1, Ausgang an Bus A1.
struct Pult {
    p: Arc<Params>,
    // Der DSP-Zustand ist gross (16 Rauschunterdrueckungen mit festen
    // Puffern, rund 0,5 MB). Er entsteht auf einem Thread mit grossem Stapel
    // und liegt danach im Heap - so bleibt der Teststapel frei.
    dsp: Box<EngineDsp>,
    ins: Vec<Vec<f32>>,
    outs: Vec<Vec<f32>>,
    n: usize,
}

impl Pult {
    fn neu(n: usize) -> Pult {
        let dsp = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| Box::new(EngineDsp::new(BLOCK)))
            .expect("Thread")
            .join()
            .expect("DSP");
        Pult {
            p: Arc::new(Params::new(SR, BLOCK as u32)),
            dsp,
            ins: vec![vec![0.0f32; n]; STRIPS * 2],
            outs: vec![vec![0.0f32; n]; BUSES * 2],
            n,
        }
    }

    fn setze(&self, param: &str, v: f32) {
        assert!(
            self.p.set("strip", 0, param, v),
            "Parameter {param} unbekannt"
        );
    }

    fn block_io(&mut self, ab: usize, bis: usize) -> Io {
        let mut io = Io {
            ins: [[std::ptr::null(); 2]; STRIPS],
            outs: [[std::ptr::null_mut(); 2]; BUSES],
        };
        for s in 0..STRIPS {
            for c in 0..2 {
                io.ins[s][c] = unsafe { self.ins[s * 2 + c].as_ptr().add(ab) };
            }
        }
        for b in 0..BUSES {
            for c in 0..2 {
                io.outs[b][c] = unsafe { self.outs[b * 2 + c].as_mut_ptr().add(ab) };
            }
        }
        let _ = bis;
        io
    }

    /// Signal auf beide Eingangskanaele von Streifen 1 legen.
    fn eingang(&mut self, sig: &[f32]) {
        self.ins[0][..sig.len()].copy_from_slice(sig);
        self.ins[1][..sig.len()].copy_from_slice(sig);
    }

    /// Rechnen und den linken A1-Ausgang zurueckgeben.
    fn lauf(&mut self) -> Vec<f32> {
        let n = self.n;
        let mut ab = 0;
        while ab < n {
            let bis = (ab + BLOCK).min(n);
            let io = self.block_io(ab, bis);
            self.dsp.process(&self.p, &io, bis - ab);
            ab = bis;
        }
        self.outs[0].clone()
    }
}

fn sinus(f: f32, amp: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| amp * (2.0 * std::f32::consts::PI * f * i as f32 / SR).sin())
        .collect()
}

/// Deterministisches weisses Rauschen (drei gleichverteilte Werte summiert).
fn weiss(n: usize, rms: f32, seed: u32) -> Vec<f32> {
    let mut s = seed | 1;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let mut sum = 0.0;
        for _ in 0..3 {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            sum += (s >> 8) as f32 / 8_388_608.0 - 1.0;
        }
        v.push(sum * 0.577);
    }
    let m = (v.iter().map(|x| x * x).sum::<f32>() / n as f32).sqrt();
    v.iter().map(|x| x * rms / m).collect()
}

fn rms(v: &[f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt()
}

fn db(v: f32) -> f32 {
    if v <= 1e-9 {
        -140.0
    } else {
        20.0 * v.log10()
    }
}

/// Amplitude eines Tones bei `f` (Projektion auf sin/cos).
fn ton(v: &[f32], f: f32) -> f32 {
    let (mut s, mut c) = (0.0f64, 0.0f64);
    for (i, x) in v.iter().enumerate() {
        let w = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / SR as f64;
        s += *x as f64 * w.sin();
        c += *x as f64 * w.cos();
    }
    ((2.0 / v.len() as f64) * (s * s + c * c).sqrt()) as f32
}

/* ------------------------------------------------------------------ */
/* 1. NR: Sinus 1 kHz + weisses Rauschen, beide -20 dBFS                */
/* ------------------------------------------------------------------ */

#[test]
fn messung_nr_am_streifen() {
    let n = 96_000; // 2 s
    let ton_sig = sinus(1000.0, 0.1, n);
    let rauschen = weiss(n, 0.1, 0x1234_5678);
    let ein: Vec<f32> = ton_sig.iter().zip(&rauschen).map(|(a, b)| a + b).collect();
    let ab = n / 2;

    // Referenz ohne NR
    let mut trocken = Pult::neu(n);
    trocken.eingang(&ein);
    let aus_0 = trocken.lauf();

    let mut zeilen = Vec::new();
    for staerke in [0.5f32, 1.0] {
        let mut p = Pult::neu(n);
        p.eingang(&ein);
        p.setze("nr", staerke);
        p.setze("nr_schwelle", -45.0);
        let aus = p.lauf();
        let (t1, t2) = (ton(&ein[ab..], 1000.0), ton(&aus[ab..], 1000.0));
        let (r1, r2) = (rms(&ein[ab..]), rms(&aus[ab..]));
        let n1 = (r1 * r1 - t1 * t1 / 2.0).max(0.0).sqrt();
        let n2 = (r2 * r2 - t2 * t2 / 2.0).max(0.0).sqrt();
        zeilen.push(format!(
            "NR {staerke:.1}: Rauschen {:.2} -> {:.2} dBFS ({:+.2} dB), Sinus {:.2} -> {:.2} dBFS ({:+.2} dB)",
            db(n1),
            db(n2),
            db(n2) - db(n1),
            db(t1),
            db(t2),
            db(t2) - db(t1)
        ));
        assert!(
            db(n2) - db(n1) < -10.0,
            "Rauschen sank bei {staerke} nur um {:.2} dB",
            db(n2) - db(n1)
        );
        assert!(
            (db(t2) - db(t1)).abs() <= 1.0,
            "Sinus verlor {:.2} dB",
            db(t2) - db(t1)
        );
    }
    assert!(db(ton(&aus_0[ab..], 1000.0)) > -21.0);
    for z in zeilen {
        println!("{z}");
    }
}

/* ------------------------------------------------------------------ */
/* 2. Lowcut am Streifen: 40 Hz gegen 200 Hz                            */
/* ------------------------------------------------------------------ */

#[test]
fn messung_lowcut_am_streifen() {
    let n = 48_000;
    let mut zeilen = Vec::new();
    for hz in [60.0f32, 120.0] {
        let mut tief = Pult::neu(n);
        tief.eingang(&sinus(40.0, 0.5, n));
        tief.setze("lowcut", hz);
        let t = rms(&tief.lauf()[n / 2..]);

        let mut hoch = Pult::neu(n);
        hoch.eingang(&sinus(200.0, 0.5, n));
        hoch.setze("lowcut", hz);
        let h = rms(&hoch.lauf()[n / 2..]);

        let unterschied = db(t) - db(h);
        zeilen.push(format!(
            "LOWCUT {hz:>3} Hz: 40 Hz = {:.2} dBFS, 200 Hz = {:.2} dBFS, Unterschied {:.2} dB",
            db(t),
            db(h),
            unterschied
        ));
        assert!(
            unterschied < -7.0,
            "Lowcut {hz} Hz daempfte 40 Hz nur um {unterschied:.2} dB"
        );
    }
    for z in zeilen {
        println!("{z}");
    }
}

/* ------------------------------------------------------------------ */
/* 3. Gate: Rauschen in den Pausen                                      */
/* ------------------------------------------------------------------ */

/// Sprache (Sinus-Burst) mit Rauschen dazwischen; gibt die Pausenabschnitte
/// in Samples zurueck.
fn sprache_mit_pausen(n: usize) -> (Vec<f32>, Vec<(usize, usize)>) {
    // Rauschen im Saal: -60 dBFS, also unter der Gate-Schwelle (-45 dBFS).
    let rauschen = weiss(n, 0.001, 0xA1B2_C3D4);
    let mut sig = rauschen.clone();
    let mut pausen = Vec::new();
    // 0,4 s Sprache, 0,5 s Pause, im Wechsel
    let mut pos = 0;
    let mut sprache = true;
    while pos < n {
        // 0,4 s Sprache, 1 s Pause: das Gate braucht Release (200 ms) plus
        // Huellkurve, bis es wirklich zu ist.
        let len = if sprache { 19_200 } else { 48_000 };
        let bis = (pos + len).min(n);
        if sprache {
            for (i, x) in sig.iter_mut().enumerate().take(bis).skip(pos) {
                *x += 0.1 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / SR).sin();
            }
        } else {
            pausen.push((pos, bis));
        }
        pos = bis;
        sprache = !sprache;
    }
    (sig, pausen)
}

#[test]
fn messung_gate_in_den_pausen() {
    let n = 240_000; // 5 s
    let (sig, pausen) = sprache_mit_pausen(n);

    let mut ohne = Pult::neu(n);
    ohne.eingang(&sig);
    let aus_ohne = ohne.lauf();

    let mut mit = Pult::neu(n);
    mit.eingang(&sig);
    mit.setze("gate", 1.0);
    mit.setze("gate_schwelle", -45.0);
    mit.setze("gate_hold", 80.0);
    mit.setze("gate_release", 120.0);
    mit.setze("gate_tiefe", 60.0);
    let aus_mit = mit.lauf();

    // Nur die letzten 300 ms jeder Pause messen (Hold/Release abklingen lassen)
    let mut summe_ohne = 0.0f64;
    let mut summe_mit = 0.0f64;
    let mut anzahl = 0usize;
    for (a, b) in &pausen {
        // Die letzten 0,3 s jeder Pause: Hold (80 ms), Detektor (20 ms) und
        // Release (120 ms) sind dann durch.
        let von = a + 33_600;
        if von + 14_400 > *b {
            continue;
        }
        for i in von..(von + 14_400) {
            summe_ohne += (aus_ohne[i] as f64).powi(2);
            summe_mit += (aus_mit[i] as f64).powi(2);
            anzahl += 1;
        }
    }
    let r_ohne = (summe_ohne / anzahl as f64).sqrt() as f32;
    let r_mit = (summe_mit / anzahl as f64).sqrt() as f32;
    println!(
        "GATE Pausen: Rauschen {:.2} -> {:.2} dBFS ({:+.2} dB), {} Pausen, Schwelle -45 dBFS, Tiefe 60 dB",
        db(r_ohne),
        db(r_mit),
        db(r_mit) - db(r_ohne),
        pausen.len()
    );
    assert!(
        db(r_mit) - db(r_ohne) < -25.0,
        "Gate senkte die Pausen nur um {:.2} dB",
        db(r_mit) - db(r_ohne)
    );
    // Und die Sprache bleibt: Pegel der Sprachabschnitte
    let mut sprach_mit = 0.0f64;
    let mut k = 0usize;
    for (i, x) in aus_mit.iter().enumerate().take(n) {
        let in_pause = pausen.iter().any(|(a, b)| i >= *a && i < *b);
        if !in_pause && i > 20_000 {
            sprach_mit += (*x as f64).powi(2);
            k += 1;
        }
    }
    println!(
        "GATE Sprache: {:.2} dBFS (durchgelassen)",
        db((sprach_mit / k as f64).sqrt() as f32)
    );
}

/* ------------------------------------------------------------------ */
/* 4. Leiser Sprachanteil (-45 dBFS): mit Gate weg, ohne Gate da        */
/* ------------------------------------------------------------------ */

#[test]
fn messung_leise_sprache_und_gate() {
    let n = 48_000;
    let leise = sinus(500.0, 0.00795, n); // -45 dBFS Effektivwert

    let mut ohne = Pult::neu(n);
    ohne.eingang(&leise);
    let aus_ohne = rms(&ohne.lauf()[n / 2..]);

    let mut mit = Pult::neu(n);
    mit.eingang(&leise);
    mit.setze("gate", 1.0);
    mit.setze("gate_schwelle", -40.0);
    mit.setze("gate_hold", 80.0);
    let aus_mit = rms(&mit.lauf()[n / 2..]);

    println!(
        "LEISE SPRACHE -45 dBFS: ohne Gate {:.2} dBFS, mit Gate (Schwelle -40 dBFS) {:.2} dBFS ({:+.2} dB)",
        db(aus_ohne),
        db(aus_mit),
        db(aus_mit) - db(aus_ohne)
    );
    assert!(db(aus_ohne) > -47.0, "ohne Gate fehlte das Signal");
    assert!(
        db(aus_mit) - db(aus_ohne) < -40.0,
        "mit Gate blieb zu viel uebrig: {:.2} dB",
        db(aus_mit) - db(aus_ohne)
    );
}

/* ------------------------------------------------------------------ */
/* 5. Reihenfolge: GATE vor COMP                                        */
/* ------------------------------------------------------------------ */

#[test]
fn reihenfolge_gate_vor_comp() {
    use crosspoint::dsp::{Compressor, Gate};

    let n = 48_000;
    // Signal deutlich unter der Gate-Schwelle: -50 dBFS
    let leise = sinus(400.0, 0.00316 * 1.414, n);

    // Neuer Pfad (so wie im Streifen): Gate -> Comp mit viel Makeup
    let mut p = Pult::neu(n);
    p.eingang(&leise);
    p.setze("gate", 1.0);
    p.setze("gate_schwelle", -45.0);
    p.setze("comp", 1.0);
    p.setze("comp_threshold", -50.0);
    p.setze("comp_ratio", 8.0);
    // Makeup hinter dem Comp: im alten Pfad hebt er das Signal ueber die
    // Gate-Schwelle, im neuen Pfad nicht mehr.
    p.setze("comp_gain_out", 18.0);
    p.setze("comp_gain_in", 0.0);
    let neu = rms(&p.lauf()[n / 2..]);

    // Gegenprobe: alter Pfad (Comp vor Gate), gleiche Stufen von Hand
    let mut comp = Compressor::default();
    comp.enabled = true;
    comp.set_times(20.0, 200.0, SR);
    let mut gate = Gate::default();
    gate.enabled = true;
    gate.set_times(5.0, 80.0, 200.0, SR);
    gate.set_tiefe(60.0);
    let mut alt = Vec::with_capacity(n);
    for x in &leise {
        let mut y = comp.process(*x, -50.0, 8.0, 0.0, 18.0);
        y = gate.process(y, -45.0);
        alt.push(y);
    }
    let alt = rms(&alt[n / 2..]);

    println!(
        "REIHENFOLGE: Eingang {:.2} dBFS, neuer Pfad (Gate vor Comp) {:.2} dBFS, alter Pfad (Comp vor Gate) {:.2} dBFS",
        db(rms(&leise[n / 2..])),
        db(neu),
        db(alt)
    );
    // Gate zuerst: das Gate sieht -50 dBFS, schliesst, der Comp danach kann
    // daraus nichts mehr machen.
    assert!(
        db(neu) < -70.0,
        "neuer Pfad liess {:.2} dBFS durch",
        db(neu)
    );
    // Comp zuerst: er hebt das Signal ueber die Schwelle, das Gate oeffnet.
    assert!(
        db(alt) > db(neu) + 40.0,
        "Gegenprobe war nicht deutlich lauter ({:.2} dBFS)",
        db(alt)
    );
}

/* ------------------------------------------------------------------ */
/* 6. Mit ausgeschalteten Stufen bit-genau wie die alte Kette           */
/* ------------------------------------------------------------------ */

#[test]
fn ohne_neue_stufen_bitgenau_wie_vorher() {
    use crosspoint::dsp::{db_to_lin, Biquad, Compressor, FilterType};

    let n = 24_000;
    let mut sig = weiss(n, 0.05, 0xDEAD_BEEF);
    for (i, x) in sig.iter_mut().enumerate() {
        *x += 0.2 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / SR).sin();
    }

    let mut p = Pult::neu(n);
    p.eingang(&sig);
    // Zwei EQ-Zellen und der Compressor an - der Rest aus.
    p.setze("eq", 1.0);
    p.setze("eq0_on", 1.0);
    p.setze("eq0_type", 0.0);
    p.setze("eq0_freq", 800.0);
    p.setze("eq0_q", 1.0);
    p.setze("eq0_gain", 6.0);
    p.setze("eq1_on", 1.0);
    p.setze("eq1_type", 3.0);
    p.setze("eq1_freq", 6000.0);
    p.setze("eq1_q", 0.7);
    p.setze("eq1_gain", 0.0);
    p.setze("comp", 1.0);
    p.setze("comp_threshold", -18.0);
    p.setze("comp_ratio", 4.0);
    p.setze("comp_attack", 10.0);
    p.setze("comp_release", 150.0);
    p.setze("gate", 0.0);
    p.setze("denoiser", 0.0);
    p.setze("nr", 0.0);
    p.setze("lowcut", 0.0);
    let aus = p.lauf();

    // Alte Kette von Hand: EQ -> Comp -> Fader
    let mut b0 = Biquad::bypass();
    b0.coeffs(FilterType::Peak, 800.0, 1.0, 6.0, SR);
    let mut b1 = Biquad::bypass();
    b1.coeffs(FilterType::LowPass, 6000.0, 0.7, 0.0, SR);
    let mut comp = Compressor::default();
    comp.enabled = true;
    comp.set_times(10.0, 150.0, SR);
    let mut erwartet = Vec::with_capacity(n);
    for x in &sig {
        let mut y = b0.process(*x);
        y = b1.process(y);
        y = comp.process(y, -18.0, 4.0, 0.0, 0.0);
        y *= db_to_lin(0.0);
        erwartet.push(y);
    }
    let mut abweichungen = 0usize;
    for i in 0..n {
        if aus[i].to_bits() != erwartet[i].to_bits() {
            abweichungen += 1;
            if abweichungen < 4 {
                println!("  bei {i}: {} != {}", aus[i], erwartet[i]);
            }
        }
    }
    println!(
        "BITGLEICH: {n} Samples verglichen, {abweichungen} Abweichungen (EQ + Comp an, LC/NR/Gate/Denoiser aus)"
    );
    assert_eq!(abweichungen, 0, "Kette weicht von der alten Reihenfolge ab");
}

/* ------------------------------------------------------------------ */
/* 7. Zustand und Setter-Pfad                                           */
/* ------------------------------------------------------------------ */

#[test]
fn zustand_meldet_die_neuen_felder() {
    let p = Params::new(SR, BLOCK as u32);
    assert!(p.set("strip", 0, "lowcut", 120.0));
    assert!(p.set("strip", 0, "nr", 0.7));
    assert!(p.set("strip", 0, "nr_schwelle", -38.0));
    assert!(p.set("strip", 0, "gate", 1.0));
    assert!(p.set("strip", 0, "gate_schwelle", -42.0));
    assert!(p.set("strip", 0, "gate_hold", 120.0));
    assert!(p.set("strip", 0, "gate_tiefe", 40.0));

    let j = p.to_json();
    let strips = j.get("strips").and_then(|v| v.as_arr()).unwrap();
    let s = &strips[0];
    assert_eq!(s.get("lowcut").and_then(|v| v.as_f64()), Some(120.0));
    let nr = s.get("nr").and_then(|v| v.as_f64()).unwrap();
    assert!((nr - 0.7).abs() < 1e-6, "nr war {nr}");
    assert_eq!(s.get("nr_schwelle").and_then(|v| v.as_f64()), Some(-38.0));
    assert_eq!(
        s.get("gate").and_then(|v| v.as_bool()),
        Some(true),
        "gate fehlt"
    );
    assert_eq!(s.get("gate_schwelle").and_then(|v| v.as_f64()), Some(-42.0));
    assert_eq!(s.get("gate_hold").and_then(|v| v.as_f64()), Some(120.0));
    assert_eq!(s.get("gate_tiefe").and_then(|v| v.as_f64()), Some(40.0));
    println!("ZUSTAND: {}", j.to_json());
}

#[test]
fn lowcut_rastet_und_klemmt() {
    let p = Params::new(SR, BLOCK as u32);
    p.set("strip", 0, "lowcut", 110.0);
    assert_eq!(
        p.to_json()
            .get("strips")
            .and_then(|v| v.as_arr())
            .and_then(|a| a[0].get("lowcut"))
            .and_then(|v| v.as_f64()),
        Some(120.0)
    );
    p.set("strip", 0, "lowcut", 0.0);
    p.set("strip", 0, "gate_schwelle", -5.0);
    p.set("strip", 0, "gate_hold", 999.0);
    let s = p.to_json();
    let s = s.get("strips").and_then(|v| v.as_arr()).unwrap();
    assert_eq!(
        s[0].get("gate_schwelle").and_then(|v| v.as_f64()),
        Some(-20.0)
    );
    assert_eq!(s[0].get("gate_hold").and_then(|v| v.as_f64()), Some(500.0));
}

/* ------------------------------------------------------------------ */
/* 8. Zugabe: dritter Modus mit RNNoise (nur mit Bibliothek)            */
/* ------------------------------------------------------------------ */

/// Laeuft nur mit gesetzter Umgebungsvariable `CROSSPOINT_RNNOISE`, also mit
/// einer (echten oder Stub-)Bibliothek. Ohne sie gibt es den Modus nicht -
/// dann fehlt auch nichts, siehe `ohne_bibliothek_kein_kanal` im Kern.
#[test]
fn modus_rnnoise_nutzt_die_bibliothek() {
    if std::env::var("CROSSPOINT_RNNOISE").is_err() {
        println!("RNNOISE: uebersprungen (keine Bibliothek gesetzt)");
        return;
    }
    let n = 24_000;
    let sig = sinus(500.0, 0.1, n);
    let mut p = Pult::neu(n);
    p.eingang(&sig);
    p.setze("nr", 0.5);
    p.setze("nr_modus", 2.0);
    let aus = p.lauf();
    // Die Stub-Bibliothek halbiert den Pegel: -6 dB beweisen, dass Modus 2
    // wirklich ueber die Bibliothek laeuft und nicht ueber die eigene NR.
    let trocken = rms(&sig[n / 2..]);
    let nass = rms(&aus[n / 2..]);
    println!(
        "RNNOISE-Modus: {:.2} -> {:.2} dBFS ({:+.2} dB)",
        db(trocken),
        db(nass),
        db(nass) - db(trocken)
    );
    assert!(
        (db(nass) - db(trocken) + 6.0).abs() < 1.0,
        "Modus 2 rechnete nicht ueber die Bibliothek ({:+.2} dB)",
        db(nass) - db(trocken)
    );
}

/* ------------------------------------------------------------------ */
/* 9. Preset nimmt die neuen Werte mit                                  */
/* ------------------------------------------------------------------ */

#[test]
fn preset_speichert_und_laedt_die_neuen_stufen() {
    let quelle = Arc::new(Params::new(SR, BLOCK as u32));
    for (k, v) in [
        ("lowcut", 160.0),
        ("nr", 0.4),
        ("nr_schwelle", -35.0),
        ("nr_modus", 1.0),
        ("nr_gate", 1.0),
        ("gate", 1.0),
        ("gate_schwelle", -38.0),
        ("gate_hold", 150.0),
        ("gate_tiefe", 45.0),
    ] {
        assert!(quelle.set("strip", 3, k, v), "Parameter {k}");
    }
    let json = quelle.to_json();

    let ziel = Arc::new(Params::new(SR, BLOCK as u32));
    crosspoint::http::apply_preset(&ziel, &json);

    let s = ziel.to_json();
    let s = s.get("strips").and_then(|v| v.as_arr()).unwrap();
    let f = |k: &str| s[3].get(k).and_then(|v| v.as_f64()).unwrap();
    println!(
        "PRESET: lowcut {} nr {} nr_schwelle {} nr_modus {} gate_schwelle {} gate_hold {} gate_tiefe {}",
        f("lowcut"),
        f("nr"),
        f("nr_schwelle"),
        f("nr_modus"),
        f("gate_schwelle"),
        f("gate_hold"),
        f("gate_tiefe")
    );
    assert_eq!(f("lowcut"), 160.0);
    assert!((f("nr") - 0.4).abs() < 1e-6);
    assert_eq!(f("nr_schwelle"), -35.0);
    assert_eq!(f("nr_modus"), 1.0);
    assert_eq!(f("gate_schwelle"), -38.0);
    assert_eq!(f("gate_hold"), 150.0);
    assert_eq!(f("gate_tiefe"), 45.0);
    assert_eq!(s[3].get("gate").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(s[3].get("nr_gate").and_then(|v| v.as_bool()), Some(true));
}
