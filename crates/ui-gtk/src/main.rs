//! Crosspoint GTK4 — Engine und Oberflaeche in einem Prozess.
//!
//! Der frueher eigenstaendige Oberflaechen-Prozess (ohne Ton, mit Attrappen fuer
//! Pegel, Tape und Makros) ist jetzt derselbe Ablauf wie das Binary
//! `crosspoint`: `crosspoint_ui::betrieb` startet die Engine, baut das Fenster
//! auf ihren Handles und schaltet beides zusammen ab. Der Name `crosspoint-gtk`
//! bleibt fuer Skripte und Screenshots erhalten.

fn main() {
    let args = crosspoint_ui::betrieb::args_lesen();
    crosspoint_ui::betrieb::starten("de.weppelmann.Crosspoint.Ui", args);
}
