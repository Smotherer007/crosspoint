//! crosspoint-engine – Einstiegspunkt: JACK-Client, Parameter, Steuer-API.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crosspoint::engine::{EngineDsp, Io, Params};
use crosspoint::jack::{self, Jack, Port};
use crosspoint::macros_io::MacroControl;
use crosspoint::vban_io::{VbanControl, VbanRecvControl};
use crosspoint::{BUSES, BUS_NAMES, STRIPS};

struct Rt {
    params: Arc<Params>,
    dsp: EngineDsp,
    ins: [[*mut Port; 2]; STRIPS],
    outs: [[*mut Port; 2]; BUSES],
    /// Ausgangspaare für empfangenes VBAN-Audio (`vban_L`/`vban_R`).
    vban_out: [*mut Port; 2],
    tape: Arc<crosspoint::tape_io::TapeControl>,
    vban: Arc<VbanControl>,
    vban_ring: Arc<crosspoint::tape_io::Ring>,
    vban_recv_ring: Arc<crosspoint::tape_io::Ring>,
    /// Blockpuffer für den VBAN-Eingang (fest, keine Allokation im Callback).
    vban_buf: Vec<f32>,
    vban_pos: usize,
    vban_len: usize,
    scratch: Vec<f32>,
}

impl Rt {
    /// Legt empfangenes VBAN-Audio auf `vban_L`/`vban_R`.
    ///
    /// Arbeitet blockweise: ein Ringblock (1024 Frames) wird gepuffert und über
    /// so viele Callbacks verteilt, wie nötig – so bleiben Blockgröße des Rings
    /// und des JACK-Callbacks unabhängig. Ist der Ring leer, wird der Rest des
    /// Puffers still geschrieben. Kein Lock, keine Allokation.
    fn vban_eingang(&mut self, frames: usize) {
        let (l, r) = unsafe {
            (
                jack::buffer(self.vban_out[0], frames as u32).as_mut_ptr(),
                jack::buffer(self.vban_out[1], frames as u32).as_mut_ptr(),
            )
        };
        let mut i = 0;
        while i < frames {
            if self.vban_pos >= self.vban_len {
                match self.vban_recv_ring.pop(&mut self.vban_buf) {
                    Some(n) => {
                        self.vban_pos = 0;
                        self.vban_len = (n * 2).min(self.vban_buf.len());
                    }
                    None => {
                        // Ring leer: Stille bis zum Blockende.
                        unsafe {
                            std::ptr::write_bytes(l.add(i), 0, frames - i);
                            std::ptr::write_bytes(r.add(i), 0, frames - i);
                        }
                        return;
                    }
                }
            }
            let verfuegbar = (self.vban_len - self.vban_pos) / 2;
            if verfuegbar == 0 {
                self.vban_pos = self.vban_len;
                continue;
            }
            let n = verfuegbar.min(frames - i);
            unsafe {
                for k in 0..n {
                    *l.add(i + k) = self.vban_buf[self.vban_pos + k * 2];
                    *r.add(i + k) = self.vban_buf[self.vban_pos + k * 2 + 1];
                }
            }
            self.vban_pos += n * 2;
            i += n;
        }
    }

    fn run(&mut self, frames: usize) {
        let mut io = Io {
            ins: [[std::ptr::null(); 2]; STRIPS],
            outs: [[std::ptr::null_mut(); 2]; BUSES],
        };
        unsafe {
            for s in 0..STRIPS {
                for c in 0..2 {
                    if !self.ins[s][c].is_null() {
                        io.ins[s][c] = jack::buffer(self.ins[s][c], frames as u32).as_ptr();
                    }
                }
            }
            for b in 0..BUSES {
                for c in 0..2 {
                    if !self.outs[b][c].is_null() {
                        io.outs[b][c] = jack::buffer(self.outs[b][c], frames as u32).as_mut_ptr();
                    }
                }
            }
        }
        self.dsp.process(&self.params, &io, frames);

        // Bus A1 (Stereo) in den Tape-Ring schieben - ohne Lock, ohne Allokation.
        if self
            .tape
            .recording
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let n = frames.min(self.scratch.len() / 2);
            if !io.outs[0][0].is_null() && !io.outs[0][1].is_null() {
                unsafe {
                    for i in 0..n {
                        self.scratch[i * 2] = *io.outs[0][0].add(i);
                        self.scratch[i * 2 + 1] = *io.outs[0][1].add(i);
                    }
                }
                self.tape.ring.push(&self.scratch[..n * 2]);
            }
        }

