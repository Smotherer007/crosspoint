//! Fensteraufbau aus der Layout-Beschreibung.
//!
//! Es gibt keine fest verdrahtete Anordnung: `layout::Plan` sagt, welche
//! Bereiche in welcher Reihenfolge in welcher Zeile stehen; hier werden sie
//! eingesetzt, ausgeblendet oder verschoben. Ein Layout-Wechsel baut nur die
//! Container neu - der Zustand (`Arc<Params>`) bleibt derselbe.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crosspoint::engine::Params;
use crosspoint::macros_io::MacroControl;
use crosspoint::pegel::Pegelbank;
use crosspoint::tape_io::TapeControl;
use gtk::prelude::*;

use crate::config::Konfiguration;
use crate::geraete::{self, Bestand};
use crate::layout::{Bereich, Dichte, Masse, MasterPlatz, Seite};
use crate::strip::{self, GeraeteUi, Master, Rolle, Takt, Zug, ZugUmfeld};
use crate::theme::{self, Tokens};
use crate::Anbindung;

/// Tape-Leiste: Aufnahme/Stopp, Laufzeit, Spitzenpegel, Zielordner, letzte Datei.
pub struct TapeUi {
    pub rec: RefCell<Option<gtk::ToggleButton>>,
    pub stop: RefCell<Option<gtk::Button>>,
    pub zeit: RefCell<Option<gtk::Label>>,
    pub segs: RefCell<Vec<gtk::Box>>,
    pub datei: RefCell<Option<gtk::Label>>,
    pub status: RefCell<Option<gtk::Label>>,
    pub laeuft: Cell<bool>,
    pub start: Cell<Instant>,
    pub letzte_segmente: Cell<u32>,
    pub ctl: RefCell<Option<Arc<TapeControl>>>,
    pub ordner: RefCell<std::path::PathBuf>,
}

impl Default for TapeUi {
    fn default() -> Self {
        TapeUi {
            rec: RefCell::new(None),
            stop: RefCell::new(None),
            zeit: RefCell::new(None),
            segs: RefCell::new(Vec::new()),
            datei: RefCell::new(None),
            status: RefCell::new(None),
            laeuft: Cell::new(false),
            start: Cell::new(Instant::now()),
            letzte_segmente: Cell::new(0),
            ctl: RefCell::new(None),
            ordner: RefCell::new(std::path::PathBuf::new()),
        }
    }
}

/// Die Anwendung: ein Fenster, ein Zustand, viele Zuege.
pub struct Shell {
    pub params: Arc<Params>,
    /// Pegelbank des Kerns: der Audio-Thread schreibt, die Zuege lesen.
    pub pegel: Arc<Pegelbank>,
    pub sample_rate: u32,
    pub tokens: Arc<RwLock<Tokens>>,
    pub masse: Arc<RwLock<Masse>>,
    pub master: Arc<Master>,
    pub takt: Arc<Takt>,
    pub konfig: RefCell<Konfiguration>,
    pub provider: RefCell<Option<gtk::CssProvider>>,
    pub fenster: gtk::ApplicationWindow,
    pub inhalt: gtk::Box,
    /// Rollbereich um den Inhalt: bei zu schmalem Fenster wird horizontal
    /// gescrollt, statt Zuege ausserhalb des Fensters zu zeichnen.
    pub scroll: gtk::ScrolledWindow,
    /// Zuletzt benutzte Innenbreite (gegen Selbstausloesung beim Nachrechnen).
    pub letzte_breite: Cell<f64>,
    pub zuege: RefCell<Vec<Zug>>,
    pub tape: TapeUi,
    pub status: RefCell<Option<gtk::Label>>,
    pub hinweis_label: RefCell<Option<gtk::Label>>,
    pub themenwahl: RefCell<Option<gtk::DropDown>>,
    pub frames: Cell<u64>,
    pub frames_zeit: Cell<Instant>,
    pub start: Instant,
    pub sichtbare_streifen: RefCell<Vec<usize>>,
    pub geraete: Rc<GeraeteUi>,
    pub geraete_empfang: RefCell<Option<Receiver<Bestand>>>,
    pub geraete_takt: Cell<Instant>,
    /// Makro-Steuerung des Kerns (dieselbe Instanz, die der Engine-Thread
    /// abarbeitet – ein Klick hier wirkt dort).
    pub makros: RefCell<Option<Arc<MacroControl>>>,
    pub makro_knoepfe: RefCell<Vec<gtk::Button>>,
    pub makro_seite: Cell<usize>,
    pub makro_seiten: RefCell<Vec<Option<macros::Bank>>>,
}

