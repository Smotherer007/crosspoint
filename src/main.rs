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
    tape: Arc<vm8::tape_io::TapeControl>,
    scratch: Vec<f32>,
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

        // Bus A1 (Stereo) in den Tape-Ring schieben - ohne Lock, ohne Allokation.
        if self.tape.recording.load(std::sync::atomic::Ordering::Relaxed) {
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
    let ring = Arc::new(vm8::tape_io::Ring::new(8, 2048));
    let tape = Arc::new(vm8::tape_io::TapeControl::new(ring.clone(), 2048));
    let rt: &'static mut Rt = Box::leak(Box::new(Rt {
        params: params.clone(),
        dsp: EngineDsp::new(2048),
        ins: [[std::ptr::null_mut(); 2]; STRIPS],
        outs: [[std::ptr::null_mut(); 2]; BUSES],
        tape: tape.clone(),
        scratch: vec![0.0f32; 2048 * 2],
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
    let tape_dir = preset.with_file_name("tape");
    let _ = std::fs::create_dir_all(&tape_dir);
    vm8::tape_io::spawn_writer(tape.clone(), tape_dir, jack.sample_rate);

    let listen_owned = listen.clone();
    let params_ui = params.clone();
    let tape_ui = tape.clone();
    thread::spawn(move || {
        if let Err(e) = vm8::http::serve(listen_owned, params_ui, preset, tape_ui) {
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
