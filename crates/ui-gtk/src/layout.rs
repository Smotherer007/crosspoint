//! Layout-Engine: Anordnung als eigenes Datenmodell, getrennt vom Token-Satz.
//!
//! Das Fenster wird **aus der Beschreibung** zusammengesetzt. [`Layout::plan`]
//! liefert reine Daten (welche Bereiche in welcher Reihenfolge in welcher
//! Zeile stehen); der Aufbau in `shell.rs` folgt diesem Plan und kennt keine
//! feste Anordnung. Dadurch ist die Anordnung ohne Fenster pruefbar.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::theme::Tokens;

/// Die Bereiche der Oberflaeche. Jeder Bereich ist ein eigenes Widget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bereich {
    Kopf,
    Tape,
    Streifen,
    /// Zuordnungs-Tasten (A1..B3) unter den Streifen - eigener Bereich, weil
    /// er unabhaengig von Streifen und Bussen ein- und ausgeblendet wird.
    Zuordnung,
    Busse,
    Master,
    Makros,
}

impl Bereich {
    pub const ALLE: [Bereich; 7] = [
        Bereich::Kopf,
        Bereich::Tape,
        Bereich::Streifen,
        Bereich::Zuordnung,
        Bereich::Busse,
        Bereich::Master,
        Bereich::Makros,
    ];

    pub fn schluessel(self) -> &'static str {
        match self {
            Bereich::Kopf => "kopf",
            Bereich::Tape => "tape",
            Bereich::Streifen => "streifen",
            Bereich::Zuordnung => "zuordnung",
            Bereich::Busse => "busse",
            Bereich::Master => "master",
            Bereich::Makros => "makros",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Bereich::Kopf => "Kopfzeile",
            Bereich::Tape => "Tape-Leiste",
            Bereich::Streifen => "Streifen",
            Bereich::Zuordnung => "Zuordnungs-Tasten",
            Bereich::Busse => "Busspalte",
            Bereich::Master => "Master",
            Bereich::Makros => "Makros",
        }
    }

    /// Bereiche, die in der Hauptzeile stehen (Streifen, Busse, Master).
    fn ist_mitte(self) -> bool {
        matches!(self, Bereich::Streifen | Bereich::Busse | Bereich::Master)
    }
}

/// Seite der Busspalte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Seite {
    Links,
    Rechts,
}

/// Platz des Masters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MasterPlatz {
    /// Als letzter Zug in der Busspalte (Vorgabe, wie im Vorbild).
    InBusspalte,
    /// Eigene Spalte zwischen Streifen und Bussen.
    EigeneSpalte,
    /// Ganz aussen rechts.
    RechtsAussen,
}

/// Dichte: Faktor auf Faderbreite, Hoehen und Abstaende.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Dichte {
    Kompakt,
    Normal,
    Breit,
}

impl Dichte {
    pub fn faktor(self) -> f64 {
        match self {
            Dichte::Kompakt => 0.82,
            Dichte::Normal => 1.0,
            Dichte::Breit => 1.22,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dichte::Kompakt => "kompakt",
            Dichte::Normal => "normal",
            Dichte::Breit => "breit",
        }
    }
}

/// Sichtbarkeit je Bereich.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sichtbarkeit {
    pub kopf: bool,
    pub tape: bool,
    pub streifen: bool,
    pub zuordnung: bool,
    pub busse: bool,
    pub master: bool,
    pub makros: bool,
}

impl Default for Sichtbarkeit {
    fn default() -> Self {
        Sichtbarkeit {
            kopf: true,
            tape: true,
            streifen: true,
            zuordnung: true,
            busse: true,
            master: true,
            makros: true,
        }
    }
}

impl Sichtbarkeit {
    pub fn get(&self, b: Bereich) -> bool {
        match b {
            Bereich::Kopf => self.kopf,
            Bereich::Tape => self.tape,
            Bereich::Streifen => self.streifen,
            Bereich::Zuordnung => self.zuordnung,
            Bereich::Busse => self.busse,
            Bereich::Master => self.master,
            Bereich::Makros => self.makros,
        }
    }

    pub fn set(&mut self, b: Bereich, an: bool) {
        match b {
            Bereich::Kopf => self.kopf = an,
            Bereich::Tape => self.tape = an,
            Bereich::Streifen => self.streifen = an,
            Bereich::Zuordnung => self.zuordnung = an,
            Bereich::Busse => self.busse = an,
            Bereich::Master => self.master = an,
            Bereich::Makros => self.makros = an,
        }
    }
}

/// Die vollstaendige Anordnung.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub name: String,
    /// Reihenfolge aller Bereiche. Wird beim Aufbau in Zeilen einsortiert.
    pub reihenfolge: Vec<Bereich>,
    pub sichtbar: Sichtbarkeit,
    pub busse_seite: Seite,
    pub master: MasterPlatz,
    pub dichte: Dichte,
    /// Streifenindizes in Anzeigereihenfolge.
    pub streifen_reihenfolge: Vec<usize>,
    /// Hoechstzahl gleichzeitig sichtbarer Streifen (1..=8).
    pub streifen_max: usize,
    /// Makro-Felder eingeklappt (nur Kopfzeile des Bereichs).
    pub makros_eingeklappt: bool,
    /// Wie viele Makro-Knoepfe sichtbar sind. Der Kern kennt 80 Felder; die
    /// Oberflaeche zeigt die ersten `makros_anzahl` davon (Vorgabe 5).
    pub makros_anzahl: usize,
}

/// Zulaessige Anzahlen sichtbarer Makro-Felder.
pub const MAKROS_ANZAHL: [usize; 7] = [5, 8, 10, 16, 20, 40, 80];

/// Vorgabe, wenn nichts anderes eingestellt ist.
pub const MAKROS_ANZAHL_VORGABE: usize = 5;

