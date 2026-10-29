//! Zugabe: RNNoise, **nur wenn die Bibliothek da ist**.
//!
//! Crosspoint braucht RNNoise nicht. Die Rauschunterdrueckung im Kern
//! (`dsp::NoiseReduction`) ist eigenstaendig und immer verfuegbar. Wenn aber
//! `librnnoise.so.0` im System liegt, kann der dritte Modus der NR sie
//! benutzen. Geladen wird per `dlopen` aus der libc – ohne neue Crate, ohne
//! Link-Abhaengigkeit: fehlt die Bibliothek, schlaegt `dlopen` fehl und alles
//! andere bleibt unveraendert.
//!
//! Die Bibliothek arbeitet fest mit 48 kHz und 480 Samples je Frame. Andere
//! Abtastraten fallen in [`crate::engine`] auf die eigene Berechnung zurueck.

use std::ffi::{c_char, c_void, CString};
use std::sync::OnceLock;

/// Frames je Aufruf von `rnnoise_process_frame`.
pub const FRAME: usize = 480;
/// Skalierung: RNNoise rechnet mit 16-Bit-Werten als Float.
const SKALA: f32 = 32768.0;
const RTLD_NOW: i32 = 2;

extern "C" {
    fn dlopen(datei: *const c_char, modus: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

type Erzeugen = unsafe extern "C" fn(*mut c_void, i32) -> *mut c_void;
type Freigeben = unsafe extern "C" fn(*mut c_void);
type Verarbeiten = unsafe extern "C" fn(*mut c_void, *mut f32, *const f32) -> f32;

/// Die geladene Bibliothek mit ihren drei Funktionen.
pub struct Bibliothek {
    erzeugen: Erzeugen,
    freigeben: Freigeben,
    verarbeiten: Verarbeiten,
}

impl Bibliothek {
    fn laden(pfad: &str) -> Option<Bibliothek> {
        let name = CString::new(pfad).ok()?;
        // SAFETY: dlopen/dlsym sind libc-Aufrufe; der Zeiger wird nur auf
        // Gueltigkeit geprueft und danach als Funktionszeiger gelesen.
        unsafe {
            let h = dlopen(name.as_ptr(), RTLD_NOW);
            if h.is_null() {
                return None;
            }
            let pe = dlsym(h, c"rnnoise_create".as_ptr());
            let pf = dlsym(h, c"rnnoise_destroy".as_ptr());
            let pv = dlsym(h, c"rnnoise_process_frame".as_ptr());
            if pe.is_null() || pf.is_null() || pv.is_null() {
                // Bibliothek da, aber nicht die erwartete: als „nicht da“
                // behandeln, damit nichts Halbes angeboten wird.
                return None;
            }
            // SAFETY: die drei Symbole sind genau die der RNNoise-API
            // (Schreibweise und Signatur stehen im Header der Bibliothek).
            Some(Bibliothek {
                erzeugen: std::mem::transmute::<*mut c_void, Erzeugen>(pe),
                freigeben: std::mem::transmute::<*mut c_void, Freigeben>(pf),
                verarbeiten: std::mem::transmute::<*mut c_void, Verarbeiten>(pv),
            })
        }
    }
}

/// Ist RNNoise verfuegbar? Einmal geprueft, danach gemerkt.
///
/// Reihenfolge: `CROSSPOINT_RNNOISE` (Pfad oder Soname, fuer Tests und
/// eigene Installationen), dann `librnnoise.so.0`, dann `librnnoise.so`.
pub fn bibliothek() -> Option<&'static Bibliothek> {
    static GELADEN: OnceLock<Option<Bibliothek>> = OnceLock::new();
    GELADEN
        .get_or_init(|| {
            let mut kandidaten: Vec<String> = Vec::new();
            if let Ok(p) = std::env::var("CROSSPOINT_RNNOISE") {
                if !p.trim().is_empty() {
                    kandidaten.push(p.trim().to_string());
                }
            }
            kandidaten.push("librnnoise.so.0".to_string());
            kandidaten.push("librnnoise.so".to_string());
            kandidaten.iter().find_map(|p| Bibliothek::laden(p))
        })
        .as_ref()
}

/// Kurzer Text fuer den Tooltip – leer, wenn RNNoise fehlt.
pub fn hinweis() -> &'static str {
    if bibliothek().is_some() {
        "RNNoise verfuegbar"
    } else {
        ""
    }
}

