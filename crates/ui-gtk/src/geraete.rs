//! Geraetezuordnung je Kanal - ohne Umweg ueber HTTP.
//!
//! Gelesen wird ueber die oeffentliche Bibliothek (`crosspoint::pw::PipeWire`
//! als [`crosspoint::pw::Zugriff`]), geschrieben ueber dieselben Funktionen, die
//! auch die HTTP-Handler benutzen: `crosspoint::inputs::assign` fuer Streifen,
//! `crosspoint::devices::DeviceMap::{assign,clear}` fuer Busse. Beide
//! persistieren in `inputs.conf` bzw. `devices.json` und verdrahten sofort.
//!
//! Ein gemerktes Gerät, das gerade nicht angeschlossen ist, bleibt in dieser
//! Liste stehen und wird als „nicht verbunden“ geführt (`*_fehlt`). Verworfen
//! wird nichts: die Zuordnung ist eine Absicht, kein Ist-Zustand.

use crosspoint::devices::{self, DeviceMap};
use crosspoint::inputs;
use crosspoint::pw::{Graph, Lautstaerke, PipeWire, Zugriff};

/// Beschriftung fuer "nichts zugeordnet".
pub const KEIN_EINGANG: &str = "kein Eingang";
pub const KEIN_AUSGANG: &str = "kein Ausgang";

/// Zusatz an einem Eintrag, dessen Geraet nicht angeschlossen ist.
pub const NICHT_VERBUNDEN: &str = "(nicht verbunden)";

/// Tooltip fuer einen fehlenden Eintrag.
pub const FEHLT_TIPP: &str = "Geraet ist nicht angeschlossen (abgesteckt?)";

/// Kurzform eines technischen Namens fuer die schmale Spalte.
///
/// `alsa_input.usb-Sennheiser_Sennheiser_Profile_0226…-00.mono-fallback` wird zu
/// `Sennheiser_Sennheiser_Profile_0226…` – in 65 px Spaltenbreite ist das der
/// Teil, der das Geraet erkennbar macht. Der vollstaendige Name steht im Tooltip.
pub fn kurzname(name: &str) -> String {
    let ohne_klasse = [
        "alsa_input.usb-",
        "alsa_output.usb-",
        "alsa_input.pci-",
        "alsa_output.pci-",
        "alsa_input.",
        "alsa_output.",
    ]
    .iter()
    .find_map(|p| name.strip_prefix(p))
    .unwrap_or(name);
    ohne_klasse
        .split('.')
        .next()
        .unwrap_or(ohne_klasse)
        .to_string()
}

/// Ein Eintrag im Aufklappmenue.
#[derive(Clone, Debug, PartialEq)]
pub struct Eintrag {
    /// Technischer Name, wie ihn der Kern erwartet.
    pub name: String,
    /// Anzeigename (bei Monitoren mit Zusatz).
    pub anzeige: String,
    /// Geraet ist gerade nicht im Graphen (gemerkt, aber abgesteckt).
    pub fehlt: bool,
}

/// Was die Oberflaeche vom Graphen braucht - ein Schnappschuss, kein Zustand.
#[derive(Clone, Debug, Default)]
pub struct Bestand {
    pub quellen: Vec<Eintrag>,
    pub senken: Vec<Eintrag>,
    /// Belegung der Hardware-Streifen 1..5.
    pub streifen: Vec<Option<String>>,
    /// Je Streifen 1..5: gemerkte Quelle ist gerade nicht angeschlossen.
    pub streifen_fehlt: Vec<bool>,
    /// Belegung der Hardware-Busse A1..A5.
    pub busse: Vec<(String, Option<String>)>,
    /// Je Bus A1..A5: gemerktes Geraet ist gerade nicht angeschlossen.
    pub busse_fehlt: Vec<bool>,
    /// Je Bus A1..A5: Geraetelautstaerke des Ziels (None = unbekannt).
    pub busse_lautstaerke: Vec<Option<Lautstaerke>>,
    /// Klartext der fehlenden Geraete (fuer die Hinweiszeile).
    pub fehlend: Vec<String>,
    pub fehler: Option<String>,
}

/// Anzeigename eines Eintrags im Aufklappmenue.
///
/// Ein fehlendes Geraet wird **nicht** stillschweigend weggelassen: es steht
/// mit Zusatz in der Liste, damit sichtbar bleibt, wohin der Streifen zeigt.
pub fn eintrag_text(anzeige: &str, fehlt: bool) -> String {
    if fehlt {
        format!("{anzeige} {NICHT_VERBUNDEN}")
    } else {
        anzeige.to_string()
    }
}

