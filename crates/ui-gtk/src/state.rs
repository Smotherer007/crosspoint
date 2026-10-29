//! Schreibpfade der Oberflaeche in `Params` - ohne GTK, damit pruefbar.
//!
//! Die Zuordnung eines Streifens zu einem Bus ist im Kern das Feld
//! `StripParam::sends: [AtomicU32; BUSES]` (Sendepegel 0..2, Vorgabe: Streifen 1
//! auf A1). Ein Klick auf eine Zuordnungs-Taste setzt den Pegel auf 1.0 oder
//! 0.0 - dasselbe Feld, das die Routing-Matrix im Audio-Thread liest.

use crosspoint::engine::Params;

/// Sendepegel, der als "zugeordnet" gilt.
pub const SEND_AN: f32 = 1.0;

/// Ist der Streifen auf diesen Bus geroutet?
pub fn send_an(p: &Params, strip: usize, bus: usize) -> bool {
    match p.strips.get(strip).and_then(|s| s.sends.get(bus)) {
        Some(a) => f32::from_bits(a.load(std::sync::atomic::Ordering::Relaxed)) > 0.01,
        None => false,
    }
}

/// Zuordnung setzen. Liefert `false`, wenn Streifen oder Bus nicht existieren.
pub fn send_setzen(p: &Params, strip: usize, bus: usize, an: bool) -> bool {
    if strip >= p.strips.len() || bus >= p.strips[strip].sends.len() {
        return false;
    }
    p.set(
        "strip",
        strip,
        &format!("send{bus}"),
        if an { SEND_AN } else { 0.0 },
    )
}

/// Zuordnung umschalten; liefert den neuen Zustand, falls der Index gueltig ist.
///
/// Der Knopf in der Oberflaeche schreibt direkt (`send_setzen`), diese Funktion
/// ist der Weg fuer Taste/MIDI und wird von den Tests benutzt.
#[allow(dead_code)]
pub fn send_umschalten(p: &Params, strip: usize, bus: usize) -> Option<bool> {
    let neu = !send_an(p, strip, bus);
    if send_setzen(p, strip, bus, neu) {
        Some(neu)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params::new(48000.0, 512)
    }

    #[test]
    fn vorgabe_ist_jeder_streifen_auf_a1() {
        // `StripParam::new` setzt sends[0] = 1.0 fuer jeden Streifen.
        let p = params();
        for strip in 0..8 {
            assert!(send_an(&p, strip, 0), "Streifen {strip} muss auf A1 liegen");
            for bus in 1..8 {
                assert!(
                    !send_an(&p, strip, bus),
                    "Streifen {strip}: Bus {bus} darf nicht zugeordnet sein"
                );
            }
        }
    }

    #[test]
    fn klick_schaltet_die_zuordnung_um() {
        let p = params();
        // Klick auf B1 (Bus 5) bei Streifen 2: an, A1 bleibt bestehen.
        assert_eq!(send_umschalten(&p, 1, 5), Some(true));
        assert!(send_an(&p, 1, 5));
        assert!(
            send_an(&p, 1, 0),
            "die Vorgabe A1 darf nicht verloren gehen"
        );
        // Zweiter Klick schaltet wieder aus.
        assert_eq!(send_umschalten(&p, 1, 5), Some(false));
        assert!(!send_an(&p, 1, 5));
    }

    #[test]
    fn umschalten_beruehrt_andere_streifen_nicht() {
        let p = params();
        send_setzen(&p, 2, 1, true);
        assert!(send_an(&p, 0, 0));
        assert!(!send_an(&p, 1, 1), "Streifen 2 bleibt ohne A2");
        assert!(send_an(&p, 2, 1));
        send_umschalten(&p, 2, 1);
        assert!(!send_an(&p, 2, 1));
        assert!(send_an(&p, 0, 0));
    }

    #[test]
    fn mehrere_busse_gleichzeitig() {
        let p = params();
        // Streifen 2 auf A1 und B1.
        send_setzen(&p, 1, 0, true);
        send_setzen(&p, 1, 5, true);
        assert!(send_an(&p, 1, 0) && send_an(&p, 1, 5));
        assert!(!send_an(&p, 1, 1));
        // Der Sendepegel landet wirklich in den Atomen des Kerns.
        let pegel = f32::from_bits(p.strips[1].sends[5].load(std::sync::atomic::Ordering::Relaxed));
        assert!((pegel - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ungueltige_indizes_aendern_nichts() {
        let p = params();
        assert_eq!(send_umschalten(&p, 99, 0), None);
        assert_eq!(send_umschalten(&p, 0, 99), None);
        assert!(send_an(&p, 0, 0));
    }
}
