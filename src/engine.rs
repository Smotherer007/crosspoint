//! Echtzeit-Kern: Parameter als Atome, DSP-Zustand ausschließlich im Audio-Thread.
//!
//! Signalkette je Streifen (Reihenfolge wie im Original dokumentiert):
//!   Eingang -> 6-Band-EQ -> Compressor -> Gate -> Denoiser -> Gain -> Pan
//!   -> Routing-Matrix (8 Busse)
//! Je Bus: Summe -> Limiter -> Delay pro Kanal -> Trim -> Gain/Mute -> Ausgang.
//!
//! Es gibt **keinen** Mutex im Audio-Pfad: die Steuerung schreibt Atome, der
//! Callback liest sie mit `Relaxed`.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};

use crate::dsp::*;
use crate::json::Value;
use crate::{BUS_NAMES, BUSES, EQ_CELLS, STRIP_NAMES, STRIPS};

#[inline]
fn fget(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Relaxed))
}
#[inline]
fn fset(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Relaxed)
}

pub struct EqParam {
    pub on: AtomicBool,
    pub kind: AtomicU32,
    pub freq: AtomicU32,
    pub q: AtomicU32,
    pub gain: AtomicU32,
}

impl EqParam {
    fn new() -> EqParam {
        EqParam {
            on: AtomicBool::new(false),
            kind: AtomicU32::new(0),
            freq: AtomicU32::new(1000.0f32.to_bits()),
            q: AtomicU32::new(0.7f32.to_bits()),
            gain: AtomicU32::new(0.0f32.to_bits()),
        }
    }
}

pub struct CompParam {
    pub on: AtomicBool,
    pub threshold: AtomicU32,
    pub ratio: AtomicU32,
    pub attack: AtomicU32,
    pub release: AtomicU32,
    pub knee: AtomicU32,
    pub gain_in: AtomicU32,
    pub gain_out: AtomicU32,
}

impl CompParam {
    fn new() -> CompParam {
        CompParam {
            on: AtomicBool::new(false),
            threshold: AtomicU32::new((-18.0f32).to_bits()),
            ratio: AtomicU32::new(1.0f32.to_bits()),
            attack: AtomicU32::new(20.0f32.to_bits()),
            release: AtomicU32::new(200.0f32.to_bits()),
            knee: AtomicU32::new(0.0f32.to_bits()),
            gain_in: AtomicU32::new(0.0f32.to_bits()),
            gain_out: AtomicU32::new(0.0f32.to_bits()),
        }
    }
}

pub struct GateParam {
    pub on: AtomicBool,
    pub threshold: AtomicU32,
    pub attack: AtomicU32,
    pub hold: AtomicU32,
    pub release: AtomicU32,
}

impl GateParam {
    fn new() -> GateParam {
        GateParam {
            on: AtomicBool::new(false),
            threshold: AtomicU32::new((-40.0f32).to_bits()),
            attack: AtomicU32::new(5.0f32.to_bits()),
            hold: AtomicU32::new(100.0f32.to_bits()),
            release: AtomicU32::new(200.0f32.to_bits()),
        }
    }
}

pub struct StripParam {
    pub gain: AtomicU32,
    pub limit: AtomicU32,
    pub pan: AtomicU32,
    pub mute: AtomicBool,
    pub solo: AtomicBool,
    pub mono: AtomicBool,
    pub sends: [AtomicU32; BUSES],
    pub eq_on: AtomicBool,
    pub eq: [[EqParam; EQ_CELLS]; 2],
    pub comp: CompParam,
    pub gate: GateParam,
    pub dn_on: AtomicBool,
    pub dn_floor: AtomicU32,
    pub dn_threshold: AtomicU32,
    pub level: AtomicU32,
}

