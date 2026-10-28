// Linkt gegen die JACK-ABI, die PipeWire als pipewire-jack bereitstellt.
// Bazzite hat keine JACK-Header und keinen libjack.so-Symlink, deshalb direkt
// gegen die vorhandene .so.0.
fn main() {
    println!("cargo:rustc-link-arg=-L/usr/lib64/pipewire-0.3/jack");
    println!("cargo:rustc-link-arg=-l:libjack.so.0");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib64/pipewire-0.3/jack");
}