/// Naechstliegende zulaessige Anzahl: ungueltige Werte fallen auf die Vorgabe
/// zurueck (nie auf "alles", das waere ein 80-Feld-Raster aus Versehen).
pub fn makros_anzahl_pruefen(n: usize) -> usize {
    if MAKROS_ANZAHL.contains(&n) {
        n
    } else {
        MAKROS_ANZAHL_VORGABE
    }
}

impl Default for Layout {
    fn default() -> Self {
        Layout::vorgabe("klassisch")
    }
}

impl Layout {
    /// Vorgabe `klassisch`: Streifen links, Busse rechts, Tape oben, Makros unten.
    pub fn vorgabe(name: &str) -> Layout {
        let mut l = Layout {
            name: name.to_string(),
            reihenfolge: vec![
                Bereich::Kopf,
                Bereich::Tape,
                Bereich::Streifen,
                Bereich::Zuordnung,
                Bereich::Busse,
                Bereich::Master,
                Bereich::Makros,
            ],
            sichtbar: Sichtbarkeit::default(),
            busse_seite: Seite::Rechts,
            master: MasterPlatz::InBusspalte,
            dichte: Dichte::Normal,
            streifen_reihenfolge: (0..8).collect(),
            streifen_max: 8,
            makros_eingeklappt: false,
            makros_anzahl: MAKROS_ANZAHL_VORGABE,
        };
        match name {
            "breit" => {
                l.dichte = Dichte::Breit;
                l.master = MasterPlatz::EigeneSpalte;
                l.busse_seite = Seite::Rechts;
            }
            "kompakt" => {
                l.dichte = Dichte::Kompakt;
                l.busse_seite = Seite::Rechts;
                l.master = MasterPlatz::InBusspalte;
                l.makros_eingeklappt = true;
            }
            "nur-streifen" => {
                l.sichtbar = Sichtbarkeit {
                    kopf: true,
                    tape: false,
                    streifen: true,
                    zuordnung: false,
                    busse: false,
                    master: false,
                    makros: false,
                };
                l.reihenfolge = vec![Bereich::Kopf, Bereich::Streifen];
            }
            _ => {}
        }
        l
    }

    /// Alle eingebauten Vorgaben in Dateireihenfolge.
    pub fn vorgaben() -> Vec<(&'static str, Layout)> {
        ["klassisch", "breit", "kompakt", "nur-streifen"]
            .iter()
            .map(|n| (*n, Layout::vorgabe(n)))
            .collect()
    }

    /// Anordnung in drei Zeilen uebersetzen.
    pub fn plan(&self) -> Plan {
        let mut oben = Vec::new();
        let mut mitte = Vec::new();
        let mut unten = Vec::new();

        // Reihenfolge respektieren, aber Dubletten und Unbekanntes verwerfen.
        let mut gesehen = Vec::new();
        for b in self.reihenfolge.iter().copied() {
            if !self.sichtbar.get(b) || gesehen.contains(&b) {
                continue;
            }
            if b == Bereich::Zuordnung {
                // Gehoert zum Streifenblock, keine eigene Zeile.
                continue;
            }
            gesehen.push(b);
            if b.ist_mitte() {
                mitte.push(b);
            } else if b == Bereich::Makros {
                unten.push(b);
            } else {
                oben.push(b);
            }
        }

        // Seite der Busspalte und Platz des Masters bestimmen die Reihenfolge in
        // der Hauptzeile - nicht die Reihenfolge in `reihenfolge`.
        mitte.retain(|b| *b != Bereich::Busse && *b != Bereich::Master);
        let streifen_da = gesehen.contains(&Bereich::Streifen);
        let busse_da = gesehen.contains(&Bereich::Busse);
        let master_allein = busse_da && self.master != MasterPlatz::InBusspalte;

        let mut haupt = Vec::new();
        if self.busse_seite == Seite::Links {
            if busse_da {
                haupt.push(Bereich::Busse);
            }
            if master_allein && self.master == MasterPlatz::EigeneSpalte {
                haupt.push(Bereich::Master);
            }
            if streifen_da {
                haupt.push(Bereich::Streifen);
            }
        } else {
            if streifen_da {
                haupt.push(Bereich::Streifen);
            }
            if master_allein && self.master == MasterPlatz::EigeneSpalte {
                haupt.push(Bereich::Master);
            }
            if busse_da {
                haupt.push(Bereich::Busse);
            }
        }
        if master_allein && self.master == MasterPlatz::RechtsAussen {
            haupt.push(Bereich::Master);
        }
        // Master allein sichtbar, ohne Busse: eigene Spalte am Rand.
        if !busse_da && gesehen.contains(&Bereich::Master) {
            haupt.push(Bereich::Master);
        }

        Plan {
            oben,
            mitte: haupt,
            unten,
        }
    }

    /// Prueft die Beschreibung und liefert Logzeilen fuer Korrekturen.
    ///
    /// Ungueltige Layouts (kein sichtbarer Bereich, falsche Streifenindizes)
    /// werden auf `klassisch` zurueckgefuehrt.
    pub fn bereinigen(&mut self) -> Vec<String> {
        let mut log = Vec::new();
        if self.reihenfolge.is_empty() {
            self.reihenfolge = Layout::vorgabe("klassisch").reihenfolge;
            log.push("Layout ohne Reihenfolge - Vorgabe 'klassisch' benutzt".into());
        }
        // Fehlende Bereiche werden ergaenzt: ein unvollstaendiger Eintrag soll
        // keinen sichtbaren Bereich stillschweigend verschlucken.
        for b in Bereich::ALLE {
            if !self.reihenfolge.contains(&b) {
                self.reihenfolge.push(b);
                log.push(format!(
                    "Bereich '{}' in der Reihenfolge ergaenzt",
                    b.schluessel()
                ));
            }
        }
        let mut sauber: Vec<usize> = Vec::new();
        for i in self.streifen_reihenfolge.iter().copied() {
            if i < 8 && !sauber.contains(&i) {
                sauber.push(i);
            } else {
                log.push(format!(
                    "Streifenindex {i} ungueltig oder doppelt - entfernt"
                ));
            }
        }
        for i in 0..8 {
            if !sauber.contains(&i) {
                sauber.push(i);
            }
        }
        self.streifen_reihenfolge = sauber;
        if self.streifen_max == 0 || self.streifen_max > 8 {
            log.push(format!(
                "streifen_max {} ungueltig - auf 8 gesetzt",
                self.streifen_max
            ));
            self.streifen_max = 8;
        }
        if !Bereich::ALLE
            .iter()
            .any(|b| self.sichtbar.get(*b) && b.ist_mitte())
        {
            log.push("kein Hauptbereich sichtbar - Layout 'klassisch'".into());
            let v = Layout::vorgabe("klassisch");
            let name = self.name.clone();
            *self = Layout { name, ..v };
        }
        log
    }