impl StripParam {
    fn new() -> StripParam {
        let mut sends = std::array::from_fn(|_| AtomicU32::new(0f32.to_bits()));
        sends[0] = AtomicU32::new(1.0f32.to_bits()); // Vorgabe: Streifen -> A1
        StripParam {
            gain: AtomicU32::new(0f32.to_bits()),
            limit: AtomicU32::new(12.0f32.to_bits()),
            pan: AtomicU32::new(0f32.to_bits()),
            mute: AtomicBool::new(false),
            solo: AtomicBool::new(false),
            mono: AtomicBool::new(false),
            sends,
            eq_on: AtomicBool::new(false),
            eq: std::array::from_fn(|_| std::array::from_fn(|_| EqParam::new())),
            comp: CompParam::new(),
            gate: GateParam::new(),
            dn_on: AtomicBool::new(false),
            dn_floor: AtomicU32::new((-30.0f32).to_bits()),
            dn_threshold: AtomicU32::new((-55.0f32).to_bits()),
            level: AtomicU32::new(0f32.to_bits()),
        }
    }
}

pub struct BusParam {
    pub gain: AtomicU32,
    pub limit: AtomicU32,
    pub mute: AtomicBool,
    pub mono: AtomicBool,
    pub trim: [AtomicU32; 2],
    pub delay_ms: [AtomicU32; 2],
    pub eq_on: AtomicBool,
    pub eq: [[EqParam; EQ_CELLS]; 2],
    pub limiter_on: AtomicBool,
    pub c5_on: AtomicBool,
    pub c5_threshold: AtomicU32,
    pub c5_ceiling: AtomicU32,
    pub level: AtomicU32,
}

impl BusParam {
    fn new() -> BusParam {
        BusParam {
            gain: AtomicU32::new(0f32.to_bits()),
            limit: AtomicU32::new(0.0f32.to_bits()),
            mute: AtomicBool::new(false),
            mono: AtomicBool::new(false),
            trim: std::array::from_fn(|_| AtomicU32::new(0f32.to_bits())),
            delay_ms: std::array::from_fn(|_| AtomicU32::new(0f32.to_bits())),
            eq_on: AtomicBool::new(false),
            eq: std::array::from_fn(|_| std::array::from_fn(|_| EqParam::new())),
            limiter_on: AtomicBool::new(false),
            c5_on: AtomicBool::new(false),
            c5_threshold: AtomicU32::new((-18.0f32).to_bits()),
            c5_ceiling: AtomicU32::new((-0.3f32).to_bits()),
            level: AtomicU32::new(0f32.to_bits()),
        }
    }
}

pub struct Params {
    pub strips: [StripParam; STRIPS],
    pub buses: [BusParam; BUSES],
    pub sample_rate: AtomicU32,
    pub block: AtomicU32,
}

impl Params {
    pub fn new(sample_rate: f32, block: u32) -> Params {
        Params {
            strips: std::array::from_fn(|_| StripParam::new()),
            buses: std::array::from_fn(|_| BusParam::new()),
            sample_rate: AtomicU32::new(sample_rate.to_bits()),
            block: AtomicU32::new(block),
        }
    }

    pub fn set(&self, target: &str, index: usize, param: &str, value: f32) -> bool {
        match target {
            "strip" => match self.strips.get(index) {
                Some(s) => set_strip(s, param, value),
                None => false,
            },
            "bus" => match self.buses.get(index) {
                Some(b) => set_bus(b, param, value),
                None => false,
            },
            _ => false,
        }
    }

