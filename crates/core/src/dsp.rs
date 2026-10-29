//! DSP-Bausteine.
//!
//! **Herkunft der Algorithmen:** Der Render-Pfad des Originals ist belegt
//! (`render(ctx, float* out, frames)`, siehe docs/03), die *Koeffizientenbildung*
//! der FX-Kerne ist es **nicht**. Hier stehen deshalb Standard-Implementierungen
//! (RBJ-Biquads, Peak-Detektor mit Attack/Release) mit derselben Parameterliste
//! wie im Original. Was vom Original abweicht, steht im Doc-Kommentar.

use std::f32::consts::PI;

#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
pub fn lin_to_db(v: f32) -> f32 {
    if v <= 1e-7 {
        -140.0
    } else {
        20.0 * v.log10()
    }
}

#[inline]
fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// Filtertyp einer EQ-Zelle (Reihenfolge der `EQtype`-Werte im Original unbelegt).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FilterType {
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    BandPass,
    Notch,
}

impl FilterType {
    pub fn from_u8(v: u8) -> FilterType {
        match v {
            1 => FilterType::LowShelf,
            2 => FilterType::HighShelf,
            3 => FilterType::LowPass,
            4 => FilterType::HighPass,
            5 => FilterType::BandPass,
            6 => FilterType::Notch,
            _ => FilterType::Peak,
        }
    }
    pub fn as_u8(self) -> u8 {
        match self {
            FilterType::Peak => 0,
            FilterType::LowShelf => 1,
            FilterType::HighShelf => 2,
            FilterType::LowPass => 3,
            FilterType::HighPass => 4,
            FilterType::BandPass => 5,
            FilterType::Notch => 6,
        }
    }
}

/// Biquad in Direct-Form-I, RBJ-Cookbook.
#[derive(Clone, Copy, Default)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    pub fn bypass() -> Biquad {
        Biquad {
            b0: 1.0,
            ..Default::default()
        }
    }

    pub fn coeffs(&mut self, kind: FilterType, freq: f32, q: f32, gain_db: f32, sr: f32) {
        let f = clamp(freq, 10.0, sr * 0.49);
        let q = clamp(q, 0.05, 20.0);
        let a = db_to_lin(gain_db / 2.0);
        let w0 = 2.0 * PI * f / sr;
        let (sn, cs) = (w0.sin(), w0.cos());
        let alpha = sn / (2.0 * q);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            FilterType::Peak => (
                1.0 + alpha * a,
                -2.0 * cs,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cs,
                1.0 - alpha / a,
            ),
            FilterType::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cs + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cs),
                    a * ((a + 1.0) - (a - 1.0) * cs - s),
                    (a + 1.0) + (a - 1.0) * cs + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cs),
                    (a + 1.0) + (a - 1.0) * cs - s,
                )
            }
            FilterType::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cs + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cs),
                    a * ((a + 1.0) + (a - 1.0) * cs - s),
                    (a + 1.0) - (a - 1.0) * cs + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cs),
                    (a + 1.0) - (a - 1.0) * cs - s,
                )
            }
            FilterType::LowPass => (
                (1.0 - cs) / 2.0,
                1.0 - cs,
                (1.0 - cs) / 2.0,
                1.0 + alpha,
                -2.0 * cs,
                1.0 - alpha,
            ),
            FilterType::HighPass => (
                (1.0 + cs) / 2.0,
                -(1.0 + cs),
                (1.0 + cs) / 2.0,
                1.0 + alpha,
                -2.0 * cs,
                1.0 - alpha,
            ),
            FilterType::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
            FilterType::Notch => (1.0, -2.0 * cs, 1.0, 1.0 + alpha, -2.0 * cs, 1.0 - alpha),
        };
        let inv = 1.0 / a0;
        self.b0 = b0 * inv;
        self.b1 = b1 * inv;
        self.b2 = b2 * inv;
        self.a1 = a1 * inv;
        self.a2 = a2 * inv;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }
}

/// Kompressor mit weichem Knie, Peak-Detektor.
/// Weicht in der Kennlinie vom Original ab (dort nicht rekonstruiert).
#[derive(Clone, Copy)]
pub struct Compressor {
    pub enabled: bool,
    env: f32,
    att: f32,
    rel: f32,
}

impl Default for Compressor {
    fn default() -> Self {
        Compressor {
            enabled: false,
            env: 0.0,
            att: 0.0,
            rel: 0.0,
        }
    }
}

impl Compressor {
    pub fn set_times(&mut self, attack_ms: f32, release_ms: f32, sr: f32) {
        self.att = (-1.0 / (clamp(attack_ms, 0.1, 500.0) * 0.001 * sr)).exp();
        self.rel = (-1.0 / (clamp(release_ms, 1.0, 5000.0) * 0.001 * sr)).exp();
    }

    pub fn process(
        &mut self,
        x: f32,
        threshold_db: f32,
        ratio: f32,
        knee_db: f32,
        makeup_db: f32,
    ) -> f32 {
        if !self.enabled {
            return x;
        }
        let lvl = x.abs();
        let coef = if lvl > self.env { self.att } else { self.rel };
        self.env = coef * self.env + (1.0 - coef) * lvl;
        let env_db = lin_to_db(self.env);
        let ratio = ratio.max(1.0);
        let over = env_db - threshold_db;
        let gr_db = if over <= -knee_db * 0.5 {
            0.0
        } else if over >= knee_db * 0.5 && knee_db > 0.0 {
            over * (1.0 - 1.0 / ratio)
        } else if knee_db > 0.0 {
            let t = over + knee_db * 0.5;
            (1.0 - 1.0 / ratio) * t * t / (2.0 * knee_db)
        } else {
            over.max(0.0) * (1.0 - 1.0 / ratio)
        };
        x * db_to_lin(makeup_db - gr_db)
    }
}

/// Gate mit Hold. Kennlinie ebenfalls Standard, nicht aus dem Original.
///
/// Schwelle, Hold, Attack und Release kommen aus den Parametern; `tiefe_db`
/// bestimmt, wie weit das Gate zumacht (Boden), damit ein offenes Mikrofon
/// nicht auf digitale Stille fällt, sondern auf einen einstellbaren Rest.
#[derive(Clone, Copy)]
pub struct Gate {
    pub enabled: bool,
    env: f32,
    hold: f32,
    gain: f32,
    att: f32,
    rel: f32,
    hold_s: f32,
    /// Freigabe des **Detektors** (fest 20 ms): die Huellkurve muss dem
    /// Signal schnell folgen, sonst bleibt das Gate nach einer Pause noch
    /// Sekunden offen. Die hoerbare Regelzeit ist `rel` (Parameter).
    env_rel: f32,
    /// Linearer Boden bei geschlossenem Gate (aus `tiefe_db`).
    boden: f32,
}

