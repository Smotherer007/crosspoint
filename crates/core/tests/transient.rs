//! Belege fuer den Transienten-Unterdruecker (TK).
//!
//! 1. **Synthetik:** 1-kHz-Sinus (-20 dBFS) mit eingestreuten 5-ms-Knacksen
//!    (-10 dBFS). Die Knackse muessen um mindestens 10 dB abgesenkt werden,
//!    der Sinus darf hoechstens 1 dB verlieren.
//! 2. **Sprachbeginn:** eine Silbe, die sich ueber 40 ms aufbaut, darf nicht
//!    als Transient gelten (Sperre bei steigendem langsamem Pegel).
//! 3. **Echtes Material:** `tippen3.wav` (Sennheiser, 48 kHz mono) durch einen
//!    Streifen mit TK=0 und TK=1 (Schwelle 6 dB, Tiefe 15 dB). Berichtet
//!    werden die Spitzen der Klackser (dieselben Ereignisse vorher/nachher)
//!    und der Median der Sprachphase.
//!
//! Der Pfad zur WAV-Datei steht in `TK_WAV` (Vorgabe: der Ablageort der
//! Messung). Fehlt die Datei, entfaellt nur Teil 3.

use std::sync::Arc;

use crosspoint::engine::{EngineDsp, Io, Params};
use crosspoint::{BUSES, STRIPS};

const SR: f32 = 48000.0;
const BLOCK: usize = 1024;

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

/* ------------------------------------------------------------------ */
/* Pult: ein Streifen, Ausgang A1                                      */
/* ------------------------------------------------------------------ */

struct Pult {
    p: Arc<Params>,
    dsp: Box<EngineDsp>,
    ins: Vec<Vec<f32>>,
    outs: Vec<Vec<f32>>,
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
        }
    }

    fn setze(&self, param: &str, v: f32) {
        assert!(
            self.p.set("strip", 0, param, v),
            "Parameter {param} unbekannt"
        );
    }

    /// Signal auf beide Eingangskanaele von Streifen 1 legen.
    fn eingang(&mut self, sig: &[f32]) {
        self.ins[0][..sig.len()].copy_from_slice(sig);
        self.ins[1][..sig.len()].copy_from_slice(sig);
    }

    /// Rechnen und den linken A1-Ausgang zurueckgeben.
    fn lauf(&mut self) -> Vec<f32> {
        let n = self.ins[0].len();
        let mut ab = 0;
        while ab < n {
            let frames = BLOCK.min(n - ab);
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
            self.dsp.process(&self.p, &io, frames);
            ab += frames;
        }
        self.outs[0][..n].to_vec()
    }
}

/* ------------------------------------------------------------------ */
/* Huellkurve und Transientensuche – dasselbe Mass wie in der Messung  */
/* ------------------------------------------------------------------ */

/// 5-ms-Effektivwerte (wie in der Voruntersuchung des Materials).
fn huellkurve(v: &[f32]) -> Vec<f32> {
    let fl = (SR * 0.005) as usize;
    v.chunks_exact(fl)
        .map(|f| (f.iter().map(|x| x * x).sum::<f32>() / fl as f32).sqrt())
        .collect()
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/* ------------------------------------------------------------------ */
/* 1. Synthetik: Sinus mit Knacksen                                    */
/* ------------------------------------------------------------------ */

fn sinus_mit_knacksen(n: usize, mit_tk: bool) -> (f32, f32) {
    let ton = 0.1; // -20 dBFS
    let knack = 0.316; // -10 dBFS
    let mut p = Pult::neu(n);
    let mut sig = vec![0.0f32; n];
    let knack_len = (0.005 * SR) as usize;
    let mut stelle = (0.250 * SR) as usize; // erster Knacks nach 250 ms
    let mut knackse = Vec::new();
    while stelle + knack_len < n {
        knackse.push(stelle);
        stelle += (0.400 * SR) as usize; // alle 400 ms
    }
    for (i, x) in sig.iter_mut().enumerate() {
        *x = ton * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / SR).sin();
    }
    for &k in &knackse {
        for j in 0..knack_len {
            // 5 ms Knacks: derselbe Sinus, 10 dB lauter, hart ein-/ausgeschaltet.
            sig[k + j] += knack * (2.0 * std::f32::consts::PI * 1000.0 * (k + j) as f32 / SR).sin();
        }
    }
    p.eingang(&sig);
    p.setze("tk", if mit_tk { 1.0 } else { 0.0 });
    p.setze("tk_schwelle", 6.0);
    p.setze("tk_tiefe", 15.0);
    let aus = p.lauf();

    // Pegel im Knacks: Maximum im 5-ms-Fenster; Pegel des Sinus: Median der
    // 5-ms-Effektivwerte abseits der Knackse.
    let h = huellkurve(&aus);
    let fl = (0.005 * SR) as usize;
    let mut knack_peak = 0.0f32;
    for &k in &knackse {
        let von = k / fl;
        let bis = ((k + knack_len) / fl + 2).min(h.len());
        for v in &h[von..bis] {
            knack_peak = knack_peak.max(*v);
        }
    }
    let mut ruhig: Vec<f32> = Vec::new();
    for (i, v) in h.iter().enumerate() {
        let t = (i * fl) as f32 / SR;
        let nahe = knackse.iter().any(|&k| (t - k as f32 / SR).abs() < 0.030);
        if !nahe && t > 0.050 {
            ruhig.push(*v);
        }
    }
    (db(knack_peak), db(median(&mut ruhig)))
}

