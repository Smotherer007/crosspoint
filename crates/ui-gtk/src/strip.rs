//! Der Kanalzug als eigenes Widget: Fader, Pegel, Pan und EQ-Raster werden in
//! `snapshot()` gezeichnet, die Knoepfe (Mute/Solo/Mono) sind echte GTK-Buttons.
//!
//! Gelesen und geschrieben wird direkt in `Arc<Params>`; es gibt keinen
//! Zwischenzustand und keine Serialisierung. Die Farben kommen ausschliesslich
//! aus dem aktiven Token-Satz, die Masse aus Token-Satz x Dichte.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

use crosspoint::engine::Params;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::draw;
use crate::fmt::{self, fader_pos, lin_to_db, pos_db};
use crate::layout::Masse;
use crate::state;
use crate::theme::Tokens;

/// Gemeinsame Geraetelisten fuer alle Zuege: ein Modell je Art, damit ein
/// Nachladen alle Aufklappmenues gleichzeitig aktualisiert.
pub struct GeraeteUi {
    pub quellen: gtk::StringList,
    pub senken: gtk::StringList,
    pub quellen_namen: RefCell<Vec<Option<String>>>,
    pub senken_namen: RefCell<Vec<Option<String>>>,
    /// Sperre, damit das Nachfuehren kein "selected" ausloest.
    pub sperre: Cell<bool>,
    /// Hinweiszeile im Kopf (wird beim Aufbau gesetzt).
    pub hinweis: RefCell<Option<gtk::Label>>,
}

impl GeraeteUi {
    pub fn neu() -> GeraeteUi {
        GeraeteUi {
            quellen: gtk::StringList::new(&[crate::geraete::KEIN_EINGANG]),
            senken: gtk::StringList::new(&[crate::geraete::KEIN_AUSGANG]),
            quellen_namen: RefCell::new(vec![None]),
            senken_namen: RefCell::new(vec![None]),
            sperre: Cell::new(false),
            hinweis: RefCell::new(None),
        }
    }

    /// Meldung im Kopf anzeigen (Erfolg oder Fehler).
    pub fn melden(&self, text: &str, fehler: bool) {
        if let Some(l) = self.hinweis.borrow().as_ref() {
            l.set_text(text);
            if fehler {
                l.add_css_class("fehler");
            } else {
                l.remove_css_class("fehler");
            }
        }
        eprintln!("crosspoint-gtk: {text}");
    }

    pub fn quelle_name(&self, idx: u32) -> String {
        self.quellen_namen
            .borrow()
            .get(idx as usize)
            .cloned()
            .flatten()
            .unwrap_or_default()
    }

    pub fn senke_name(&self, idx: u32) -> String {
        self.senken_namen
            .borrow()
            .get(idx as usize)
            .cloned()
            .flatten()
            .unwrap_or_default()
    }
}

/// Was ein Zug darstellt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rolle {
    Kanal(usize),
    Bus(usize),
    Master,
}

impl Rolle {
    pub fn name(self) -> String {
        match self {
            Rolle::Kanal(i) => crosspoint::STRIP_NAMES
                .get(i)
                .copied()
                .unwrap_or("?")
                .to_string(),
            Rolle::Bus(i) => crosspoint::BUS_NAMES
                .get(i)
                .copied()
                .unwrap_or("?")
                .to_string(),
            Rolle::Master => "MASTER".to_string(),
        }
    }

    pub fn nummer(self) -> String {
        match self {
            Rolle::Kanal(i) => format!("{}", i + 1),
            Rolle::Bus(i) => format!("{}", i + 1),
            Rolle::Master => "SUM".to_string(),
        }
    }

    pub fn untertitel(self) -> String {
        match self {
            // Kurzformen: bei 65 px Spaltenbreite sind "HARDWARE" und
            // "VIRTUELL" zu breit und werden mitten im Wort abgeschnitten.
            Rolle::Kanal(i) if i < 5 => format!("IN {}", i + 1),
            Rolle::Kanal(i) => format!("VIRT {}", i - 4),
            Rolle::Bus(i) if i < 5 => "HW".to_string(),
            Rolle::Bus(_) => "VIRT".to_string(),
            Rolle::Master => "SUM".to_string(),
        }
    }

    pub fn ist_kanal(self) -> bool {
        matches!(self, Rolle::Kanal(_))
    }

    /// (Ziel, Index) fuer `Params::set`/`set_text`.
    pub fn ziel(self) -> (&'static str, usize) {
        match self {
            Rolle::Kanal(i) => ("strip", i),
            Rolle::Bus(i) => ("bus", i),
            Rolle::Master => ("master", 0),
        }
    }
}

/// Summenzustand des Masters. Der Kern hat kein Master-Feld; der Fader bleibt
/// deshalb lokal, Mute wirkt auf alle Busse.
#[derive(Default)]
pub struct Master {
    pub gain: AtomicU32,
    pub mute: AtomicBool,
}

/// Taktgeber: Sekunden seit Start und ob der Audio-Thread laeuft.
#[derive(Default)]
pub struct Takt {
    pub sekunden: AtomicU32,
    pub audio_aktiv: AtomicBool,
}

impl Takt {
    pub fn zeit(&self) -> f32 {
        f32::from_bits(self.sekunden.load(Ordering::Relaxed))
    }
    pub fn set_zeit(&self, t: f32) {
        self.sekunden.store(t.to_bits(), Ordering::Relaxed);
    }
    pub fn aktiv(&self) -> bool {
        self.audio_aktiv.load(Ordering::Relaxed)
    }
}

/// Bereich des Zugs, in dem ein Zeigerdruck etwas ausloest.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Treffer {
    Fader,
    Pan,
    Eq(usize),
    Nichts,
}

/// Laufende Ziehbewegung.
#[derive(Clone, Copy, Debug)]
struct Griff {
    art: Treffer,
    start_wert: f32,
}