    /// Sichtbare Streifenindizes in Anzeigereihenfolge.
    pub fn sichtbare_streifen(&self) -> Vec<usize> {
        self.streifen_reihenfolge
            .iter()
            .copied()
            .take(self.streifen_max.min(8))
            .collect()
    }
}

/// Feste Masse, die sich aus Tokens und Dichte ergeben.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Masse {
    pub fader_breite: f64,
    pub fader_hoehe: f64,
    pub pegel_breite: f64,
    pub abstand: f64,
    pub streifen_breite: f64,
    pub schrift_basis: f64,
    pub schrift_klein: f64,
    pub schrift_gross: f64,
}

impl Masse {
    pub fn aus(t: &Tokens, d: Dichte) -> Masse {
        let f = d.faktor();
        Masse {
            fader_breite: (t.fader_breite * f).clamp(4.0, 26.0),
            fader_hoehe: t.fader_hoehe * f,
            pegel_breite: (t.pegel_breite * f).clamp(4.0, 22.0),
            abstand: t.abstand * f,
            streifen_breite: 104.0 * f,
            schrift_basis: t.schrift_basis,
            schrift_klein: t.schrift_klein,
            schrift_gross: t.schrift_gross,
        }
    }
}

/* ==================================================================
Spaltenbreiten aus der Fensterbreite
================================================================== */

/// Grenzen der Spaltenbreiten in Pixeln.
///
/// Innerhalb dieser Grenzen darf sich das Pult dehnen; darunter wird
/// **gescrollt**, statt Zuege abzuschneiden. Die Untergrenzen sind
/// Zeichengrenzen (Fader + Pegel + zwei Beschriftungszeilen passen noch),
/// keine Geschmacksfrage.
pub const STREIFEN_MIN: f64 = 88.0;
pub const STREIFEN_MAX: f64 = 150.0;
pub const BUS_MIN: f64 = 64.0;
pub const BUS_MAX: f64 = 120.0;
pub const MASTER_MIN: f64 = 96.0;
pub const MASTER_MAX: f64 = 140.0;

/// Abstand zwischen zwei Zuegen im Streifenblock bzw. in der Busspalte.
pub const STREIFEN_ABSTAND: f64 = 4.0;
pub const BUS_ABSTAND: f64 = 3.0;

/// Rand links und rechts je Block (Streifen, Busspalte).
///
/// Ohne ihn klebten beim schmalsten Pult die Randzuege am Fensterrand; er
/// gehoert deshalb zur Mindestbreite und nicht zum Zierrat.
pub const BLOCK_RAND: f64 = 4.0;

/// Gewichte der Aufteilung: ein Streifen zaehlt 1,0, ein Bus 0,62, der Master
/// 0,85. Busse sind schmaler (kuerzere Namen, kein EQ/Pan), sollen aber auch
/// nicht auf Faderbreite zusammenschrumpfen.
const GEWICHT_STREIFEN: f64 = 1.0;
const GEWICHT_BUS: f64 = 0.62;
const GEWICHT_MASTER: f64 = 0.85;

/// Ergebnis der Breitenrechnung: was passt in welche Fensterbreite.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spaltenbreiten {
    /// Breite eines Streifens (88..150).
    pub streifen: f64,
    /// Breite eines Busses (64..120).
    pub bus: f64,
    /// Breite des Masters (96..140).
    pub master: f64,
    /// Breite des ganzen Pults (Streifen + Busspalte + Master + alle
    /// Abstaende und Raender) bei diesen Spaltenbreiten.
    pub benoetigt: f64,
    /// Kleinste Breite, bei der das Pult ohne Scrollen auskommt (alle Spalten
    /// auf ihrer Untergrenze).
    pub mindestens: f64,
    /// Ungenutzter Rest bei sehr breiten Fenstern (alle Spalten am Maximum).
    pub luecke: f64,
    /// Fenster schmaler als [`Self::mindestens`]: horizontal scrollen, die
    /// Spalten bleiben auf ihrer Untergrenze.
    pub scrollt: bool,
}

impl Spaltenbreiten {
    /// Breite, die der Hauptblock bei dieser Fensterbreite belegt.
    pub fn zeile(&self) -> String {
        format!(
            "Streifen {:.0} px, Busse {:.0} px, Master {:.0} px -> {} px von {} px{}",
            self.streifen,
            self.bus,
            self.master,
            self.benoetigt,
            self.mindestens,
            if self.scrollt {
                " (zu schmal: Leiste)"
            } else {
                ""
            }
        )
    }
}

