//! Ein Prozess: Engine und Fenster zusammen.
//!
//! Der Ablauf ist bewusst kurz und in dieser Reihenfolge:
//!
//! 1. Engine starten (`crosspoint::lauf::starte`) – sie legt ihre Threads an
//!    (Audio-Callback, HTTP, OSC, Tape, VBAN, Makros, Autosave).
//! 2. GTK im **Hauptthread** starten; das Fenster bekommt die Handles der
//!    Engine ([`crate::Anbindung`]) und schreibt/liest direkt darin.
//! 3. Beenden in beide Richtungen:
//!    * Fenster zu → `Lauf::abschalten`: Aufnahme schliessen, OSC anhalten,
//!      virtuelle Geraete abraeumen, JACK-Client deaktivieren.
//!    * Engine zu (Signal, Leerlauf) → `app.quit()`: das Fenster schliesst.
//!
//! Scheitert der Audio-Client, erscheint statt des Pultes ein Fenster mit der
//! Fehlermeldung – die Oberflaeche zeigt nie ein Pult, das nicht rechnet.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use crosspoint::lauf::Konfig as LaufKonfig;
use gtk::prelude::*;

use crate::{config, dialogs, shell, theme, Anbindung};

/// Kommandozeile (bewusst ohne zusaetzliche Crate).
#[derive(Clone, Debug)]
pub struct Args {
    pub theme: Option<String>,
    pub layout: Option<String>,
    /// Dialog beim Start oeffnen (fuer Belege und zum Ausprobieren).
    pub dialog: Option<String>,
    pub breite: i32,
    pub hoehe: i32,
    /// JACK-Clientname, an dem die Portnamen haengen.
    pub client: String,
    /// Adresse des HTTP-Adapters (bleibt Rand, nicht Bedienweg).
    pub listen: String,
}

impl Default for Args {
    fn default() -> Args {
        Args {
            theme: None,
            layout: None,
            dialog: None,
            breite: 1600,
            hoehe: 920,
            client: "crosspoint".to_string(),
            listen: "127.0.0.1:8770".to_string(),
        }
    }
}

pub const HILFE: &str = "crosspoint [--theme NAME] [--layout NAME] \
[--dialog vban|midi|darstellung] [--size 1600x920] [--client NAME] [--listen ADDR]";

pub fn args_lesen() -> Args {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--theme" => a.theme = it.next(),
            "--layout" => a.layout = it.next(),
            "--dialog" => a.dialog = it.next(),
            "--client" => {
                if let Some(v) = it.next() {
                    a.client = v;
                }
            }
            "--listen" => {
                if let Some(v) = it.next() {
                    a.listen = v;
                }
            }
            "--size" => {
                if let Some(v) = it.next() {
                    if let Some((w, h)) = v.split_once('x') {
                        a.breite = w.parse().unwrap_or(a.breite);
                        a.hoehe = h.parse().unwrap_or(a.hoehe);
                    }
                }
            }
            "--help" | "-h" => {
                println!("{HILFE}");
                std::process::exit(0);
            }
            _ => {}
        }
    }
    a
}

/// Startet Engine und Fenster im selben Prozess. Kehrt zurueck, wenn das
/// Fenster zu ist und die Engine abgeschaltet wurde.
pub fn starten(app_id: &str, args: Args) {
    let app = gtk::Application::builder().application_id(app_id).build();
    let args = Rc::new(RefCell::new(args));
    // Ein `activate` kann mehrfach kommen (zweiter Start derselben App);
    // die Engine darf nur einmal hochfahren.
    let gestartet = Rc::new(Cell::new(false));

    {
        let args = Rc::clone(&args);
        let gestartet = Rc::clone(&gestartet);
        app.connect_activate(move |app| {
            if gestartet.replace(true) {
                return;
            }
            let a = args.borrow().clone();

            // 1. Engine: eigener Satz Threads, kein Fenster noetig.
            let mut lk = LaufKonfig::vorgabe().dauerbetrieb();
            lk.client_name = a.client.clone();
            lk.listen = a.listen.clone();
            let lauf = match crosspoint::lauf::starte(lk) {
                Ok(l) => Arc::new(l),
                Err(e) => {
                    eprintln!("crosspoint: {e}");
                    shell::fehler_fenster(app, &e);
                    let app = app.clone();
                    gtk::glib::timeout_add_local_once(Duration::from_millis(200), move || {
                        app.quit();
                    });
                    return;
                }
            };

            // 2. Konfiguration der Oberflaeche (Theme/Layout aus ui.json).
            let mut konfig = config::Konfiguration::laden(theme::konfig_pfad());
            if let Some(name) = a.theme.as_deref() {
                let log = konfig.theme.aktiv_setzen(name);
                konfig.log.extend(log);
            }
            if let Some(name) = a.layout.as_deref() {
                let log = konfig.layout.aktiv_setzen(name);
                konfig.log.extend(log);
            }
            for z in &konfig.log {
                eprintln!("crosspoint: {z}");
            }

            // 3. Fenster auf denselben Handles.
            let s = shell::Shell::neu(app, konfig, Anbindung::aus_lauf(&lauf));
            s.fenster.set_default_size(a.breite, a.hoehe);
            // Mindestgroesse: acht Streifen plus Busspalte duerfen nicht
            // abgeschnitten werden (sonst scrollt die Oberflaeche).
            s.fenster
                .set_size_request(a.breite.min(1400), a.hoehe.min(820));
            s.fenster.present();

            // Fenster zu -> Engine und Geraete weg.
            {
                let lauf = Arc::clone(&lauf);
                s.fenster.connect_close_request(move |_| {
                    lauf.abschalten();
                    gtk::glib::Propagation::Proceed
                });
            }
            // Sicherheitsnetz: endet die Anwendung anders (z. B. per Signal),
            // wird trotzdem abgeschaltet.
            {
                let lauf = Arc::clone(&lauf);
                app.connect_shutdown(move |_| {
                    lauf.abschalten();
                });
            }
            // Engine zu (Signal, Leerlauf) -> Fenster zu.
            {
                let lauf = Arc::clone(&lauf);
                let app = app.clone();
                gtk::glib::timeout_add_local(Duration::from_millis(250), move || {
                    if lauf.beendet() {
                        app.quit();
                        gtk::glib::ControlFlow::Break
                    } else {
                        gtk::glib::ControlFlow::Continue
                    }
                });
            }

            if let Some(was) = a.dialog.clone() {
                let s2 = Rc::clone(&s);
                gtk::glib::timeout_add_local_once(Duration::from_millis(600), move || {
                    match was.as_str() {
                        "vban" => {
                            dialogs::vban_fenster(&s2.fenster, &dialogs::BeispielVban);
                        }
                        "midi" => {
                            dialogs::midi_fenster(&s2.fenster, &[]);
                        }
                        "darstellung" => {
                            dialogs::darstellung_fenster(&s2);
                        }
                        _ => eprintln!("crosspoint: unbekannter Dialog '{was}'"),
                    }
                });
            }
        });
    }

    // GTK soll die eigenen Argumente nicht pruefen: sie sind oben gelesen.
    let _ = app.run_with_args::<&str>(&[]);
}