        // Derselbe Block geht zusätzlich in den VBAN-Ring, wenn VBAN aktiv ist.
        if self.vban.enabled() && !io.outs[0][0].is_null() {
            let n = frames.min(self.scratch.len() / 2);
            unsafe {
                for i in 0..n {
                    self.scratch[i * 2] = *io.outs[0][0].add(i);
                    self.scratch[i * 2 + 1] = *io.outs[0][1].add(i);
                }
            }
            self.vban_ring.push(&self.scratch[..n * 2]);
        }

        // Empfangenes VBAN-Audio auf die beiden Zusatzports legen.
        if !self.vban_out[0].is_null() && !self.vban_out[1].is_null() {
            self.vban_eingang(frames);
        }
    }
}

extern "C" fn process_cb(nframes: u32, arg: *mut c_void) -> i32 {
    let rt = unsafe { &mut *(arg as *mut Rt) };
    if nframes > 0 {
        rt.run(nframes as usize);
    }
    0
}

static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_signal(_sig: i32) {
    SHUTDOWN.store(true, std::sync::atomic::Ordering::SeqCst);
}

extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}

/// Legt die virtuellen Knoten an bzw. raeumt sie ab. Damit haengen die Geraete
/// am Leben der Engine: laeuft die Engine, gibt es sie; wird sie beendet,
/// verschwinden sie wieder. CROSSPOINT_NO_WIRE=1 schaltet das ab.
fn wire(richtung: &str) {
    if std::env::var("CROSSPOINT_NO_WIRE").is_ok() {
        return;
    }
    // Reihenfolge: ausdruecklicher Pfad, neben dem Binary (Flatpak: /app/bin),
    // dann der Repo-Pfad (target/release -> ../../scripts), zuletzt relativ.
    let mut kandidaten: Vec<std::path::PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("CROSSPOINT_WIRE").filter(|p| !p.is_empty()) {
        kandidaten.push(std::path::PathBuf::from(p));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        kandidaten.push(dir.join("crosspoint-wire.sh"));
        kandidaten.push(dir.join("../../scripts/crosspoint-wire.sh"));
    }
    kandidaten.push(std::path::PathBuf::from("scripts/crosspoint-wire.sh"));

    let script = match kandidaten.into_iter().find(|p| p.is_file()) {
        Some(p) => p,
        None => return,
    };
    let _ = std::process::Command::new("bash")
        .arg(&script)
        .arg(richtung)
        .status();
}

