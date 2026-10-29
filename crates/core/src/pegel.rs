//! Pegelbank: der Audio-Thread schreibt, die Oberflaeche liest.
//!
//! Je Streifen und je Bus ein `AtomicU32` mit den Bits eines `f32` (linearer
//! Spitzenwert des letzten Blocks). Kein Lock, keine Nachricht, kein JSON: die
//! Oberflaeche holt im 60-Hz-Takt `strip()`/`bus()` und zeigt den Wert an.
//!
//! Warum das nicht ueber `/api/levels` laeuft: die Anzeige waere dann zwei
//! Netzwerk-Hops hinter dem Ton (Callback -> JSON -> HTTP -> JSON -> Pixel).
//! Dieselben Atome stehen auch dem Web-UI und der `VBVMR_*`-Schnittstelle zur
//! Verfuegung, ohne dass dort ein zweiter Pegelweg entsteht.
//!
//! Der Audio-Callback darf hier **nur** schreiben, was er ohne Allokation und
//! ohne Blockieren kann: `Ordering::Relaxed` auf `u32`/`u64`-Atomen.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::{BUSES, STRIPS};

#[derive(Debug)]
pub struct Pegelbank {
    strips: [AtomicU32; STRIPS],
    buses: [AtomicU32; BUSES],
    /// Im Callback verarbeitete Bloecke – Nachweis, dass Audio laeuft.
    bloecke: AtomicU64,
}

impl Default for Pegelbank {
    fn default() -> Self {
        Pegelbank::neu()
    }
}

impl Pegelbank {
    pub fn neu() -> Pegelbank {
        Pegelbank {
            strips: std::array::from_fn(|_| AtomicU32::new(0f32.to_bits())),
            buses: std::array::from_fn(|_| AtomicU32::new(0f32.to_bits())),
            bloecke: AtomicU64::new(0),
        }
    }

    /// Linearer Spitzenwert eines Streifens (0.0, wenn nie geschrieben).
    pub fn strip(&self, index: usize) -> f32 {
        match self.strips.get(index) {
            Some(a) => f32::from_bits(a.load(Ordering::Relaxed)),
            None => 0.0,
        }
    }

    /// Linearer Spitzenwert eines Busses.
    pub fn bus(&self, index: usize) -> f32 {
        match self.buses.get(index) {
            Some(a) => f32::from_bits(a.load(Ordering::Relaxed)),
            None => 0.0,
        }
    }

    /// Hoechster Buspegel – das ist der Wert, den der Master-Streifen zeigt
    /// (der Master ist die Summe aller Busse, kein eigener Port).
    pub fn master(&self) -> f32 {
        (0..BUSES).map(|b| self.bus(b)).fold(0.0f32, f32::max)
    }

    /// Vom Audio-Callback aufgerufen (kein Lock, keine Allokation).
    #[inline]
    pub fn schreibe_strip(&self, index: usize, linear: f32) {
        if let Some(a) = self.strips.get(index) {
            a.store(linear.to_bits(), Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn schreibe_bus(&self, index: usize, linear: f32) {
        if let Some(a) = self.buses.get(index) {
            a.store(linear.to_bits(), Ordering::Relaxed);
        }
    }

    /// Ein fertiger Block: Pegel und Blockzaehler in einem Aufruf.
    ///
    /// `strips` und `buses` duerfen kuerzer sein als die Anzahl der Kanaele;
    /// fehlende Werte bleiben stehen.
    #[inline]
    pub fn schreibe_block(&self, strips: &[f32; STRIPS], buses: &[f32; BUSES]) {
        for (i, v) in strips.iter().enumerate() {
            self.schreibe_strip(i, *v);
        }
        for (i, v) in buses.iter().enumerate() {
            self.schreibe_bus(i, *v);
        }
        self.bloecke.fetch_add(1, Ordering::Release);
    }

    /// Anzahl der im Callback verarbeiteten Bloecke.
    pub fn bloecke(&self) -> u64 {
        self.bloecke.load(Ordering::Acquire)
    }

    /// Alle Pegel auf Null (z. B. nach dem Abschalten des Clients).
    pub fn zuruecksetzen(&self) {
        for a in &self.strips {
            a.store(0f32.to_bits(), Ordering::Relaxed);
        }
        for a in &self.buses {
            a.store(0f32.to_bits(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frisch_ist_alles_still() {
        let p = Pegelbank::neu();
        for s in 0..STRIPS {
            assert_eq!(p.strip(s), 0.0);
        }
        for b in 0..BUSES {
            assert_eq!(p.bus(b), 0.0);
        }
        assert_eq!(p.master(), 0.0);
        assert_eq!(p.bloecke(), 0);
    }

    #[test]
    fn schreiben_und_lesen_ueber_die_bits() {
        let p = Pegelbank::neu();
        p.schreibe_strip(3, 0.5);
        p.schreibe_bus(1, 0.25);
        assert!((p.strip(3) - 0.5).abs() < 1e-6);
        assert!((p.bus(1) - 0.25).abs() < 1e-6);
        // Die Nachbarn bleiben unberuehrt.
        assert_eq!(p.strip(2), 0.0);
        assert_eq!(p.bus(0), 0.0);
    }

    #[test]
    fn master_ist_der_hoechste_bus() {
        let p = Pegelbank::neu();
        p.schreibe_bus(0, 0.1);
        p.schreibe_bus(4, 0.8);
        p.schreibe_bus(7, 0.3);
        assert!((p.master() - 0.8).abs() < 1e-6);
    }

    #[test]
    fn ungueltige_indizes_sind_wirkungslos() {
        let p = Pegelbank::neu();
        p.schreibe_strip(99, 1.0);
        p.schreibe_bus(99, 1.0);
        assert_eq!(p.strip(99), 0.0);
        assert_eq!(p.bus(99), 0.0);
        assert_eq!(p.master(), 0.0);
    }

    #[test]
    fn block_zaehlt_und_setzt_alle_werte() {
        let p = Pegelbank::neu();
        let strips = std::array::from_fn(|i| i as f32 / 10.0);
        let buses = std::array::from_fn(|i| i as f32 / 100.0);
        p.schreibe_block(&strips, &buses);
        p.schreibe_block(&strips, &buses);
        assert_eq!(p.bloecke(), 2);
        assert!((p.strip(7) - 0.7).abs() < 1e-6);
        assert!((p.bus(5) - 0.05).abs() < 1e-6);
    }

    #[test]
    fn zuruecksetzen_leert_die_anzeige() {
        let p = Pegelbank::neu();
        p.schreibe_strip(0, 1.0);
        p.schreibe_bus(0, 1.0);
        p.zuruecksetzen();
        assert_eq!(p.strip(0), 0.0);
        assert_eq!(p.bus(0), 0.0);
    }
}