impl Default for Gate {
    fn default() -> Self {
        Gate {
            enabled: false,
            env: 0.0,
            hold: 0.0,
            gain: 0.0,
            att: 0.0,
            rel: 0.0,
            hold_s: 0.0,
            env_rel: 0.0,
            boden: db_to_lin(-60.0),
        }
    }
}

impl Gate {
    pub fn set_times(&mut self, attack_ms: f32, hold_ms: f32, release_ms: f32, sr: f32) {
        self.att = (-1.0 / (clamp(attack_ms, 0.1, 500.0) * 0.001 * sr)).exp();
        self.rel = (-1.0 / (clamp(release_ms, 1.0, 5000.0) * 0.001 * sr)).exp();
        self.hold_s = clamp(hold_ms, 0.0, 2000.0) * 0.001 * sr;
        self.env_rel = (-1.0 / (0.020 * sr)).exp();
    }

    /// Tiefe in dB (0..80): Daempfung, wenn das Gate zu ist.
    pub fn set_tiefe(&mut self, db: f32) {
        self.boden = db_to_lin(-db.clamp(0.0, 80.0));
    }

    pub fn process(&mut self, x: f32, threshold_db: f32) -> f32 {
        if !self.enabled {
            return x;
        }
        let lvl = x.abs();
        // Einpolige Huellkurve: oeffnet mit Attack, schliesst mit Release.
        // (Frueher stand hier `0.999.max(att)` bei `1.0 - att`, wodurch die
        // Huellkurve den Pegel um Faktor 1/(1-0,999) zu hoch zeigte und die
        // Schwelle nie erreichte.)
        let coef = if lvl > self.env {
            self.att
        } else {
            self.env_rel
        };
        self.env = coef * self.env + (1.0 - coef) * lvl;
        if lin_to_db(self.env) > threshold_db {
            self.hold = self.hold_s;
            self.gain += (1.0 - self.gain) * (1.0 - self.att);
        } else if self.hold > 0.0 {
            self.hold -= 1.0;
        } else {
            self.gain += (self.boden - self.gain) * (1.0 - self.rel);
        }
        x * self.gain
    }
}

/// Denoiser als weicher Downward-Expander (Original-Modul "Audibility" nicht rekonstruiert).
#[derive(Clone, Copy, Default)]
pub struct Denoiser {
    pub enabled: bool,
    env: f32,
}

impl Denoiser {
    pub fn process(&mut self, x: f32, floor_db: f32, threshold_db: f32) -> f32 {
        if !self.enabled {
            return x;
        }
        let lvl = x.abs();
        self.env = 0.9995 * self.env + 0.0005 * lvl;
        let env_db = lin_to_db(self.env);
        if env_db >= threshold_db {
            x
        } else {
            let t = clamp((env_db - (threshold_db - 30.0)) / 30.0, 0.0, 1.0);
            x * db_to_lin(floor_db * (1.0 - t))
        }
    }
}

/// Bus-Limiter: Peak-Limiter mit sofortigem Angriff und einstellbarer Freigabe.
#[derive(Clone, Copy)]
pub struct Limiter {
    pub enabled: bool,
    gain: f32,
    rel: f32,
}

impl Default for Limiter {
    fn default() -> Self {
        Limiter {
            enabled: true,
            gain: 1.0,
            rel: 0.0,
        }
    }
}

impl Limiter {
    pub fn set_release(&mut self, release_ms: f32, sr: f32) {
        self.rel = (-1.0 / (clamp(release_ms, 5.0, 2000.0) * 0.001 * sr)).exp();
    }

    pub fn process(&mut self, x: f32, ceiling_db: f32) -> f32 {
        if !self.enabled {
            return x;
        }
        let ceil = db_to_lin(ceiling_db);
        let want = if x.abs() > ceil { ceil / x.abs() } else { 1.0 };
        if want < self.gain {
            self.gain = want; // sofortiger Angriff
        } else {
            self.gain = self.rel * self.gain + (1.0 - self.rel) * want;
        }
        x * self.gain
    }
}

/// Verzögerungsleitung für Bus-Delay und Kanaltrennung.
pub struct DelayLine {
    buf: Vec<f32>,
    pos: usize,
    len: usize,
}

impl DelayLine {
    pub fn new(max_frames: usize) -> DelayLine {
        DelayLine {
            buf: vec![0.0; max_frames.max(1)],
            pos: 0,
            len: 0,
        }
    }