#[test]
fn sinus_bleibt_knackse_verschwinden() {
    let n = (2.0 * SR) as usize;
    let (k0, s0) = sinus_mit_knacksen(n, false);
    let (k1, s1) = sinus_mit_knacksen(n, true);
    println!(
        "TK SYNTHETIK: Knacks {k0:.2} -> {k1:.2} dBFS ({:+.2} dB), Sinus {s0:.2} -> {s1:.2} dBFS ({:+.2} dB)",
        k1 - k0,
        s1 - s0
    );
    assert!(
        k1 - k0 < -10.0,
        "Knackse nur um {:.2} dB abgesenkt",
        k1 - k0
    );
    assert!((s1 - s0).abs() <= 1.0, "Sinus verlor {:.2} dB", s1 - s0);
}

/* ------------------------------------------------------------------ */
/* 2. Sprachbeginn: Aufbau ueber 40 ms darf nicht als Transient gelten */
/* ------------------------------------------------------------------ */

/// Silbenartiges Signal (200 Hz + Harmonische), das ueber `aufbau_ms` aus dem
/// Ruhepegel auf -20 dBFS ansteigt und 300 ms steht.
fn silbe(n: usize, aufbau_ms: f32) -> Vec<f32> {
    let mut sig = vec![0.0f32; n];
    let aufbau = (aufbau_ms * 0.001 * SR) as usize;
    let mut r = 0x2345_6789u32;
    for (i, platz) in sig.iter_mut().enumerate() {
        // Rauschteppich bei -60 dBFS
        r = r.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = ((r >> 8) as f32 / 8_388_608.0 - 1.0) * 0.001;
        let h = if i < aufbau {
            i as f32 / aufbau as f32
        } else {
            1.0
        };
        let mut v = 0.0;
        for (k, a) in [(1.0f32, 1.0f32), (2.0, 0.5), (3.0, 0.3), (5.0, 0.2)] {
            v += a * (2.0 * std::f32::consts::PI * 200.0 * k * i as f32 / SR).sin();
        }
        *platz = 0.05 * h * v + noise;
    }
    sig
}

#[test]
fn sprachbeginn_wird_nicht_abgesenkt() {
    let n = (0.4 * SR) as usize;
    let sig = silbe(n, 40.0);
    let mut p = Pult::neu(n);
    p.eingang(&sig);
    p.setze("tk", 1.0);
    p.setze("tk_schwelle", 6.0);
    p.setze("tk_tiefe", 15.0);
    let aus = p.lauf();
    // Pegel am Ende der Silbe gegen den Spitzenwert des Anstiegs: der
    // Einschwingvorgang darf hoechstens 2 dB unter dem Stand danach liegen.
    let aufbau = (0.040 * SR) as usize;
    let spitze_aufbau = aus[aufbau..aufbau + 4800]
        .iter()
        .fold(0.0f32, |m, x| m.max(x.abs()));
    let mut spaet = 0.0f32;
    for x in &aus[(0.200 * SR) as usize..(0.250 * SR) as usize] {
        spaet = spaet.max(x.abs());
    }
    println!(
        "TK SPRACHBEGINN: Pegel im Aufbau {:.2} dBFS, danach {:.2} dBFS ({:+.2} dB)",
        db(spitze_aufbau),
        db(spaet),
        db(spitze_aufbau) - db(spaet)
    );
    assert!(
        db(spitze_aufbau) - db(spaet) > -2.0,
        "Sprachbeginn wurde um {:.2} dB abgesenkt",
        db(spaet) - db(spitze_aufbau)
    );
}