/// Bestand aus einem Graphen lesen (rein, damit ohne PipeWire pruefbar).
pub fn bestand_aus(
    graph: Result<Graph, String>,
    streifen: &[Option<String>],
    busse: &[(String, Option<String>)],
) -> Bestand {
    let mut b = Bestand {
        streifen: streifen.to_vec(),
        busse: busse.to_vec(),
        ..Bestand::default()
    };
    let graph = match graph {
        Ok(g) => g,
        Err(e) => {
            b.fehler = Some(e);
            return b;
        }
    };

    b.quellen = inputs::quellen(&graph, true)
        .into_iter()
        .map(|q| Eintrag {
            name: q.name,
            anzeige: q.desc,
            fehlt: false,
        })
        .collect();
    b.senken = devices::sinks(&graph, false)
        .into_iter()
        .map(|s| Eintrag {
            name: s.name,
            anzeige: s.desc,
            fehlt: false,
        })
        .collect();

    // Gemerkte, aber fehlende Geraete: in die Liste aufnehmen und markieren.
    b.streifen_fehlt = streifen
        .iter()
        .map(|q| q.as_ref().is_some_and(|n| graph.knoten(n).is_none()))
        .collect();
    b.busse_fehlt = busse
        .iter()
        .map(|(_, s)| s.as_ref().is_some_and(|n| graph.knoten(n).is_none()))
        .collect();

    for (i, quelle) in streifen.iter().enumerate() {
        let (Some(name), Some(true)) = (quelle, b.streifen_fehlt.get(i).copied()) else {
            continue;
        };
        b.quellen.push(Eintrag {
            name: name.clone(),
            anzeige: kurzname(name),
            fehlt: true,
        });
    }
    for (i, (_, senke)) in busse.iter().enumerate() {
        let (Some(name), Some(true)) = (senke, b.busse_fehlt.get(i).copied()) else {
            continue;
        };
        b.senken.push(Eintrag {
            name: name.clone(),
            anzeige: kurzname(name),
            fehlt: true,
        });
    }

    // Geraetelautstaerke je Bus (nur bei vorhandenem Geraet).
    b.busse_lautstaerke = busse
        .iter()
        .map(|(_, s)| s.as_ref().and_then(|n| graph.lautstaerke(n)))
        .collect();

    for (i, (bus, senke)) in busse.iter().enumerate() {
        if b.busse_fehlt.get(i).copied().unwrap_or(false) {
            if let Some(name) = senke {
                b.fehlend.push(format!("Bus {bus} ({})", kurzname(name)));
            }
        }
    }
    for (i, quelle) in streifen.iter().enumerate() {
        if b.streifen_fehlt.get(i).copied().unwrap_or(false) {
            if let Some(name) = quelle {
                b.fehlend
                    .push(format!("Streifen {} ({})", i + 1, kurzname(name)));
            }
        }
    }
    b
}