    pub fn to_json(&self) -> Value {
        Value::obj(vec![
            (
                "strips",
                Value::Arr(
                    self.strips
                        .iter()
                        .enumerate()
                        .map(|(i, s)| {
                            let sends: Vec<Value> =
                                s.sends.iter().map(|a| Value::num(fget(a) as f64)).collect();
                            Value::obj(vec![
                                ("name", Value::str(STRIP_NAMES[i])),
                                ("gain", Value::num(fget(&s.gain) as f64)),
                                ("pan", Value::num(fget(&s.pan) as f64)),
                                ("mute", Value::Bool(s.mute.load(Relaxed))),
                                ("solo", Value::Bool(s.solo.load(Relaxed))),
                                ("mono", Value::Bool(s.mono.load(Relaxed))),
                                ("sends", Value::Arr(sends)),
                                ("eq", Value::Bool(s.eq_on.load(Relaxed))),
                                ("comp", Value::Bool(s.comp.on.load(Relaxed))),
                                ("gate", Value::Bool(s.gate.on.load(Relaxed))),
                                ("denoiser", Value::Bool(s.dn_on.load(Relaxed))),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "buses",
                Value::Arr(
                    self.buses
                        .iter()
                        .enumerate()
                        .map(|(i, b)| {
                            Value::obj(vec![
                                ("name", Value::str(BUS_NAMES[i])),
                                ("gain", Value::num(fget(&b.gain) as f64)),
                                ("mute", Value::Bool(b.mute.load(Relaxed))),
                                ("mono", Value::Bool(b.mono.load(Relaxed))),
                                ("eq", Value::Bool(b.eq_on.load(Relaxed))),
                                ("limiter", Value::Bool(b.limiter_on.load(Relaxed))),
                                ("delay_ms", Value::num(fget(&b.delay_ms[0]) as f64)),
                                ("trim", Value::num(fget(&b.trim[0]) as f64)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("sample_rate", Value::num(fget_sr(self) as f64)),
            ("block", Value::num(self.block.load(Relaxed) as f64)),
        ])
    }

    pub fn levels_json(&self) -> String {
        let s: Vec<String> = self
            .strips
            .iter()
            .map(|x| format!("{:.1}", lin_to_db(fget(&x.level))))
            .collect();
        let b: Vec<String> = self
            .buses
            .iter()
            .map(|x| format!("{:.1}", lin_to_db(fget(&x.level))))
            .collect();
        format!(
            "{{\"strips\":[{}],\"buses\":[{}]}}",
            s.join(","),
            b.join(",")
        )
    }
}

fn fget_sr(p: &Params) -> u32 {
    fget(&p.sample_rate) as u32
}

fn set_strip(s: &StripParam, param: &str, v: f32) -> bool {
    match param {
        "gain" => { fset(&s.gain, v.clamp(-60.0, 12.0)); true }
        "pan" => { fset(&s.pan, v.clamp(-1.0, 1.0)); true }
        "mute" => { s.mute.store(v != 0.0, Relaxed); true }
        "solo" => { s.solo.store(v != 0.0, Relaxed); true }
        "mono" => { s.mono.store(v != 0.0, Relaxed); true }
        "eq" => { s.eq_on.store(v != 0.0, Relaxed); true }
        "comp" => { s.comp.on.store(v != 0.0, Relaxed); true }
        "gate" => { s.gate.on.store(v != 0.0, Relaxed); true }
        "denoiser" => { s.dn_on.store(v != 0.0, Relaxed); true }
        "comp_threshold" => { fset(&s.comp.threshold, v); true }
        "comp_ratio" => { fset(&s.comp.ratio, v.max(1.0)); true }
        "comp_attack" => { fset(&s.comp.attack, v); true }
        "comp_release" => { fset(&s.comp.release, v); true }
        "comp_knee" => { fset(&s.comp.knee, v); true }
        "gate_threshold" => { fset(&s.gate.threshold, v); true }
        "gate_attack" => { fset(&s.gate.attack, v); true }
        "gate_hold" => { fset(&s.gate.hold, v); true }
        "gate_release" => { fset(&s.gate.release, v); true }
        _ => set_strip_ext(s, param, v),
    }
}

/// Sends (`send0..send7`) und EQ-Zellen (`eq<zelle>_<feld>`).
fn set_strip_ext(s: &StripParam, param: &str, v: f32) -> bool {
    if let Some(rest) = param.strip_prefix("send") {
        return match rest.parse::<usize>().ok().and_then(|b| s.sends.get(b)) {
            Some(a) => { fset(a, v.clamp(0.0, 2.0)); true }
            None => false,
        };
    }
    if let Some(rest) = param.strip_prefix("eq") {
        let (cell_s, field) = match rest.split_once('_') {
            Some(x) => x,
            None => return false,
        };
        let c: usize = match cell_s.parse() {
            Ok(c) => c,
            Err(_) => return false,
        };
        for ch in 0..2 {
            match s.eq[ch].get(c) {
                Some(cell) => {
                    if !set_eq_cell(cell, field, v) {
                        return false;
                    }
                }
                None => return false,
            }
        }
        return true;
    }
    false
}

fn set_eq_cell(cell: &EqParam, field: &str, v: f32) -> bool {
    match field {
        "on" => cell.on.store(v != 0.0, Relaxed),
        "type" => cell.kind.store(v.max(0.0) as u32, Relaxed),
        "freq" => fset(&cell.freq, v.clamp(10.0, 24000.0)),
        "q" => fset(&cell.q, v.clamp(0.05, 20.0)),
        "gain" => fset(&cell.gain, v.clamp(-24.0, 24.0)),
        _ => return false,
    }
    true
}

fn set_bus(b: &BusParam, param: &str, v: f32) -> bool {
    match param {
        "gain" => fset(&b.gain, v.clamp(-60.0, 12.0)),
        "mute" => b.mute.store(v != 0.0, Relaxed),
        "mono" => b.mono.store(v != 0.0, Relaxed),
        "eq" => b.eq_on.store(v != 0.0, Relaxed),
        "limiter" => b.limiter_on.store(v != 0.0, Relaxed),
        "c5" => b.c5_on.store(v != 0.0, Relaxed),
        "c5_threshold" => fset(&b.c5_threshold, v.clamp(-60.0, 0.0)),
        "c5_ceiling" => fset(&b.c5_ceiling, v.clamp(-24.0, 0.0)),
        "trim" => {
            fset(&b.trim[0], v);
            fset(&b.trim[1], v);
        }
        "delay" => {
            fset(&b.delay_ms[0], v.clamp(0.0, 2000.0));
            fset(&b.delay_ms[1], v.clamp(0.0, 2000.0));
        }
        _ => return false,
    }
    true
}

/// Zeiger auf die Kanalpuffer eines Blocks (aus den JACK-Ports).
pub struct Io {
    pub ins: [[*const f32; 2]; STRIPS],
    pub outs: [[*mut f32; 2]; BUSES],
}

/// DSP-Zustand, ausschließlich vom Audio-Thread benutzt.
pub struct EngineDsp {
    strip_eq: [[[Biquad; EQ_CELLS]; 2]; STRIPS],
    strip_comp: [[Compressor; 2]; STRIPS],
    strip_gate: [[Gate; 2]; STRIPS],
    strip_dn: [[Denoiser; 2]; STRIPS],
    bus_eq: [[[Biquad; EQ_CELLS]; 2]; BUSES],
    bus_lim: [[Limiter; 2]; BUSES],
    bus_c5: [C5Limiter; BUSES],
    bus_delay: [[DelayLine; 2]; BUSES],
    prev_strip_gain: [f32; STRIPS],
    prev_bus_gain: [f32; BUSES],
    last_sr: f32,
    pub blocks: u64,
}

impl EngineDsp {
    pub fn new(max_block: usize) -> EngineDsp {
        EngineDsp {
            strip_eq: std::array::from_fn(|_| std::array::from_fn(|_| [Biquad::bypass(); EQ_CELLS])),
            strip_comp: [[Compressor::default(); 2]; STRIPS],
            strip_gate: [[Gate::default(); 2]; STRIPS],
            strip_dn: [[Denoiser::default(); 2]; STRIPS],
            bus_eq: std::array::from_fn(|_| std::array::from_fn(|_| [Biquad::bypass(); EQ_CELLS])),
            bus_lim: [[Limiter::default(); 2]; BUSES],
            bus_c5: std::array::from_fn(|_| C5Limiter::default()),
            bus_delay: std::array::from_fn(|_| {
                [DelayLine::new(max_block * 4), DelayLine::new(max_block * 4)]
            }),
            prev_strip_gain: [1.0; STRIPS],
            prev_bus_gain: [1.0; BUSES],
            last_sr: 0.0,
            blocks: 0,
        }
    }

    fn reconfigure(&mut self, p: &Params, sr: f32) {
        let need_eq = (self.last_sr - sr).abs() > 0.5 || self.blocks == 0;
        for i in 0..STRIPS {
            let s = &p.strips[i];
            for ch in 0..2 {
                if need_eq || s.eq_on.load(Relaxed) {
                    for c in 0..EQ_CELLS {
                        let cell = &s.eq[ch][c];
                        let on = cell.on.load(Relaxed) && s.eq_on.load(Relaxed);
                        let b = &mut self.strip_eq[i][ch][c];
                        if on {
                            b.coeffs(
                                FilterType::from_u8(cell.kind.load(Relaxed) as u8),
                                fget(&cell.freq),
                                fget(&cell.q),
                                fget(&cell.gain),
                                sr,
                            );
                        }
                    }
                }
                self.strip_comp[i][ch].enabled = s.comp.on.load(Relaxed);
                self.strip_comp[i][ch].set_times(
                    fget(&s.comp.attack),
                    fget(&s.comp.release),
                    sr,
                );
                self.strip_gate[i][ch].enabled = s.gate.on.load(Relaxed);
                self.strip_gate[i][ch].set_times(
                    fget(&s.gate.attack),
                    fget(&s.gate.hold),
                    fget(&s.gate.release),
                    sr,
                );
                self.strip_dn[i][ch].enabled = s.dn_on.load(Relaxed);
            }
        }
        for i in 0..BUSES {
            let b = &p.buses[i];
            for ch in 0..2 {
                if need_eq || b.eq_on.load(Relaxed) {
                    for c in 0..EQ_CELLS {
                        let cell = &b.eq[ch][c];
                        let on = cell.on.load(Relaxed) && b.eq_on.load(Relaxed);
                        let f = &mut self.bus_eq[i][ch][c];
                        if on {
                            f.coeffs(
                                FilterType::from_u8(cell.kind.load(Relaxed) as u8),
                                fget(&cell.freq),
                                fget(&cell.q),
                                fget(&cell.gain),
                                sr,
                            );
                        }
                    }
                }
                self.bus_lim[i][ch].enabled = b.limiter_on.load(Relaxed);
                self.bus_lim[i][ch].set_release(50.0, sr);
                self.bus_delay[i][ch].set_ms(fget(&b.delay_ms[ch]), sr);
            }
        }
        self.last_sr = sr;
    }

    /// Ein Block. Die Kanäle werden als rohe Zeiger übergeben, damit der
    /// Callback ohne Kopieren direkt in die JACK-Puffer schreiben kann.
    pub fn process(&mut self, p: &Params, io: &Io, frames: usize) {
        if frames == 0 {
            return;
        }
        let sr = fget(&p.sample_rate).max(8000.0);
        self.reconfigure(p, sr);

        let any_solo = p.strips.iter().any(|s| s.solo.load(Relaxed));
        let mut peak_s = [0f32; STRIPS];
        let mut peak_b = [0f32; BUSES];

        for n in 0..frames {
            let mut acc = [[0f32; 2]; BUSES];
            for si in 0..STRIPS {
                let sp = &p.strips[si];
                let audible = !sp.mute.load(Relaxed) && (!any_solo || sp.solo.load(Relaxed));
                let gain = if audible { db_to_lin(fget(&sp.gain)) } else { 0.0 };
                let pan = fget(&sp.pan).clamp(-1.0, 1.0);
                let mono = sp.mono.load(Relaxed);
                let mut s = unsafe { [*io.ins[si][0].add(n), *io.ins[si][1].add(n)] };

                if mono {
                    let m = 0.5 * (s[0] + s[1]);
                    s = [m, m];
                }
                for ch in 0..2 {
                    let mut x = s[ch];
                    if sp.eq_on.load(Relaxed) {
                        for c in 0..EQ_CELLS {
                            x = self.strip_eq[si][ch][c].process(x);
                        }
                    }
                    let prefix = db_to_lin(fget(&sp.comp.gain_in));
                    x *= prefix;
                    let comp = &mut self.strip_comp[si][ch];
                    x = comp.process(
                        x,
                        fget(&sp.comp.threshold),
                        fget(&sp.comp.ratio),
                        fget(&sp.comp.knee),
                        auto_makeup(sp),
                    );
                    let gate = &mut self.strip_gate[si][ch];
                    x = gate.process(x, fget(&sp.gate.threshold));
                    let dn = &mut self.strip_dn[si][ch];
                    x = dn.process(x, fget(&sp.dn_floor), fget(&sp.dn_threshold));
                    x *= gain;
                    s[ch] = x;
                }
                let g = [1.0 - pan, 1.0 + pan];
                let l = s[0] * g[0].min(1.0);
                let r = s[1] * g[1].min(1.0);
                let level = l.abs().max(r.abs());
                if level > peak_s[si] {
                    peak_s[si] = level;
                }
                for bi in 0..BUSES {
                    let send = fget(&sp.sends[bi]);
                    if send != 0.0 {
                        acc[bi][0] += l * send;
                        acc[bi][1] += r * send;
                    }
                }
            }

            for bi in 0..BUSES {
                let bp = &p.buses[bi];
                let gain = if bp.mute.load(Relaxed) { 0.0 } else { db_to_lin(fget(&bp.gain)) };
                let mono = bp.mono.load(Relaxed);
                let mut v = acc[bi];
                if mono {
                    let m = 0.5 * (v[0] + v[1]);
                    v = [m, m];
                }
                for ch in 0..2 {
                    let mut x = v[ch];
                    if bp.eq_on.load(Relaxed) {
                        for c in 0..EQ_CELLS {
                            x = self.bus_eq[bi][ch][c].process(x);
                        }
                    }
                    x = self.bus_lim[bi][ch].process(x, fget(&bp.limit));
                    if bp.c5_on.load(Relaxed) {
                        let mut cfg = C5Config::default();
                        cfg.enabled = true;
                        let th = fget(&bp.c5_threshold);
                        let ce = fget(&bp.c5_ceiling);
                        cfg.threshold_db = [th; 3];
                        cfg.ceiling_db = [ce; 3];
                        x = self.bus_c5[bi].process(x, &cfg, sr);
                    }
                    x = self.bus_delay[bi][ch].process(x);
                    x *= db_to_lin(fget(&bp.trim[ch]));
                    x *= gain;
                    v[ch] = x;
                }
                unsafe {
                    *io.outs[bi][0].add(n) = v[0];
                    *io.outs[bi][1].add(n) = v[1];
                }
                let level = v[0].abs().max(v[1].abs());
                if level > peak_b[bi] {
                    peak_b[bi] = level;
                }
            }

            // Die akkumulierten Werte gelten fuer alle Streifen dieses Frames.
            if n + 1 == frames {
                for si in 0..STRIPS {
                    let old = fget(&p.strips[si].level);
                    fset(&p.strips[si].level, peak_s[si].max(old * 0.72));
                }
                for bi in 0..BUSES {
                    let old = fget(&p.buses[bi].level);
                    fset(&p.buses[bi].level, peak_b[bi].max(old * 0.72));
                }
            }
        }
        self.prev_strip_gain = std::array::from_fn(|i| db_to_lin(fget(&p.strips[i].gain)));
        self.prev_bus_gain = std::array::from_fn(|i| db_to_lin(fget(&p.buses[i].gain)));
        self.blocks += 1;
    }
}

fn auto_makeup(s: &StripParam) -> f32 {
    fget(&s.comp.gain_out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Testpuffer: 2 Kanäle je Streifen/Bus, alle mit demselben Pegel gefüllt.
    struct Buf {
        ins: Vec<Vec<f32>>,
        outs: Vec<Vec<f32>>,
    }

    impl Buf {
        /// Nur Streifen 0 wird angeregt, alle anderen bleiben still.
        fn new(level: f32, frames: usize) -> Buf {
            let mut b = Buf {
                ins: vec![vec![0.0f32; frames]; STRIPS * 2],
                outs: vec![vec![0.0f32; frames]; BUSES * 2],
            };
            b.set_strip(0, level);
            b
        }

        fn set_strip(&mut self, strip: usize, level: f32) {
            for ch in 0..2 {
                self.ins[strip * 2 + ch].iter_mut().for_each(|x| *x = level);
            }
        }

        fn io(&mut self) -> Io {
            let mut io = Io {
                ins: [[std::ptr::null(); 2]; STRIPS],
                outs: [[std::ptr::null_mut(); 2]; BUSES],
            };
            for s in 0..STRIPS {
                for c in 0..2 {
                    io.ins[s][c] = self.ins[s * 2 + c].as_ptr();
                }
            }
            for b in 0..BUSES {
                for c in 0..2 {
                    io.outs[b][c] = self.outs[b * 2 + c].as_mut_ptr();
                }
            }
            io
        }

        fn bus(&self, bus: usize) -> f32 {
            self.outs[bus * 2][0]
        }
    }

    fn params() -> Params {
        Params::new(48000.0, 64)
    }

    #[test]
    fn durchgang_und_mute() {
        let p = params();
        let mut dsp = EngineDsp::new(64);
        let mut buf = Buf::new(0.5, 64);
        let io = buf.io();
        dsp.process(&p, &io, 64);
        assert!((buf.bus(0) - 0.5).abs() < 1e-6, "A1 war {}", buf.bus(0));
        p.set("strip", 0, "mute", 1.0);
        let io = buf.io();
        dsp.process(&p, &io, 64);
        assert!(buf.bus(0).abs() < 1e-6);
    }

    #[test]
    fn routing_auf_zweiten_bus() {
        let p = params();
        let mut dsp = EngineDsp::new(64);
        p.set("strip", 0, "send0", 0.0);
        p.set("strip", 0, "send1", 1.0);
        let mut buf = Buf::new(0.25, 64);
        let io = buf.io();
        dsp.process(&p, &io, 64);
        assert!(buf.bus(0).abs() < 1e-6);
        assert!((buf.bus(1) - 0.25).abs() < 1e-6, "A2 war {}", buf.bus(1));
    }

    #[test]
    fn solo_schaltet_andere_streifen_ab() {
        let p = params();
        let mut dsp = EngineDsp::new(64);
        p.set("strip", 1, "solo", 1.0);
        let mut buf = Buf::new(0.1, 64);
        buf.set_strip(1, 0.4);
        let io = buf.io();
        dsp.process(&p, &io, 64);
        assert!((buf.bus(0) - 0.4).abs() < 1e-6, "nur Streifen 2 darf durch: {}", buf.bus(0));
    }

    #[test]
    fn gain_und_meter() {
        let p = params();
        let mut dsp = EngineDsp::new(64);
        p.set("strip", 0, "gain", -6.0);
        let mut buf = Buf::new(0.5, 64);
        let io = buf.io();
        dsp.process(&p, &io, 64);
        assert!((buf.bus(0) - 0.25).abs() < 1e-3, "A1 war {}", buf.bus(0));
        assert!((fget(&p.strips[0].level) - 0.25).abs() < 1e-3);
    }

    #[test]
    fn c5_limiter_haelt_bus_decke() {
        let p = params();
        let mut dsp = EngineDsp::new(64);
        p.set("bus", 0, "c5", 1.0);
        p.set("bus", 0, "c5_ceiling", -6.0);
        p.set("bus", 0, "c5_threshold", 0.0);
        p.set("strip", 0, "gain", 12.0); // 0.5 * 4 = 2.0 in den Bus
        let mut buf = Buf::new(0.5, 64);
        let mut peak = 0f32;
        for _ in 0..40 {
            let io = buf.io();
            dsp.process(&p, &io, 64);
            peak = peak.max(buf.bus(0).abs());
        }
        let db = crate::dsp::lin_to_db(peak);
        assert!(db <= -4.0, "Bus-Spitze war {db} dB, soll unter -6 dB liegen");
    }

    #[test]
    fn eq_und_eq_parameter() {
        let p = params();
        let mut dsp = EngineDsp::new(64);
        p.set("strip", 0, "eq", 1.0);
        assert!(p.set("strip", 0, "eq0_gain", 12.0));
        assert!(p.set("strip", 0, "eq0_freq", 1000.0));
        assert!(p.set("strip", 0, "eq0_on", 1.0));
        assert!(!p.set("strip", 0, "eq9_gain", 1.0));
        let mut buf = Buf::new(0.1, 64);
        let io = buf.io();
        dsp.process(&p, &io, 64);
        assert!(buf.bus(0).is_finite());
    }
}