fn main() {
    // SIGINT/SIGTERM nur als Merker: die Aufraeumarbeit macht die Hauptschleife.
    unsafe {
        signal(2, on_signal as usize);
        signal(15, on_signal as usize);
    }
    // Einmalige Migration des Konfigurationsverzeichnisses, bevor irgendetwas
    // daraus gelesen wird (~/.config/vm8-linux -> ~/.config/crosspoint).
    if let Some(neu) = crosspoint::pfade::migriere() {
        println!("Konfiguration uebernommen: {}", neu.display());
    }
    let mut client_name = String::from("crosspoint");
    let mut listen = String::from("127.0.0.1:8770");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--client" => client_name = args.next().unwrap_or(client_name),
            "--listen" => listen = args.next().unwrap_or(listen),
            "-h" | "--help" => {
                println!("crosspoint-engine [--client NAME] [--listen ADDR] [--headless]");
                println!("  startet die Engine und die Web-UI auf 127.0.0.1:8770");
                println!(
                    "  CROSSPOINT_NO_RESTORE=1: Zustand nicht wiederherstellen, kein Autosave"
                );
                return;
            }
            other => {
                eprintln!("unbekanntes Argument: {other}");
                std::process::exit(2);
            }
        }
    }

    let params = Arc::new(Params::new(48_000.0, 512));
    let ring = Arc::new(crosspoint::tape_io::Ring::new(8, 2048));
    let tape = Arc::new(crosspoint::tape_io::TapeControl::new(ring.clone(), 2048));
    let vban = Arc::new(VbanControl::new());
    let vban_ring = Arc::new(crosspoint::tape_io::Ring::new(8, 2048));
    let vban_recv = Arc::new(VbanRecvControl::new());
    let vban_recv_ring = Arc::new(crosspoint::tape_io::Ring::new(
        16,
        crosspoint::vban_io::RECV_BLOCK_FRAMES,
    ));
    let macroctl = Arc::new(MacroControl::new());
    let rt: &'static mut Rt = Box::leak(Box::new(Rt {
        params: params.clone(),
        dsp: EngineDsp::new(2048),
        ins: [[std::ptr::null_mut(); 2]; STRIPS],
        outs: [[std::ptr::null_mut(); 2]; BUSES],
        vban_out: [std::ptr::null_mut(); 2],
        tape: tape.clone(),
        vban: vban.clone(),
        vban_ring: vban_ring.clone(),
        vban_recv_ring: vban_recv_ring.clone(),
        vban_buf: vec![0.0f32; crosspoint::vban_io::RECV_BLOCK_FRAMES * 2],
        vban_pos: 0,
        vban_len: 0,
        scratch: vec![0.0f32; 2048 * 2],
    }));

    // SAFETY: `rt` liegt in einer Arc, die bis zum Programmende gehalten wird
    // (siehe unten) - der Zeiger bleibt also gueltig, solange der Client lebt.
    let jack = match unsafe { Jack::open(&client_name, process_cb, rt as *mut Rt as *mut c_void) } {
        Ok(j) => j,
        Err(e) => {
            eprintln!("JACK/PipeWire: {e}");
            std::process::exit(1);
        }
    };
    params.sample_rate.store(
        (jack.sample_rate as f32).to_bits(),
        std::sync::atomic::Ordering::Relaxed,
    );
    params
        .block
        .store(jack.block, std::sync::atomic::Ordering::Relaxed);
    println!(
        "Client '{}' verbunden: {} Hz, Block {} Frames",
        jack.name, jack.sample_rate, jack.block
    );

    for s in 0..STRIPS {
        let l = jack.port_in(&format!("strip{}_L", s + 1)).expect("Port");
        let r = jack.port_in(&format!("strip{}_R", s + 1)).expect("Port");
        rt.ins[s] = [l, r];
    }
    for b in 0..BUSES {
        let l = jack
            .port_out(&format!("bus{}_L", BUS_NAMES[b]))
            .expect("Port");
        let r = jack
            .port_out(&format!("bus{}_R", BUS_NAMES[b]))
            .expect("Port");
        rt.outs[b] = [l, r];
    }
    // Zusatz-Ausgänge für empfangenes VBAN-Audio. Sie werden wie ein
    // Hardware-Eingang per pw-link auf einen Streifen gelegt, z. B.
    //   pw-link crosspoint:vban_L crosspoint:strip1_L
    //   pw-link crosspoint:vban_R crosspoint:strip1_R
    rt.vban_out = [
        jack.port_out("vban_L").expect("Port"),
        jack.port_out("vban_R").expect("Port"),
    ];
    if let Err(e) = jack.activate() {
        eprintln!("{e}");
        std::process::exit(1);
    }

    println!("\nPorts ({}:<name>):", jack.name);
    for s in 0..STRIPS {
        println!("  strip{}_L / strip{}_R", s + 1, s + 1);
    }
    for b in 0..BUSES {
        println!("  bus{}_L / bus{}_R", BUS_NAMES[b], BUS_NAMES[b]);
    }
    println!("  vban_L / vban_R (VBAN-Eingang, /api/vban/recv)");
    println!("\nVerdrahtung: scripts/crosspoint-wire.sh up\n");

    let preset = preset_path();
    let tape_dir = preset.with_file_name("tape");
    let _ = std::fs::create_dir_all(&tape_dir);
    crosspoint::tape_io::spawn_writer(tape.clone(), tape_dir, jack.sample_rate);

    crosspoint::vban_io::spawn_sender(vban.clone(), vban_ring.clone(), jack.sample_rate);
    crosspoint::vban_io::spawn_empfaenger(vban_recv.clone(), vban_recv_ring.clone());

    // Macro-Kommandos aus der UI auf die Parameter anwenden.
    let params_mac = params.clone();
    let macro_worker = macroctl.clone();
    thread::spawn(move || loop {
        for cmd in macro_worker.drain() {
            apply_macro_command(&params_mac, &cmd);
        }
        thread::sleep(std::time::Duration::from_millis(25));
    });

    let devices = Arc::new(std::sync::Mutex::new(
        crosspoint::devices::DeviceMap::load_default(),
    ));
    // Ergebnis der letzten Wiederherstellung; die API liest es, der Start
    // schreibt es hinein.
    let restore_ergebnis = Arc::new(std::sync::Mutex::new(
        crosspoint::restore::Ergebnis::default(),
    ));
    let preset_pfad = preset.clone();

    {
        let listen_owned = listen.clone();
        let params_ui = params.clone();
        let tape_ui = tape.clone();
        let vban_ui = vban.clone();
        let vban_recv_ui = vban_recv.clone();
        let macro_ui = macroctl.clone();
        let dev_ui = devices.clone();
        let restore_ui = restore_ergebnis.clone();
        thread::spawn(move || {
            if let Err(e) = crosspoint::http::serve(
                listen_owned,
                params_ui,
                preset,
                tape_ui,
                vban_ui,
                vban_recv_ui,
                macro_ui,
                dev_ui,
                restore_ui,
            ) {
                eprintln!("HTTP: {e}");
            }
        });
    }

    wire("up");

    // Jetzt stehen die Busports im Graphen: gemerkte Zuordnungen anwenden.
    if crosspoint::restore::deaktiviert() {
        println!("Wiederherstellen abgeschaltet (CROSSPOINT_NO_RESTORE)");
    } else {
        let ergebnis = crosspoint::restore::beim_start(&params, &devices, &preset_pfad);
        if let Ok(mut letzte) = restore_ergebnis.lock() {
            *letzte = ergebnis;
        }
    }
    // Ab hier haelt der Autosave das Preset aktuell (2 s nach der letzten
    // Aenderung, im eigenen Thread).
    crosspoint::restore::spawn_autosave(params.clone(), preset_pfad.clone());

    // Beenden, wenn die Oberflaeche verschwindet (Fenster zu) - dann raeumt die
    // Engine ihre virtuellen Geraete ab. CROSSPOINT_KEEP_RUNNING=1 schaltet das ab,
    // z. B. fuer Dauerbetrieb ohne Fenster.
    let leerlauf_grenze: u64 = std::env::var("CROSSPOINT_IDLE_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30_000);
    let dauerbetrieb = std::env::var("CROSSPOINT_KEEP_RUNNING").is_ok();

    // Aufpasser: verschwinden die virtuellen Geraete von aussen (z. B. weil
    // jemand aufgeraeumt hat), legt die Engine sie wieder an.
    let mut letzte_pruefung = std::time::Instant::now();

    while !SHUTDOWN.load(std::sync::atomic::Ordering::SeqCst) {
        thread::sleep(std::time::Duration::from_millis(250));
        if letzte_pruefung.elapsed() >= std::time::Duration::from_secs(5) {
            letzte_pruefung = std::time::Instant::now();
            let vorhanden = std::process::Command::new("pactl")
                .args(["list", "short", "sinks"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).contains("crosspoint_in_1"))
                .unwrap_or(true);
            if !vorhanden {
                eprintln!("virtuelle Geraete fehlen - lege sie neu an");
                wire("up");
            }
        }
        if !dauerbetrieb && crosspoint::http::leerlauf_ms() > leerlauf_grenze {
            eprintln!("keine Oberflaeche seit {} ms - beende", leerlauf_grenze);
            break;
        }
    }

    eprintln!("beende: virtuelle Geraete werden entfernt");
    wire("down");
}

/// Wendet ein Makro-Kommando der Form `strip 0 gain -6` an.
fn apply_macro_command(params: &Params, cmd: &str) {
    let teile: Vec<&str> = cmd.split_whitespace().collect();
    if teile.len() < 4 || teile[0] == "midi" || teile[0] == "wait" {
        return;
    }
    let index: usize = match teile[1].parse() {
        Ok(i) => i,
        Err(_) => return,
    };
    let wert = match teile[3] {
        "toggle" => {
            let ist = params
                .to_json()
                .get("strips")
                .and_then(|s| s.as_arr())
                .and_then(|a| a.get(index))
                .and_then(|s| s.get(teile[2]))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if ist > 0.0 {
                0.0
            } else {
                1.0
            }
        }
        w => match w.parse::<f32>() {
            Ok(f) => f,
            Err(_) => return,
        },
    };
    params.set(teile[0], index, teile[2], wert);
}

fn preset_path() -> PathBuf {
    crosspoint::pfade::datei("preset.json")
}