impl Shell {
    /// Baut das Fenster auf dem laufenden Kern.
    ///
    /// `an` kommt von aussen (`crosspoint::lauf::Lauf`): dieselben `Params`,
    /// dieselbe Pegelbank, dieselbe Tape-Steuerung und Makro-Steuerung, die
    /// die Engine benutzt. Die Oberflaeche legt nichts davon selbst an – sonst
    /// waere sie wieder eine zweite Wahrheit neben dem Ton.
    pub fn neu(app: &gtk::Application, konfig: Konfiguration, an: Anbindung) -> Rc<Shell> {
        let tokens = konfig.theme.tokens();
        let masse = Masse::aus(&tokens, konfig.layout.aktuell().dichte);

        let fenster = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Crosspoint")
            .default_width(1600)
            .default_height(920)
            .build();
        fenster.add_css_class("grund");

        let inhalt = gtk::Box::new(gtk::Orientation::Vertical, 0);
        inhalt.add_css_class("grund");
        // Rollbereich: wird das Fenster schmaler als die Mindestbreiten des
        // Pults, scrollt die Oberflaeche horizontal - gezeichnet wird nie
        // ausserhalb des Fensters. `propagate_natural_width` bleibt aus, sonst
        // fordert der Inhalt die Fensterbreite ein, statt zu scrollen (das
        // Fenster waere dann breiter als der Bildschirm). Die Leiste ist keine
        // Ueberlagerung, sondern nimmt Platz - sie muss sichtbar sein, sonst
        // merkt niemand, dass rechts noch Zuege stehen.
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&inhalt));
        scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
        scroll.set_overlay_scrolling(false);
        scroll.set_propagate_natural_height(true);
        fenster.set_child(Some(&scroll));

        let (tx, rx) = std::sync::mpsc::channel::<Bestand>();

        let s = Rc::new(Shell {
            params: an.params,
            pegel: an.pegel,
            sample_rate: an.sample_rate,
            tokens: Arc::new(RwLock::new(tokens)),
            masse: Arc::new(RwLock::new(masse)),
            master: Arc::new(Master::default()),
            takt: Arc::new(Takt::default()),
            konfig: RefCell::new(konfig),
            provider: RefCell::new(None),
            fenster,
            inhalt,
            scroll,
            letzte_breite: Cell::new(0.0),
            zuege: RefCell::new(Vec::new()),
            tape: TapeUi::default(),
            status: RefCell::new(None),
            hinweis_label: RefCell::new(None),
            themenwahl: RefCell::new(None),
            frames: Cell::new(0),
            frames_zeit: Cell::new(Instant::now()),
            start: Instant::now(),
            sichtbare_streifen: RefCell::new(Vec::new()),
            geraete: Rc::new(GeraeteUi::neu()),
            geraete_empfang: RefCell::new(Some(rx)),
            geraete_takt: Cell::new(Instant::now()),
            makros: RefCell::new(None),
            makro_knoepfe: RefCell::new(Vec::new()),
            makro_seite: Cell::new(0),
            makro_seiten: RefCell::new(vec![None; 6]),
        });

        // CSS aus dem Token-Satz anwenden (ein Provider, austauschbar).
        let p = theme::anwenden(&s.tokens.read().unwrap().clone(), None);
        *s.provider.borrow_mut() = p;

        // Geraeteliste im Hintergrund holen (blockiert die Oberflaeche nicht).
        {
            let tx = tx.clone();
            std::thread::spawn(move || loop {
                let b = geraete::laden();
                if tx.send(b).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_secs(5));
            });
        }

        // Tape: Steuerung und Ordner kommen vom Kern – der Ring und der
        // Schreiber-Thread laufen in der Engine, hier wird nur bedient.
        {
            *s.tape.ordner.borrow_mut() = an.tape_ordner;
            *s.tape.ctl.borrow_mut() = Some(an.tape);
        }

        // Makros: dieselbe Bank, die der Engine-Thread abarbeitet.
        *s.makros.borrow_mut() = Some(an.makros);

        Shell::bauen(&s);

        // Die Spaltenbreiten haengen an der Breite, die dem Inhalt wirklich zur
        // Verfuegung steht (Rollbereich minus senkrechte Leiste). Die Zahl
        // kommt vom Einstellwert der waagerechten Leiste und aendert sich mit
        // jedem Groessenschritt des Fensters - nicht aus `fenster.width()`,
        // denn das ist die Aussenbreite samt Leisten.
        {
            let this = Rc::downgrade(&s);
            let adj = s.scroll.hadjustment();
            adj.connect_notify_local(Some("page-size"), move |adj, _| {
                if let Some(s) = this.upgrade() {
                    Shell::breiten_anwenden(&s, adj.page_size());
                }
            });
        }

        Shell::takt_starten(&s);
        s
    }

    /// Spaltenbreiten nach der verfuegbaren Breite setzen.
    ///
    /// Rechnet ueber [`crate::layout::spaltenbreiten`] und gibt das Ergebnis an
    /// die Zuege weiter: Streifen, Busse und Master bekommen ihre Breite
    /// zugeteilt, statt sie aus ihrem Inhalt zu fordern. Dadurch passt bei
    /// 1600 px alles ins Fenster, und bei schmaleren Fenstern bleiben die
    /// Mindestbreiten stehen (die Leiste scrollt).
    pub fn breiten_anwenden(this: &Rc<Shell>, verfuegbar: f64) {
        // Vor der ersten Zuteilung ist der Einstellwert 0; dann mit der
        // Fensterbreite rechnen (die Leiste korrigiert gleich nach).
        let breite = if verfuegbar > 1.0 {
            verfuegbar
        } else {
            this.fenster.default_width() as f64
        };
        if (breite - this.letzte_breite.get()).abs() < 1.0 && this.letzte_breite.get() > 1.0 {
            return;
        }
        this.letzte_breite.set(breite);

        let layout = this.konfig.borrow().layout.aktuell();
        let masse = this.masse_jetzt();
        let streifen = this
            .zuege
            .borrow()
            .iter()
            .filter(|z| matches!(z.view.rolle(), Rolle::Kanal(_)))
            .count()
            .max(1);
        let busse = this
            .zuege
            .borrow()
            .iter()
            .filter(|z| matches!(z.view.rolle(), Rolle::Bus(_)))
            .count();
        let master_da = this
            .zuege
            .borrow()
            .iter()
            .any(|z| z.view.rolle() == Rolle::Master);
        // Steht der Master nicht in der Busspalte, kostet er eine eigene Luecke
        // samt Blockrand - die Breitenrechnung kennt nur den Normalfall.
        let eigene_spalte = master_da && layout.master != MasterPlatz::InBusspalte;
        let platz = if eigene_spalte {
            (breite - masse.abstand - 2.0 * crate::layout::BLOCK_RAND).max(0.0)
        } else {
            breite
        };
        let b = crate::layout::spaltenbreiten(platz, masse.abstand, streifen, busse, master_da);
        for z in this.zuege.borrow().iter() {
            let w = match z.view.rolle() {
                Rolle::Kanal(_) => b.streifen,
                Rolle::Bus(_) => b.bus,
                Rolle::Master => b.master,
            };
            // Der Rahmen des Zugs (CSS: 1 px je Seite) gehoert zur Spalte: die
            // Zeichenflaeche bekommt 2 px weniger, damit der Zug insgesamt
            // genau `w` belegt und die Summe zur Rechnung passt.
            z.widget.set_size_request(w as i32, -1);
            z.view.breite_setzen(w - 2.0);
        }
        if std::env::var_os("CROSSPOINT_UI_BREITEN").is_some() {
            eprintln!("Spalten bei {breite:.0} px: {}", b.zeile());
        }
    }

    /// Kurze Meldung im Kopf (Erfolg/Misserfolg sichtbar machen).
    pub fn hinweis(&self, text: &str, fehler: bool) {
        if let Some(l) = self.hinweis_label.borrow().as_ref() {
            l.set_text(text);
            if fehler {
                l.add_css_class("fehler");
            } else {
                l.remove_css_class("fehler");
            }
        }
        eprintln!("crosspoint-gtk: {text}");
    }

    /// Wie [`Shell::hinweis`], aber nur bei geändertem Text – der Gerätetakt
    /// läuft alle 400 ms und würde sonst dasselbe ins Protokoll schreiben.
    pub fn hinweis_neu(&self, text: &str, fehler: bool) {
        let gleich = self
            .hinweis_label
            .borrow()
            .as_ref()
            .map(|l| l.text().to_string() == text)
            .unwrap_or(false);
        if !gleich {
            self.hinweis(text, fehler);
        }
    }

    pub fn tokens_jetzt(&self) -> Tokens {
        match self.tokens.read() {
            Ok(t) => t.clone(),
            Err(e) => e.into_inner().clone(),
        }
    }

    fn masse_jetzt(&self) -> Masse {
        match self.masse.read() {
            Ok(m) => *m,
            Err(e) => *e.into_inner(),
        }
    }

    /// Nach dem Laden eines Presets: Anzeigen aus `Params` nachziehen.
    pub fn presets_auffrischen(&self) {
        for z in self.zuege.borrow().iter() {
            (z.sync)();
            z.view.queue_draw();
        }
    }

    /// Baut die Container aus dem Plan neu auf.
    pub fn bauen(this: &Rc<Shell>) {
        while let Some(kind) = this.inhalt.first_child() {
            this.inhalt.remove(&kind);
        }
        this.zuege.borrow_mut().clear();
        this.tape.segs.borrow_mut().clear();
        this.makro_knoepfe.borrow_mut().clear();

        let layout = this.konfig.borrow().layout.aktuell();
        let plan = layout.plan();
        let masse = this.masse_jetzt();
        let abstand = (masse.abstand.round() as i32).max(3);

        this.inhalt.set_spacing(abstand);
        this.inhalt.set_margin_top(abstand);
        this.inhalt.set_margin_bottom(abstand);
        this.inhalt.set_margin_start(abstand);
        this.inhalt.set_margin_end(abstand);

        if !plan.oben.is_empty() {
            // Kopfzeile und Tape-Leiste stehen beide oben. Jede bekommt ihre
            // eigene Zeile: nebeneinander waeren sie zusammen breiter als das
            // Fenster, und die Tape-Leiste (REC/STOP) waere nicht erreichbar.
            for b in plan.oben.iter().copied() {
                let zeile = gtk::Box::new(gtk::Orientation::Horizontal, abstand);
                zeile.set_hexpand(true);
                match b {
                    Bereich::Kopf => {
                        let kopf = Shell::kopf_bauen(this);
                        kopf.set_hexpand(true);
                        zeile.append(&kopf);
                    }
                    Bereich::Tape => {
                        let tape = Shell::tape_bauen(this);
                        tape.set_hexpand(true);
                        zeile.append(&tape);
                    }
                    _ => {}
                }
                this.inhalt.append(&zeile);
            }
        }

        if !plan.mitte.is_empty() {
            let zeile = gtk::Box::new(gtk::Orientation::Horizontal, abstand);
            zeile.set_vexpand(true);
            zeile.set_hexpand(true);
            for b in plan.mitte.iter().copied() {
                match b {
                    Bereich::Streifen => {
                        let w = Shell::streifen_bauen(this, &layout);
                        w.set_hexpand(true);
                        zeile.append(&w);
                    }
                    Bereich::Busse => {
                        let w = Shell::busse_bauen(this, &layout);
                        zeile.append(&w);
                    }
                    Bereich::Master => {
                        let w = Shell::master_bauen(this);
                        zeile.append(&w);
                    }
                    _ => {}
                }
            }
            this.inhalt.append(&zeile);
        }

        if plan.unten.contains(&Bereich::Makros) {
            let makros = Shell::makros_bauen(this, layout.makros_eingeklappt);
            this.inhalt.append(&makros);
        }

        Shell::alle_sync(this);

        // Nach jedem Neubau (Layout-/Theme-Wechsel) die Breiten neu zuteilen:
        // die neuen Zuege starten mit ihrer Vorgabebreite.
        let verfuegbar = this.scroll.hadjustment().page_size();
        this.letzte_breite.set(0.0);
        Shell::breiten_anwenden(this, verfuegbar);

        // Diagnose (CROSSPOINT_UI_BREITEN=1): einmal nach der Zuteilung
        // ausgeben, welche Breite wovon gefordert wird. Ohne die Variable
        // passiert nichts.
        if std::env::var_os("CROSSPOINT_UI_BREITEN").is_some() {
            let this2 = Rc::clone(this);
            gtk::glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                Shell::breiten_probe(&this2);
            });
        }
    }

    /// Einmalige Breitendiagnose: Fenster, Inhalt, Hauptzeile und die kleinste
    /// Forderung je Kind. Nur fuer die Fehlersuche (Umgebungsvariable).
    fn breiten_probe(this: &Rc<Shell>) {
        let (min, nat, _, _) = this.inhalt.measure(gtk::Orientation::Horizontal, -1);
        let mut zeile = String::new();
        if this.inhalt.first_child().is_some() {
            let mut k = this.inhalt.first_child();
            while let Some(w) = k {
                let (mi, na, _, _) = w.measure(gtk::Orientation::Horizontal, -1);
                let art = w.type_().name().to_string();
                zeile.push_str(&format!("\n    {art:28} min {mi:5} nat {na:5}"));
                // Eine Ebene tiefer: was fordert das Kind selbst?
                let mut e = w.first_child();
                while let Some(x) = e {
                    let (mi, na, _, _) = x.measure(gtk::Orientation::Horizontal, -1);
                    zeile.push_str(&format!(
                        "\n      {:26} min {mi:5} nat {na:5}",
                        x.type_().name()
                    ));
                    e = x.next_sibling();
                }
                k = w.next_sibling();
            }
        }
        let (hmin, hnat, _, _) = this.inhalt.measure(gtk::Orientation::Vertical, -1);
        let mut hoehen = String::new();
        let mut k = this.inhalt.first_child();
        while let Some(w) = k {
            let (mi, na, _, _) = w.measure(gtk::Orientation::Vertical, -1);
            hoehen.push_str(&format!(" {}:{mi}/{na}", w.type_().name()));
            k = w.next_sibling();
        }
        eprintln!(
            "Breiten: Fenster {}x{}, Inhalt min {min} nat {nat}, Hoehe min {hmin} nat {hnat}{hoehen}{zeile}",
            this.fenster.width(),
            this.fenster.height()
        );
        // Tiefer: der erste Zug im Streifenblock und im Busblock, zwei Ebenen.
        let mut mitte = this.inhalt.first_child();
        let mut i = 0;
        while let Some(w) = mitte {
            if i == 2 {
                let mut block = w.first_child();
                while let Some(b) = block {
                    if let Some(zug) = b.first_child() {
                        eprintln!("  Zug-Block {}:", b.type_().name());
                        dump(&zug, 2);
                    }
                    block = b.next_sibling();
                }
            }
            i += 1;
            mitte = w.next_sibling();
        }
        for z in this.zuege.borrow().iter() {
            let (mi, na, _, _) = z.view.measure(gtk::Orientation::Horizontal, -1);
            eprintln!(
                "  Zug {:?}: zugeteilt {} px, min {mi}, nat {na}",
                z.view.rolle(),
                z.view.width()
            );
        }
    }

    fn kopf_bauen(this: &Rc<Shell>) -> gtk::Widget {
        let kopf = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        kopf.add_css_class("kopf");
        kopf.set_valign(gtk::Align::Start);

        let marke = gtk::Label::new(Some("crosspoint"));
        marke.add_css_class("marke");
        kopf.append(&marke);
        let punkt = gtk::Label::new(Some("▪"));
        punkt.add_css_class("marke-punkt");
        punkt.set_valign(gtk::Align::Center);
        kopf.append(&punkt);

        let trenner = gtk::Box::new(gtk::Orientation::Vertical, 0);
        trenner.add_css_class("marke-trenner");
        trenner.set_valign(gtk::Align::Center);
        kopf.append(&trenner);

        let unter = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let ub = gtk::Label::new(Some("8×8 DIGITAL MIXER"));
        ub.add_css_class("unterzeile-b");
        ub.set_xalign(0.0);
        let ui = gtk::Label::new(Some("8 x 8 · PIPEWIRE / JACK"));
        ui.add_css_class("unterzeile-i");
        ui.set_xalign(0.0);
        unter.append(&ub);
        unter.append(&ui);
        kopf.append(&unter);

        let sr = f32::from_bits(this.params.sample_rate.load(Ordering::Relaxed));
        let block = this.params.block.load(Ordering::Relaxed);
        kopf.append(&Shell::chip("SR", &format!("{:.1} kHz", sr / 1000.0)));
        kopf.append(&Shell::chip("BLK", &format!("{block}")));
        kopf.append(&Shell::chip(
            "RATE",
            &format!("{:.1} ms", block as f32 / sr * 1000.0),
        ));

        // Meldungszeile: Geraete-/Preset-Fehler sichtbar machen.
        let hinweis = gtk::Label::new(Some(""));
        hinweis.add_css_class("hinweis");
        hinweis.set_ellipsize(gtk::pango::EllipsizeMode::End);
        hinweis.set_max_width_chars(36);
        kopf.append(&hinweis);
        *this.hinweis_label.borrow_mut() = Some(hinweis.clone());
        *this.geraete.hinweis.borrow_mut() = Some(hinweis);

        let platz = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        platz.set_hexpand(true);
        kopf.append(&platz);

        let an = this.takt.aktiv();
        let status = gtk::Label::new(Some(if an { "● LAEUFT" } else { "● BEREIT" }));
        status.add_css_class("status");
        if !an {
            status.add_css_class("aus");
        }
        let log = this.konfig.borrow().log.clone();
        status.set_tooltip_text(Some(&if log.is_empty() {
            "Oberflaeche direkt auf Arc<Params>; kein HTTP".to_string()
        } else {
            log.join("\n")
        }));
        kopf.append(&status);
        *this.status.borrow_mut() = Some(status);

        let namen = this.konfig.borrow().theme.namen();
        let refs: Vec<&str> = namen.iter().map(|s| s.as_str()).collect();
        let dropdown = gtk::DropDown::from_strings(&refs);
        dropdown.set_selected(
            namen
                .iter()
                .position(|n| *n == this.konfig.borrow().theme.aktiv)
                .unwrap_or(0) as u32,
        );
        dropdown.set_tooltip_text(Some("Theme (Konfiguration: ~/.config/crosspoint/ui.json)"));
        {
            let this2 = Rc::clone(this);
            let namen = namen.clone();
            dropdown.connect_selected_notify(move |d| {
                if let Some(name) = namen.get(d.selected() as usize) {
                    Shell::thema_setzen(&this2, name, true);
                }
            });
        }
        kopf.append(&dropdown);
        *this.themenwahl.borrow_mut() = Some(dropdown);

        let bauen = |text: &str| {
            let b = gtk::Button::with_label(text);
            b.add_css_class("tbtn");
            b
        };

        let presets = bauen("PRESETS");
        {
            let this2 = Rc::clone(this);
            presets.connect_clicked(move |_| {
                crate::dialogs::preset_fenster(&this2.fenster, &this2);
            });
        }
        let vban = bauen("VBAN");
        {
            let this2 = Rc::clone(this);
            vban.connect_clicked(move |_| {
                let quelle = crate::dialogs::BeispielVban;
                crate::dialogs::vban_fenster(&this2.fenster, &quelle);
            });
        }
        let midi = bauen("MIDI");
        {
            let this2 = Rc::clone(this);
            midi.connect_clicked(move |_| {
                let namen: Vec<String> = this2
                    .geraete
                    .quellen_namen
                    .borrow()
                    .iter()
                    .flatten()
                    .cloned()
                    .collect();
                crate::dialogs::midi_fenster(&this2.fenster, &namen);
            });
        }
        let darstellung = bauen("DARSTELLUNG");
        {
            let this2 = Rc::clone(this);
            darstellung.connect_clicked(move |_| {
                crate::dialogs::darstellung_fenster(&this2);
            });
        }
        kopf.append(&presets);
        kopf.append(&vban);
        kopf.append(&midi);
        kopf.append(&darstellung);

        kopf.upcast()
    }

    fn chip(schluessel: &str, wert: &str) -> gtk::Widget {
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        b.add_css_class("chip");
        b.set_valign(gtk::Align::Center);
        let k = gtk::Label::new(Some(schluessel));
        k.add_css_class("chip-k");
        let w = gtk::Label::new(Some(wert));
        b.append(&k);
        b.append(&w);
        b.upcast()
    }

    /// Tape-Leiste: eine ruhige Zeile - Aufnahme/Stopp, Laufzeit, Pegel, Ziel,
    /// Status. Vorher standen Ziel und Status untereinander in einer eigenen
    /// Spalte; die Zeile wuchs dadurch in die Hoehe und "idle" wurde
    /// abgeschnitten. Jetzt teilen sich alle Elemente eine Zeile: das Ziel
    /// nimmt den freien Platz (Ellipse mit Tooltip), der Status steht rechts.
    fn tape_bauen(this: &Rc<Shell>) -> gtk::Widget {
        let tape = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        tape.add_css_class("tape");
        tape.set_valign(gtk::Align::Center);

        let rec = gtk::ToggleButton::with_label("● REC");
        rec.add_css_class("tbtn");
        rec.add_css_class("tape-rec");
        {
            let this2 = Rc::clone(this);
            rec.connect_toggled(move |b| {
                let an = b.is_active();
                this2.tape.laeuft.set(an);
                this2.tape.start.set(Instant::now());
                if let Some(ctl) = this2.tape.ctl.borrow().as_ref() {
                    ctl.recording.store(an, Ordering::Relaxed);
                    ctl.set_status(if an {
                        format!("rec [{}]", this2.tape.ordner.borrow().display())
                    } else {
                        "stop".to_string()
                    });
                }
                if let Some(t) = this2.tape.zeit.borrow().as_ref() {
                    t.remove_css_class("aus");
                    // Waehrend der Aufnahme steht die Zeit in Rot: das ist das
                    // Signal, das man im Augenwinkel sieht.
                    if an {
                        t.add_css_class("an");
                    } else {
                        t.remove_css_class("an");
                    }
                }
            });
        }
        let stop = gtk::Button::with_label("■ STOP");
        stop.add_css_class("tbtn");
        {
            let this2 = Rc::clone(this);
            let rec2 = rec.clone();
            stop.connect_clicked(move |_| {
                rec2.set_active(false);
                this2.tape.laeuft.set(false);
                if let Some(ctl) = this2.tape.ctl.borrow().as_ref() {
                    ctl.recording.store(false, Ordering::Relaxed);
                }
                if let Some(t) = this2.tape.zeit.borrow().as_ref() {
                    t.set_text("0:00.000");
                    t.add_css_class("aus");
                    t.remove_css_class("an");
                }
            });
        }
        tape.append(&rec);
        tape.append(&stop);

        let zeit = gtk::Label::new(Some("0:00.000"));
        zeit.add_css_class("tape-zeit");
        zeit.add_css_class("aus");
        tape.append(&zeit);

        let pegel = gtk::Box::new(gtk::Orientation::Horizontal, 1);
        pegel.set_valign(gtk::Align::Center);
        let mut segs = Vec::new();
        for i in 0..22 {
            let s = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            s.add_css_class("seg");
            if i >= 19 {
                s.add_css_class("spitze");
            }
            pegel.append(&s);
            segs.push(s);
        }
        tape.append(&pegel);

        // Ziel: nimmt den freien Platz der Zeile. Gekuerzt wird nur, wenn der
        // Platz nicht reicht (Ellipse in der Mitte, vollstaendiger Pfad im
        // Tooltip) - der Rest der Zeile bleibt stehen.
        let ziel_pfad = this.tape.ordner.borrow().display().to_string();
        let datei = gtk::Label::new(Some(&format!("Ziel: {ziel_pfad}")));
        datei.add_css_class("tape-datei");
        datei.set_xalign(0.0);
        datei.set_hexpand(true);
        datei.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        datei.set_tooltip_text(Some(&format!("Ziel: {ziel_pfad}")));
        tape.append(&datei);

        let st = gtk::Label::new(Some("idle"));
        st.add_css_class("tape-status");
        st.set_xalign(1.0);
        st.set_valign(gtk::Align::Center);
        st.set_ellipsize(gtk::pango::EllipsizeMode::End);
        st.set_max_width_chars(32);
        st.set_tooltip_text(Some("idle"));
        tape.append(&st);

        *this.tape.rec.borrow_mut() = Some(rec);
        *this.tape.stop.borrow_mut() = Some(stop);
        *this.tape.zeit.borrow_mut() = Some(zeit);
        *this.tape.segs.borrow_mut() = segs;
        *this.tape.datei.borrow_mut() = Some(datei);
        *this.tape.status.borrow_mut() = Some(st);

        tape.upcast()
    }

    fn streifen_bauen(this: &Rc<Shell>, layout: &crate::layout::Layout) -> gtk::Widget {
        let box_ = gtk::Box::new(
            gtk::Orientation::Horizontal,
            crate::layout::STREIFEN_ABSTAND as i32,
        );
        box_.set_hexpand(true);
        // Rand des Blocks - er gehoert zur Mindestbreite (siehe layout::BLOCK_RAND).
        box_.set_margin_start(crate::layout::BLOCK_RAND as i32);
        box_.set_margin_end(crate::layout::BLOCK_RAND as i32);
        let indizes = layout.sichtbare_streifen();
        *this.sichtbare_streifen.borrow_mut() = indizes.clone();
        let zuordnung = layout.sichtbar.get(Bereich::Zuordnung);
        for i in indizes {
            let this2 = Rc::clone(this);
            let umfeld = ZugUmfeld {
                geraete: Some(Rc::clone(&this.geraete)),
                zuordnung,
                eq_editor: Some(Rc::new(move |strip: usize| {
                    crate::dialogs::eq_fenster(&this2.fenster, this2.params.clone(), strip);
                })),
                pegel: Some(this.pegel.clone()),
            };
            let z = strip::zug(
                this.params.clone(),
                Rolle::Kanal(i),
                Arc::clone(&this.tokens),
                Arc::clone(&this.masse),
                this.master.clone(),
                this.takt.clone(),
                umfeld,
            );
            box_.append(&z.widget);
            this.zuege.borrow_mut().push(z);
        }
        box_.upcast()
    }

    fn busse_bauen(this: &Rc<Shell>, layout: &crate::layout::Layout) -> gtk::Widget {
        let box_ = gtk::Box::new(
            gtk::Orientation::Horizontal,
            crate::layout::BUS_ABSTAND as i32,
        );
        box_.add_css_class("busse");
        box_.set_margin_start(crate::layout::BLOCK_RAND as i32);
        box_.set_margin_end(crate::layout::BLOCK_RAND as i32);
        for i in 0..crosspoint::BUSES {
            let umfeld = ZugUmfeld {
                geraete: Some(Rc::clone(&this.geraete)),
                zuordnung: false,
                eq_editor: None,
                pegel: Some(this.pegel.clone()),
            };
            let z = strip::zug(
                this.params.clone(),
                Rolle::Bus(i),
                Arc::clone(&this.tokens),
                Arc::clone(&this.masse),
                this.master.clone(),
                this.takt.clone(),
                umfeld,
            );
            box_.append(&z.widget);
            this.zuege.borrow_mut().push(z);
        }
        if layout.master == MasterPlatz::InBusspalte {
            let z = strip::zug(
                this.params.clone(),
                Rolle::Master,
                Arc::clone(&this.tokens),
                Arc::clone(&this.masse),
                this.master.clone(),
                this.takt.clone(),
                ZugUmfeld {
                    pegel: Some(this.pegel.clone()),
                    ..ZugUmfeld::default()
                },
            );
            box_.append(&z.widget);
            this.zuege.borrow_mut().push(z);
        }
        box_.upcast()
    }

    fn master_bauen(this: &Rc<Shell>) -> gtk::Widget {
        let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let z = strip::zug(
            this.params.clone(),
            Rolle::Master,
            Arc::clone(&this.tokens),
            Arc::clone(&this.masse),
            this.master.clone(),
            this.takt.clone(),
            ZugUmfeld {
                pegel: Some(this.pegel.clone()),
                ..ZugUmfeld::default()
            },
        );
        box_.append(&z.widget);
        this.zuege.borrow_mut().push(z);
        box_.upcast()
    }

    /// Makro-Raster: 80 Tasten, sechs Seiten, Klick geht an `macros_io`.
    fn makros_bauen(this: &Rc<Shell>, eingeklappt: bool) -> gtk::Widget {
        let rahmen = gtk::Box::new(gtk::Orientation::Vertical, 3);
        rahmen.add_css_class("makrobereich");

        let kopf = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let titel = gtk::Label::new(Some("MACRO"));
        titel.add_css_class("makro-titel");
        let info = gtk::Label::new(Some(&format!(
            "{} FELDER ·",
            crate::layout::makros_anzahl_pruefen(
                this.konfig.borrow().layout.aktuell().makros_anzahl
            )
        )));
        info.add_css_class("makro-info");
        kopf.append(&titel);
        kopf.append(&info);

        let seitenwahl = gtk::DropDown::from_strings(&["1", "2", "3", "4", "5", "6"]);
        seitenwahl.set_selected(this.makro_seite.get() as u32);
        kopf.append(&seitenwahl);
        let seitenzeige = gtk::Label::new(Some(&format!("SEITE {}/6", this.makro_seite.get() + 1)));
        seitenzeige.add_css_class("makro-info");
        kopf.append(&seitenzeige);

        let platz = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        platz.set_hexpand(true);
        kopf.append(&platz);

        let pfeil = gtk::ToggleButton::with_label(if eingeklappt {
            "▸ AUFKLAPPEN"
        } else {
            "▾ EINKLAPPEN"
        });
        pfeil.add_css_class("tbtn");
        pfeil.set_active(eingeklappt);
        kopf.append(&pfeil);
        rahmen.append(&kopf);

        // Der Kern kennt 80 Felder; gezeigt werden die ersten `makros_anzahl`
        // (Layout-Einstellung, Vorgabe 5). Quadratisch mit 96 px Mindestkante,
        // 8 px Abstand, Umbruch je nach Anzahl: 5 nebeneinander, 8 und 10
        // zweizeilig.
        let anzahl = crate::layout::makros_anzahl_pruefen(
            this.konfig.borrow().layout.aktuell().makros_anzahl,
        );
        let spalten = makro_spalten(anzahl);
        let raster = gtk::Grid::new();
        raster.set_row_spacing(8);
        raster.set_column_spacing(8);
        raster.set_margin_top(8);
        raster.set_margin_bottom(8);
        raster.set_margin_start(6);
        raster.set_margin_end(6);
        raster.set_halign(gtk::Align::Start);
        for i in 0..anzahl {
            let b = gtk::Button::new();
            b.add_css_class("makro");
            b.set_size_request(96, 96);
            b.set_valign(gtk::Align::Center);
            b.set_halign(gtk::Align::Center);
            let innen = gtk::Box::new(gtk::Orientation::Vertical, 2);
            innen.set_valign(gtk::Align::Center);
            innen.set_halign(gtk::Align::Center);
            let name = gtk::Label::new(Some(&format!("MACRO {}", i + 1)));
            name.add_css_class("makro-name");
            name.set_justify(gtk::Justification::Center);
            let unter = gtk::Label::new(Some(&format!("FELD {:02}", i + 1)));
            unter.add_css_class("makro-unter");
            innen.append(&name);
            innen.append(&unter);
            b.set_child(Some(&innen));
            // 1:1 halten, auch wenn die Rasterzelle breiter wird.
            let kasten = gtk::AspectFrame::builder()
                .ratio(1.0)
                .obey_child(false)
                .xalign(0.5)
                .yalign(0.5)
                .build();
            kasten.set_child(Some(&b));
            {
                let this2 = Rc::clone(this);
                let b2 = b.clone();
                b.connect_clicked(move |_| {
                    let Some(ctl) = this2.makros.borrow().clone() else {
                        return;
                    };
                    let _ = ctl.press(i);
                    let b3 = b2.clone();
                    let ctl2 = Arc::clone(&ctl);
                    let this3 = Rc::clone(&this2);
                    gtk::glib::timeout_add_local_once(Duration::from_millis(140), move || {
                        let _ = ctl2.release(i);
                        if !ctl2.latched(i) {
                            b3.remove_css_class("an");
                        }
                        this3.hinweis(&format!("Makro {} gesendet", i + 1), false);
                    });
                    b2.add_css_class("an");
                });
            }
            raster.attach(&kasten, (i % spalten) as i32, (i / spalten) as i32, 1, 1);
            this.makro_knoepfe.borrow_mut().push(b);
        }
        raster.set_visible(!eingeklappt);
        rahmen.append(&raster);

        {
            let raster2 = raster.clone();
            let pfeil2 = pfeil.clone();
            let this2 = Rc::clone(this);
            pfeil.connect_toggled(move |b| {
                raster2.set_visible(!b.is_active());
                pfeil2.set_label(if b.is_active() {
                    "▸ AUFKLAPPEN"
                } else {
                    "▾ EINKLAPPEN"
                });
                let mut k = this2.konfig.borrow_mut();
                let aktiv = k.layout.aktiv.clone();
                if let Some(l) = k.layout.layouts.get_mut(&aktiv) {
                    l.makros_eingeklappt = b.is_active();
                }
                k.speichern();
            });
        }
        {
            let this2 = Rc::clone(this);
            let seitenzeige = seitenzeige.clone();
            seitenwahl.connect_selected_notify(move |d| {
                let neu = d.selected() as usize;
                Shell::makro_seite_setzen(&this2, neu);
                seitenzeige.set_text(&format!("SEITE {}/6", neu + 1));
            });
        }

        rahmen.upcast()
    }

    /// Seitenwechsel: aktuelle Bank merken, Zielbank laden.
    fn makro_seite_setzen(this: &Rc<Shell>, seite: usize) {
        let Some(ctl) = this.makros.borrow().clone() else {
            return;
        };
        let alt = this.makro_seite.get();
        if alt == seite {
            return;
        }
        this.makro_seiten.borrow_mut()[alt] = Some(ctl.belegung());
        let ziel = this.makro_seiten.borrow()[seite].clone();
        if let Some(bank) = ziel {
            ctl.set_bank(bank);
        }
        this.makro_seite.set(seite);
    }

    /// Theme wechseln: Provider tauschen, Tokens/Masse setzen, neu zeichnen.
    pub fn thema_setzen(this: &Rc<Shell>, name: &str, speichern: bool) {
        {
            let mut k = this.konfig.borrow_mut();
            let log = k.theme.aktiv_setzen(name);
            if !log.is_empty() {
                k.log.extend(log);
            }
        }
        let tokens = this.konfig.borrow().theme.tokens();
        if let Ok(mut t) = this.tokens.write() {
            *t = tokens.clone();
        }
        let dichte = this.konfig.borrow().layout.aktuell().dichte;
        let masse = Masse::aus(&tokens, dichte);
        if let Ok(mut m) = this.masse.write() {
            *m = masse;
        }
        let alt = this.provider.borrow_mut().take();
        let neu = theme::anwenden(&tokens, alt);
        *this.provider.borrow_mut() = neu;

        if let Some(d) = this.themenwahl.borrow().as_ref() {
            let idx = this
                .konfig
                .borrow()
                .theme
                .namen()
                .iter()
                .position(|n| n == name)
                .unwrap_or(0);
            if d.selected() != idx as u32 {
                d.set_selected(idx as u32);
            }
        }
        for z in this.zuege.borrow().iter() {
            z.view.queue_draw();
        }
        if speichern {
            this.konfig.borrow_mut().speichern();
        }
    }

    pub fn layout_setzen(this: &Rc<Shell>, name: &str, speichern: bool) {
        {
            let mut k = this.konfig.borrow_mut();
            let log = k.layout.aktiv_setzen(name);
            if !log.is_empty() {
                k.log.extend(log);
            }
        }
        let dichte = this.konfig.borrow().layout.aktuell().dichte;
        let tokens = this.tokens_jetzt();
        if let Ok(mut m) = this.masse.write() {
            *m = Masse::aus(&tokens, dichte);
        }
        Shell::bauen(this);
        if speichern {
            this.konfig.borrow_mut().speichern();
        }
    }

    /// Anzahl sichtbarer Makro-Felder umstellen.
    ///
    /// Wirkt sofort: der Wert gehoert zum aktiven Layout in `ui.json`, das
    /// Raster wird neu gebaut. Der Kern behaelt seine 80 Felder – gezeigt
    /// werden die ersten `anzahl`.
    pub fn makro_anzahl_setzen(this: &Rc<Shell>, anzahl: usize) {
        let anzahl = crate::layout::makros_anzahl_pruefen(anzahl);
        {
            let mut k = this.konfig.borrow_mut();
            let aktiv = k.layout.aktiv.clone();
            if let Some(l) = k.layout.layouts.get_mut(&aktiv) {
                l.makros_anzahl = anzahl;
            }
            k.speichern();
        }
        Shell::bauen(this);
        this.hinweis(&format!("{anzahl} Makro-Felder"), false);
    }

    pub fn dichte_setzen(this: &Rc<Shell>, dichte: Dichte) {
        {
            let mut k = this.konfig.borrow_mut();
            let aktiv = k.layout.aktiv.clone();
            if let Some(l) = k.layout.layouts.get_mut(&aktiv) {
                l.dichte = dichte;
            }
        }
        let tokens = this.tokens_jetzt();
        if let Ok(mut m) = this.masse.write() {
            *m = Masse::aus(&tokens, dichte);
        }
        Shell::bauen(this);
        this.konfig.borrow_mut().speichern();
    }

    pub fn bereich_setzen(this: &Rc<Shell>, bereich: Bereich, an: bool) {
        {
            let mut k = this.konfig.borrow_mut();
            let aktiv = k.layout.aktiv.clone();
            if let Some(l) = k.layout.layouts.get_mut(&aktiv) {
                l.sichtbar.set(bereich, an);
            }
        }
        Shell::bauen(this);
        this.konfig.borrow_mut().speichern();
    }

    pub fn busseite_setzen(this: &Rc<Shell>, seite: Seite) {
        {
            let mut k = this.konfig.borrow_mut();
            let aktiv = k.layout.aktiv.clone();
            if let Some(l) = k.layout.layouts.get_mut(&aktiv) {
                l.busse_seite = seite;
            }
        }
        Shell::bauen(this);
        this.konfig.borrow_mut().speichern();
    }

    pub fn masterplatz_setzen(this: &Rc<Shell>, platz: MasterPlatz) {
        {
            let mut k = this.konfig.borrow_mut();
            let aktiv = k.layout.aktiv.clone();
            if let Some(l) = k.layout.layouts.get_mut(&aktiv) {
                l.master = platz;
            }
        }
        Shell::bauen(this);
        this.konfig.borrow_mut().speichern();
    }

    pub fn streifen_verschieben(this: &Rc<Shell>, index: usize, richtung: i32) {
        {
            let mut k = this.konfig.borrow_mut();
            let aktiv = k.layout.aktiv.clone();
            if let Some(l) = k.layout.layouts.get_mut(&aktiv) {
                if index < l.streifen_reihenfolge.len() {
                    let ziel = (index as i32 + richtung)
                        .clamp(0, l.streifen_reihenfolge.len() as i32 - 1)
                        as usize;
                    l.streifen_reihenfolge.swap(index, ziel);
                }
            }
        }
        Shell::bauen(this);
        this.konfig.borrow_mut().speichern();
    }

    fn alle_sync(this: &Rc<Shell>) {
        for z in this.zuege.borrow().iter() {
            (z.sync)();
        }
    }

    /// Geraetelisten in die Aufklappmenues schreiben (Modelle sind geteilt).
    fn geraete_anwenden(this: &Rc<Shell>) {
        let bestand = {
            let slot = this.geraete_empfang.borrow();
            let Some(rx) = slot.as_ref() else {
                return;
            };
            match rx.try_recv() {
                Ok(b) => b,
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
            }
        };
        this.geraete.sperre.set(true);

        // Ein gemerktes, aber abgestecktes Gerät bleibt in der Liste – mit
        // Zusatz „(nicht verbunden)“, damit das Menü nicht stillschweigend auf
        // „kein Eingang“ zurückfällt.
        let mut q_namen: Vec<Option<String>> = vec![None];
        let mut q_texte: Vec<String> = vec![geraete::KEIN_EINGANG.to_string()];
        for e in &bestand.quellen {
            q_namen.push(Some(e.name.clone()));
            q_texte.push(geraete::eintrag_text(&e.anzeige, e.fehlt));
        }
        let mut s_namen: Vec<Option<String>> = vec![None];
        let mut s_texte: Vec<String> = vec![geraete::KEIN_AUSGANG.to_string()];
        for e in &bestand.senken {
            s_namen.push(Some(e.name.clone()));
            s_texte.push(geraete::eintrag_text(&e.anzeige, e.fehlt));
        }

        let q_refs: Vec<&str> = q_texte.iter().map(|s| s.as_str()).collect();
        let s_refs: Vec<&str> = s_texte.iter().map(|s| s.as_str()).collect();
        this.geraete
            .quellen
            .splice(0, this.geraete.quellen.n_items(), &q_refs);
        this.geraete
            .senken
            .splice(0, this.geraete.senken.n_items(), &s_refs);
        *this.geraete.quellen_namen.borrow_mut() = q_namen.clone();
        *this.geraete.senken_namen.borrow_mut() = s_namen.clone();

        for z in this.zuege.borrow().iter() {
            let rolle = z.view.rolle();
            if let Some(dd) = z.geraetewahl.as_ref() {
                let (idx, fehlt, bus) = match rolle {
                    Rolle::Kanal(i) if i < 5 => {
                        let fehlt = bestand.streifen_fehlt.get(i).copied().unwrap_or(false);
                        let idx = bestand
                            .streifen
                            .get(i)
                            .and_then(|q| q.as_ref())
                            .and_then(|name| q_namen.iter().position(|n| n.as_ref() == Some(name)))
                            .unwrap_or(0);
                        (idx, fehlt, None)
                    }
                    Rolle::Bus(i) if i < 5 => {
                        let fehlt = bestand.busse_fehlt.get(i).copied().unwrap_or(false);
                        let bus = crosspoint::BUS_NAMES[i];
                        let idx = bestand
                            .busse
                            .iter()
                            .find(|(b, _)| b == bus)
                            .and_then(|(_, s)| s.as_ref())
                            .and_then(|name| s_namen.iter().position(|n| n.as_ref() == Some(name)))
                            .unwrap_or(0);
                        (idx, fehlt, Some((i, bus)))
                    }
                    _ => (0, false, None),
                };
                dd.set_selected(idx as u32);
                if fehlt {
                    dd.add_css_class("fehlt");
                } else {
                    dd.remove_css_class("fehlt");
                }

                // Statuszeile: Streifen zeigen „EINGANG FEHLT“, Busse ihre
                // Gerätelautstärke („A1 · 45 %“, „A1 · STUMM“, „A1 · FEHLT“).
                if let Some(zeile) = z.geraetezeile.as_ref() {
                    let (text, tipp) = match bus {
                        Some((i, name)) => {
                            let ls = bestand.busse_lautstaerke.get(i).copied().flatten();
                            (
                                geraete::bus_lautstaerke(name, ls, fehlt),
                                if fehlt {
                                    "Gerät ist nicht angeschlossen (abgesteckt?)"
                                } else {
                                    "Gerätelautstärke des Ziels – Klick setzt 100 % und hebt Stumm auf"
                                },
                            )
                        }
                        None => (
                            if fehlt {
                                "EINGANG FEHLT".to_string()
                            } else {
                                String::new()
                            },
                            geraete::FEHLT_TIPP,
                        ),
                    };
                    if zeile.text() != text {
                        zeile.set_text(&text);
                    }
                    if fehlt {
                        zeile.add_css_class("fehlt");
                    } else {
                        zeile.remove_css_class("fehlt");
                    }
                    zeile.set_tooltip_text(Some(tipp).filter(|_| fehlt || bus.is_some()));
                }
            }
        }

        if let Some(f) = bestand.fehler.clone() {
            this.hinweis_neu(&format!("Geraete: {f}"), true);
        } else if !bestand.fehlend.is_empty() {
            this.hinweis_neu(
                &format!("Nicht angeschlossen: {}", bestand.fehlend.join(" · ")),
                true,
            );
        }
        this.geraete.sperre.set(false);
    }

    /// 60-Hz-Takt: Pegel, Tape, Makros, Geraete, Knoepfe.
    fn takt_starten(this: &Rc<Shell>) {
        let this2 = Rc::clone(this);
        gtk::glib::timeout_add_local(Duration::from_millis(16), move || {
            let jetzt = this2.start.elapsed().as_secs_f32();
            this2.takt.set_zeit(jetzt);

            let frames = this2.pegel.bloecke();
            if frames != this2.frames.get() {
                this2.frames.set(frames);
                this2.frames_zeit.set(Instant::now());
                this2.takt.audio_aktiv.store(true, Ordering::Relaxed);
            } else if this2.frames_zeit.get().elapsed().as_millis() > 400 {
                this2.takt.audio_aktiv.store(false, Ordering::Relaxed);
            }

            for z in this2.zuege.borrow().iter() {
                z.view.ticken();
                (z.sync)();
            }

            if this2.geraete_takt.get().elapsed().as_millis() > 400 {
                this2.geraete_takt.set(Instant::now());
                Shell::geraete_anwenden(&this2);
            }

            // Tape: Laufzeit in m:ss.mmm, Status und letzte Datei
            if this2.tape.laeuft.get() {
                let dauer = this2.tape.start.get().elapsed().as_secs_f64();
                if let Some(t) = this2.tape.zeit.borrow().as_ref() {
                    t.set_text(&format!("{}:{:06.3}", (dauer as u64) / 60, dauer % 60.0));
                }
            }
            if let Some(ctl) = this2.tape.ctl.borrow().as_ref() {
                let st = ctl.status();
                if let Some(l) = this2.tape.status.borrow().as_ref() {
                    if l.text() != st {
                        l.set_text(&st);
                    }
                    // Vollstaendiger Zustand (bei Aufnahme inkl. Datei) im
                    // Tooltip: in der Zeile steht er gekuerzt, damit REC/STOP,
                    // Zeit, Pegel und Ziel ihren Platz behalten.
                    if l.tooltip_text().as_deref() != Some(st.as_str()) {
                        l.set_tooltip_text(Some(&st));
                    }
                }
            }

            let db = this2
                .zuege
                .borrow()
                .iter()
                .find(|z| z.view.rolle() == Rolle::Master)
                .map(|z| z.view.db())
                .unwrap_or(-60.0);
            {
                let segs = this2.tape.segs.borrow();
                if !segs.is_empty() {
                    let anzahl = (((db + 60.0) / 60.0) * segs.len() as f32)
                        .clamp(0.0, segs.len() as f32) as u32;
                    if anzahl != this2.tape.letzte_segmente.get() {
                        this2.tape.letzte_segmente.set(anzahl);
                        for (i, s) in segs.iter().enumerate() {
                            if (i as u32) < anzahl {
                                s.add_css_class("an");
                            } else {
                                s.remove_css_class("an");
                            }
                        }
                    }
                }
            }

            // Makros: gehaltene (latched) Tasten sichtbar machen
            if let Some(ctl) = this2.makros.borrow().clone() {
                for (i, b) in this2.makro_knoepfe.borrow().iter().enumerate() {
                    let an = ctl.latched(i);
                    if an && !b.has_css_class("an") {
                        b.add_css_class("an");
                    } else if !an && b.has_css_class("an") {
                        b.remove_css_class("an");
                    }
                }
            }

            if let Some(st) = this2.status.borrow().as_ref() {
                let an = this2.takt.aktiv();
                let text = if an { "● LAEUFT" } else { "● BEREIT" };
                if st.text() != text {
                    st.set_text(text);
                    if an {
                        st.remove_css_class("aus");
                    } else {
                        st.add_css_class("aus");
                    }
                }
            }

            gtk::glib::ControlFlow::Continue
        });
    }
}