/// Spaltenbreiten fuer eine verfuegbare Breite ausrechnen.
///
/// `fenster` ist die Breite, die dem Pult wirklich zur Verfuegung steht (also
/// die Fensterbreite abzueglich einer senkrechten Leiste), `abstand` der
/// Grundabstand des Themes (Rand aussen, Luecke zwischen den Bloecken).
///
/// Verfahren: alles, was keine Spalte ist (Raender, Abstaende), wird
/// abgezogen; der Rest wird im Verhaeltnis der Gewichte aufgeteilt
/// (Wasserfuellung). Faellt eine Spalte dabei unter ihre Unter- oder ueber
/// ihre Obergrenze, wird sie auf die Grenze gesetzt und der Rest neu verteilt.
/// Bleibt am Ende etwas uebrig, ist das Fenster breiter als das Pult je werden
/// darf - der Rest bleibt als Luft (`luecke`).
pub fn spaltenbreiten(
    fenster: f64,
    abstand: f64,
    streifen: usize,
    busse: usize,
    master: bool,
) -> Spaltenbreiten {
    let s = streifen.max(1) as f64;
    let b = busse as f64;
    let m = if master { 1.0 } else { 0.0 };

    // Was keine Spalte ist: Aussenraender, Luecke zwischen den Bloecken,
    // Abstaende zwischen den Zuegen, Rand der Bloecke.
    let mut fest = 2.0 * abstand + 2.0 * BLOCK_RAND;
    if b + m > 0.0 {
        fest += abstand + 2.0 * BLOCK_RAND;
    }
    fest += (s - 1.0) * STREIFEN_ABSTAND;
    if b + m > 1.0 {
        fest += (b + m - 1.0) * BUS_ABSTAND;
    }

    let mindestens = fest + s * STREIFEN_MIN + b * BUS_MIN + m * MASTER_MIN;
    if !(fenster.is_finite() && fenster > 0.0) || fenster < mindestens {
        return Spaltenbreiten {
            streifen: STREIFEN_MIN,
            bus: BUS_MIN,
            master: MASTER_MIN,
            benoetigt: mindestens,
            mindestens,
            luecke: 0.0,
            scrollt: true,
        };
    }

    // (Anzahl, Gewicht, Minimum, Maximum) je Spaltenart.
    let typen = [
        (s, GEWICHT_STREIFEN, STREIFEN_MIN, STREIFEN_MAX),
        (b, GEWICHT_BUS, BUS_MIN, BUS_MAX),
        (m, GEWICHT_MASTER, MASTER_MIN, MASTER_MAX),
    ];
    let mut breite = [0.0f64; 3];
    let mut fix = [false; 3];
    let mut frei = fenster - fest;

    // Hoechstens eine Klemmung je Runde; drei Arten, also reichen vier Runden.
    for _ in 0..4 {
        let summe: f64 = typen
            .iter()
            .enumerate()
            .filter(|(i, t)| !fix[*i] && t.0 > 0.0)
            .map(|(_, t)| t.0 * t.1)
            .sum();
        if summe <= 0.0 {
            break;
        }
        let einheit = frei / summe;
        let mut klemmung = None;
        for (i, t) in typen.iter().enumerate() {
            if fix[i] || t.0 <= 0.0 {
                continue;
            }
            let roh = einheit * t.1;
            if roh < t.2 {
                klemmung = Some((i, t.2));
                break;
            }
            if roh > t.3 {
                klemmung = Some((i, t.3));
                break;
            }
        }
        match klemmung {
            Some((i, grenze)) => {
                breite[i] = grenze;
                fix[i] = true;
                frei -= typen[i].0 * grenze;
            }
            None => {
                for (i, t) in typen.iter().enumerate() {
                    if !fix[i] && t.0 > 0.0 {
                        breite[i] = einheit * t.1;
                    }
                }
                break;
            }
        }
    }

    // Auf ganze Pixel: halbe Pixel flimmern bei 1-px-Linien. Abgerundet wird
    // auf die Spaltenbreite, der Rest (0..8 px je Streifen) kommt als ganze
    // Pixel zu den Streifen - so bleibt die Summe immer innerhalb der
    // Fensterbreite und waechst trotzdem mit ihr.
    let mut streifen = breite[0].floor().clamp(STREIFEN_MIN, STREIFEN_MAX);
    let bus = if b > 0.0 {
        breite[1].floor().clamp(BUS_MIN, BUS_MAX)
    } else {
        0.0
    };
    let master = if m > 0.0 {
        breite[2].floor().clamp(MASTER_MIN, MASTER_MAX)
    } else {
        0.0
    };
    let mut benoetigt = fest + s * streifen + b * bus + m * master;
    let rest = fenster - benoetigt;
    if rest >= s {
        let zusatz = (rest / s).floor().min(STREIFEN_MAX - streifen);
        if zusatz > 0.0 {
            streifen += zusatz;
            benoetigt = fest + s * streifen + b * bus + m * master;
        }
    }
    Spaltenbreiten {
        streifen,
        bus,
        master,
        benoetigt,
        mindestens,
        luecke: (fenster - benoetigt).max(0.0),
        scrollt: false,
    }
}

/// Zeilenplan: was steht oben, in der Hauptzeile, unten.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Plan {
    pub oben: Vec<Bereich>,
    pub mitte: Vec<Bereich>,
    pub unten: Vec<Bereich>,
}

impl Plan {
    /// Alle eingesetzten Bereiche in Aufbaureihenfolge (Tests, Diagnose).
    #[allow(dead_code)]
    pub fn alle(&self) -> impl Iterator<Item = Bereich> + '_ {
        self.oben
            .iter()
            .chain(self.mitte.iter())
            .chain(self.unten.iter())
            .copied()
    }
}

/// Dateiform: nur Abweichungen, alles optional.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LayoutDatei {
    #[serde(default)]
    pub reihenfolge: Option<Vec<String>>,
    #[serde(default)]
    pub sichtbar: Option<BTreeMap<String, bool>>,
    #[serde(default)]
    pub busse_seite: Option<Seite>,
    #[serde(default)]
    pub master: Option<MasterPlatz>,
    #[serde(default)]
    pub dichte: Option<Dichte>,
    #[serde(default)]
    pub streifen_reihenfolge: Option<Vec<usize>>,
    #[serde(default)]
    pub streifen_max: Option<usize>,
    #[serde(default)]
    pub makros_eingeklappt: Option<bool>,
}

