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
    if v <= 1e-7 { -140.0 } else { 20.0 * v.log10() }
}

#[inline]
fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if v < lo { lo } else if v > hi { hi } else { v }
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
        Biquad { b0: 1.0, ..Default::default() }
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
        Compressor { enabled: false, env: 0.0, att: 0.0, rel: 0.0 }
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

/// Gate/Expander mit Hold. Kennlinie ebenfalls Standard, nicht aus dem Original.
#[derive(Clone, Copy)]
pub struct Gate {
    pub enabled: bool,
    env: f32,
    hold: f32,
    gain: f32,
    att: f32,
    rel: f32,
    hold_s: f32,
}

impl Default for Gate {
    fn default() -> Self {
        Gate { enabled: false, env: 0.0, hold: 0.0, gain: 0.0, att: 0.0, rel: 0.0, hold_s: 0.0 }
    }
}

impl Gate {
    pub fn set_times(&mut self, attack_ms: f32, hold_ms: f32, release_ms: f32, sr: f32) {
        self.att = (-1.0 / (clamp(attack_ms, 0.1, 500.0) * 0.001 * sr)).exp();
        self.rel = (-1.0 / (clamp(release_ms, 1.0, 5000.0) * 0.001 * sr)).exp();
        self.hold_s = clamp(hold_ms, 0.0, 2000.0) * 0.001 * sr;
    }

    pub fn process(&mut self, x: f32, threshold_db: f32) -> f32 {
        if !self.enabled {
            return x;
        }
        let lvl = x.abs();
        self.env = 0.999f32.max(self.att) * self.env + (1.0 - self.att) * lvl;
        if lin_to_db(self.env) > threshold_db {
            self.hold = self.hold_s;
            self.gain += (1.0 - self.gain) * (1.0 - self.att);
        } else if self.hold > 0.0 {
            self.hold -= 1.0;
        } else {
            self.gain += (0.0 - self.gain) * (1.0 - self.rel);
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
        Limiter { enabled: true, gain: 1.0, rel: 0.0 }
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
        DelayLine { buf: vec![0.0; max_frames.max(1)], pos: 0, len: 0 }
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
            if i > 4800 { peak = peak.max(y.abs()); }
        }
        let db = lin_to_db(peak);
        assert!(db > 10.0 && db < 14.0, "Peak-Gain war {db} dB");
    }

    #[test]
    fn biquad_bypass_laesst_durch() {
        let mut b = Biquad::bypass();
        for _ in 0..100 { assert!((b.process(0.5) - 0.5).abs() < 1e-6); }
    }

    #[test]
    fn kompressor_reduziert_ueber_schwelle() {
        let mut c = Compressor { enabled: true, ..Default::default() };
        c.set_times(5.0, 100.0, 48000.0);
        let mut out = 0.0;
        for _ in 0..48000 { out = c.process(0.5, -20.0, 4.0, 0.0, 0.0); }
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
        for k in 0..48 { assert_eq!(d.process(if k == 0 { 1.0 } else { 0.0 }), 0.0); }
        assert!((d.process(0.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn gate_laesst_lautes_durch() {
        let mut g = Gate { enabled: true, ..Default::default() };
        g.set_times(1.0, 50.0, 50.0, 48000.0);
        let mut out = 0.0;
        for _ in 0..9600 { out = g.process(0.5, -40.0); }
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
            sr: 0.0,
        }
    }
}

impl C5Limiter {
    fn setup(&mut self, cfg: &C5Config, sr: f32) {
        if (self.sr - sr).abs() > 0.5 {
            self.lp_low.coeffs(FilterType::LowPass, cfg.crossover_low_hz, 0.707, 0.0, sr);
            self.hp_low.coeffs(FilterType::HighPass, cfg.crossover_low_hz, 0.707, 0.0, sr);
            self.lp_high.coeffs(FilterType::LowPass, cfg.crossover_high_hz, 0.707, 0.0, sr);
            self.hp_high.coeffs(FilterType::HighPass, cfg.crossover_high_hz, 0.707, 0.0, sr);
            for b in 0..3 {
                self.comp[b].set_times(10.0, 120.0, sr);
                self.lim[b].set_release(60.0, sr);
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
        sum
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
        assert!(db <= -4.5, "Spitze war {db} dB, soll unter der Decke liegen");
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
        assert!(peak > 0.02, "leises Signal wurde zu stark abgesenkt: {peak}");
    }
}
