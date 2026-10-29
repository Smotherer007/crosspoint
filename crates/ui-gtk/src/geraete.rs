//! Geraetezuordnung je Kanal - ohne Umweg ueber HTTP.
//!
//! Gelesen wird ueber die oeffentliche Bibliothek (`crosspoint::pw::PipeWire`
//! als [`crosspoint::pw::Zugriff`]), geschrieben ueber dieselben Funktionen, die
//! auch die HTTP-Handler benutzen: `crosspoint::inputs::assign` fuer Streifen,
//! `crosspoint::devices::DeviceMap::{assign,clear}` fuer Busse. Beide
//! persistieren in `inputs.conf` bzw. `devices.json` und verdrahten sofort.

use crosspoint::devices::{self, DeviceMap};
use crosspoint::inputs;
use crosspoint::pw::PipeWire;

/// Beschriftung fuer "nichts zugeordnet".
pub const KEIN_EINGANG: &str = "kein Eingang";
pub const KEIN_AUSGANG: &str = "kein Ausgang";

/// Ein Eintrag im Aufklappmenue.
#[derive(Clone, Debug, PartialEq)]
pub struct Eintrag {
    /// Technischer Name, wie ihn der Kern erwartet.
    pub name: String,
    /// Anzeigename (bei Monitoren mit Zusatz).
    pub anzeige: String,
}

/// Was die Oberflaeche vom Graphen braucht - ein Schnappschuss, kein Zustand.
#[derive(Clone, Debug, Default)]
pub struct Bestand {
    pub quellen: Vec<Eintrag>,
    pub senken: Vec<Eintrag>,
    /// Belegung der Hardware-Streifen 1..5.
    pub streifen: Vec<Option<String>>,
    /// Belegung der Hardware-Busse A1..A5.
    pub busse: Vec<(String, Option<String>)>,
    pub fehler: Option<String>,
}

/// Bestand aus dem PipeWire-Graph lesen (blockiert; im Thread aufrufen).
pub fn laden() -> Bestand {
    let pw = PipeWire::new();
    let mut b = Bestand::default();
    let mut fehler: Vec<String> = Vec::new();

    match inputs::list_sources_mit(&pw, true) {
        Ok(quellen) => {
            b.quellen = quellen
                .into_iter()
                .map(|q| Eintrag {
                    name: q.name,
                    anzeige: q.desc,
                })
                .collect()
        }
        Err(e) => fehler.push(format!("Quellen: {e}")),
    }
    match devices::list_sinks(&pw) {
        Ok(senken) => {
            b.senken = senken
                .into_iter()
                .map(|s| Eintrag {
                    name: s.name,
                    anzeige: s.desc,
                })
                .collect()
        }
        Err(e) => fehler.push(format!("Ausgaenge: {e}")),
    }
    b.streifen = inputs::mapping().to_vec();
    let map = DeviceMap::load_default();
    b.busse = crosspoint::BUS_NAMES[..5]
        .iter()
        .map(|bus| (bus.to_string(), map.get(bus).map(|s| s.to_string())))
        .collect();
    if !fehler.is_empty() {
        b.fehler = Some(fehler.join(" · "));
    }
    b
}

/// Ziel einer Zuordnung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ziel<'a> {
    Streifen(usize),
    Bus(&'a str),
}

/// Schreibpfad - als Trait, damit die Auswahllogik ohne PipeWire pruefbar ist.
pub trait Zuordner {
    fn streifen(&self, strip: usize, quelle: Option<&str>) -> Result<(), String>;
    fn bus(&self, bus: &str, senke: Option<&str>) -> Result<(), String>;
}

/// Der echte Weg in den Kern.
pub struct KernZuordner;

impl Zuordner for KernZuordner {
    fn streifen(&self, strip: usize, quelle: Option<&str>) -> Result<(), String> {
        inputs::assign(&PipeWire::new(), strip, quelle)
    }

