//! Crosspoint GTK4-Oberflaeche als Bibliothek.
//!
//! Die Oberflaeche ist kein eigenes Programm mehr mit eigenen Attrappen,
//! sondern ein Widget-Baum auf einem **laufenden Kern**. Was sie vom Kern
//! braucht, steht in [`Anbindung`]: dieselben `Arc<Params>`, dieselbe
//! Pegelbank, dieselbe Tape- und Makro-Steuerung. Angelegt wird davon nichts
//! hier – das macht `crosspoint::lauf::starte`.
//!
//! ```ignore
//! let lauf = crosspoint::lauf::starte(crosspoint::lauf::Konfig::vorgabe())?;
//! let app = gtk::Application::builder().application_id("de.weppelmann.Crosspoint").build();
//! app.connect_activate(move |app| {
//!     let konfig = crosspoint_ui::Konfiguration::laden(crosspoint_ui::konfig_pfad());
//!     let shell = crosspoint_ui::start(app, konfig, crosspoint_ui::Anbindung::aus_lauf(&lauf));
//!     shell.fenster.present();
//! });
//! app.run();
//! ```
//!
//! Der fertige Weg steht in [`betrieb`]: ein Prozess, GTK im Hauptthread,
//! Engine in ihren eigenen Threads, Abschalten in beide Richtungen.

pub mod betrieb;
pub mod config;
pub mod dialogs;
pub mod draw;
pub mod fmt;
pub mod geraete;
pub mod layout;
pub mod midimonster;
pub mod shell;
pub mod state;
pub mod strip;
pub mod theme;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crosspoint::engine::Params;
use crosspoint::macros_io::MacroControl;
use crosspoint::pegel::Pegelbank;
use crosspoint::tape_io::TapeControl;

pub use config::Konfiguration;
pub use theme::konfig_pfad;

/// Was die Oberflaeche vom laufenden Kern bekommt.
///
/// Alles darin sind `Arc`s auf die Instanzen, die die Engine benutzt: ein
/// Faderzug schreibt in dieselben Atome, die der Audio-Thread liest; die
/// Pegelanzeige liest die Pegelbank, die der Callback fuellt; der Tape-Knopf
/// bedient den Ring, den der Callback beschickt; ein Makro-Klick landet in der
/// Bank, die der Engine-Thread abarbeitet.
pub struct Anbindung {
    pub params: Arc<Params>,
    pub pegel: Arc<Pegelbank>,
    pub tape: Arc<TapeControl>,
    pub makros: Arc<MacroControl>,
    /// Ordner, in den der Tape-Schreiber schreibt (fuer die Anzeige).
    pub tape_ordner: PathBuf,
    pub sample_rate: u32,
}

impl Anbindung {
    /// Die Handles eines laufenden [`crosspoint::lauf::Lauf`].
    pub fn aus_lauf(lauf: &crosspoint::lauf::Lauf) -> Anbindung {
        Anbindung {
            params: lauf.params.clone(),
            pegel: lauf.pegel.clone(),
            tape: lauf.tape.clone(),
            makros: lauf.makros.clone(),
            tape_ordner: lauf.tape_ordner.clone(),
            sample_rate: lauf.sample_rate,
        }
    }
}

/// Baut das Fenster (inkl. CSS, Geraeteliste, Takt). Der Aufrufer zeigt es.
pub fn start(app: &gtk::Application, konfig: Konfiguration, an: Anbindung) -> Rc<shell::Shell> {
    shell::Shell::neu(app, konfig, an)
}
