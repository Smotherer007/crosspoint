//! `crosspoint` — Engine und Oberflaeche in **einem** Prozess.
//!
//! Gebaut nur mit Feature `ui` (`cargo build --release --features ui`), weil
//! das Fenster GTK4 braucht. Der Inhalt steht in
//! `crosspoint_ui::betrieb`; hier bleibt der Einstieg.
//!
//! GTK laeuft im Hauptthread, die Engine in ihren eigenen Threads
//! (`crosspoint::lauf`). Fenster zu → Engine und virtuelle Geraete weg;
//! Engine zu → Fenster zu.

fn main() {
    let args = crosspoint_ui::betrieb::args_lesen();
    crosspoint_ui::betrieb::starten("de.weppelmann.Crosspoint", args);
}