/// Kleines Rechteck in Zeichenkoordinaten.
#[derive(Clone, Copy, Debug, Default)]
pub struct Kasten {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Kasten {
    fn enthaelt(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

mod imp {
    use super::*;

    pub struct StripView {
        pub params: RefCell<Option<Arc<Params>>>,
        pub rolle: Cell<Rolle>,
        pub tokens: RefCell<Option<Arc<RwLock<Tokens>>>>,
        pub masse: RefCell<Option<Arc<RwLock<Masse>>>>,
        pub master: RefCell<Option<Arc<Master>>>,
        pub takt: RefCell<Option<Arc<Takt>>>,
        /// Pegelbank des Kerns (Audio-Thread schreibt, hier wird gelesen).
        pub pegel: RefCell<Option<Arc<crosspoint::pegel::Pegelbank>>>,
        /// Geglaetteter Pegel in dB.
        pub db: Cell<f32>,
        /// Spitzenhalter in dB.
        pub spitze: Cell<f32>,
        /// Bevorzugte Breite (Busse schmaler als Kanaele).
        pub nat: Cell<i32>,
        zug: Cell<Option<Griff>>,
    }

    impl Default for StripView {
        fn default() -> Self {
            StripView {
                params: RefCell::new(None),
                rolle: Cell::new(Rolle::Kanal(0)),
                tokens: RefCell::new(None),
                masse: RefCell::new(None),
                master: RefCell::new(None),
                takt: RefCell::new(None),
                pegel: RefCell::new(None),
                db: Cell::new(-60.0),
                spitze: Cell::new(-60.0),
                nat: Cell::new(104),
                zug: Cell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for StripView {
        const NAME: &'static str = "CrosspointStripView";
        type Type = super::StripView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for StripView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // Doppelklick setzt Gain/Pan/EQ auf Null, einfacher Klick auf ein
            // EQ-Band schaltet es ein oder aus.
            let click = gtk::GestureClick::new();
            click.set_button(1);
            let schwach = obj.downgrade();
            click.connect_pressed(move |_, n, x, y| {
                if let Some(w) = schwach.upgrade() {
                    let imp = w.imp();
                    if n >= 2 {
                        imp.zuruecksetzen(x as f32, y as f32);
                    } else {
                        imp.klick(x as f32, y as f32);
                    }
                }
            });
            obj.add_controller(click);

            // Ziehen: Fader, Pan, EQ-Band.
            let drag = gtk::GestureDrag::new();
            drag.set_button(1);
            let schwach = obj.downgrade();
            drag.connect_drag_begin(move |_, x, y| {
                if let Some(w) = schwach.upgrade() {
                    w.imp().zug_start(x as f32, y as f32);
                }
            });
            let schwach = obj.downgrade();
            drag.connect_drag_update(move |_, dx, dy| {
                if let Some(w) = schwach.upgrade() {
                    w.imp().zug_update(dx, dy);
                }
            });
            let schwach = obj.downgrade();
            drag.connect_drag_end(move |_, _, _| {
                if let Some(w) = schwach.upgrade() {
                    w.imp().zug.set(None);
                }
            });
            obj.add_controller(drag);

            // Zeigerform je Region.
            let motion = gtk::EventControllerMotion::new();
            let schwach = obj.downgrade();
            motion.connect_motion(move |_, x, y| {
                if let Some(w) = schwach.upgrade() {
                    let name = match w.imp().treffer(x as f32, y as f32) {
                        Treffer::Fader | Treffer::Pan => Some("ns-resize"),
                        Treffer::Eq(_) => Some("ew-resize"),
                        Treffer::Nichts => None,
                    };
                    w.set_cursor_from_name(name);
                }
            });
            obj.add_controller(motion);
        }
    }

    impl WidgetImpl for StripView {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (60, self.nat.get(), -1, -1),
                _ => (150, 400, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let w = self.obj().width() as f32;
            let h = self.obj().height() as f32;
            if w < 8.0 || h < 40.0 {
                return;
            }
            self.zeichnen(snapshot, w, h);
        }
    }

    impl StripView {
        pub fn params(&self) -> Option<Arc<Params>> {
            self.params.borrow().clone()
        }

        pub fn tokens(&self) -> Tokens {
            match self.tokens.borrow().as_ref() {
                Some(t) => match t.read() {
                    Ok(g) => g.clone(),
                    Err(e) => e.into_inner().clone(),
                },
                None => Tokens::default(),
            }
        }

        pub fn masse(&self) -> Masse {
            match self.masse.borrow().as_ref() {
                Some(m) => match m.read() {
                    Ok(g) => *g,
                    Err(e) => *e.into_inner(),
                },
                None => Masse::aus(&Tokens::default(), crate::layout::Dichte::Normal),
            }
        }

        fn master(&self) -> Option<Arc<Master>> {
            self.master.borrow().clone()
        }

        pub fn rolle(&self) -> Rolle {
            self.rolle.get()
        }

        // ----- Werte lesen/schreiben -------------------------------------

        pub fn gain(&self) -> f32 {
            if self.rolle() == Rolle::Master {
                return self
                    .master()
                    .map(|m| f32::from_bits(m.gain.load(Ordering::Relaxed)))
                    .unwrap_or(0.0);
            }
            let Some(p) = self.params() else {
                return 0.0;
            };
            match self.rolle() {
                Rolle::Kanal(i) => f32::from_bits(p.strips[i].gain.load(Ordering::Relaxed)),
                Rolle::Bus(i) => f32::from_bits(p.buses[i].gain.load(Ordering::Relaxed)),
                Rolle::Master => 0.0,
            }
        }

        pub fn set_gain(&self, v: f32) {
            let v = v.clamp(-60.0, 12.0);
            match self.rolle() {
                Rolle::Master => {
                    if let Some(m) = self.master() {
                        m.gain.store(v.to_bits(), Ordering::Relaxed);
                    }
                }
                Rolle::Kanal(i) => {
                    if let Some(p) = self.params() {
                        p.set("strip", i, "gain", v);
                    }
                }
                Rolle::Bus(i) => {
                    if let Some(p) = self.params() {
                        p.set("bus", i, "gain", v);
                    }
                }
            }
            self.obj().queue_draw();
        }

        fn pan(&self) -> f32 {
            match (self.rolle(), self.params()) {
                (Rolle::Kanal(i), Some(p)) => {
                    f32::from_bits(p.strips[i].pan.load(Ordering::Relaxed))
                }
                _ => 0.0,
            }
        }

        fn set_pan(&self, v: f32) {
            if let (Rolle::Kanal(i), Some(p)) = (self.rolle(), self.params()) {
                p.set("strip", i, "pan", v.clamp(-1.0, 1.0));
                self.obj().queue_draw();
            }
        }

        /// (Frequenz, Verstaerkung, aktiv) eines EQ-Bandes.
        fn eq(&self, band: usize) -> (f32, f32, bool) {
            match (self.rolle(), self.params()) {
                (Rolle::Kanal(i), Some(p)) if band < 6 => {
                    let cell = &p.strips[i].eq[0][band];
                    (
                        f32::from_bits(cell.freq.load(Ordering::Relaxed)),
                        f32::from_bits(cell.gain.load(Ordering::Relaxed)),
                        cell.on.load(Ordering::Relaxed),
                    )
                }
                _ => (1000.0, 0.0, false),
            }
        }

        fn set_eq_gain(&self, band: usize, v: f32) {
            if let (Rolle::Kanal(i), Some(p)) = (self.rolle(), self.params()) {
                p.set("strip", i, &format!("eq{band}_gain"), v.clamp(-24.0, 24.0));
                self.obj().queue_draw();
            }
        }

        fn eq_umschalten(&self, band: usize) {
            if let (Rolle::Kanal(i), Some(p)) = (self.rolle(), self.params()) {
                let an = !p.strips[i].eq[0][band].on.load(Ordering::Relaxed);
                p.set(
                    "strip",
                    i,
                    &format!("eq{band}_on"),
                    if an { 1.0 } else { 0.0 },
                );
                self.obj().queue_draw();
            }
        }

        /// Rohpegel (linear) aus der Pegelbank des Kerns.
        ///
        /// Der Audio-Thread schreibt dort je Block einen Spitzenwert je
        /// Streifen und Bus hinein (siehe `crosspoint::pegel`). Ohne laufende
        /// Engine bleiben die Atome auf 0 – die Anzeige ist dann still und
        /// zeigt keine erfundenen Ausschlaege.
        fn pegel_lin(&self) -> f32 {
            let bank = self.pegel.borrow().clone();
            let Some(bank) = bank else {
                return 0.0;
            };
            match self.rolle() {
                Rolle::Kanal(i) => bank.strip(i),
                Rolle::Bus(i) => bank.bus(i),
                Rolle::Master => bank.master(),
            }
        }

        /// Zielwert der Anzeige in dB, direkt aus der Pegelbank.
        fn ziel_db(&self) -> f32 {
            lin_to_db(self.pegel_lin())
        }

        /// Wird vom 60-Hz-Takt aufgerufen: Pegel glaetten, Spitze halten.
        pub fn ticken(&self) {
            let ziel = self.ziel_db();
            let alt = self.db.get();
            let neu = if ziel > alt {
                alt + (ziel - alt) * 0.72
            } else {
                alt + (ziel - alt) * 0.22
            };
            self.db.set(neu);
            let spitze = self.spitze.get();
            self.spitze
                .set(if neu > spitze { neu } else { spitze - 0.45 });
            self.obj().queue_draw();
        }

        pub fn db(&self) -> f32 {
            self.db.get()
        }

        // ----- Treffer und Ziehen ----------------------------------------

        fn treffer(&self, x: f32, y: f32) -> Treffer {
            let w = self.obj().width() as f32;
            let h = self.obj().height() as f32;
            let m = self.masse();
            let (fader, _len, _top, _tx, _tw) = self.fader_geometrie(w, h, &m);
            if fader.enthaelt(x, y) {
                return Treffer::Fader;
            }
            if self.rolle().ist_kanal() {
                if self.pan_geometrie(w, h, &m).enthaelt(x, y) {
                    return Treffer::Pan;
                }
                for band in 0..3 {
                    if self.eq_zeile(w, h, &m, band).enthaelt(x, y) {
                        return Treffer::Eq(band);
                    }
                }
            }
            Treffer::Nichts
        }

        fn klick(&self, x: f32, y: f32) {
            if let Treffer::Eq(band) = self.treffer(x, y) {
                self.eq_umschalten(band);
            }
        }

        fn zuruecksetzen(&self, x: f32, y: f32) {
            match self.treffer(x, y) {
                Treffer::Fader => self.set_gain(0.0),
                Treffer::Pan => self.set_pan(0.0),
                Treffer::Eq(band) => self.set_eq_gain(band, 0.0),
                Treffer::Nichts => {}
            }
        }

        fn zug_start(&self, x: f32, y: f32) {
            let art = self.treffer(x, y);
            let wert = match art {
                Treffer::Fader => self.gain(),
                Treffer::Pan => self.pan(),
                Treffer::Eq(band) => self.eq(band).1,
                Treffer::Nichts => return,
            };
            self.zug.set(Some(Griff {
                art,
                start_wert: wert,
            }));
            // Fader springt auf die Zeigerposition (wie im Web-Vorbild).
            if art == Treffer::Fader {
                let w = self.obj().width() as f32;
                let h = self.obj().height() as f32;
                let m = self.masse();
                let (_r, len, top, _tx, _tw) = self.fader_geometrie(w, h, &m);
                if len > 1.0 {
                    let pos = 1.0 - ((y - top) / len).clamp(0.0, 1.0);
                    let v = pos_db(pos);
                    self.set_gain(v);
                    self.zug.set(Some(Griff { art, start_wert: v }));
                }
            }
        }

        fn zug_update(&self, dx: f64, dy: f64) {
            let Some(zug) = self.zug.get() else {
                return;
            };
            match zug.art {
                Treffer::Fader => self.set_gain(zug.start_wert - dy as f32 * 0.32),
                Treffer::Pan => self.set_pan(zug.start_wert + (dx as f32 + dy as f32 * 0.3) / 70.0),
                Treffer::Eq(band) => self.set_eq_gain(band, zug.start_wert + dx as f32 * 0.45),
                Treffer::Nichts => {}
            }
        }

        // ----- Geometrie -------------------------------------------------

        /// (Faderflaeche, Laenge, obere Kante, x der Bahn, Breite der Bahn)
        fn fader_geometrie(&self, w: f32, h: f32, m: &Masse) -> (Kasten, f32, f32, f32, f32) {
            let pad = 5.0;
            let eq_h = if self.rolle().ist_kanal() {
                3.0 * 21.0 + 15.0
            } else {
                0.0
            };
            let pan_h = if self.rolle().ist_kanal() { 52.0 } else { 0.0 };
            let block_h = (h - pad * 2.0 - eq_h - pan_h).max(60.0);
            let top = pad + 7.0;
            let len = (block_h - 7.0 - 16.0 - 6.0).max(20.0);
            let bahn = m.fader_breite as f32;
            let x_bahn = (w * 0.5 - bahn * 0.5 - 3.0).max(pad);
            let kasten = Kasten {
                x: pad,
                y: pad,
                w: (w - pad * 2.0).max(4.0),
                h: block_h,
            };
            (kasten, len, top, x_bahn, bahn)
        }

        fn pan_geometrie(&self, w: f32, h: f32, _m: &Masse) -> Kasten {
            let eq_h = 3.0 * 21.0 + 15.0;
            Kasten {
                x: 4.0,
                y: h - 5.0 - eq_h - 52.0,
                w: (w - 8.0).max(8.0),
                h: 52.0,
            }
        }

        fn eq_zeile(&self, w: f32, h: f32, _m: &Masse, band: usize) -> Kasten {
            let eq_h = 3.0 * 21.0 + 15.0;
            let oben = h - 5.0 - eq_h + 13.0;
            Kasten {
                x: 4.0,
                y: oben + band as f32 * 21.0,
                w: (w - 8.0).max(8.0),
                h: 20.0,
            }
        }

        // ----- Zeichnen ---------------------------------------------------

        fn zeichnen(&self, snap: &gtk::Snapshot, w: f32, h: f32) {
            let t = self.tokens();
            let m = self.masse();
            let fam = &t.font_family;
            // Schrift nie verkleinern: schmale Zuege lassen Details weg.
            // 13 px ist die Untergrenze (Lesbarkeit vor Dichte); die Namen
            // stehen mit 15 px in `.zugname`.
            let fs = (m.schrift_klein as f32).max(13.0);
            let schmal = w < 96.0;

            let grund = draw::farbe(&t.grund);
            let panel = draw::farbe(&t.panel);
            let hell = draw::farbe(&t.panel_hell);
            let linie = draw::farbe(&t.linie);
            let akzent = draw::farbe(&t.akzent);
            let akzent_d = draw::farbe(&t.akzent_dunkel);
            let schrift = draw::farbe(&t.schrift);
            let dim = draw::farbe(&t.gedaempft);
            let warn = draw::farbe(&t.warnung);

            // Flaeche des Zugs (Rahmen zeichnet das CSS des GTK-Widgets).
            draw::flaeche(snap, draw::rechteck(0.0, 0.0, w, h), &panel);

            let (block, len, top, x_bahn, bahn) = self.fader_geometrie(w, h, &m);
            draw::flaeche_rund(
                snap,
                draw::rechteck(block.x, block.y, block.w, block.h),
                3.0,
                &draw::alpha(&grund, 0.75),
            );

            let db = self.db.get();
            let spitze = self.spitze.get();
            let gain = self.gain();

            // Bahn
            draw::flaeche_rund(
                snap,
                draw::rechteck(x_bahn, top, bahn, len),
                2.0,
                &draw::alpha(&linie, 0.55),
            );
            draw::flaeche_rund(
                snap,
                draw::rechteck(x_bahn + 1.0, top + 1.0, (bahn - 2.0).max(1.0), len - 2.0),
                1.5,
                &draw::alpha(&grund, 0.9),
            );

            // Fuellung bis zum Griff
            let gy = top + (1.0 - fader_pos(gain)) * len;
            if gy < top + len - 2.0 {
                draw::flaeche_rund(
                    snap,
                    draw::rechteck(
                        x_bahn + 1.0,
                        gy + 2.0,
                        (bahn - 2.0).max(1.0),
                        (top + len - gy - 3.0).max(0.0),
                    ),
                    1.5,
                    &akzent_d,
                );
            }

            // Skala links
            let skala_rechts = x_bahn - 5.0;
            let skala_breite = skala_rechts - block.x - 2.0;
            if skala_breite > 12.0 {
                for (db_wert, beschriftung) in [
                    (12.0, "+12"),
                    (6.0, "+6"),
                    (0.0, "0"),
                    (-6.0, "-6"),
                    (-12.0, "-12"),
                    (-24.0, "-24"),
                    (-36.0, "-36"),
                    (-48.0, "-48"),
                    (-60.0, "-60"),
                ] {
                    let y = top + (1.0 - fader_pos(db_wert)) * len;
                    let haupt = db_wert == 0.0;
                    let farbe = if haupt { schrift } else { dim };
                    draw::flaeche(
                        snap,
                        draw::rechteck(
                            skala_rechts,
                            y,
                            if haupt { 5.0 } else { 3.0 },
                            if haupt { 1.5 } else { 1.0 },
                        ),
                        &draw::alpha(&farbe, if haupt { 0.9 } else { 0.55 }),
                    );
                    if skala_breite > 22.0 {
                        draw::text_rechts(
                            snap,
                            &*self.obj(),
                            beschriftung,
                            block.x,
                            y - fs * 0.62,
                            skala_breite - 5.0,
                            fs,
                            false,
                            fam,
                            &draw::alpha(&farbe, if haupt { 0.95 } else { 0.6 }),
                        );
                    }
                }
            }

            // Pegelsaeule rechts
            let meter_w = (m.pegel_breite as f32).max(6.0);
            let meter_x = w - 5.0 - meter_w;
            let meter_y = top;
            let meter_h = len;
            draw::flaeche_rund(
                snap,
                draw::rechteck(meter_x, meter_y, meter_w, meter_h),
                2.0,
                &draw::alpha(&grund, 0.95),
            );
            let segmente = 26;
            let seg_h = meter_h / segmente as f32;
            let unlit = draw::alpha(&linie, 0.26);
            for i in 0..segmente {
                let von_unten = (i as f32 + 1.0) / segmente as f32;
                let seg_db = pos_db(von_unten);
                let y = meter_y + meter_h - (i as f32 + 1.0) * seg_h;
                let lit = seg_db <= db;
                let farbe = if seg_db > -3.0 {
                    warn
                } else if seg_db > -12.0 {
                    draw::alpha(&warn, 0.9)
                } else {
                    akzent
                };
                draw::flaeche(
                    snap,
                    draw::rechteck(
                        meter_x + 1.0,
                        y + 0.6,
                        (meter_w - 2.0).max(1.0),
                        (seg_h - 1.4).max(0.8),
                    ),
                    if lit { &farbe } else { &unlit },
                );
            }
            if spitze > -55.0 {
                let y = meter_y + (1.0 - fader_pos(spitze)) * meter_h;
                draw::flaeche(
                    snap,
                    draw::rechteck(meter_x + 1.0, y, (meter_w - 2.0).max(1.0), 1.5),
                    &draw::alpha(&schrift, 0.85),
                );
            }

            // Griff
            let griff_w = (bahn + 12.0).min(block.w - 8.0).max(bahn + 4.0);
            let griff_h = 13.0;
            let gx = x_bahn + bahn * 0.5 - griff_w * 0.5;
            draw::flaeche_rund(
                snap,
                draw::rechteck(gx, gy - griff_h * 0.5, griff_w, griff_h),
                2.0,
                &hell,
            );
            draw::rahmen(
                snap,
                draw::rechteck(gx, gy - griff_h * 0.5, griff_w, griff_h),
                2.0,
                1.0,
                &linie,
            );
            draw::waagerecht(
                snap,
                gx + 1.0,
                gy - 0.75,
                (griff_w - 2.0).max(2.0),
                1.5,
                &akzent,
            );

            // Zwei Anzeigefelder unter dem Fader: Gain und Pegel in dB.
            let unten = block.y + block.h - 15.0;
            let feld_w = ((block.w - 3.0) * 0.5).max(14.0);
            for (i, (wert, farbe)) in [(fmt::gain_text(gain), schrift), (fmt::db_text(db), akzent)]
                .into_iter()
                .enumerate()
            {
                let x = block.x + 1.0 + i as f32 * (feld_w + 1.0);
                draw::flaeche_rund(
                    snap,
                    draw::rechteck(x, unten, feld_w, 13.0),
                    2.0,
                    &draw::alpha(&grund, 0.85),
                );
                draw::text_mitte(
                    snap,
                    &*self.obj(),
                    &wert,
                    x,
                    unten + 2.5,
                    feld_w,
                    fs,
                    false,
                    fam,
                    &farbe,
                );
            }

            if self.rolle().ist_kanal() {
                self.zeichne_pan(snap, w, h, &m, &t, fs, schmal);
                self.zeichne_eq(snap, w, h, &m, &t, fs, schmal);
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn zeichne_pan(
            &self,
            snap: &gtk::Snapshot,
            w: f32,
            h: f32,
            m: &Masse,
            t: &Tokens,
            fs: f32,
            schmal: bool,
        ) {
            let r = self.pan_geometrie(w, h, m);
            let fam = &t.font_family;
            let dim = draw::farbe(&t.gedaempft);
            let schrift = draw::farbe(&t.schrift);
            let akzent = draw::farbe(&t.akzent);
            let hell = draw::farbe(&t.panel_hell);
            let linie = draw::farbe(&t.linie);
            let grund = draw::farbe(&t.grund);

            draw::text(
                snap,
                &*self.obj(),
                "PAN",
                r.x + 1.0,
                r.y,
                30.0,
                fs,
                false,
                fam,
                &draw::alpha(&dim, 0.85),
            );

            let pan = self.pan();
            let cx = r.x + r.w * 0.5 - 6.0;
            let cy = r.y + 30.0;
            let radius = 13.0_f32.min(r.w * 0.2).max(9.0);

            draw::flaeche_rund(
                snap,
                draw::rechteck(cx - radius, cy - radius, radius * 2.0, radius * 2.0),
                radius,
                &hell,
            );
            draw::rahmen(
                snap,
                draw::rechteck(cx - radius, cy - radius, radius * 2.0, radius * 2.0),
                radius,
                1.0,
                &linie,
            );
            draw::flaeche_rund(
                snap,
                draw::rechteck(
                    cx - radius + 3.0,
                    cy - radius + 3.0,
                    (radius - 3.0) * 2.0,
                    (radius - 3.0) * 2.0,
                ),
                radius - 3.0,
                &draw::alpha(&grund, 0.35),
            );

            // Raster um den Regler (21 Striche von -135 bis +135 Grad)
            let zeiger = pan.clamp(-1.0, 1.0) * 135.0;
            for i in 0..=20 {
                let winkel = -135.0 + i as f32 * 13.5;
                let rad = winkel.to_radians();
                let aktiv = if zeiger >= 0.0 {
                    winkel >= 0.0 && winkel <= zeiger
                } else {
                    winkel <= 0.0 && winkel >= zeiger
                };
                let r1 = radius + 2.0;
                let r2 = radius + if i % 5 == 0 { 4.5 } else { 3.2 };
                let (s, c) = rad.sin_cos();
                let x1 = cx + s * r1;
                let y1 = cy - c * r1;
                let x2 = cx + s * r2;
                let y2 = cy - c * r2;
                draw::flaeche(
                    snap,
                    draw::rechteck(
                        x1.min(x2) - 0.5,
                        y1.min(y2),
                        (x2 - x1).abs() + 1.0,
                        (y2 - y1).abs() + 1.0,
                    ),
                    &draw::alpha(
                        if aktiv { &akzent } else { &linie },
                        if aktiv { 0.95 } else { 0.55 },
                    ),
                );
            }

            // Zeiger als Punktreihe
            let rad = (-pan.clamp(-1.0, 1.0) * 135.0).to_radians();
            for k in 0..=5 {
                let anteil = k as f32 / 5.0;
                let rr = 2.0 + anteil * (radius - 3.0);
                let x = cx + rad.sin() * rr;
                let y = cy - rad.cos() * rr;
                draw::flaeche(snap, draw::rechteck(x - 1.0, y - 1.0, 2.0, 2.0), &akzent);
            }

            let text = if pan.abs() < 0.02 {
                "C".to_string()
            } else if pan < 0.0 {
                format!("L{:>2}", (-pan * 100.0).round() as i32)
            } else {
                format!("R{:>2}", (pan * 100.0).round() as i32)
            };
            if !schmal {
                draw::text(
                    snap,
                    &*self.obj(),
                    &text,
                    cx + radius + 7.0,
                    cy - fs * 0.5,
                    (r.x + r.w - cx - radius - 8.0).max(10.0),
                    fs,
                    false,
                    fam,
                    &schrift,
                );
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn zeichne_eq(
            &self,
            snap: &gtk::Snapshot,
            w: f32,
            h: f32,
            m: &Masse,
            t: &Tokens,
            fs: f32,
            schmal: bool,
        ) {
            let fam = &t.font_family;
            let dim = draw::farbe(&t.gedaempft);
            let schrift = draw::farbe(&t.schrift);
            let akzent = draw::farbe(&t.akzent);
            let linie = draw::farbe(&t.linie);

            let oben = h - 5.0 - (3.0 * 21.0 + 15.0);
            draw::waagerecht(
                snap,
                m.abstand as f32 * 0.35,
                oben - 3.0,
                w - 7.0,
                1.0,
                &draw::alpha(&linie, 0.8),
            );
            draw::text(
                snap,
                &*self.obj(),
                "EQ",
                5.0,
                oben,
                20.0,
                fs,
                false,
                fam,
                &draw::alpha(&dim, 0.9),
            );
            if !schmal {
                draw::text_rechts(
                    snap,
                    &*self.obj(),
                    "3 BAND",
                    5.0,
                    oben,
                    w - 11.0,
                    fs - 0.5,
                    false,
                    fam,
                    &draw::alpha(&dim, 0.55),
                );
            }

            // Raster hinter den Baendern
            let raster_oben = oben + 11.0;
            let raster_h = 3.0 * 21.0 - 4.0;
            let raster = draw::alpha(&linie, 0.12);
            let mut x = 6.0;
            while x < w - 6.0 {
                draw::senkrecht(snap, x, raster_oben, raster_h, 1.0, &raster);
                x += 10.0;
            }

            for band in 0..3 {
                let r = self.eq_zeile(w, h, m, band);
                let (freq, gain, an) = self.eq(band);
                let y_mitte = r.y + 9.5;

                draw::text(
                    snap,
                    &*self.obj(),
                    &format!("{}", band + 1),
                    5.0,
                    y_mitte - fs * 0.62,
                    8.0,
                    fs,
                    an,
                    fam,
                    &if an { akzent } else { draw::alpha(&dim, 0.75) },
                );

                let x_anfang = 13.0;
                // Frequenz und Wert rechts brauchen bei 13-px-Schrift zusammen
                // rund 58 px (21 px Frequenz, 30 px Wert, Rand). Ist der Zug
                // schmaler, bleiben sie weg - lieber keine Zahl als eine
                // abgeschnittene.
                let text_an = !schmal && w >= 118.0;
                let x_ende = if text_an { w - 58.0 } else { w - 6.0 };
                let breite = (x_ende - x_anfang).max(10.0);
                draw::waagerecht(
                    snap,
                    x_anfang,
                    y_mitte,
                    breite,
                    1.0,
                    &draw::alpha(&linie, 0.5),
                );
                let mitte = x_anfang + breite * 0.5;
                let x_wert = mitte + (gain / 24.0) * (breite * 0.5);
                if gain.abs() > 0.05 {
                    let (xa, xb) = if x_wert >= mitte {
                        (mitte, x_wert)
                    } else {
                        (x_wert, mitte)
                    };
                    draw::flaeche(
                        snap,
                        draw::rechteck(xa, y_mitte - 2.0, (xb - xa).max(1.0), 4.0),
                        &draw::alpha(&akzent, 0.5),
                    );
                }
                draw::senkrecht(
                    snap,
                    mitte - 0.5,
                    y_mitte - 3.5,
                    8.0,
                    1.0,
                    &draw::alpha(&schrift, 0.4),
                );
                draw::flaeche_rund(
                    snap,
                    draw::rechteck(x_wert - 2.5, y_mitte - 3.5, 5.0, 7.0),
                    1.5,
                    &if an {
                        akzent
                    } else {
                        draw::alpha(&schrift, 0.65)
                    },
                );

                if text_an {
                    draw::text(
                        snap,
                        &*self.obj(),
                        &fmt::hz_text(freq),
                        x_ende + 2.0,
                        y_mitte - fs * 0.62,
                        21.0,
                        fs - 0.5,
                        false,
                        fam,
                        &draw::alpha(&dim, 0.75),
                    );
                    let wert_farbe = if gain.abs() > 0.05 {
                        akzent
                    } else {
                        draw::alpha(&dim, 0.7)
                    };
                    draw::text_rechts(
                        snap,
                        &*self.obj(),
                        &fmt::gain_text(gain),
                        w - 32.0,
                        y_mitte - fs * 0.62,
                        28.0,
                        fs - 0.5,
                        false,
                        fam,
                        &wert_farbe,
                    );
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct StripView(ObjectSubclass<imp::StripView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for StripView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl StripView {
    #[allow(clippy::too_many_arguments)]
    pub fn neu(
        params: Arc<Params>,
        rolle: Rolle,
        tokens: Arc<RwLock<Tokens>>,
        masse: Arc<RwLock<Masse>>,
        master: Arc<Master>,
        takt: Arc<Takt>,
        pegel: Option<Arc<crosspoint::pegel::Pegelbank>>,
    ) -> StripView {
        let view: StripView = glib::Object::new();
        {
            let imp = view.imp();
            *imp.params.borrow_mut() = Some(params);
            imp.rolle.set(rolle);
            *imp.master.borrow_mut() = Some(master);
            *imp.takt.borrow_mut() = Some(takt);
            *imp.pegel.borrow_mut() = pegel;
            *imp.tokens.borrow_mut() = Some(tokens);
            *imp.masse.borrow_mut() = Some(masse);
            imp.nat.set(match rolle {
                Rolle::Kanal(_) => 132,
                Rolle::Bus(_) => 92,
                Rolle::Master => 108,
            });
        }
        view.set_vexpand(true);
        view.set_hexpand(false);
        view.set_focusable(true);
        view
    }

    pub fn rolle(&self) -> Rolle {
        self.imp().rolle.get()
    }

    pub fn db(&self) -> f32 {
        self.imp().db.get()
    }

    /// Breite vorgeben (aus der Breitenrechnung des Pults).
    ///
    /// Die Breite kommt von aussen, nicht aus dem Inhalt: sonst bestimmt der
    /// laengste Geraetename oder die Zuordnungstastenreihe, wie breit das
    /// ganze Pult wird. Die Zeichenflaeche richtet sich nach der Zuteilung
    /// (`snapshot` liest `width()`), Fader und Pegel wandern mit.
    pub fn breite_setzen(&self, breite: f64) {
        let b = breite.round().max(1.0) as i32;
        if self.imp().nat.get() != b {
            self.imp().nat.set(b);
            self.set_size_request(b, -1);
            self.queue_resize();
        }
    }

    /// 60-Hz-Takt: Pegel fortschreiben und neu zeichnen.
    pub fn ticken(&self) {
        self.imp().ticken();
    }

    /// Beschriftung des Zugs (Name oder Vorgabe).
    pub fn beschriftung(&self) -> String {
        let (ziel, idx) = self.rolle().ziel();
        match self.imp().params() {
            Some(p) if ziel != "master" => match p.label(ziel, idx) {
                Some(l) if !l.is_empty() => l,
                _ => self.rolle().name(),
            },
            _ => self.rolle().name(),
        }
    }
}

/// Ein fertiger Zug: Kopf mit Namen (Doppelklick = GtkEntry), Zeichenflaeche,
/// Knoepfe. `sync` zieht Werte aus `Params` nach, `ticken` laesst den Pegel
/// laufen.
/// Alles, was der Zug von aussen braucht, ausser Zustand und Farben.
#[derive(Clone, Default)]
pub struct ZugUmfeld {
    pub geraete: Option<std::rc::Rc<GeraeteUi>>,
    pub zuordnung: bool,
    /// Oeffnet den EQ-Editor des Streifens.
    pub eq_editor: Option<std::rc::Rc<dyn Fn(usize)>>,
    /// Pegelbank des Kerns (die Anzeige liest direkt daraus).
    pub pegel: Option<Arc<crosspoint::pegel::Pegelbank>>,
}

pub struct Zug {
    pub widget: gtk::Widget,
    pub view: StripView,
    pub sync: std::rc::Rc<dyn Fn()>,
    /// Aufklappmenue der Geraetezuordnung (Streifen: Quelle, Hardware-Bus: Sink).
    pub geraetewahl: Option<gtk::DropDown>,
}

/// Baut einen Kanalzug, Bus oder Master.
pub fn zug(
    params: Arc<Params>,
    rolle: Rolle,
    tokens: Arc<RwLock<Tokens>>,
    masse: Arc<RwLock<Masse>>,
    master: Arc<Master>,
    takt: Arc<Takt>,
    umfeld: ZugUmfeld,
) -> Zug {
    let zuordnung = umfeld.zuordnung;
    let geraete = umfeld.geraete.clone();
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    box_.add_css_class("zug");
    if rolle == Rolle::Master {
        box_.add_css_class("master");
    }

    // ----- Geraetezuordnung (ueber dem Namen) -----
    let mut geraetewahl: Option<gtk::DropDown> = None;
    // Nur echte Hardware-Kanaele bekommen eine Geraetewahl: Streifen 1-5
    // (Eingang) und Busse A1-A5 (Ausgang). Virtuelle Streifen und die Busse
    // B1-B3 haengen nicht an einem Geraet - dort steht ein ruhiger Hinweis
    // gleicher Hoehe, damit die Zeilen buendig bleiben.
    let hat_geraet =
        matches!(rolle, Rolle::Kanal(i) if i < 5) || matches!(rolle, Rolle::Bus(i) if i < 5);
    if !hat_geraet {
        // Kein Geraet -> kein Bedienelement und kein Text: eine leere Zelle
        // gleicher Hoehe haelt die Zeilen buendig, ohne die Mindestbreite der
        // Spalte zu heben (ein Label wuerde genau das tun und das Pult wieder
        // ueber den Fensterrand schieben).
        let leer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        leer.set_size_request(0, 21);
        box_.append(&leer);
    }
    if let Some(g) = geraete.as_ref().filter(|_| hat_geraet) {
        let (modell, art) = match rolle {
            Rolle::Kanal(i) if i < 5 => (g.quellen.clone(), true),
            Rolle::Bus(i) if i < 5 => (g.senken.clone(), false),
            _ => (gtk::StringList::new(&["—"]), true),
        };
        let aktiv =
            matches!(rolle, Rolle::Kanal(i) if i < 5) || matches!(rolle, Rolle::Bus(i) if i < 5);
        let dd = gtk::DropDown::new(Some(modell.clone()), None::<gtk::Expression>);
        dd.add_css_class("geraetewahl");
        dd.set_enable_search(false);
        dd.set_valign(gtk::Align::Center);
        dd.set_margin_top(3);
        dd.set_margin_start(4);
        dd.set_margin_end(4);
        dd.set_sensitive(aktiv);
        // Anzeige: ellipsierendes Label. Ohne das bestimmt der laengste
        // Geraetename die Mindestbreite des ganzen Zugs - und damit die Breite
        // des Pults. Vollstaendig steht der Name im Tooltip.
        {
            // Zwei Fabriken: die Knopf-Anzeige ist knapp (im Zug ist wenig
            // Platz, "kein Eingang" wuerde zu "ke..." schrumpfen), die Liste
            // im Aufklappmenue zeigt den vollen Namen.
            let knopf = gtk::SignalListItemFactory::new();
            knopf.connect_setup(|_, obj| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let l = gtk::Label::new(None);
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                l.set_xalign(0.0);
                l.set_single_line_mode(true);
                // Natuerliche Breite deckeln: sonst fordert der Zug so viel
                // wie der laengste Geraetename. Angezeigt wird trotzdem so
                // viel, wie der Zug hergibt (Ellipse + Tooltip).
                l.set_max_width_chars(6);
                item.set_child(Some(&l));
            });
            knopf.connect_bind(|_, obj| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let text = item
                    .item()
                    .and_then(|i| i.downcast::<gtk::StringObject>().ok())
                    .map(|s| s.string().to_string())
                    .unwrap_or_default();
                if let Some(l) = item.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                    // "kein Eingang"/"kein Ausgang" stehen im Zug als Strich:
                    // vollstaendig waeren sie breiter als ein Bus.
                    let kurz = if text == crate::geraete::KEIN_EINGANG
                        || text == crate::geraete::KEIN_AUSGANG
                    {
                        "—".to_string()
                    } else {
                        text.clone()
                    };
                    l.set_text(&kurz);
                    l.set_tooltip_text(Some(&text));
                }
            });
            let liste = gtk::SignalListItemFactory::new();
            liste.connect_setup(|_, obj| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let l = gtk::Label::new(None);
                l.set_xalign(0.0);
                l.set_single_line_mode(true);
                l.set_max_width_chars(40);
                item.set_child(Some(&l));
            });
            liste.connect_bind(|_, obj| {
                let Some(item) = obj.downcast_ref::<gtk::ListItem>() else {
                    return;
                };
                let text = item
                    .item()
                    .and_then(|i| i.downcast::<gtk::StringObject>().ok())
                    .map(|s| s.string().to_string())
                    .unwrap_or_default();
                if let Some(l) = item.child().and_then(|c| c.downcast::<gtk::Label>().ok()) {
                    l.set_text(&text);
                }
            });
            dd.set_factory(Some(&knopf));
            dd.set_list_factory(Some(&liste));
        }
        if !aktiv {
            dd.set_tooltip_text(Some(match rolle {
                Rolle::Kanal(_) => "Virtueller Streifen: kein Hardware-Eingang",
                Rolle::Bus(_) => "Virtueller Bus (VAIO): kein Ausgabegeraet",
                Rolle::Master => "Summe ohne eigenes Geraet",
            }));
        }
        {
            let g2 = std::rc::Rc::clone(g);
            let r = rolle;
            dd.connect_selected_notify(move |d| {
                if g2.sperre.get() {
                    return;
                }
                let anzeige = if art {
                    g2.quelle_name(d.selected())
                } else {
                    g2.senke_name(d.selected())
                };
                let ziel = match r {
                    Rolle::Kanal(i) => crate::geraete::Ziel::Streifen(i + 1),
                    Rolle::Bus(i) => crate::geraete::Ziel::Bus(crosspoint::BUS_NAMES[i]),
                    Rolle::Master => return,
                };
                match crate::geraete::auswahl_anwenden(
                    &crate::geraete::KernZuordner,
                    ziel,
                    &anzeige,
                ) {
                    Ok(()) => {
                        let was = if anzeige.is_empty() {
                            "Zuordnung geloest".to_string()
                        } else {
                            format!("{} -> {}", r.name(), crate::fmt::kurz(&anzeige, 28))
                        };
                        g2.melden(&was, false);
                    }
                    Err(e) => g2.melden(&format!("{}: {e}", r.name()), true),
                }
            });
        }
        box_.append(&dd);
        geraetewahl = Some(dd);
    }

    // ----- Kopf: Name (umschaltbar auf Eingabe) und Nummer -----
    let kopf = gtk::Box::new(gtk::Orientation::Vertical, 0);
    kopf.add_css_class("zugkopf");
    let zeile = gtk::Box::new(gtk::Orientation::Horizontal, 4);

    let stack = gtk::Stack::new();
    stack.set_hexpand(true);
    stack.set_hhomogeneous(false);
    let label = gtk::Label::new(None);
    label.add_css_class("zugname");
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_single_line_mode(true);
    let eingabe = gtk::Entry::new();
    eingabe.add_css_class("zugname-eingabe");
    eingabe.set_has_frame(false);
    eingabe.set_max_length(crosspoint::engine::MAX_LABEL as i32);
    stack.add_named(&label, Some("anzeige"));
    stack.add_named(&eingabe, Some("eingabe"));
    stack.set_visible_child_name("anzeige");
    zeile.append(&stack);

    let idx = gtk::Label::new(Some(&rolle.nummer()));
    idx.add_css_class("zugidx");
    idx.set_valign(gtk::Align::Start);
    zeile.append(&idx);
    kopf.append(&zeile);

    let sub = gtk::Label::new(Some(&rolle.untertitel()));
    sub.add_css_class("zugsub");
    sub.set_xalign(0.0);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    // Gekuerzt wird nur, wenn der Zug schmal ist - dann steht der volle Text
    // im Tooltip.
    sub.set_tooltip_text(Some(&rolle.untertitel()));
    kopf.append(&sub);
    box_.append(&kopf);

    // ----- Zeichenflaeche -----
    let view = StripView::neu(
        params.clone(),
        rolle,
        tokens,
        masse,
        master.clone(),
        takt,
        umfeld.pegel.clone(),
    );
    box_.append(&view);

    // ----- Zuordnung A1..B3 (eigener Layout-Bereich) -----
    let mut zuordnung_knoepfe: Vec<gtk::ToggleButton> = Vec::new();
    if zuordnung && rolle.ist_kanal() {
        // Zwei Reihen zu vier Tasten statt einer Reihe zu acht: in einer Reihe
        // forderte die Reihe 149 px und bestimmte damit die Zugbreite (ein Zug
        // haette nie unter 150 px kommen koennen). Die Kuerzel A1..B3 stehen
        // vollstaendig, nichts wird abgeschnitten.
        let reihe = gtk::Grid::new();
        reihe.add_css_class("zuordnung");
        reihe.set_column_homogeneous(true);
        reihe.set_column_spacing(1);
        reihe.set_row_spacing(1);
        reihe.set_margin_start(3);
        reihe.set_margin_end(3);
        reihe.set_margin_bottom(2);
        for bus in 0..crosspoint::BUSES {
            let b = gtk::ToggleButton::with_label(crosspoint::BUS_NAMES[bus]);
            b.add_css_class("map");
            b.set_tooltip_text(Some(&format!(
                "Streifen auf {} schalten",
                crosspoint::BUS_NAMES[bus]
            )));
            {
                let p = params.clone();
                let i = match rolle {
                    Rolle::Kanal(i) => i,
                    _ => 0,
                };
                b.connect_toggled(move |b| {
                    state::send_setzen(&p, i, bus, b.is_active());
                });
            }
            reihe.attach(&b, (bus % 4) as i32, (bus / 4) as i32, 1, 1);
            zuordnung_knoepfe.push(b);
        }
        box_.append(&reihe);
    }

    // ----- Knoepfe -----
    // Zwei Spalten: M|EQ, S|MONO, LC|NR, darunter GATE ueber beide Spalten.
    // In einer Reihe forderte "MONO" 34 px und die Reihe (homogen) 153 px -
    // mehr als ein Zug bei schmalem Fenster haben darf; eine dritte Spalte
    // waere bei 64 px Busbreite nicht unterzubringen. Die Beschriftungen
    // bleiben vollstaendig.
    let knopf_zeile = gtk::Grid::new();
    // Spalten nicht gleich breit: sonst muesste die Spalte von "M" so breit
    // sein wie "MONO" und die Knopfreihe forderte 79 px - mehr als ein Bus bei
    // schmalem Fenster haben darf (64 px).
    knopf_zeile.set_column_homogeneous(false);
    knopf_zeile.set_column_spacing(3);
    knopf_zeile.set_row_spacing(3);
    knopf_zeile.set_margin_top(3);
    knopf_zeile.set_margin_bottom(4);
    knopf_zeile.set_margin_start(4);
    knopf_zeile.set_margin_end(4);

    let (_, index) = rolle.ziel();
    let knopf = |text: &str, klasse: &str| {
        let b = gtk::ToggleButton::with_label(text);
        b.add_css_class("knopf");
        b.add_css_class(klasse);
        b.set_hexpand(true);
        b
    };

    let mute = knopf("M", "mute");
    let eq_knopf = if let (Rolle::Kanal(i), Some(f)) = (rolle, umfeld.eq_editor.clone()) {
        let b = gtk::Button::with_label("EQ");
        b.add_css_class("knopf");
        b.set_tooltip_text(Some("EQ-Editor oeffnen"));
        b.connect_clicked(move |_| f(i));
        Some(b)
    } else {
        None
    };
    let solo = if rolle.ist_kanal() {
        Some(knopf("S", "solo"))
    } else {
        None
    };
    let mono = if rolle.ist_kanal() {
        Some(knopf("MONO", "mono"))
    } else if matches!(rolle, Rolle::Bus(_)) {
        Some(knopf("MONO", "mono"))
    } else {
        None
    };

    // Die drei Stufen der Sprache gibt es nur an Kanaelen: Lowcut (Rumpeln),
    // NR (Rauschen unter dem Signal), GATE (Pausen). Busse haben sie nicht.
    let lc = if rolle.ist_kanal() {
        Some(knopf("LC", "lc"))
    } else {
        None
    };
    let nr = if rolle.ist_kanal() {
        Some(knopf("NR", "nr"))
    } else {
        None
    };
    let gate_knopf = if rolle.ist_kanal() {
        Some(knopf("GATE", "gate"))
    } else {
        None
    };

    {
        let p = params.clone();
        let m = master.clone();
        let r = rolle;
        mute.connect_toggled(move |b| {
            let an = b.is_active();
            if r == Rolle::Master {
                m.mute.store(an, Ordering::Relaxed);
                for i in 0..crosspoint::BUSES {
                    p.set("bus", i, "mute", if an { 1.0 } else { 0.0 });
                }
            } else {
                let (ziel, idx) = r.ziel();
                p.set(ziel, idx, "mute", if an { 1.0 } else { 0.0 });
            }
        });
    }
    if let Some(b) = &solo {
        let p = params.clone();
        let b2 = b.clone();
        b.connect_toggled(move |b| {
            let _ = &b2;
            p.set(
                "strip",
                index,
                "solo",
                if b.is_active() { 1.0 } else { 0.0 },
            );
        });
    }
    if let Some(b) = &mono {
        let p = params.clone();
        let r = rolle;
        b.connect_toggled(move |b| {
            let an = if b.is_active() { 1.0 } else { 0.0 };
            match r {
                Rolle::Kanal(i) => {
                    p.set("strip", i, "mono", an);
                }
                Rolle::Bus(i) => {
                    p.set("bus", i, "mono", an);
                }
                Rolle::Master => {}
            }
        });
    }

    // ----- Lowcut, NR, Gate: Klick schaltet, Rechtsklick oeffnet das Menue -----
    //
    // Alle drei schreiben ueber denselben Setter-Pfad wie M/S/MONO. Rechts-
    // klick oeffnet ein kleines Popover; der Wert steht danach im Tooltip
    // (siehe `sync`).
    // Letzte gueltige Werte je Zug: schaltet der Knopf aus und wieder ein,
    // kommt die vorherige Frequenz bzw. Staerke zurueck. Der Wert wird in
    // `sync` aus den Parametern nachgezogen, damit ein Zug von aussen (API,
    // Preset) den Knopf nicht ueberschreibt.
    let lc_zelle = std::rc::Rc::new(Cell::new(80.0f32));
    let nr_zelle = std::rc::Rc::new(Cell::new(0.0f32));
    if let Some(b) = &lc {
        let p = params.clone();
        let hz = lc_zelle.clone();
        {
            let p = p.clone();
            let hz = hz.clone();
            b.connect_toggled(move |btn| {
                let wert = if btn.is_active() { hz.get() } else { 0.0 };
                p.set("strip", index, "lowcut", wert);
            });
        }
        let menue = lowcut_menue(&p, index, hz);
        stufenmenue(b, &menue);
    }
    if let Some(b) = &nr {
        let p = params.clone();
        nr_zelle.set(nr_wert(&p, index).max(0.5));
        let staerke = nr_zelle.clone();
        {
            let p = p.clone();
            let staerke = staerke.clone();
            b.connect_toggled(move |btn| {
                let wert = if btn.is_active() { staerke.get() } else { 0.0 };
                p.set("strip", index, "nr", wert);
            });
        }
        let menue = nr_menue(&p, index, staerke);
        stufenmenue(b, &menue);
    }
    if let Some(b) = &gate_knopf {
        let p = params.clone();
        {
            let p = p.clone();
            b.connect_toggled(move |btn| {
                p.set(
                    "strip",
                    index,
                    "gate",
                    if btn.is_active() { 1.0 } else { 0.0 },
                );
            });
        }
        let menue = gate_menue(&p, index);
        stufenmenue(b, &menue);
    }

    // Reihe 0: M | EQ (bzw. ohne EQ: M | S), Reihe 1: S | MONO,
    // Reihe 2: LC | NR, Reihe 3: GATE ueber beide Spalten.
    let mut spalte = 0;
    let mut reihe = 0;
    let mut naechster_platz = || {
        let platz = (spalte, reihe);
        spalte += 1;
        if spalte == 2 {
            spalte = 0;
            reihe += 1;
        }
        platz
    };
    let (sx, sy) = naechster_platz();
    knopf_zeile.attach(&mute, sx, sy, 1, 1);
    if let Some(b) = &eq_knopf {
        let (x, y) = naechster_platz();
        knopf_zeile.attach(b, x, y, 1, 1);
    }
    if let Some(b) = &solo {
        let (x, y) = naechster_platz();
        knopf_zeile.attach(b, x, y, 1, 1);
    }
    if let Some(b) = &mono {
        let (x, y) = naechster_platz();
        knopf_zeile.attach(b, x, y, 1, 1);
    }
    if let Some(b) = &lc {
        let (x, y) = naechster_platz();
        knopf_zeile.attach(b, x, y, 1, 1);
    }
    if let Some(b) = &nr {
        let (x, y) = naechster_platz();
        knopf_zeile.attach(b, x, y, 1, 1);
    }
    if let Some(b) = &gate_knopf {
        // GATE steht allein in der letzten Reihe und nutzt beide Spalten.
        let (_, y) = naechster_platz();
        knopf_zeile.attach(b, 0, y, 2, 1);
    }
    box_.append(&knopf_zeile);

    // ----- Namen bearbeiten -----
    {
        let stack = stack.clone();
        let eingabe2 = eingabe.clone();
        let label2 = label.clone();
        let r = rolle;
        let click = gtk::GestureClick::new();
        click.set_button(1);
        let stack2 = stack.clone();
        let eingabe3 = eingabe.clone();
        let p2 = params.clone();
        click.connect_pressed(move |_, n, _, _| {
            if n < 2 {
                return;
            }
            let aktuell = match p2.label(r.ziel().0, r.ziel().1) {
                Some(l) if !l.is_empty() => l,
                _ => r.name(),
            };
            eingabe3.set_text(&aktuell);
            eingabe3.select_region(0, -1);
            stack2.set_visible_child_name("eingabe");
            eingabe3.grab_focus();
        });
        label.add_controller(click);

        // Uebernehmen: Enter oder Fokusverlust.
        let commit = {
            let stack = stack.clone();
            let label = label2.clone();
            let eingabe = eingabe2.clone();
            let p = params.clone();
            let r = rolle;
            std::rc::Rc::new(move || {
                let text = eingabe.text().to_string();
                let (ziel, idx) = r.ziel();
                if ziel != "master" {
                    p.set_text(ziel, idx, "label", &text);
                }
                let neu = if text.trim().is_empty() {
                    r.name()
                } else {
                    text.trim().to_string()
                };
                label.set_text(&crate::fmt::kurz(&neu, 16));
                stack.set_visible_child_name("anzeige");
            })
        };
        let c = commit.clone();
        eingabe.connect_activate(move |_| c());
        let c = commit.clone();
        let fokus = gtk::EventControllerFocus::new();
        fokus.connect_leave(move |_| c());
        eingabe.add_controller(fokus);
    }

    // ----- Nachfuehren aus Params -----
    let sync: std::rc::Rc<dyn Fn()> = {
        let p = params.clone();
        let r = rolle;
        let zuordnung_knoepfe = zuordnung_knoepfe.clone();
        let label = label.clone();
        let mute = mute.clone();
        let solo = solo.clone();
        let mono = mono.clone();
        let lc = lc.clone();
        let nr = nr.clone();
        let gate_knopf = gate_knopf.clone();
        let lc_zelle = lc_zelle.clone();
        let nr_zelle = nr_zelle.clone();
        let m = master.clone();
        std::rc::Rc::new(move || {
            let (ziel, idx) = r.ziel();
            let name = if ziel == "master" {
                r.name()
            } else {
                match p.label(ziel, idx) {
                    Some(l) if !l.is_empty() => l,
                    _ => r.name(),
                }
            };
            let kurz = crate::fmt::kurz(&name, 16);
            if label.text() != kurz {
                label.set_text(&kurz);
            }
            // Der Name wird nur bei Platzmangel gekuerzt; vollstaendig steht er
            // im Tooltip.
            if label.tooltip_text().as_deref() != Some(name.as_str()) {
                label.set_tooltip_text(Some(&name));
            }
            let (mute_an, solo_an, mono_an) = match r {
                Rolle::Master => (m.mute.load(Ordering::Relaxed), false, false),
                Rolle::Kanal(i) => (
                    p.strips[i].mute.load(Ordering::Relaxed),
                    p.strips[i].solo.load(Ordering::Relaxed),
                    p.strips[i].mono.load(Ordering::Relaxed),
                ),
                Rolle::Bus(i) => (
                    p.buses[i].mute.load(Ordering::Relaxed),
                    false,
                    p.buses[i].mono.load(Ordering::Relaxed),
                ),
            };
            if mute.is_active() != mute_an {
                mute.set_active(mute_an);
            }
            if let Some(b) = &solo {
                if b.is_active() != solo_an {
                    b.set_active(solo_an);
                }
            }
            if let Some(b) = &mono {
                if b.is_active() != mono_an {
                    b.set_active(mono_an);
                }
            }
            if let Rolle::Kanal(i) = r {
                if let Some(b) = &lc {
                    let hz = f32_von(&p.strips[i].lc_hz);
                    // Von aussen gesetzte Werte (API, Preset) werden zur
                    // letzten Frequenz des Knopfes - sonst schriebe das
                    // naechste Einschalten den alten Wert zurueck.
                    if hz > 0.0 {
                        lc_zelle.set(hz);
                    }
                    if b.is_active() != (hz > 0.0) {
                        b.set_active(hz > 0.0);
                    }
                    b.set_tooltip_text(Some(&lc_hinweis(hz)));
                }
                if let Some(b) = &nr {
                    let st = nr_wert(&p, i);
                    if st > 0.0 {
                        nr_zelle.set(st);
                    }
                    if b.is_active() != (st > 0.0) {
                        b.set_active(st > 0.0);
                    }
                    b.set_tooltip_text(Some(&nr_hinweis(&p, i)));
                }
                if let Some(b) = &gate_knopf {
                    let an = p.strips[i].gate.on.load(Ordering::Relaxed);
                    if b.is_active() != an {
                        b.set_active(an);
                    }
                    b.set_tooltip_text(Some(&gate_hinweis(&p, i)));
                }
            }
            if let Rolle::Kanal(i) = r {
                for (bus, b) in zuordnung_knoepfe.iter().enumerate() {
                    let an = state::send_an(&p, i, bus);
                    if b.is_active() != an {
                        b.set_active(an);
                    }
                }
            }
        })
    };
    sync();

    Zug {
        widget: box_.upcast(),
        view,
        sync,
        geraetewahl,
    }
}

/* ==================================================================
Popover der drei Stufen (Lowcut, Rauschunterdrueckung, Gate)
------------------------------------------------------------------
Alle Werte gehen ueber `Params::set` – derselbe Weg wie M/S/MONO, also
auch derselbe Weg in Preset und Autosave. Der Tooltip des Knopfes zeigt
den aktuellen Wert (gesetzt in `sync`).
================================================================== */

/// Wert eines Atoms als f32.
fn f32_von(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}

/// Staerke der Rauschunterdrueckung eines Streifens (0..1).
fn nr_wert(p: &Arc<Params>, index: usize) -> f32 {
    f32_von(&p.strips[index].nr)
}

fn lc_hinweis(hz: f32) -> String {
    if hz > 0.0 {
        format!("Lowcut {hz:.0} Hz (12 dB/Okt) – Rechtsklick: 60/80/120/160 Hz")
    } else {
        "Lowcut aus – Rechtsklick: 60/80/120/160 Hz".to_string()
    }
}

fn nr_hinweis(p: &Arc<Params>, i: usize) -> String {
    let st = nr_wert(p, i);
    let modus = match p.strips[i].nr_modus.load(Ordering::Relaxed) {
        1 => "einfacher Denoiser",
        2 => "RNNoise",
        _ => "eigene Berechnung",
    };
    let mut s = if st > 0.0 {
        format!(
            "NR {st:.1} ({modus}), Schwelle {:.0} dBFS – Rechtsklick: Staerke/Schwelle",
            f32_von(&p.strips[i].nr_schwelle)
        )
    } else {
        "NR aus – Rechtsklick: Staerke/Schwelle".to_string()
    };
    if !crosspoint::rnnoise::hinweis().is_empty() {
        s.push_str(" – RNNoise verfuegbar");
    }
    s
}

fn gate_hinweis(p: &Arc<Params>, i: usize) -> String {
    let g = &p.strips[i].gate;
    format!(
        "GATE {:.0} dBFS, Hold {:.0} ms, Tiefe {:.0} dB – Rechtsklick: einstellen",
        f32_von(&g.threshold),
        f32_von(&g.hold),
        f32_von(&g.tiefe)
    )
}

/// Haengt ein Popover an einen Knopf; Rechtsklick oeffnet es.
fn stufenmenue(knopf: &gtk::ToggleButton, inhalt: &gtk::Box) {
    let pop = gtk::Popover::new();
    pop.set_parent(knopf);
    pop.set_child(Some(inhalt));
    pop.add_css_class("stufenmenue");
    let g = gtk::GestureClick::new();
    g.set_button(3);
    let pop2 = pop.clone();
    g.connect_pressed(move |_, _, x, y| {
        pop2.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop2.popup();
    });
    knopf.add_controller(g);
}

/// Schliesst das Popover, in dem `w` steckt (nach einer Auswahl).
fn menue_zu(w: &impl IsA<gtk::Widget>) {
    let oben: gtk::Widget = w.clone().upcast();
    if let Some(p) = oben
        .ancestor(gtk::Popover::static_type())
        .and_then(|x| x.downcast::<gtk::Popover>().ok())
    {
        p.popdown();
    }
}

/// Eine Zeile „Titel + Wert" mit Schieberegler darunter.
fn schieber(
    titel: &str,
    min: f64,
    max: f64,
    schritt: f64,
    wert: f64,
    einheit: &str,
) -> (gtk::Box, gtk::Scale, gtk::Label) {
    let kasten = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let kopf = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let t = gtk::Label::new(Some(titel));
    t.set_xalign(0.0);
    t.set_hexpand(true);
    t.add_css_class("stufen-titel");
    let w = gtk::Label::new(Some(&format!("{wert:.1}{einheit}")));
    w.add_css_class("stufen-wert");
    kopf.append(&t);
    kopf.append(&w);
    let s = gtk::Scale::with_range(gtk::Orientation::Horizontal, min, max, schritt);
    s.set_draw_value(false);
    s.set_value(wert);
    s.set_size_request(180, -1);
    s.add_css_class("stufenregler");
    kasten.append(&kopf);
    kasten.append(&s);
    (kasten, s, w)
}

fn kopfzeile(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.add_css_class("stufen-kopf");
    l
}

/// Lowcut: vier Grenzfrequenzen und „aus".
fn lowcut_menue(p: &Arc<Params>, index: usize, hz: std::rc::Rc<Cell<f32>>) -> gtk::Box {
    let kasten = gtk::Box::new(gtk::Orientation::Vertical, 2);
    kasten.add_css_class("stufenmenue");
    kasten.append(&kopfzeile("LOWCUT · HOCHPASS 12 dB/OKT"));
    for (wert, text) in [
        (60.0f32, "60 Hz"),
        (80.0, "80 Hz"),
        (120.0, "120 Hz"),
        (160.0, "160 Hz"),
        (0.0, "aus"),
    ] {
        let b = gtk::Button::with_label(text);
        b.add_css_class("stufenwahl");
        let p2 = p.clone();
        let hz2 = hz.clone();
        b.connect_clicked(move |b| {
            p2.set("strip", index, "lowcut", wert);
            if wert > 0.0 {
                hz2.set(wert);
            }
            menue_zu(b);
        });
        kasten.append(&b);
    }
    kasten
}

/// Rauschunterdrueckung: Staerke, Schwelle, Expander, Modus.
fn nr_menue(p: &Arc<Params>, index: usize, staerke: std::rc::Rc<Cell<f32>>) -> gtk::Box {
    let kasten = gtk::Box::new(gtk::Orientation::Vertical, 5);
    kasten.add_css_class("stufenmenue");
    kasten.append(&kopfzeile("RAUSCHUNTERDRUECKUNG"));

    let st = nr_wert(p, index);
    let (k1, s1, w1) = schieber("Staerke", 0.0, 100.0, 10.0, st as f64 * 100.0, "");
    {
        let p = p.clone();
        let staerke = staerke.clone();
        let w1 = w1.clone();
        s1.connect_value_changed(move |s| {
            let v = (s.value() / 100.0) as f32;
            w1.set_text(&format!("{v:.1}"));
            if v > 0.0 {
                staerke.set(v);
            }
            p.set("strip", index, "nr", v);
        });
    }
    kasten.append(&k1);

    let sch = f32_von(&p.strips[index].nr_schwelle);
    let (k2, s2, w2) = schieber(
        "Schwelle (Expander)",
        -60.0,
        -20.0,
        1.0,
        sch as f64,
        " dBFS",
    );
    {
        let p = p.clone();
        let w2 = w2.clone();
        s2.connect_value_changed(move |s| {
            w2.set_text(&format!("{:.0} dBFS", s.value()));
            p.set("strip", index, "nr_schwelle", s.value() as f32);
        });
    }
    kasten.append(&k2);

    let exp = gtk::CheckButton::with_label("Expander in der NR");
    exp.set_active(p.strips[index].nr_gate.load(Ordering::Relaxed));
    exp.set_tooltip_text(Some(
        "Aus: die NR senkt nur spektral. Fuer Sprachpausen ist der GATE-Knopf da.",
    ));
    {
        let p = p.clone();
        exp.connect_toggled(move |c| {
            p.set(
                "strip",
                index,
                "nr_gate",
                if c.is_active() { 1.0 } else { 0.0 },
            );
        });
    }
    kasten.append(&exp);

    let mut modi: Vec<&str> = vec!["Eigene Berechnung", "Einfacher Denoiser"];
    let rn_da = !crosspoint::rnnoise::hinweis().is_empty();
    if rn_da {
        modi.push("RNNoise (Bibliothek)");
    }
    let dd = gtk::DropDown::from_strings(&modi);
    // Ohne Bibliothek gibt es nur zwei Eintraege; ein gespeicherter Modus 2
    // faellt dann auf die eigene Berechnung (so rechnet auch die Engine).
    let gespeichert = p.strips[index].nr_modus.load(Ordering::Relaxed);
    dd.set_selected(if rn_da {
        gespeichert.min(2)
    } else {
        gespeichert.min(1)
    });
    {
        let p = p.clone();
        dd.connect_selected_notify(move |d| {
            p.set("strip", index, "nr_modus", d.selected() as f32);
        });
    }
    kasten.append(&dd);
    // Nur wenn die Bibliothek da ist, steht hier etwas Zusaetzliches. Fehlt
    // sie, bleibt das Menue vollstaendig und nichts ist ausgegraut.
    if rn_da {
        let l = gtk::Label::new(Some(crosspoint::rnnoise::hinweis()));
        l.set_xalign(0.0);
        l.add_css_class("stufen-hinweis");
        kasten.append(&l);
    }
    kasten
}

/// Gate: Schwelle, Hold, Tiefe.
fn gate_menue(p: &Arc<Params>, index: usize) -> gtk::Box {
    let kasten = gtk::Box::new(gtk::Orientation::Vertical, 5);
    kasten.add_css_class("stufenmenue");
    kasten.append(&kopfzeile("GATE · STILLE UNTER DER SCHWELLE"));

    let g = &p.strips[index].gate;
    let sch = f32_von(&g.threshold);
    let (k1, s1, w1) = schieber("Schwelle", -60.0, -20.0, 1.0, sch as f64, " dBFS");
    {
        let p = p.clone();
        let w1 = w1.clone();
        s1.connect_value_changed(move |s| {
            w1.set_text(&format!("{:.0} dBFS", s.value()));
            p.set("strip", index, "gate_schwelle", s.value() as f32);
        });
    }
    kasten.append(&k1);

    let hold = f32_von(&g.hold);
    let (k2, s2, w2) = schieber("Hold", 0.0, 500.0, 10.0, hold as f64, " ms");
    {
        let p = p.clone();
        let w2 = w2.clone();
        s2.connect_value_changed(move |s| {
            w2.set_text(&format!("{:.0} ms", s.value()));
            p.set("strip", index, "gate_hold", s.value() as f32);
        });
    }
    kasten.append(&k2);

    let tiefe = f32_von(&g.tiefe);
    let (k3, s3, w3) = schieber("Tiefe", 0.0, 80.0, 5.0, tiefe as f64, " dB");
    {
        let p = p.clone();
        let w3 = w3.clone();
        s3.connect_value_changed(move |s| {
            w3.set_text(&format!("{:.0} dB", s.value()));
            p.set("strip", index, "gate_tiefe", s.value() as f32);
        });
    }
    kasten.append(&k3);
    kasten
}