    fn bus(&self, bus: &str, senke: Option<&str>) -> Result<(), String> {
        let mut map = DeviceMap::load_default();
        match senke.map(str::trim).filter(|s| !s.is_empty()) {
            Some(sink) => map.assign(&PipeWire::new(), bus, sink),
            None => map.clear(&PipeWire::new(), bus),
        }
    }
}

/// Auswahl anwenden. Ein leerer Eintrag ("kein Eingang"/"kein Ausgang") loescht
/// die Zuordnung, statt einen leeren Namen zu schreiben.
pub fn auswahl_anwenden<Z: Zuordner + ?Sized>(
    z: &Z,
    ziel: Ziel<'_>,
    anzeige: &str,
) -> Result<(), String> {
    let text = anzeige.trim();
    let wert = if text.is_empty() || text == KEIN_EINGANG || text == KEIN_AUSGANG {
        None
    } else {
        Some(text)
    };
    match ziel {
        Ziel::Streifen(i) => z.streifen(i, wert),
        Ziel::Bus(b) => z.bus(b, wert),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Merker {
        aufrufe: RefCell<Vec<String>>,
    }

    impl Zuordner for Merker {
        fn streifen(&self, strip: usize, quelle: Option<&str>) -> Result<(), String> {
            self.aufrufe
                .borrow_mut()
                .push(format!("streifen {strip} {:?}", quelle));
            Ok(())
        }
        fn bus(&self, bus: &str, senke: Option<&str>) -> Result<(), String> {
            self.aufrufe
                .borrow_mut()
                .push(format!("bus {bus} {:?}", senke));
            Ok(())
        }
    }

    #[test]
    fn leerer_eintrag_loescht_die_zuordnung() {
        let m = Merker::default();
        auswahl_anwenden(&m, Ziel::Bus("A1"), KEIN_AUSGANG).unwrap();
        assert_eq!(m.aufrufe.borrow().as_slice(), ["bus A1 None"]);
        assert!(m.aufrufe.borrow()[0].contains("None"));
    }

    #[test]
    fn leerer_text_loescht_ebenfalls() {
        let m = Merker::default();
        auswahl_anwenden(&m, Ziel::Bus("A2"), "   ").unwrap();
        auswahl_anwenden(&m, Ziel::Streifen(1), "").unwrap();
        let a = m.aufrufe.borrow();
        assert_eq!(a[0], "bus A2 None");
        assert_eq!(a[1], "streifen 1 None");
    }

    #[test]
    fn geraetename_wird_durchgereicht() {
        let m = Merker::default();
        auswahl_anwenden(
            &m,
            Ziel::Bus("A1"),
            "alsa_output.pci-0000_00_1f.3.analog-stereo",
        )
        .unwrap();
        auswahl_anwenden(&m, Ziel::Streifen(0), "alsa_input.usb-mic").unwrap();
        let a = m.aufrufe.borrow();
        assert_eq!(
            a[0],
            "bus A1 Some(\"alsa_output.pci-0000_00_1f.3.analog-stereo\")"
        );
        assert_eq!(a[1], "streifen 0 Some(\"alsa_input.usb-mic\")");
    }

    #[test]
    fn fehler_des_kerns_kommt_beim_aufrufer_an() {
        struct Kaputt;
        impl Zuordner for Kaputt {
            fn streifen(&self, _s: usize, _q: Option<&str>) -> Result<(), String> {
                Err("Port nicht vorhanden".into())
            }
            fn bus(&self, _b: &str, _s: Option<&str>) -> Result<(), String> {
                Err("Sink nicht im Graph".into())
            }
        }
        let e = auswahl_anwenden(&Kaputt, Ziel::Bus("A1"), KEIN_AUSGANG).unwrap_err();
        assert!(e.contains("Sink"));
        let e = auswahl_anwenden(&Kaputt, Ziel::Streifen(0), "mic").unwrap_err();
        assert!(e.contains("Port"));
    }
}
