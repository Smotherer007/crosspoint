//! vm8-engine – Einstiegspunkt: JACK-Client, Parameter, Steuer-API.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use vm8::engine::{EngineDsp, Io, Params};
use vm8::jack::{self, Jack, Port};
use vm8::{BUSES, BUS_NAMES, STRIPS};

struct Rt {
    params: Arc<Params>,
    dsp: EngineDsp,
    ins: [[*mut Port; 2]; STRIPS],
    outs: [[*mut Port; 2]; BUSES],
}

impl Rt {
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
    }
}

extern "C" fn process_cb(nframes: u32, arg: *mut c_void) -> i32 {
    let rt = unsafe { &mut *(arg as *mut Rt) };
    if nframes > 0 {
        rt.run(nframes as usize);
    }
    0
}

fn main() {
    let mut client_name = String::from("vm8");
    let mut listen = String::from("127.0.0.1:8770");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--client" => client_name = args.next().unwrap_or(client_name),
            "--listen" => listen = args.next().unwrap_or(listen),
            "-h" | "--help" => {
                println!("vm8-engine [--client NAME] [--listen ADDR]");
                return;
            }
            other => {
                eprintln!("unbekanntes Argument: {other}");
                std::process::exit(2);
            }
        }
    }

    let params = Arc::new(Params::new(48_000.0, 512));
    let rt: &'static mut Rt = Box::leak(Box::new(Rt {
        params: params.clone(),
        dsp: EngineDsp::new(2048),
        ins: [[std::ptr::null_mut(); 2]; STRIPS],
        outs: [[std::ptr::null_mut(); 2]; BUSES],
    }));

    let jack = match Jack::open(&client_name, process_cb, rt as *mut Rt as *mut c_void) {
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
        let l = jack.port_out(&format!("bus{}_L", BUS_NAMES[b])).expect("Port");
        let r = jack.port_out(&format!("bus{}_R", BUS_NAMES[b])).expect("Port");
        rt.outs[b] = [l, r];
    }
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
    println!("\nVerdrahtung: scripts/vm8-wire.sh up\n");

    let preset = preset_path();
    let listen_owned = listen.clone();
    let params_ui = params.clone();
    thread::spawn(move || {
        if let Err(e) = vm8::http::serve(listen_owned, params_ui, preset) {
            eprintln!("HTTP: {e}");
        }
    });

    loop {
        thread::sleep(std::time::Duration::from_secs(60));
    }
}

fn preset_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/vm8-linux/preset.json")
}