/// Bestand aus dem PipeWire-Graph lesen (blockiert; im Thread aufrufen).
pub fn laden() -> Bestand {
    let pw = PipeWire::new();
    let streifen = inputs::mapping().to_vec();
    let map = DeviceMap::load_default();
    let busse: Vec<(String, Option<String>)> = crosspoint::BUS_NAMES[..5]
        .iter()
        .map(|bus| (bus.to_string(), map.get(bus).map(|s| s.to_string())))
        .collect();
    bestand_aus(Zugriff::graph(&pw), &streifen, &busse)
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
    /// Geraetelautstaerke setzen (`None` laesst den Wert stehen).
    fn lautstaerke(
        &self,
        senke: &str,
        volumen: Option<f32>,
        stumm: Option<bool>,
    ) -> Result<(), String>;
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

    fn lautstaerke(
        &self,
        senke: &str,
        volumen: Option<f32>,
        stumm: Option<bool>,
    ) -> Result<(), String> {
        // Derselbe Weg wie die Zuordnung: ein Auftrag an den Registry-Thread,
        // kein wpctl von aussen.
        Zugriff::lautstaerke_setzen(&PipeWire::new(), senke, volumen, stumm)
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

/// Steht die Zuordnung schon so, wie sie geschrieben werden soll?
///
/// Das Nachführen der Aufklappmenüs (alle 5 s, aus dem Graphen) darf eine
/// gespeicherte Zuordnung niemals ändern. Ohne diese Prüfung schrieb die
/// Oberfläche beim bloßen Anzeigen „kein Eingang“ in `inputs.conf` – genau der
/// Weg, auf dem eine Zuordnung bisher stillschweigend verschwand.
pub fn zuordnung_gleich(ziel: Ziel<'_>, quelle: Option<&str>) -> bool {
    let aktuell: Option<String> = match ziel {
        Ziel::Streifen(i) => inputs::mapping()
            .get(i.saturating_sub(1))
            .cloned()
            .flatten(),
        Ziel::Bus(b) => DeviceMap::load_default().get(b).map(str::to_string),
    };
    let gewuenscht = quelle
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(inputs::kanonisch);
    aktuell == gewuenscht
}

/// Beschriftung der Busspalte: `A1 · 45 %`, `A1 · STUMM`, `A1 · FEHLT`.
///
/// Die Gerätelautstärke steht bewusst neben dem Busnamen: bei einem USB-Headset
/// ohne ALSA-Mixer ist sie die einzige Hör-Schraube, und PipeWire stellt beim
/// Neustart seinen eigenen Wert wieder her.
pub fn bus_lautstaerke(bus: &str, lautstaerke: Option<Lautstaerke>, fehlt: bool) -> String {
    let wert = if fehlt {
        "FEHLT".to_string()
    } else {
        match lautstaerke {
            Some(l) => l.text(),
            None => "—".to_string(),
        }
    };
    format!("{bus} · {wert}")
}

/// Ist die Beschriftung ein Hinweis auf ein fehlendes Geraet?
pub fn bus_fehlt_text(bus: &str) -> String {
    format!("{bus} · FEHLT")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crosspoint::pw::Node;
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
        fn lautstaerke(
            &self,
            senke: &str,
            volumen: Option<f32>,
            stumm: Option<bool>,
        ) -> Result<(), String> {
            self.aufrufe
                .borrow_mut()
                .push(format!("lautstaerke {senke} {:?} {:?}", volumen, stumm));
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
            fn lautstaerke(
                &self,
                _s: &str,
                _v: Option<f32>,
                _m: Option<bool>,
            ) -> Result<(), String> {
                Err("Knoten nicht im Graph".into())
            }
        }
        let e = auswahl_anwenden(&Kaputt, Ziel::Bus("A1"), KEIN_AUSGANG).unwrap_err();
        assert!(e.contains("Sink"));
        let e = auswahl_anwenden(&Kaputt, Ziel::Streifen(0), "mic").unwrap_err();
        assert!(e.contains("Port"));
        assert!(Kaputt
            .lautstaerke("x", Some(1.0), Some(false))
            .unwrap_err()
            .contains("Knoten"));
    }

    #[test]
    fn lautstaerke_wird_durchgereicht() {
        let m = Merker::default();
        m.lautstaerke("sink", Some(1.0), Some(false)).unwrap();
        assert_eq!(
            m.aufrufe.borrow().as_slice(),
            ["lautstaerke sink Some(1.0) Some(false)"]
        );
    }

    #[test]
    fn gleiche_zuordnung_wird_nicht_erneut_geschrieben() {
        // Leere Zuordnung und "kein Eingang": nichts zu tun.
        assert!(zuordnung_gleich(Ziel::Streifen(5), None));
        assert!(zuordnung_gleich(Ziel::Bus("A5"), None));
        assert!(zuordnung_gleich(Ziel::Bus("A5"), Some("   ")));
        // Etwas anderes gewünscht: schreiben.
        assert!(!zuordnung_gleich(
            Ziel::Bus("A5"),
            Some("alsa_output.irgendwas")
        ));
        assert!(!zuordnung_gleich(
            Ziel::Streifen(5),
            Some("alsa_input.irgendwas")
        ));
    }

    #[test]
    fn kurzname_macht_die_spalte_lesbar() {
        assert_eq!(
            kurzname("alsa_input.usb-Sennheiser_Sennheiser_Profile_0226044674-00.mono-fallback"),
            "Sennheiser_Sennheiser_Profile_0226044674-00"
        );
        assert_eq!(
            kurzname("alsa_output.pci-0000_03_00.1.hdmi-stereo-extra2"),
            "0000_03_00"
        );
        assert_eq!(kurzname("ohne_praefix"), "ohne_praefix");
        assert_eq!(kurzname(""), "");
    }

    #[test]
    fn busspalte_zeigt_prozent_stumm_und_fehlt() {
        assert_eq!(
            bus_lautstaerke("A1", Some(Lautstaerke::neu(0.091_125, false)), false),
            "A1 · 45 %"
        );
        assert_eq!(
            bus_lautstaerke("A2", Some(Lautstaerke::neu(1.0, false)), false),
            "A2 · 100 %"
        );
        assert_eq!(
            bus_lautstaerke("A3", Some(Lautstaerke::neu(0.091_125, true)), false),
            "A3 · STUMM"
        );
        // Fehlendes Geraet geht vor: kein Prozentwert, sondern FEHLT.
        assert_eq!(
            bus_lautstaerke("A4", Some(Lautstaerke::neu(0.091_125, false)), true),
            "A4 · FEHLT"
        );
        assert_eq!(bus_lautstaerke("A5", None, true), "A5 · FEHLT");
        assert_eq!(bus_lautstaerke("A5", None, false), "A5 · —");
    }

    #[test]
    fn fehlendes_geraet_bleibt_mit_zusatz_in_der_liste() {
        // Graph von Hand: den Test-Dump-Parser gibt es nur in der Kern-Crate.
        let knoten = |id, name: &str, desc: &str, klasse: &str, ls: Option<Lautstaerke>| Node {
            id,
            name: name.to_string(),
            description: desc.to_string(),
            nick: String::new(),
            media_class: klasse.to_string(),
            ist_monitor: false,
            lautstaerke: ls,
        };
        let graph = Graph {
            nodes: vec![
                knoten(
                    1,
                    "alsa_output.da",
                    "Kopfhoerer",
                    "Audio/Sink",
                    Some(Lautstaerke::neu(0.001, false)),
                ),
                knoten(2, "alsa_input.da", "Mikrofon", "Audio/Source", None),
            ],
            ports: Vec::new(),
            links: Vec::new(),
        };
        let streifen = vec![Some("alsa_input.weg".to_string())];
        let busse = vec![
            ("A1".to_string(), Some("alsa_output.da".to_string())),
            ("A2".to_string(), Some("alsa_output.weg".to_string())),
        ];
        let b = bestand_aus(Ok(graph), &streifen, &busse);
        assert_eq!(b.streifen_fehlt, vec![true]);
        assert_eq!(b.busse_fehlt, vec![false, true]);
        assert!(b.fehler.is_none());

        // Fehlende Geraete stehen zusaetzlich in der Liste, als fehlend markiert.
        let weg = b
            .quellen
            .iter()
            .find(|e| e.name == "alsa_input.weg")
            .expect("fehlende Quelle bleibt in der Liste");
        assert!(weg.fehlt);
        assert_eq!(
            eintrag_text(&weg.anzeige, weg.fehlt),
            "weg (nicht verbunden)"
        );
        assert!(b
            .senken
            .iter()
            .any(|e| e.name == "alsa_output.weg" && e.fehlt));
        // Vorhandene Geraete sind unmarkiert.
        assert!(b
            .senken
            .iter()
            .any(|e| e.name == "alsa_output.da" && !e.fehlt));

        // Lautstaerke je Bus: vorhanden -> Wert, fehlend -> nichts.
        assert_eq!(
            b.busse_lautstaerke[0].map(|l| l.text()).as_deref(),
            Some("10 %")
        );
        assert!(b.busse_lautstaerke[1].is_none());

        // Klartext fuer die Hinweiszeile.
        assert_eq!(b.fehlend.len(), 2, "{:?}", b.fehlend);
        assert!(b.fehlend.iter().any(|z| z.starts_with("Streifen 1")));
        assert!(b.fehlend.iter().any(|z| z.starts_with("Bus A2")));
    }

    #[test]
    fn ohne_pipewire_wird_nichts_als_fehlend_behauptet() {
        let streifen = vec![Some("alsa_input.weg".to_string())];
        let b = bestand_aus(Err("keine Verbindung zu PipeWire".into()), &streifen, &[]);
        assert!(b.fehler.is_some());
        assert!(b.streifen_fehlt.is_empty(), "nicht raten");
        assert!(b.fehlend.is_empty());
    }
}