impl LayoutDatei {
    pub fn von(l: &Layout) -> LayoutDatei {
        let mut sichtbar = BTreeMap::new();
        for b in Bereich::ALLE {
            sichtbar.insert(b.schluessel().to_string(), l.sichtbar.get(b));
        }
        LayoutDatei {
            reihenfolge: Some(
                l.reihenfolge
                    .iter()
                    .map(|b| b.schluessel().into())
                    .collect(),
            ),
            sichtbar: Some(sichtbar),
            busse_seite: Some(l.busse_seite),
            master: Some(l.master),
            dichte: Some(l.dichte),
            streifen_reihenfolge: Some(l.streifen_reihenfolge.clone()),
            streifen_max: Some(l.streifen_max),
            makros_eingeklappt: Some(l.makros_eingeklappt),
        }
    }

    /// Auf eine Vorgabe anwenden; unbekannte Felder/werte bleiben bei der
    /// Vorgabe, fehlende ebenfalls.
    pub fn anwenden(&self, basis: &Layout) -> Layout {
        let mut l = basis.clone();
        if let Some(names) = &self.reihenfolge {
            let mut reihe = Vec::new();
            for n in names {
                if let Some(b) = Bereich::ALLE
                    .iter()
                    .find(|b| b.schluessel().eq_ignore_ascii_case(n.trim()))
                {
                    reihe.push(*b);
                }
            }
            if !reihe.is_empty() {
                l.reihenfolge = reihe;
            }
        }
        if let Some(s) = &self.sichtbar {
            for (k, v) in s {
                if let Some(b) = Bereich::ALLE
                    .iter()
                    .find(|b| b.schluessel().eq_ignore_ascii_case(k.trim()))
                {
                    l.sichtbar.set(*b, *v);
                }
            }
        }
        if let Some(v) = self.busse_seite {
            l.busse_seite = v;
        }
        if let Some(v) = self.master {
            l.master = v;
        }
        if let Some(v) = self.dichte {
            l.dichte = v;
        }
        if let Some(v) = &self.streifen_reihenfolge {
            l.streifen_reihenfolge = v.clone();
        }
        if let Some(v) = self.streifen_max {
            l.streifen_max = v;
        }
        if let Some(v) = self.makros_eingeklappt {
            l.makros_eingeklappt = v;
        }
        l
    }
}

/// Abschnitt `layout` aus `ui.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LayoutAbschnitt {
    #[serde(default)]
    pub aktiv: Option<String>,
    /// Anzahl sichtbarer Makro-Felder (5, 8, 10, 16, 20, 40 oder 80).
    ///
    /// Steht im `layout`-Abschnitt und nicht im einzelnen Layout: die Zahl
    /// gehoert zur Bedienung, nicht zur Anordnung, und soll auch fuer die
    /// eingebauten Layouts erhalten bleiben (die nicht in der Datei stehen).
    #[serde(default)]
    pub makros_anzahl: Option<usize>,
    /// Eigene Layouts; Werte legen sich ueber die gleichnamige Vorgabe.
    #[serde(default)]
    pub layouts: Option<BTreeMap<String, LayoutDatei>>,
}

/// Alle bekannten Layouts plus der aktive Name.
#[derive(Clone, Debug)]
pub struct LayoutSet {
    pub aktiv: String,
    pub layouts: BTreeMap<String, Layout>,
}

impl Default for LayoutSet {
    fn default() -> Self {
        LayoutSet {
            aktiv: "klassisch".into(),
            layouts: Layout::vorgaben()
                .into_iter()
                .map(|(n, l)| (n.to_string(), l))
                .collect(),
        }
    }
}

impl LayoutSet {
    pub fn namen(&self) -> Vec<String> {
        self.layouts.keys().cloned().collect()
    }

    pub fn aktuell(&self) -> Layout {
        self.layouts.get(&self.aktiv).cloned().unwrap_or_default()
    }

    /// Aktives Layout setzen; unbekannt -> `klassisch` mit Logzeile.
    pub fn aktiv_setzen(&mut self, name: &str) -> Vec<String> {
        let mut log = Vec::new();
        if self.layouts.contains_key(name) {
            self.aktiv = name.to_string();
        } else {
            log.push(format!("Layout '{name}' unbekannt - es bleibt 'klassisch'"));
            self.aktiv = "klassisch".to_string();
        }
        log
    }

    pub fn aus_abschnitt(abschnitt: Option<&LayoutAbschnitt>) -> (LayoutSet, Vec<String>) {
        let mut log = Vec::new();
        let mut set = LayoutSet::default();
        let Some(abschnitt) = abschnitt else {
            return (set, log);
        };
        for (name, datei) in abschnitt.layouts.iter().flatten() {
            if name.trim().is_empty() {
                log.push("Layout ohne Namen uebersprungen".into());
                continue;
            }
            let basis = set
                .layouts
                .get(name)
                .cloned()
                .unwrap_or_else(|| Layout::vorgabe("klassisch"));
            let mut l = datei.anwenden(&basis);
            l.name = name.clone();
            log.extend(l.bereinigen());
            set.layouts.insert(name.clone(), l);
        }
        if let Some(aktiv) = abschnitt.aktiv.as_deref() {
            log.extend(set.aktiv_setzen(aktiv));
        }
        if let Some(n) = abschnitt.makros_anzahl {
            // Ungueltige Werte (0, 7, 999) fallen auf die Vorgabe 5 zurueck.
            let n = makros_anzahl_pruefen(n);
            if let Some(l) = set.layouts.get_mut(&set.aktiv.clone()) {
                l.makros_anzahl = n;
            }
        }
        (set, log)
    }