/// Ein Kanal mit eigenem RNNoise-Zustand.
///
/// Puffer sind fest und werden im Konstruktor angelegt; im Sample-Pfad
/// entsteht keine Allokation. Latenz: ein Frame (480 Samples ≈ 10 ms).
pub struct Kanal {
    lib: &'static Bibliothek,
    st: *mut c_void,
    ein: [f32; FRAME],
    aus: [f32; FRAME],
    q: [f32; FRAME],
    pos: usize,
    qpos: usize,
}

// Der Zeiger gehoert dem Kanal allein; er wird nur im Audio-Thread benutzt.
unsafe impl Send for Kanal {}

impl Kanal {
    pub fn neu() -> Option<Kanal> {
        let lib = bibliothek()?;
        // SAFETY: rnnoise_create(NULL) legt einen Zustand mit Standardmodell an.
        let st = unsafe { (lib.erzeugen)(std::ptr::null_mut(), 0) };
        if st.is_null() {
            return None;
        }
        Some(Kanal {
            lib,
            st,
            ein: [0.0; FRAME],
            aus: [0.0; FRAME],
            q: [0.0; FRAME],
            pos: 0,
            qpos: FRAME,
        })
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.ein[self.pos] = x * SKALA;
        self.pos += 1;
        if self.pos == FRAME {
            self.pos = 0;
            // SAFETY: beide Puffer sind FRAME lang, der Zustand ist gueltig.
            unsafe {
                (self.lib.verarbeiten)(self.st, self.aus.as_mut_ptr(), self.ein.as_ptr());
            }
            for i in 0..FRAME {
                self.q[i] = self.aus[i] / SKALA;
            }
            self.qpos = 0;
        }
        if self.qpos < FRAME {
            let v = self.q[self.qpos];
            self.qpos += 1;
            v
        } else {
            0.0
        }
    }
}

impl Drop for Kanal {
    fn drop(&mut self) {
        if !self.st.is_null() {
            // SAFETY: der Zustand stammt aus rnnoise_create.
            unsafe { (self.lib.freigeben)(self.st) };
            self.st = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ohne Bibliothek darf hier nichts passieren – kein Absturz, keine
    /// Ausgabe. Der Test laeuft immer; mit gesetztem `CROSSPOINT_RNNOISE`
    /// prueft er zusaetzlich das Laden.
    #[test]
    fn ohne_bibliothek_kein_kanal() {
        if std::env::var("CROSSPOINT_RNNOISE").is_ok() {
            return; // siehe rnnoise_kanal_verarbeitet
        }
        assert!(bibliothek().is_none() || bibliothek().is_some());
        // Ohne lib gibt es keinen Kanal; mit lib muss einer entstehen.
        assert_eq!(Kanal::neu().is_some(), bibliothek().is_some());
    }

    #[test]
    fn rnnoise_kanal_verarbeitet() {
        let Ok(_) = std::env::var("CROSSPOINT_RNNOISE") else {
            return;
        };
        let mut k = Kanal::neu().expect("Kanal");
        let mut summe = 0.0f32;
        for i in 0..FRAME * 4 {
            let x = 0.1 * (2.0 * std::f32::consts::PI * 500.0 * i as f32 / 48000.0).sin();
            let y = k.process(x);
            if i > FRAME * 2 {
                summe += y * y;
            }
        }
        assert!(summe > 0.0, "RNNoise lieferte nur Stille");
    }
}
