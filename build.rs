//! Kein eigenes Linken mehr.
//!
//! Früher hing die Engine direkt an `libjack.so.0` (`-l:libjack.so.0` plus
//! `-L`/`-rpath` auf das PipeWire-Verzeichnis), weil das ABI von Hand
//! deklariert war. Mit der Crate `jack` wird die Bibliothek zur Laufzeit
//! geladen (`dynamic_loading`); ein Linker-Argument würde nur eine harte
//! Abhängigkeit auf den Build-Rechnern erzeugen (im Flatpak-SDK gibt es weder
//! `libjack.so` noch `jack.pc`).
//!
//! Was bleibt: `jack-sys` fragt pkg-config nach `jack`; der Ersatz für
//! `jack.pc` liegt in `packaging/pkgconfig/` und wird über `.cargo/config.toml`
//! gefunden. Zur Laufzeit sucht `jack-sys` die Bibliothek über ihren Sonamen
//! `libjack.so.0` – auf dem Host liefert sie PipeWire, in der
//! GNOME-Runtime liegt sie unter `/usr/lib/x86_64-linux-gnu/`.
fn main() {}