    /// Nur eigene (nicht eingebaute) Layouts serialisieren.
    pub fn als_abschnitt(&self, eigene: &[String]) -> LayoutAbschnitt {
        let mut layouts = BTreeMap::new();
        for name in eigene {
            if let Some(l) = self.layouts.get(name) {
                layouts.insert(name.clone(), LayoutDatei::von(l));
            }
        }
        LayoutAbschnitt {
            aktiv: Some(self.aktiv.clone()),
            makros_anzahl: Some(self.aktuell().makros_anzahl),
            layouts: if layouts.is_empty() {
                None
            } else {
                Some(layouts)
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vorgaben_sind_vollstaendig() {
        let namen: Vec<_> = Layout::vorgaben().iter().map(|(n, _)| *n).collect();
        assert_eq!(namen, ["klassisch", "breit", "kompakt", "nur-streifen"]);
        let k = Layout::vorgabe("kompakt");
        assert_eq!(k.dichte, Dichte::Kompakt);
        assert!(k.makros_eingeklappt);
        let b = Layout::vorgabe("breit");
        assert_eq!(b.master, MasterPlatz::EigeneSpalte);
        assert_eq!(b.dichte, Dichte::Breit);
    }

    #[test]
    fn nur_streifen_blendet_busse_und_makros_aus() {
        let l = Layout::vorgabe("nur-streifen");
        let p = l.plan();
        assert!(p.oben.contains(&Bereich::Kopf));
        assert!(!p.oben.contains(&Bereich::Tape));
        assert_eq!(p.mitte, vec![Bereich::Streifen]);
        assert!(p.unten.is_empty());
        let alle: Vec<_> = p.alle().collect();
        assert!(!alle.contains(&Bereich::Busse));
        assert!(!alle.contains(&Bereich::Master));
        assert!(!alle.contains(&Bereich::Makros));
    }

    #[test]
    fn busseite_vertauscht_die_hauptzeile() {
        let mut l = Layout::vorgabe("klassisch");
        assert_eq!(l.plan().mitte, vec![Bereich::Streifen, Bereich::Busse]);
        l.busse_seite = Seite::Links;
        assert_eq!(l.plan().mitte, vec![Bereich::Busse, Bereich::Streifen]);
        // Master in eigener Spalte steht zwischen Streifen und Bussen.
        l.master = MasterPlatz::EigeneSpalte;
        assert_eq!(
            l.plan().mitte,
            vec![Bereich::Busse, Bereich::Master, Bereich::Streifen]
        );
        l.busse_seite = Seite::Rechts;
        assert_eq!(
            l.plan().mitte,
            vec![Bereich::Streifen, Bereich::Master, Bereich::Busse]
        );
        l.master = MasterPlatz::RechtsAussen;
        assert_eq!(
            l.plan().mitte,
            vec![Bereich::Streifen, Bereich::Busse, Bereich::Master]
        );
    }

    #[test]
    fn sichtbarkeit_wirkt() {
        let mut l = Layout::vorgabe("klassisch");
        l.sichtbar.set(Bereich::Makros, false);
        assert!(l.plan().unten.is_empty());
        l.sichtbar.set(Bereich::Kopf, false);
        assert!(!l.plan().oben.contains(&Bereich::Kopf));
        l.sichtbar.set(Bereich::Busse, false);
        assert!(!l.plan().mitte.contains(&Bereich::Busse));
    }

    #[test]
    fn dichte_aendert_masse() {
        let t = Tokens::default();
        let n = Masse::aus(&t, Dichte::Normal);
        let k = Masse::aus(&t, Dichte::Kompakt);
        let b = Masse::aus(&t, Dichte::Breit);
        assert!(k.fader_breite < n.fader_breite);
        assert!(n.fader_breite < b.fader_breite);
        assert!(k.streifen_breite < b.streifen_breite);
        assert!(k.abstand < n.abstand);
    }

    #[test]
    fn streifenreihenfolge_wirkt() {
        let mut l = Layout::vorgabe("klassisch");
        l.streifen_reihenfolge = vec![7, 6, 5, 4, 3, 2, 1, 0];
        assert_eq!(l.sichtbare_streifen(), vec![7, 6, 5, 4, 3, 2, 1, 0]);
        l.streifen_max = 3;
        assert_eq!(l.sichtbare_streifen(), vec![7, 6, 5]);
    }

    #[test]
    fn layout_rundlauf_ueber_die_datei() {
        let mut set = LayoutSet::default();
        let mut eigen = Layout::vorgabe("klassisch");
        eigen.name = "studio".into();
        eigen.busse_seite = Seite::Links;
        eigen.dichte = Dichte::Kompakt;
        eigen.streifen_reihenfolge = vec![3, 1, 0, 2, 4, 5, 6, 7];
        set.layouts.insert("studio".into(), eigen.clone());
        set.aktiv = "studio".into();

        let abschnitt = set.als_abschnitt(&["studio".to_string()]);
        let datei = serde_json::json!({ "layout": abschnitt });
        let text = datei.to_string();
        let gelesen: serde_json::Value = serde_json::from_str(&text).unwrap();
        let abschnitt: LayoutAbschnitt =
            serde_json::from_value(gelesen.get("layout").cloned().unwrap()).unwrap();
        let (zurueck, log) = LayoutSet::aus_abschnitt(Some(&abschnitt));
        assert!(log.is_empty(), "{log:?}");
        assert_eq!(zurueck.aktiv, "studio");
        let l = zurueck.aktuell();
        assert_eq!(l.busse_seite, Seite::Links);
        assert_eq!(l.dichte, Dichte::Kompakt);
        assert_eq!(l.streifen_reihenfolge, vec![3, 1, 0, 2, 4, 5, 6, 7]);
    }

    #[test]
    fn fehlende_felder_bleiben_bei_der_vorgabe() {
        let abschnitt: LayoutAbschnitt = serde_json::from_value(serde_json::json!({
            "aktiv": "klassisch",
            "layouts": { "klassisch": { "dichte": "kompakt" } }
        }))
        .unwrap();
        let (set, log) = LayoutSet::aus_abschnitt(Some(&abschnitt));
        assert!(log.is_empty());
        let l = set.aktuell();
        assert_eq!(l.dichte, Dichte::Kompakt);
        assert_eq!(l.busse_seite, Seite::Rechts);
        assert!(l.sichtbar.get(Bereich::Makros));
    }

    #[test]
    fn unbekanntes_layout_faellt_auf_klassisch() {
        let abschnitt: LayoutAbschnitt = serde_json::from_value(serde_json::json!({
            "aktiv": "gibtsnicht"
        }))
        .unwrap();
        let (set, log) = LayoutSet::aus_abschnitt(Some(&abschnitt));
        assert_eq!(set.aktiv, "klassisch");
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn makro_anzahl_vorgabe_ist_fuenf() {
        let l = Layout::vorgabe("klassisch");
        assert_eq!(l.makros_anzahl, MAKROS_ANZAHL_VORGABE);
        assert_eq!(l.makros_anzahl, 5);
    }

    #[test]
    fn makro_anzahl_wird_geladen() {
        let abschnitt: LayoutAbschnitt = serde_json::from_value(serde_json::json!({
            "aktiv": "klassisch",
            "makros_anzahl": 10
        }))
        .unwrap();
        let (set, log) = LayoutSet::aus_abschnitt(Some(&abschnitt));
        assert!(log.is_empty(), "{log:?}");
        assert_eq!(set.aktuell().makros_anzahl, 10);
    }

    #[test]
    fn ungueltige_makro_anzahl_faellt_auf_fuenf() {
        for falsch in [0usize, 1, 7, 12, 81, 999] {
            let abschnitt: LayoutAbschnitt = serde_json::from_value(serde_json::json!({
                "makros_anzahl": falsch
            }))
            .unwrap();
            let (set, _) = LayoutSet::aus_abschnitt(Some(&abschnitt));
            assert_eq!(
                set.aktuell().makros_anzahl,
                5,
                "makros_anzahl {falsch} muss auf 5 zurueckfallen"
            );
        }
    }

    #[test]
    fn alle_zulaessigen_makro_anzahlen_bleiben() {
        for n in MAKROS_ANZAHL {
            let abschnitt: LayoutAbschnitt = serde_json::from_value(serde_json::json!({
                "makros_anzahl": n
            }))
            .unwrap();
            let (set, _) = LayoutSet::aus_abschnitt(Some(&abschnitt));
            assert_eq!(set.aktuell().makros_anzahl, n);
        }
    }

    #[test]
    fn makro_anzahl_geht_durch_die_datei_und_zurueck() {
        let mut set = LayoutSet::default();
        set.layouts.get_mut("klassisch").unwrap().makros_anzahl = 20;
        let abschnitt = set.als_abschnitt(&[]);
        assert_eq!(abschnitt.makros_anzahl, Some(20));
        let text = serde_json::to_string(&abschnitt).unwrap();
        let zurueck: LayoutAbschnitt = serde_json::from_str(&text).unwrap();
        let (neu, _) = LayoutSet::aus_abschnitt(Some(&zurueck));
        assert_eq!(neu.aktuell().makros_anzahl, 20);
    }

    #[test]
    fn ungueltige_layoutwerte_werden_bereinigt() {
        let abschnitt: LayoutAbschnitt = serde_json::from_value(serde_json::json!({
            "layouts": { "klassisch": {
                "streifen_reihenfolge": [9, 2, 2, 1],
                "streifen_max": 0,
                "reihenfolge": ["kopf", "quatsch"],
                "sichtbar": { "streifen": true, "busse": false, "master": false, "makros": false }
            }}
        }))
        .unwrap();
        let (set, log) = LayoutSet::aus_abschnitt(Some(&abschnitt));
        assert!(!log.is_empty());
        let l = set.aktuell();
        // Doppelte und zu grosse Indizes entfernt, auf acht aufgefuellt.
        assert_eq!(l.streifen_reihenfolge.len(), 8);
        assert_eq!(&l.streifen_reihenfolge[..3], &[2, 1, 0]);
        assert_eq!(l.streifen_max, 8);
        // Unbekannter Bereichsname wurde verworfen, fehlende Bereiche ergaenzt.
        assert_eq!(l.reihenfolge, Bereich::ALLE.to_vec());
        // Ein sichtbarer Hauptbereich genuegt: Streifen bleibt, Busse aus.
        assert_eq!(l.plan().mitte, vec![Bereich::Streifen]);
        assert!(!l.sichtbar.get(Bereich::Busse));
    }

    #[test]
    fn layout_ohne_hauptbereich_faellt_auf_klassisch() {
        let abschnitt: LayoutAbschnitt = serde_json::from_value(serde_json::json!({
            "layouts": { "klassisch": {
                "sichtbar": { "streifen": false, "busse": false, "master": false }
            }}
        }))
        .unwrap();
        let (set, log) = LayoutSet::aus_abschnitt(Some(&abschnitt));
        assert!(log.iter().any(|z| z.contains("klassisch")));
        let l = set.aktuell();
        assert!(l.plan().mitte.contains(&Bereich::Streifen));
        assert!(l.sichtbar.get(Bereich::Busse));
    }

    #[test]
    fn reihenfolge_der_bereiche_wirkt() {
        let mut l = Layout::vorgabe("klassisch");
        l.reihenfolge = vec![
            Bereich::Kopf,
            Bereich::Streifen,
            Bereich::Busse,
            Bereich::Tape,
        ];
        let p = l.plan();
        assert_eq!(p.oben, vec![Bereich::Kopf, Bereich::Tape]);
        assert!(p.unten.is_empty());
    }

    /* -------- Breitenrechnung aus der Fensterbreite -------- */

    /// Vorgabe: acht Streifen, acht Busse, Master in der Busspalte.
    fn breiten(b: f64) -> Spaltenbreiten {
        spaltenbreiten(b, 8.0, 8, 8, true)
    }

    #[test]
    fn mindestbreite_des_pults_ist_die_summe_der_untergrenzen() {
        // 8*88 + 8*64 + 96 Spalten + 92 px Raender und Abstaende.
        assert_eq!(breiten(100.0).mindestens, 1404.0);
        assert_eq!(breiten(100.0).streifen, STREIFEN_MIN);
        assert_eq!(breiten(100.0).bus, BUS_MIN);
        assert_eq!(breiten(100.0).master, MASTER_MIN);
        assert!(breiten(100.0).scrollt);
    }

    #[test]
    fn bei_1600_passt_alles_ohne_abschneiden() {
        let b = breiten(1600.0);
        assert!(!b.scrollt, "1600 px muessen ohne Leiste passen");
        assert!(b.benoetigt <= 1600.0, "benoetigt {} > 1600", b.benoetigt);
        assert!(
            (STREIFEN_MIN..=STREIFEN_MAX).contains(&b.streifen),
            "Streifen {}",
            b.streifen
        );
        assert!((BUS_MIN..=BUS_MAX).contains(&b.bus), "Bus {}", b.bus);
        assert!(
            (MASTER_MIN..=MASTER_MAX).contains(&b.master),
            "Master {}",
            b.master
        );
        // Die Vorgabe ist nicht die Untergrenze: bei 1600 ist Luft fuer breitere
        // Streifen als 88 px (sonst waere das Pult nur zufaellig passend).
        assert!(b.streifen >= 100.0, "Streifen nur {} px", b.streifen);
        assert!(b.bus >= 60.0, "Busse nur {} px", b.bus);
        assert!(b.luecke < 10.0, "Rest {} px bleibt ungenutzt", b.luecke);
    }

    #[test]
    fn bei_1400_reicht_die_breite_nicht_dann_leiste() {
        let b = breiten(1400.0);
        assert!(b.scrollt, "1400 px muessen scrollen (mindestens 1404)");
        assert_eq!(b.streifen, STREIFEN_MIN);
        assert_eq!(b.bus, BUS_MIN);
        assert_eq!(b.master, MASTER_MIN);
        assert_eq!(b.benoetigt, b.mindestens);
        assert!(b.benoetigt > 1400.0);
    }

    #[test]
    fn bei_2560_werden_die_streifen_breiter() {
        let klein = breiten(1600.0);
        let gross = breiten(2560.0);
        assert!(gross.streifen > klein.streifen);
        assert_eq!(gross.streifen, STREIFEN_MAX);
        assert_eq!(gross.bus, BUS_MAX);
        assert_eq!(gross.master, MASTER_MAX);
        assert!(!gross.scrollt);
        // Breiter als das Pult werden darf: der Rest bleibt als Luft.
        assert!(gross.luecke > 0.0);
    }

    #[test]
    fn breiten_bleiben_immer_in_den_grenzen() {
        for fenster in [
            0.0, 400.0, 900.0, 1200.0, 1380.0, 1400.0, 1440.0, 1600.0, 1920.0, 2560.0, 3840.0,
        ] {
            let b = breiten(fenster);
            assert!(
                (STREIFEN_MIN..=STREIFEN_MAX).contains(&b.streifen),
                "{fenster}: Streifen {}",
                b.streifen
            );
            assert!(
                (BUS_MIN..=BUS_MAX).contains(&b.bus),
                "{fenster}: Bus {}",
                b.bus
            );
            assert!(
                (MASTER_MIN..=MASTER_MAX).contains(&b.master),
                "{fenster}: Master {}",
                b.master
            );
            // Entweder es passt - dann nie mehr als die Fensterbreite - oder
            // es wird gescrollt und alle Spalten stehen auf ihrer Untergrenze.
            if b.scrollt {
                assert_eq!(b.benoetigt, b.mindestens);
                assert!(fenster < b.mindestens);
            } else {
                assert!(
                    b.benoetigt <= fenster,
                    "{fenster}: benoetigt {}",
                    b.benoetigt
                );
            }
        }
    }

    #[test]
    fn breiten_sind_monoton_und_setzen_sich_aus_den_spalten_zusammen() {
        let mut vorher = 0.0;
        for fenster in (1404..=3000).step_by(37) {
            let b = breiten(fenster as f64);
            assert!(!b.scrollt);
            assert!(
                b.streifen >= vorher,
                "Streifen schrumpfen bei {fenster} px: {} nach {vorher}",
                b.streifen
            );
            vorher = b.streifen;
            // Nachrechnen: Raender + Spalten + Abstaende = benoetigt.
            let nachgerechnet = 92.0 + 8.0 * b.streifen + 8.0 * b.bus + b.master;
            assert_eq!(b.benoetigt, nachgerechnet, "bei {fenster} px");
        }
    }

    #[test]
    fn ohne_busse_und_master_bleibt_nur_der_streifenblock() {
        let b = spaltenbreiten(900.0, 8.0, 8, 0, false);
        assert!(!b.scrollt);
        assert_eq!(b.bus, 0.0);
        assert_eq!(b.master, 0.0);
        assert!(b.streifen <= STREIFEN_MAX);
        assert!(b.benoetigt <= 900.0);
    }

    #[test]
    fn ein_einzelner_streifen_nutzt_die_breite_nicht_aus() {
        // Ein Streifen darf hoechstens 150 px bekommen - der Rest bleibt Luft,
        // statt den Zug ueber die Grenze zu ziehen.
        let b = spaltenbreiten(1600.0, 8.0, 1, 0, false);
        assert_eq!(b.streifen, STREIFEN_MAX);
        assert!(b.luecke > 0.0);
        assert!(b.benoetigt < 1600.0);
    }

    /// Tabelle fuer den Bericht (nicht Teil des normalen Testlaufs):
    ///   cargo test --lib breiten_tabelle -- --ignored --nocapture
    #[test]
    #[ignore]
    fn breiten_tabelle() {
        println!("Fenster | Streifen | Busse | Master | benoetigt | Luft | Leiste");
        for fenster in [
            1200.0, 1280.0, 1400.0, 1440.0, 1600.0, 1920.0, 2560.0, 3440.0,
        ] {
            let b = breiten(fenster);
            println!(
                "{fenster:7.0} | {:8.0} | {:5.0} | {:6.0} | {:9.0} | {:4.0} | {}",
                b.streifen,
                b.bus,
                b.master,
                b.benoetigt,
                b.luecke,
                if b.scrollt { "ja" } else { "nein" }
            );
        }
        assert!(breiten(1600.0).benoetigt <= 1600.0);
    }
}
