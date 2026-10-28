//! Linkt gegen die JACK-ABI, die PipeWire bereitstellt (pipewire-jack).
//!
//! Auf dem Host (Bazzite) gibt es weder JACK-Header noch ein
//! `libjack.so`-Symlink, deshalb wird direkt gegen die vorhandene `.so.0`
//! gelinkt. Der Ablageort ist nicht ueberall gleich: Fedora/Bazzite legt sie
//! unter `/usr/lib64/pipewire-0.3/jack/` ab, der Freedesktop-Runtime (Flatpak)
//! unter `/usr/lib/x86_64-linux-gnu/`. Deshalb wird nur der Pfad gesetzt, der
//! wirklich existiert – sonst sucht der Linker in seinem Standardpfad.
fn main() {
    let kandidaten = [
        "/usr/lib64/pipewire-0.3/jack",
        "/usr/lib/x86_64-linux-gnu/pipewire-0.3/jack",
    ];
    if let Some(dir) = kandidaten.iter().find(|d| std::path::Path::new(d).is_dir()) {
        println!("cargo:rustc-link-arg=-L{dir}");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
    println!("cargo:rustc-link-arg=-l:libjack.so.0");
}