/* ------------------------------------------------------------------ */
/* 3. Echtes Material: tippen3.wav                                     */
/* ------------------------------------------------------------------ */

/// 16-bit-PCM-WAV lesen (Mono oder der erste Kanal).
fn wav_lesen(pfad: &str) -> Option<Vec<f32>> {
    let d = std::fs::read(pfad).ok()?;
    if d.len() < 44 || &d[0..4] != b"RIFF" || &d[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12;
    let (mut kanaele, mut bits, mut rate) = (1usize, 16usize, 48000u32);
    while pos + 8 <= d.len() {
        let id = &d[pos..pos + 4];
        let len = u32::from_le_bytes([d[pos + 4], d[pos + 5], d[pos + 6], d[pos + 7]]) as usize;
        if id == b"fmt " {
            kanaele = u16::from_le_bytes([d[pos + 10], d[pos + 11]]) as usize;
            rate = u32::from_le_bytes([d[pos + 12], d[pos + 13], d[pos + 14], d[pos + 15]]);
            bits = u16::from_le_bytes([d[pos + 22], d[pos + 23]]) as usize;
        } else if id == b"data" {
            if bits != 16 {
                return None;
            }
            let br = rate as f32 * kanaele as f32 * (bits as f32 / 8.0);
            let step = (br / rate as f32) as usize;
            let mut out = Vec::with_capacity(len / step);
            for i in (0..len).step_by(step) {
                if pos + 8 + i + 2 > d.len() {
                    break;
                }
                let v = i16::from_le_bytes([d[pos + 8 + i], d[pos + 8 + i + 1]]);
                out.push(v as f32 / 32768.0);
            }
            return Some(out);
        }
        pos += 8 + len + (len & 1);
    }
    None
}

fn messdatei() -> Option<String> {
    let p = std::env::var("TK_WAV").ok().unwrap_or_else(|| {
        "/home/patrickweppelmann/.hermes/profiles/re/cache/scratch/tippen3.wav".to_string()
    });
    if std::path::Path::new(&p).exists() {
        Some(p)
    } else {
        None
    }
}

fn durch_tk(sig: &[f32], an: bool) -> Vec<f32> {
    let mut p = Pult::neu(sig.len());
    p.eingang(sig);
    p.setze("tk", if an { 1.0 } else { 0.0 });
    p.setze("tk_schwelle", 6.0);
    p.setze("tk_tiefe", 15.0);
    p.lauf()
}

/// Median der 5-ms-Huellkurve in einem Zeitfenster (Sekunden).
fn median_fenster(h: &[f32], von_s: f32, bis_s: f32) -> f32 {
    let fl = (0.005 * SR) as usize;
    let von = ((von_s * SR) as usize / fl).min(h.len());
    let bis = ((bis_s * SR) as usize / fl).min(h.len());
    let mut seg: Vec<f32> = h[von..bis].to_vec();
    db(median(&mut seg))
}

#[test]
fn messung_an_tippen3() {
    let pfad = match messdatei() {
        Some(p) => p,
        None => {
            println!("TK MESSUNG: keine Datei (TK_WAV) – uebersprungen");
            return;
        }
    };
    let sig = wav_lesen(&pfad).expect("WAV lesbar");
    println!(
        "TK MESSUNG: {pfad}, {} Samples = {:.1} s",
        sig.len(),
        sig.len() as f32 / SR
    );

    let aus0 = durch_tk(&sig, false);
    let aus1 = durch_tk(&sig, true);

    // Der Kern: TK=0 muss bit-gleich der Eingang sein (Kette unveraendert).
    let mut abweichung = 0;
    for (a, b) in sig.iter().zip(aus0.iter()) {
        if a.to_bits() != b.to_bits() {
            abweichung += 1;
        }
    }
    println!(
        "TK MESSUNG: TK=0 gegen Eingang: {abweichung} von {} Samples nicht bit-gleich",
        sig.len()
    );
    assert_eq!(abweichung, 0, "TK aus veraenderte das Signal");

    let h0 = huellkurve(&aus0);
    let h1 = huellkurve(&aus1);

    // Dieselben Ereignisse vorher/nachher vergleichen – im Sample-Bereich,
    // damit die 5-ms-Rasterung und der Lookahead das Ergebnis nicht verzerren.
    let mut ereignisse: Vec<f32> = Vec::new();
    for (k, _) in h0.iter().enumerate().skip(1) {
        let t = k as f32 * 0.005;
        if t < 25.0
            && db(h0[k]) - db(h0[k - 1]) > 10.0
            && db(h0[k]) > -40.0
            && ereignisse
                .last()
                .map(|&l: &f32| t - l > 0.020)
                .unwrap_or(true)
        {
            ereignisse.push(t);
        }
    }
    let mut vorher: Vec<f32> = Vec::new();
    let mut nachher: Vec<f32> = Vec::new();
    let versatz = (0.005 * SR) as usize; // Lookahead des TK
    for &t in &ereignisse {
        // Lage der Spitze im Eingang suchen und im Ausgang an der um den
        // Lookahead verschobenen Stelle messen (gleicher Klackser).
        let von = (t * SR) as usize;
        let bis = (von + (0.015 * SR) as usize).min(aus0.len());
        let mut tp = von;
        for (i, x) in aus0[von..bis].iter().enumerate() {
            if x.abs() > aus0[tp].abs() {
                tp = von + i;
            }
        }
        vorher.push(db(aus0[tp].abs()));
        let mut nach = 0.0f32;
        let von2 = (tp + versatz).saturating_sub(48);
        let bis2 = (tp + versatz + 48).min(aus1.len());
        for x in &aus1[von2..bis2] {
            nach = nach.max(x.abs());
        }
        nachher.push(db(nach));
    }
    let (mv, mn) = (median(&mut vorher.clone()), median(&mut nachher.clone()));
    let (xv, xn) = (
        vorher.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        nachher.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
    );
    println!(
        "TK MESSUNG: {} Klackser (0-25 s, Anstieg >10 dB / 5 ms) – Spitzen Median {mv:.2} -> {mn:.2} dBFS ({:+.2} dB), Maximum {xv:.2} -> {xn:.2} dBFS ({:+.2} dB)",
        ereignisse.len(),
        mn - mv,
        xn - xv
    );
    // Die fuenf am schlechtesten gedaempften Klackser im Einzelnen.
    let mut paare: Vec<(f32, f32, f32)> = ereignisse
        .iter()
        .zip(vorher.iter().zip(nachher.iter()))
        .map(|(&t, (&a, &b))| (b - a, t, a))
        .collect();
    paare.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap());
    for (d, t, a) in paare.iter().take(5) {
        println!("TK MESSUNG:   schwach {t:6.2} s: {a:7.2} dBFS, Daempfung {d:+.2} dB");
    }

    // Sprachphasen (25-60 s) und Tippen/Klopfen (0-25 s) getrennt.
    for (name, von, bis) in [
        ("Tippen 0-15 s", 0.0f32, 15.0f32),
        ("Klopfen 15-25 s", 15.0, 25.0),
        ("Sprechen+Tippen 25-45 s", 25.0, 45.0),
        ("Sprechen 45-60 s", 45.0, 60.0),
        ("Stille 60-75 s", 60.0, 75.0),
    ] {
        println!(
            "TK MESSUNG: {name:24} Median {:7.2} -> {:7.2} dBFS ({:+.2} dB)",
            median_fenster(&h0, von, bis),
            median_fenster(&h1, von, bis),
            median_fenster(&h1, von, bis) - median_fenster(&h0, von, bis)
        );
    }

    assert!(
        mn - mv < -10.0,
        "Klackser nur um {:.2} dB abgesenkt",
        mn - mv
    );
    // Die Sprache darf nicht mitverschwinden. Gemessen wird die reine
    // Sprachphase (45-60 s); die Mischphase 25-45 s enthaelt selbst Tippen,
    // ihr Median sinkt deshalb mit.
    let s0 = median_fenster(&h0, 45.0, 60.0);
    let s1 = median_fenster(&h1, 45.0, 60.0);
    println!(
        "TK MESSUNG: Sprache 45-60 s Median {s0:.2} -> {s1:.2} dBFS ({:+.2} dB)",
        s1 - s0
    );
    assert!(s1 - s0 > -1.5, "Sprache verlor {:.2} dB", s1 - s0);
}
