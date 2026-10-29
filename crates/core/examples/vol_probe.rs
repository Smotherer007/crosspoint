//! Wegwerf-Probe: lesen und setzen der Gerätelautstärke.
use crosspoint::pw::{PipeWire, Zugriff};

fn lies() -> String {
    let g = <PipeWire as Zugriff>::graph(&PipeWire).expect("graph");
    let name = "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo";
    format!("{:?} / {}", g.lautstaerke(name), g.lautstaerke_text(name))
}

fn main() {
    let _ = <PipeWire as Zugriff>::graph(&PipeWire);
    std::thread::sleep(std::time::Duration::from_millis(400));
    let name = "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo";
    println!("vorher:    {}", lies());

    let r = Zugriff::lautstaerke_setzen(&PipeWire, name, Some(1.0), Some(false));
    println!("setzen 100%/an: {r:?}");
    std::thread::sleep(std::time::Duration::from_millis(400));
    println!("danach:    {}", lies());

    let r = Zugriff::lautstaerke_setzen(&PipeWire, name, None, Some(true));
    println!("stumm:     {r:?}");
    std::thread::sleep(std::time::Duration::from_millis(400));
    println!("stumm da:  {}", lies());

    // Zurück auf den Ausgangswert (45 %), damit am Gerät nichts verstellt bleibt.
    let r = Zugriff::lautstaerke_setzen(&PipeWire, name, Some(0.091_125), Some(false));
    println!("zurueck:   {r:?}");
    std::thread::sleep(std::time::Duration::from_millis(400));
    println!("wieder:    {}", lies());
}