    pub fn set_ms(&mut self, ms: f32, sr: f32) {
        let n = (clamp(ms, 0.0, 2000.0) * 0.001 * sr) as usize;
        self.len = n.min(self.buf.len() - 1);
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        if self.len == 0 {
            return x;
        }
        let n = self.buf.len();
        self.buf[self.pos] = x;
        let read = (self.pos + n - self.len) % n;
        self.pos = (self.pos + 1) % n;
        self.buf[read]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn biquad_peak_boost_hebt_mitte() {
        let sr = 48000.0;
        let mut b = Biquad::bypass();
        b.coeffs(FilterType::Peak, 1000.0, 1.0, 12.0, sr);
        // 1000 Hz durchlaufen lassen, Amplitude messen
        let mut peak = 0f32;
        for i in 0..48000 {
            let x = (2.0 * PI * 1000.0 * i as f32 / sr).sin();
            let y = b.process(x);
            if i > 4800 {
                peak = peak.max(y.abs());
            }
        }
        let db = lin_to_db(peak);
        assert!(db > 10.0 && db < 14.0, "Peak-Gain war {db} dB");
    }

    #[test]
    fn biquad_bypass_laesst_durch() {
        let mut b = Biquad::bypass();
        for _ in 0..100 {
            assert!((b.process(0.5) - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn kompressor_reduziert_ueber_schwelle() {
        let mut c = Compressor {
            enabled: true,
            ..Default::default()
        };
        c.set_times(5.0, 100.0, 48000.0);
        let mut out = 0.0;
        for _ in 0..48000 {
            out = c.process(0.5, -20.0, 4.0, 0.0, 0.0);
        }
        let db = lin_to_db(out.abs());
        // Eingang -6 dB, Schwelle -20 dB, Ratio 4 -> rund -16,5 dB
        assert!(db < -10.0 && db > -20.0, "Ausgang war {db} dB");
    }

    #[test]
    fn limiter_haelt_decke() {
        let mut l = Limiter::default();
        l.set_release(50.0, 48000.0);
        let mut peak = 0f32;
        for i in 0..4800 {
            let x = 2.0 * (2.0 * PI * 100.0 * i as f32 / 48000.0).sin();
            peak = peak.max(l.process(x, -6.0).abs());
        }
        assert!(lin_to_db(peak) <= -5.5, "Spitze war {} dB", lin_to_db(peak));
    }

    #[test]
    fn delay_verzoegert_um_frames() {
        let mut d = DelayLine::new(1024);
        d.set_ms(1.0, 48000.0); // 48 Frames
        for k in 0..48 {
            assert_eq!(d.process(if k == 0 { 1.0 } else { 0.0 }), 0.0);
        }
        assert!((d.process(0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn gate_laesst_lautes_durch() {
        let mut g = Gate {
            enabled: true,
            ..Default::default()
        };
        g.set_times(1.0, 50.0, 50.0, 48000.0);
        let mut out = 0.0;
        for _ in 0..9600 {
            out = g.process(0.5, -40.0);
        }
        assert!(out > 0.4, "Gate hat zu stark gedämpft: {out}");
    }
}

/// C5-Limiter: Drei-Band-Verarbeitung (Splitter + Kompressor + Peak-Limiter je Band).
///
/// Name und Parameter stammen aus dem Original (`<C5LimBand … compOn attack release
/// knee ratio threshold gainOut limOn limit distorsion>`), die Kennlinien sind
/// Standard-Implementierungen – die Original-Koeffizienten sind nicht rekonstruiert.
#[derive(Clone, Copy)]
pub struct C5Config {
    pub enabled: bool,
    pub crossover_low_hz: f32,
    pub crossover_high_hz: f32,
    pub threshold_db: [f32; 3],
    pub ratio: [f32; 3],
    pub ceiling_db: [f32; 3],
    pub gain_db: [f32; 3],
    pub comp_on: [bool; 3],
    pub lim_on: [bool; 3],
}

impl Default for C5Config {
    fn default() -> Self {
        C5Config {
            enabled: false,
            crossover_low_hz: 200.0,
            crossover_high_hz: 3000.0,
            threshold_db: [-18.0; 3],
            ratio: [2.0; 3],
            ceiling_db: [-0.3; 3],
            gain_db: [0.0; 3],
            comp_on: [true; 3],
            lim_on: [true; 3],
        }
    }
}

/// Zustand des Drei-Band-Limiters (Splitter + je Band Kompressor und Limiter).
pub struct C5Limiter {
    lp_low: Biquad,
    hp_low: Biquad,
    lp_high: Biquad,
    hp_high: Biquad,
    comp: [Compressor; 3],
    lim: [Limiter; 3],
    /// Summen-Limiter nach dem Band-Mix (sonst überschreitet die Summe die Decke).
    out_lim: Limiter,
    sr: f32,
}

impl Default for C5Limiter {
    fn default() -> Self {
        C5Limiter {
            lp_low: Biquad::bypass(),
            hp_low: Biquad::bypass(),
            lp_high: Biquad::bypass(),
            hp_high: Biquad::bypass(),
            comp: [Compressor::default(); 3],
            lim: [Limiter::default(); 3],
            out_lim: Limiter::default(),
            sr: 0.0,
        }
    }
}

impl C5Limiter {
    fn setup(&mut self, cfg: &C5Config, sr: f32) {
        if (self.sr - sr).abs() > 0.5 {
            self.lp_low
                .coeffs(FilterType::LowPass, cfg.crossover_low_hz, 0.707, 0.0, sr);
            self.hp_low
                .coeffs(FilterType::HighPass, cfg.crossover_low_hz, 0.707, 0.0, sr);
            self.lp_high
                .coeffs(FilterType::LowPass, cfg.crossover_high_hz, 0.707, 0.0, sr);
            self.hp_high
                .coeffs(FilterType::HighPass, cfg.crossover_high_hz, 0.707, 0.0, sr);
            for b in 0..3 {
                self.comp[b].set_times(10.0, 120.0, sr);
                self.lim[b].set_release(60.0, sr);
                self.out_lim.set_release(40.0, sr);
            }
            self.sr = sr;
        }
        for b in 0..3 {
            self.comp[b].enabled = cfg.comp_on[b];
            self.lim[b].enabled = cfg.lim_on[b];
        }
    }

    /// Ein Sample durch die Drei-Band-Kette.
    pub fn process(&mut self, x: f32, cfg: &C5Config, sr: f32) -> f32 {
        if !cfg.enabled {
            return x;
        }
        self.setup(cfg, sr);
        let low = self.lp_low.process(x);
        let high = self.hp_high.process(x);
        let mid = self.hp_low.process(x) - self.lp_high.process(x);
        let bands = [low, mid, high];
        let mut sum = 0.0;
        for b in 0..3 {
            let mut v = bands[b] * db_to_lin(cfg.gain_db[b]);
            v = self.comp[b].process(v, cfg.threshold_db[b], cfg.ratio[b], 0.0, 0.0);
            v = self.lim[b].process(v, cfg.ceiling_db[b]);
            sum += v;
        }
        // Summen-Limiter: die Decke gilt für den Gesamtausgang, nicht je Band.
        let decke = cfg.ceiling_db.iter().cloned().fold(f32::INFINITY, f32::min);
        self.out_lim.process(sum, decke)
    }
}

#[cfg(test)]
mod c5_tests {
    use super::*;

    #[test]
    fn aus_laesst_durch() {
        let cfg = C5Config::default(); // enabled = false
        let mut c5 = C5Limiter::default();
        for _ in 0..100 {
            assert!((c5.process(0.3, &cfg, 48000.0) - 0.3).abs() < 1e-6);
        }
    }

    #[test]
    fn begrenzt_auf_decke() {
        let mut cfg = C5Config::default();
        cfg.enabled = true;
        cfg.ceiling_db = [-6.0; 3];
        cfg.comp_on = [false; 3];
        let mut c5 = C5Limiter::default();
        let mut peak = 0f32;
        for i in 0..9600 {
            let x = 2.0 * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / 48000.0).sin();
            peak = peak.max(c5.process(x, &cfg, 48000.0).abs());
        }
        let db = lin_to_db(peak);
        assert!(
            db <= -4.5,
            "Spitze war {db} dB, soll unter der Decke liegen"
        );
    }

    #[test]
    fn leises_signal_bleibt_erhalten() {
        let mut cfg = C5Config::default();
        cfg.enabled = true;
        cfg.ceiling_db = [0.0; 3];
        cfg.comp_on = [false; 3];
        let mut c5 = C5Limiter::default();
        let mut peak = 0f32;
        for i in 0..9600 {
            let x = 0.05 * (2.0 * std::f32::consts::PI * 500.0 * i as f32 / 48000.0).sin();
            peak = peak.max(c5.process(x, &cfg, 48000.0).abs());
        }
        assert!(
            peak > 0.02,
            "leises Signal wurde zu stark abgesenkt: {peak}"
        );
    }
}

/* ==================================================================
LOWCUT – Hochpass 12 dB/Okt (Butterworth, Q = 0,707)
================================================================== */

/// Die vier waehlbaren Grenzfrequenzen des Lowcut.
pub const LOWCUT_HZ: [f32; 4] = [60.0, 80.0, 120.0, 160.0];

/// Naechstgelegene der vier Grenzfrequenzen; `hz <= 0` heisst „aus“.
pub fn lowcut_raster(hz: f32) -> f32 {
    if hz.is_nan() || hz <= 0.0 {
        return 0.0;
    }
    let mut best = LOWCUT_HZ[0];
    for &k in LOWCUT_HZ.iter() {
        if (k - hz).abs() < (best - hz).abs() {
            best = k;
        }
    }
    best
}

/// Hochpass 12 dB/Okt als ein Biquad (Butterworth, Q = 0,707).
///
/// Ein Streifenpaar hat zwei Instanzen (eine je Kanal); der Zustand liegt
/// damit dort, wo das Signal laeuft. Die Koeffizienten entstehen **beim
/// Parameterwechsel** in [`LowCut::set_hz`] – im Sample-Pfad steht nur eine
/// Multiplikation mit dem gespeicherten Zustand, keine Trigonometrie.
#[derive(Clone, Copy)]
pub struct LowCut {
    bq: Biquad,
    hz: f32,
    sr: f32,
}

impl Default for LowCut {
    fn default() -> Self {
        LowCut {
            bq: Biquad::bypass(),
            hz: 0.0,
            sr: 0.0,
        }
    }
}

impl LowCut {
    pub fn new() -> LowCut {
        LowCut::default()
    }

    /// Grenzfrequenz setzen (`0` = aus). Rechnet die Koeffizienten nur, wenn
    /// sich Hz oder Abtastrate wirklich geaendert haben.
    pub fn set_hz(&mut self, hz: f32, sr: f32) {
        let hz = if hz > 0.0 { lowcut_raster(hz) } else { 0.0 };
        if (hz - self.hz).abs() < 0.01 && (sr - self.sr).abs() < 0.5 {
            return;
        }
        self.hz = hz;
        self.sr = sr;
        if hz <= 0.0 {
            self.bq = Biquad::bypass();
        } else {
            // Butterworth 2. Ordnung = ein Biquad mit Q = 0,707.
            self.bq.coeffs(FilterType::HighPass, hz, 0.707, 0.0, sr);
        }
    }

    #[inline]
    pub fn aktiv(&self) -> bool {
        self.hz > 0.0
    }

    pub fn hz(&self) -> f32 {
        self.hz
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        if self.hz <= 0.0 {
            x
        } else {
            self.bq.process(x)
        }
    }

    pub fn reset(&mut self) {
        self.bq.reset();
    }
}

/* ==================================================================
FFT – eigene Radix-2-Implementierung, Tabellen vorab
================================================================== */

/// In-Place-FFT der Groesse `n` (Zweierpotenz) mit vorab gebauten Tabellen.
///
/// Vorwaerts und rueckwaerts ohne jede Allokation: Bit-Umkehr-Tabelle und
/// Drehfaktoren entstehen einmal in [`Fft::new`], danach lebt die Rechnung
/// nur auf den uebergebenen Puffern.
pub struct Fft {
    n: usize,
    rev: Vec<u32>,
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl Fft {
    pub fn new(n: usize) -> Fft {
        assert!(n.is_power_of_two(), "FFT-Groesse muss Zweierpotenz sein");
        let bits = n.trailing_zeros();
        let rev: Vec<u32> = (0..n as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        let mut cos = vec![0.0f32; n / 2];
        let mut sin = vec![0.0f32; n / 2];
        for k in 0..n / 2 {
            let w = 2.0 * PI * k as f32 / n as f32;
            cos[k] = w.cos();
            sin[k] = w.sin();
        }
        Fft { n, rev, cos, sin }
    }

    pub fn n(&self) -> usize {
        self.n
    }

    /// Vorwaertstransformation, in place. `re`/`im` muessen `n` lang sein.
    pub fn forward(&self, re: &mut [f32], im: &mut [f32]) {
        let n = self.n;
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            let mut start = 0;
            while start < n {
                for j in 0..half {
                    let k = j * step;
                    let (wr, wi) = (self.cos[k], -self.sin[k]);
                    let a = start + j;
                    let b = a + half;
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
                start += len;
            }
            len *= 2;
        }
    }

    /// Rueckwaertstransformation mit Faktor `1/n`, in place.
    pub fn inverse(&self, re: &mut [f32], im: &mut [f32]) {
        for v in im.iter_mut() {
            *v = -*v;
        }
        self.forward(re, im);
        let s = 1.0 / self.n as f32;
        for i in 0..self.n {
            re[i] *= s;
            im[i] = -im[i] * s;
        }
    }
}

/* ==================================================================
NOISE REDUCTION – eigener Rauschunterdruecker, zwei Stufen
================================================================== */

/// FFT-Groesse des Rauschunterdrueckers (512 Samples = 10,7 ms bei 48 kHz).
pub const NR_FFT: usize = 512;
/// Sprungweite: halbe Fensterlaenge, damit sich die Fenster ueberlappen.
pub const NR_HOP: usize = NR_FFT / 2;
/// Ausgewertete Baender (0 .. Nyquist, ohne Nyquist selbst).
const NR_BINS: usize = NR_FFT / 2;
/// Frames je Minimumsblock (24 x 5,33 ms ≈ 128 ms).
const NR_BLOCK_FRAMES: usize = 24;
/// Bloecke im Ring – 8 x 128 ms ≈ 1,02 s Rauschschaetzung.
const NR_BLOCK_RING: usize = 8;
/// Halbe Breite des Medianfensters fuer die Deckelung der Rauschschaetzung.
///
/// Das Fenster muss deutlich breiter sein als die Hauptkeule des Fensters
/// (bei Hann vier bis fuenf Baender): nur dann ist ein Dauerton eine
/// Minderheit im Fenster und der Median bleibt beim echten Rauschboden.
const NR_NACHBAR: usize = 12;

/// Median eines kleinen Fensters – ohne Allokation, per Einfuegesortieren.
fn median(v: &[f32]) -> f32 {
    let mut b = [0.0f32; 2 * NR_NACHBAR + 1];
    let n = v.len().min(b.len());
    b[..n].copy_from_slice(&v[..n]);
    for i in 1..n {
        let x = b[i];
        let mut j = i;
        while j > 0 && b[j - 1] > x {
            b[j] = b[j - 1];
            j -= 1;
        }
        b[j] = x;
    }
    b[n / 2]
}

/// Rauschunterdrueckung je Kanal: Gate/Expander **und** spektrale Subtraktion.
///
/// **Stufe a – Gate/Expander:** Huellkurve aus dem Betrag, unterhalb der
/// Schwelle faehrt der Pegel weich zu (`1 - Staerke * (1 - env/schwelle)`),
/// mit Attack, Release und Haltezeit. Kein `powf` je Sample: die Kennlinie
/// ist linear, die Tiefe kommt aus der Staerke.
///
/// **Stufe b – spektrale Subtraktion:** 512-Punkt-FFT, halbe Ueberlappung,
/// Wurzel-Hann-Fenster (Analyse und Synthese), damit sich das Signal bei
/// Verstaerkung 1 exakt rekonstruiert. Rauschschaetzung ist das gleitende
/// Betragsminimum ueber ~1 s je Band (Blockminimum, Ring aus acht Bloecken),
/// anschliessend ein Nachbarminimum ueber ±4 Baender: ein schmaler Dauerton
/// zieht den Rauschboden damit nicht nach oben. Die Subtraktion passiert im
/// Leistungsbereich (`g = 1 - alpha*n²/p`, unten durch einen Boden begrenzt),
/// was hohe Pegel fast unberuehrt laesst. Gegen „musikalisches Rauschen“
/// wirken drei Dinge: die Glaettung der Verstaerkung ueber die Zeit
/// (schnell zu, langsam auf), eine Glaettung ueber die Frequenz, die **nur
/// Kerben anhebt** (keine Spitzen absenkt), und der Spektralboden.
///
/// Alles liegt in festen Arrays: `process` allokiert nichts. Die Latenz der
/// Stufe betraegt ein Fenster (512 Samples ≈ 10,7 ms).
pub struct NoiseReduction {
    pub enabled: bool,
    /// Stufe a (Expander) zuschalten. Vorgabe: aus – fuer Sprachpausen gibt es
    /// den eigenen GATE-Knopf, damit sich beide Schalter nicht doppelt
    /// auswirken.
    pub gate_on: bool,
    fft: Fft,
    window: [f32; NR_FFT],
    /// Letzte `NR_FFT` Eingangssamples (Ring).
    in_buf: [f32; NR_FFT],
    wpos: usize,
    filled: usize,
    /// Ueberlappungs-Summe und Ausgabewarteschlange.
    ola: [f32; NR_FFT],
    out_q: [f32; NR_HOP],
    q_pos: usize,
    re: [f32; NR_FFT],
    im: [f32; NR_FFT],
    mag: [f32; NR_BINS],
    block_min: [f32; NR_BINS],
    min_ring: [[f32; NR_BINS]; NR_BLOCK_RING],
    ring_pos: usize,
    frames_in_block: usize,
    bloecke: usize,
    noise: [f32; NR_BINS],
    gain: [f32; NR_BINS],
    gain_glatt: [f32; NR_BINS],
    glatt_tmp: [f32; NR_BINS],
    // Gate
    env: f32,
    gate_gain: f32,
    hold: f32,
    att1: f32,
    rel1: f32,
    hold_s: f32,
    inv_thr: f32,
    schwelle_db: f32,
    // abgeleitete Groessen der Staerke
    staerke: f32,
    alpha: f32,
    floor: f32,
    gate_tiefe: f32,
    gate_boden: f32,
    // Glaettung der spektralen Verstaerkung je Frame
    g_att: f32,
    g_rel: f32,
    sr: f32,
    frisch: bool,
}

impl Default for NoiseReduction {
    fn default() -> Self {
        NoiseReduction::new(48000.0)
    }
}

impl NoiseReduction {
    pub fn new(sr: f32) -> NoiseReduction {
        let mut window = [0.0f32; NR_FFT];
        for (i, w) in window.iter_mut().enumerate() {
            let hann = 0.5 - 0.5 * (2.0 * PI * i as f32 / NR_FFT as f32).cos();
            // Wurzel-Hann: Analyse mal Synthese ergibt Hann, und Hann
            // summiert sich bei halber Ueberlappung exakt auf 1.
            *w = hann.max(1e-6).sqrt();
        }
        let mut n = NoiseReduction {
            enabled: false,
            gate_on: false,
            fft: Fft::new(NR_FFT),
            window,
            in_buf: [0.0; NR_FFT],
            wpos: 0,
            filled: 0,
            ola: [0.0; NR_FFT],
            out_q: [0.0; NR_HOP],
            q_pos: NR_HOP,
            re: [0.0; NR_FFT],
            im: [0.0; NR_FFT],
            mag: [0.0; NR_BINS],
            block_min: [f32::INFINITY; NR_BINS],
            min_ring: [[f32::INFINITY; NR_BINS]; NR_BLOCK_RING],
            ring_pos: 0,
            frames_in_block: 0,
            bloecke: 0,
            noise: [0.0; NR_BINS],
            gain: [1.0; NR_BINS],
            gain_glatt: [1.0; NR_BINS],
            glatt_tmp: [1.0; NR_BINS],
            env: 0.0,
            gate_gain: 1.0,
            hold: 0.0,
            att1: 0.0,
            rel1: 0.0,
            hold_s: 0.0,
            inv_thr: 1.0,
            schwelle_db: 0.0,
            staerke: -1.0,
            alpha: 1.0,
            floor: 0.0,
            gate_tiefe: 0.0,
            gate_boden: 1.0,
            g_att: 0.5,
            g_rel: 0.12,
            sr: 0.0,
            frisch: true,
        };
        n.set_sample_rate(sr);
        n.set_schwelle(-45.0);
        n
    }

    fn set_sample_rate(&mut self, sr: f32) {
        let sr = sr.max(8000.0);
        if (sr - self.sr).abs() < 0.5 {
            return;
        }
        self.sr = sr;
        // Regelzeiten des Expanders: 3 ms auf, 140 ms zu, 60 ms halten.
        self.att1 = 1.0 - (-1.0 / (0.003 * sr)).exp();
        self.rel1 = 1.0 - (-1.0 / (0.140 * sr)).exp();
        self.hold_s = 0.060 * sr;
    }

    /// Schwelle des Expanders in dBFS (-60 .. -20).
    fn set_schwelle(&mut self, db: f32) {
        let db = db.clamp(-60.0, -20.0);
        if (db - self.schwelle_db).abs() < 0.01 {
            return;
        }
        self.schwelle_db = db;
        self.inv_thr = 1.0 / db_to_lin(db);
    }

    /// Aus der Staerke abgeleitete Groessen (Ueberschuss, Boden, Tiefe).
    fn set_staerke(&mut self, staerke: f32) {
        let staerke = if staerke.is_nan() {
            0.0
        } else {
            staerke.clamp(0.0, 1.0)
        };
        if (staerke - self.staerke).abs() < 1e-4 {
            return;
        }
        self.staerke = staerke;
        // Ueberschussfaktor 1 .. 3 im Leistungsbereich.
        self.alpha = 1.0 + 2.0 * staerke;
        // Spektralboden -14 .. -30 dB: begrenzt, wie tief eine Kerbe faellt.
        self.floor = db_to_lin(-(14.0 + 16.0 * staerke));
        // Gate: Tiefe und Boden wachsen mit der Staerke.
        self.gate_tiefe = staerke;
        self.gate_boden = db_to_lin(-(4.0 + 26.0 * staerke));
    }

    /// Alles zurueck auf Anfang (auch beim Ausschalten).
    pub fn reset(&mut self) {
        self.in_buf = [0.0; NR_FFT];
        self.ola = [0.0; NR_FFT];
        self.out_q = [0.0; NR_HOP];
        self.wpos = 0;
        self.filled = 0;
        self.q_pos = NR_HOP;
        self.re = [0.0; NR_FFT];
        self.im = [0.0; NR_FFT];
        self.mag = [0.0; NR_BINS];
        self.block_min = [f32::INFINITY; NR_BINS];
        self.min_ring = [[f32::INFINITY; NR_BINS]; NR_BLOCK_RING];
        self.ring_pos = 0;
        self.frames_in_block = 0;
        self.bloecke = 0;
        self.noise = [0.0; NR_BINS];
        self.gain = [1.0; NR_BINS];
        self.gain_glatt = [1.0; NR_BINS];
        self.glatt_tmp = [1.0; NR_BINS];
        self.env = 0.0;
        self.gate_gain = 1.0;
        self.hold = 0.0;
        self.frisch = true;
    }

    /// Huellkurve + Expander-Kennlinie (Stufe a).
    #[inline]
    fn expander(&mut self, x: f32, schnelle: f32) -> f32 {
        let lvl = x.abs();
        let coef = if lvl > self.env { schnelle } else { self.rel1 };
        self.env = coef * self.env + (1.0 - coef) * lvl;
        let k = (self.env * self.inv_thr).min(1.0);
        let mut ziel = 1.0 - self.gate_tiefe * (1.0 - k);
        if k >= 1.0 {
            self.hold = self.hold_s;
        } else if self.hold > 0.0 {
            self.hold -= 1.0;
            ziel = 1.0;
        }
        if ziel < self.gate_boden {
            ziel = self.gate_boden;
        }
        let c = if ziel > self.gate_gain {
            self.att1
        } else {
            self.rel1
        };
        self.gate_gain += (ziel - self.gate_gain) * c;
        x * self.gate_gain
    }

    /// Ein Frame: Fenster, FFT, Rauschschaetzung, Subtraktion, IFFT, OLA.
    fn rahmen(&mut self) {
        let n = NR_FFT;
        for i in 0..n {
            let mut idx = self.wpos + i;
            if idx >= n {
                idx -= n;
            }
            self.re[i] = self.in_buf[idx] * self.window[i];
            self.im[i] = 0.0;
        }
        self.fft.forward(&mut self.re, &mut self.im);
        for i in 0..NR_BINS {
            let (r, im) = (self.re[i], self.im[i]);
            self.mag[i] = (r * r + im * im).sqrt();
        }

        // Rauschschaetzung: gleitendes Minimum ueber ~1 s je Band.
        //
        // Der Block nimmt das Minimum der **Leistung** ueber 24 Frames
        // (~128 ms). Ein Minimum von 24 exponentialverteilten Werten liegt im
        // Mittel bei 1/24 des Mittelwerts; der Faktor macht daraus wieder eine
        // Erwartungstreue Schaetzung des Rauschpegels. Der Ring aus acht
        // Bloecken mittelt acht solche Minima – deren Streuung ist damit
        // bereits klein (relativ ~1,5 %).
        for i in 0..NR_BINS {
            let p = self.mag[i] * self.mag[i];
            if p < self.block_min[i] {
                self.block_min[i] = p;
            }
        }
        self.frames_in_block += 1;
        if self.frames_in_block >= NR_BLOCK_FRAMES {
            self.frames_in_block = 0;
            self.ring_pos += 1;
            if self.ring_pos == NR_BLOCK_RING {
                self.ring_pos = 0;
            }
            self.min_ring[self.ring_pos] = self.block_min;
            self.block_min = [f32::INFINITY; NR_BINS];
            if self.bloecke < NR_BLOCK_RING {
                self.bloecke += 1;
            }
            let durch = self.bloecke as f32;
            for i in 0..NR_BINS {
                let mut summe = 0.0f32;
                for b in 0..self.bloecke {
                    summe += self.min_ring[b][i];
                }
                self.noise[i] = summe / durch * NR_BLOCK_FRAMES as f32;
            }
            // Gegen schmale Dauertoene: der Rauschboden ist spektral glatt.
            // Wo ein einzelnes Band weit ueber dem Median seiner Nachbarn
            // liegt, wird die Schaetzung auf diesen Median begrenzt – sonst
            // haelt die Minimumssuche nach einer Sekunde den Ton selbst fuer
            // Rauschen und schneidet ihn weg.
            let roh = self.noise;
            for i in 0..NR_BINS {
                let lo = i.saturating_sub(NR_NACHBAR);
                let hi = (i + NR_NACHBAR).min(NR_BINS - 1);
                let med = median(&roh[lo..=hi]);
                let deckel = med + 1e-12;
                if self.noise[i] > deckel {
                    self.noise[i] = deckel;
                }
            }
        }

        // Verstaerkung je Band: Leistungssubtraktion mit Boden.
        if self.bloecke == 0 {
            for g in self.gain.iter_mut() {
                *g = 1.0;
            }
        } else {
            for i in 0..NR_BINS {
                let p = self.mag[i] * self.mag[i];
                let n2 = self.noise[i];
                let mut g = if p <= self.alpha * n2 {
                    self.floor
                } else {
                    (1.0 - self.alpha * n2 / p).max(self.floor)
                };
                // Baender weit ueber dem Rauschboden (rund 10 dB) bleiben
                // erhalten – und zwar mit ihrer ganzen Hauptkeule: die
                // Nachbarn bis ±2 Baender zaehlen mit, weil ein Fenster die
                // Energie eines Tones ueber vier bis fuenf Baender verteilt.
                // Ohne das kostete die Subtraktion einem Dauerton Pegel.
                let mut spitze = p;
                let lo = i.saturating_sub(2);
                let hi = (i + 2).min(NR_BINS - 1);
                for j in lo..=hi {
                    let q = self.mag[j] * self.mag[j];
                    if q > spitze {
                        spitze = q;
                    }
                }
                if spitze > 10.0 * n2 && g < 0.97 {
                    g = 0.97;
                }
                self.gain[i] = if g > 1.0 { 1.0 } else { g };
            }
        }

        // Frequenzglaettung, die nur Kerben anhebt: eine einzelne tiefe
        // Kerbe neben einem lauten Band wird halb angehoben, Spitzen
        // bleiben unangetastet (sonst kostete die Glaettung Sinuspegel).
        self.glatt_tmp[0] = self.gain[0];
        for i in 1..NR_BINS - 1 {
            let nb = 0.5 * (self.gain[i - 1] + self.gain[i + 1]);
            let g = self.gain[i];
            self.glatt_tmp[i] = if g < nb { g + 0.5 * (nb - g) } else { g };
        }
        self.glatt_tmp[NR_BINS - 1] = self.gain[NR_BINS - 1];

        // Zeitglaettung: schnell zu, langsam auf.
        for i in 0..NR_BINS {
            let ziel = self.glatt_tmp[i];
            let k = if ziel < self.gain_glatt[i] {
                self.g_att
            } else {
                self.g_rel
            };
            self.gain_glatt[i] += (ziel - self.gain_glatt[i]) * k;
        }

        // Anwenden (reelles Spektrum bleibt hermitesch).
        let g0 = self.gain_glatt[0];
        self.re[0] *= g0;
        self.im[0] *= g0;
        for i in 1..NR_BINS {
            let g = self.gain_glatt[i];
            self.re[i] *= g;
            self.im[i] *= g;
            let j = NR_FFT - i;
            self.re[j] *= g;
            self.im[j] *= g;
        }
        let gn = self.gain_glatt[NR_BINS - 1];
        self.re[NR_BINS] *= gn;
        self.im[NR_BINS] *= gn;

        self.fft.inverse(&mut self.re, &mut self.im);

        // Ueberlappung addieren, erste Hop Samples ausgeben, dann schieben.
        for i in 0..NR_FFT {
            self.ola[i] += self.re[i] * self.window[i];
        }
        self.out_q.copy_from_slice(&self.ola[..NR_HOP]);
        for i in 0..NR_HOP {
            self.ola[i] = self.ola[i + NR_HOP];
            self.ola[i + NR_HOP] = 0.0;
        }
        self.q_pos = 0;
    }

    /// Ein Sample. `staerke` 0..1 (0 = aus), `schwelle_db` -60..-20 dBFS.
    #[inline]
    pub fn process(&mut self, x: f32, staerke: f32, schwelle_db: f32, sr: f32) -> f32 {
        if !self.enabled || staerke.is_nan() || staerke <= 0.0 {
            if !self.frisch {
                self.reset();
            }
            return x;
        }
        self.frisch = false;
        self.set_sample_rate(sr);
        self.set_schwelle(schwelle_db);
        self.set_staerke(staerke);
        let g = if self.gate_on {
            self.expander(x, self.att1)
        } else {
            x
        };
        self.in_buf[self.wpos] = g;
        self.wpos += 1;
        if self.wpos == NR_FFT {
            self.wpos = 0;
        }
        self.filled += 1;
        if self.filled == NR_HOP {
            self.filled = 0;
            self.rahmen();
        }
        if self.q_pos < NR_HOP {
            let v = self.out_q[self.q_pos];
            self.q_pos += 1;
            v
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod nr_tests {
    use super::*;

    fn sinus(f: f32, amp: f32, n: usize, sr: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * PI * f * i as f32 / sr).sin())
            .collect()
    }

    /// Deterministisches weisses Rauschen (LCG), damit Messungen reproduzierbar sind.
    struct Rausch {
        s: u32,
    }

    impl Rausch {
        fn neu(seed: u32) -> Rausch {
            Rausch { s: seed | 1 }
        }
        fn next(&mut self) -> f32 {
            // Zwei gleichverteilte Werte -> grob gaussförmig, |x| < 1.
            let mut sum = 0.0;
            for _ in 0..3 {
                self.s = self.s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                sum += (self.s >> 8) as f32 / 8_388_608.0 - 1.0;
            }
            sum * 0.577
        }
    }

    /// Effektivwert ohne Gleichanteil.
    fn rms(v: &[f32]) -> f32 {
        if v.is_empty() {
            return 0.0;
        }
        let m = v.iter().sum::<f32>() / v.len() as f32;
        (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32).sqrt()
    }

    /// Amplitude einer Einzeltone mittels Projektion auf sin/cos bei `f`.
    fn ton_amplitude(v: &[f32], f: f32, sr: f32) -> f32 {
        let n = v.len() as f32;
        let (mut s, mut c) = (0.0f64, 0.0f64);
        for (i, x) in v.iter().enumerate() {
            let w = 2.0 * std::f64::consts::PI * f as f64 * i as f64 / sr as f64;
            s += *x as f64 * w.sin();
            c += *x as f64 * w.cos();
        }
        // Rechteckfenster: Amplitude = 2/N * |Summe|, mit cos-Anteil.
        ((2.0 / n as f64) * (s * s + c * c).sqrt()) as f32
    }

    #[test]
    fn fft_rundlauf_ist_exakt() {
        let f = Fft::new(NR_FFT);
        let mut re: Vec<f32> = (0..NR_FFT)
            .map(|i| (i as f32 * 0.37).sin() + 0.3 * (i as f32 * 0.011).cos())
            .collect();
        let orig = re.clone();
        let mut im = vec![0.0f32; NR_FFT];
        f.forward(&mut re, &mut im);
        f.inverse(&mut re, &mut im);
        let mut maxdiff = 0.0f32;
        for i in 0..NR_FFT {
            maxdiff = maxdiff.max((re[i] - orig[i]).abs());
        }
        assert!(maxdiff < 1e-4, "Rundlauf wich um {maxdiff} ab");
    }

    #[test]
    fn fft_legt_sinus_in_das_richtige_band() {
        let f = Fft::new(NR_FFT);
        let mut re = sinus(1000.0, 1.0, NR_FFT, 48000.0);
        let mut im = vec![0.0f32; NR_FFT];
        f.forward(&mut re, &mut im);
        // 1000 Hz bei 48 kHz und 512 Punkten -> Band 10,67, also 10 oder 11.
        let mut best = (0usize, 0.0f32);
        for k in 0..NR_BINS {
            let m = (re[k] * re[k] + im[k] * im[k]).sqrt();
            if m > best.1 {
                best = (k, m);
            }
        }
        assert!(
            best.0 == 10 || best.0 == 11,
            "Spitze lag in Band {}",
            best.0
        );
    }

    #[test]
    fn fenster_summiert_sich_bei_halber_ueberlappung_auf_eins() {
        let mut w = [0.0f32; NR_FFT];
        for (i, x) in w.iter_mut().enumerate() {
            let h = 0.5 - 0.5 * (2.0 * PI * i as f32 / NR_FFT as f32).cos();
            *x = h.max(1e-6).sqrt();
        }
        for n in 0..NR_HOP {
            let s = w[n] * w[n] + w[n + NR_HOP] * w[n + NR_HOP];
            assert!((s - 1.0).abs() < 1e-4, "Summe bei {n} war {s}");
        }
    }

    #[test]
    fn lowcut_aus_ist_durchgang() {
        let mut lc = LowCut::new();
        lc.set_hz(0.0, 48000.0);
        for i in 0..1000 {
            let x = (i as f32 * 0.1).sin();
            assert_eq!(lc.process(x), x);
        }
    }

    #[test]
    fn lowcut_raster_kennt_vier_frequenzen() {
        assert_eq!(lowcut_raster(0.0), 0.0);
        assert_eq!(lowcut_raster(-5.0), 0.0);
        assert_eq!(lowcut_raster(59.0), 60.0);
        assert_eq!(lowcut_raster(75.0), 80.0);
        assert_eq!(lowcut_raster(110.0), 120.0);
        assert_eq!(lowcut_raster(155.0), 160.0);
        assert_eq!(lowcut_raster(400.0), 160.0);
    }

    /// Pegel bei 40 Hz gegen 200 Hz – beides durch den Hochpass.
    fn lowcut_pegel(hz: f32) -> (f32, f32) {
        let sr = 48000.0;
        let n = 48000;
        let mut lc = LowCut::new();
        lc.set_hz(hz, sr);
        let mut tief = 0f32;
        let mut hoch = 0f32;
        for i in 0..n {
            let x = 0.5 * (2.0 * PI * 40.0 * i as f32 / sr).sin();
            let y = lc.process(x);
            if i > n / 2 {
                tief = tief.max(y.abs());
            }
        }
        let mut lc = LowCut::new();
        lc.set_hz(hz, sr);
        for i in 0..n {
            let x = 0.5 * (2.0 * PI * 200.0 * i as f32 / sr).sin();
            let y = lc.process(x);
            if i > n / 2 {
                hoch = hoch.max(y.abs());
            }
        }
        (lin_to_db(tief), lin_to_db(hoch))
    }

    #[test]
    fn lowcut_daempft_40_hz_gegen_200_hz() {
        for hz in [60.0f32, 120.0] {
            let (tief, hoch) = lowcut_pegel(hz);
            println!(
                "LOWCUT {hz:>3} Hz: 40 Hz = {tief:7.2} dBFS, 200 Hz = {hoch:7.2} dBFS, Unterschied {:6.2} dB",
                tief - hoch
            );
            // 12 dB/Okt: bei 60 Hz liegen 40 Hz rund 8 dB tiefer, bei 120 Hz
            // rund 19 dB (theoretisch 7,8 bzw. 19,1 dB).
            assert!(
                tief - hoch < -7.0,
                "Lowcut {hz} Hz daempfte 40 Hz nur um {} dB",
                tief - hoch
            );
            // 200 Hz muss praktisch unbehelligt bleiben.
            assert!(hoch > -6.8, "200 Hz wurde zu stark gedaempft: {hoch} dB");
        }
    }

    /// Sinus 1 kHz (-20 dBFS) + weisses Rauschen (-20 dBFS) durch die NR.
    fn nr_lauf(staerke: f32, schwelle: f32) -> (f32, f32, f32, f32, f32) {
        let sr = 48000.0;
        let n = 96_000; // 2 s: 1 s Rauschschaetzung, 1 s Messung
        let ton = 0.1; // -20 dBFS Spitze
        let rausch = 0.1; // -20 dBFS Effektivwert
        let s = sinus(1000.0, ton, n, sr);
        let mut r = Rausch::neu(0x1234_5678);
        let mut noise = Vec::with_capacity(n);
        for _ in 0..n {
            noise.push(r.next() * rausch * 1.732); // auf RMS = rausch normiert
        }
        let ein: Vec<f32> = s.iter().zip(noise.iter()).map(|(a, b)| a + b).collect();
        let mut nr = NoiseReduction::new(sr);
        nr.enabled = true;
        let aus: Vec<f32> = ein
            .iter()
            .map(|x| nr.process(*x, staerke, schwelle, sr))
            .collect();
        let ab = n / 2; // hinteres Fenster: Schaetzung steht
        let ein_ton = ton_amplitude(&ein[ab..], 1000.0, sr);
        let aus_ton = ton_amplitude(&aus[ab..], 1000.0, sr);
        let ein_rms = rms(&ein[ab..]);
        let aus_rms = rms(&aus[ab..]);
        // Rauschanteil = Gesamtleistung minus Tonleistung
        let ein_n = (ein_rms * ein_rms - (ein_ton * ein_ton) / 2.0)
            .max(0.0)
            .sqrt();
        let aus_n = (aus_rms * aus_rms - (aus_ton * aus_ton) / 2.0)
            .max(0.0)
            .sqrt();
        (
            lin_to_db(ein_n),
            lin_to_db(aus_n),
            lin_to_db(ein_ton),
            lin_to_db(aus_ton),
            staerke,
        )
    }

    #[test]
    fn nr_senkt_rauschen_und_haelt_den_sinus() {
        for staerke in [0.5f32, 1.0] {
            let (vn, nn, vt, nt, _) = nr_lauf(staerke, -45.0);
            println!(
                "NR {staerke:.1}: Rauschen {vn:7.2} -> {nn:7.2} dBFS ({:6.2} dB), Sinus {vt:7.2} -> {nt:7.2} dBFS ({:+.2} dB)",
                nn - vn,
                nt - vt
            );
            assert!(
                nn - vn < -11.0,
                "Rauschen sank bei Staerke {staerke} nur um {} dB",
                nn - vn
            );
            assert!(
                (nt - vt).abs() <= 1.0,
                "Sinus verlor bei Staerke {staerke} {} dB",
                nt - vt
            );
        }
    }

    #[test]
    fn nr_aus_ist_bitgenau_durchgang() {
        let mut nr = NoiseReduction::new(48000.0);
        nr.enabled = true;
        let mut r = Rausch::neu(7);
        for _ in 0..5000 {
            let x = r.next() * 0.2;
            assert_eq!(nr.process(x, 0.0, -45.0, 48000.0), x);
        }
    }

    #[test]
    fn nr_laesst_stille_still() {
        let mut nr = NoiseReduction::new(48000.0);
        nr.enabled = true;
        for i in 0..20000 {
            let y = nr.process(0.0, 1.0, -45.0, 48000.0);
            assert!(y.abs() < 1e-6, "bei {i} kam {y} heraus");
        }
    }

    #[test]
    fn nr_expander_faehrt_unter_der_schwelle_zu() {
        let sr = 48000.0;
        let mut nr = NoiseReduction::new(sr);
        nr.enabled = true;
        // leises Rauschen bei -70 dBFS, Schwelle -40 dBFS
        let mut r = Rausch::neu(99);
        let mut sum = 0.0;
        let n = 24000;
        for _ in 0..n {
            let x = r.next() * 0.000316 * 1.732;
            let y = nr.process(x, 1.0, -40.0, sr);
            sum += y * y;
        }
        let db = lin_to_db((sum / n as f32).sqrt());
        println!("EXPANDER: -70 dBFS Rauschen bei Schwelle -40 dBFS -> {db:.2} dBFS");
        assert!(db < -75.0, "Expander hat nicht zugemacht: {db} dBFS");
    }
}