/// Breitendiagnose: misst ein Widget und seine Kinder (nur Fehlersuche).
fn dump(w: &gtk::Widget, tiefe: usize) {
    let (mi, na, _, _) = w.measure(gtk::Orientation::Horizontal, -1);
    let text = if let Some(dd) = w.downcast_ref::<gtk::DropDown>() {
        format!(
            "<Aufklappmenue, {} Eintraege>",
            dd.model().map(|m| m.n_items()).unwrap_or(0)
        )
    } else if let Some(l) = w.downcast_ref::<gtk::Label>() {
        format!("<{}>", l.text())
    } else {
        String::new()
    };
    eprintln!("    {:>20} min {mi:5} nat {na:5} {text}", w.type_().name());
    if tiefe == 0 {
        return;
    }
    let mut k = w.first_child();
    while let Some(c) = k {
        dump(&c, tiefe - 1);
        k = c.next_sibling();
    }
}

/// Spaltenzahl des Makro-Rasters.
///
/// 5 Felder stehen nebeneinander; 8 und 10 werden zweizeilig (5 je Zeile).
/// Groessere Zahlen bleiben in einem Raster, das in 1600 px passt.
fn makro_spalten(anzahl: usize) -> usize {
    match anzahl {
        16 | 40 => 8,
        80 => 10,
        _ => 5,
    }
}

/// Baut ein Fenster mit einem Hinweistext (fuer Fehler beim Start).
#[allow(dead_code)]
pub fn fehler_fenster(app: &gtk::Application, text: &str) {
    let w = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Crosspoint")
        .default_width(420)
        .build();
    w.add_css_class("grund");
    let l = gtk::Label::new(Some(text));
    l.set_margin_top(18);
    l.set_margin_bottom(18);
    l.set_margin_start(18);
    l.set_margin_end(18);
    w.set_child(Some(&l));
    w.present();
}
