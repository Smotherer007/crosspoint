//! Ein Prozess, ein Start: alles, was die Engine beim Hochfahren tut.
//!
//! Bis hierhin stand dieser Ablauf in `src/main.rs` des Binaries. Damit die
//! Oberflaeche im **selben** Prozess laufen kann (siehe
//! `docs/09-architektur.md`), ist er hierher gewandert:
//!
//! ```ignore
//! let lauf = crosspoint::lauf::starte(crosspoint::lauf::Konfig::vorgabe())?;
//! // ... Oberflaeche zeigt und schreibt `lauf.params`, liest `lauf.pegel` ...
//! lauf.abschalten();   // oder: der Audio-Client faellt weg -> `beendet()`
//! ```
//!
//! [`starte`] gibt einen [`Lauf`] zurueck: `Arc<Params>` (dieselben Atome, die
//! der Audio-Thread liest), die Pegelbank, die Tape-Steuerung und eine
//! Abschaltfunktion. Das Binary `crosspoint-engine` benutzt genau denselben
//! Aufruf und wartet danach nur noch auf [`Lauf::beendet`] – sein Verhalten
//! bleibt damit Zeile fuer Zeile dasselbe wie vorher.
//!
//! Was wo laeuft:
//!
//! | Thread | Aufgabe |
//! |---|---|
//! | JACK/PipeWire | [`Rt::run`] je Block: DSP, Tape-Ring, VBAN, Pegelbank |
//! | Aufpasser | Geraete weg? Dann `wire("up")`; Leerlauf? Dann beenden |
//! | HTTP | `src/http.rs` auf `listen` |
//! | OSC | `src/osc.rs` auf UDP 9000 |
//! | Tape-Schreiber | Ring -> WAV |
//! | VBAN | Sender und Empfaenger |
//! | Makros | Kommandos der Oberflaeche auf `Params` anwenden |
//! | Autosave | Preset nach 2 s Ruhe schreiben |
//!
//! Der Hauptthread des Aufrufers gehoert der Oberflaeche (GTK) bzw. der
//! Warteschleife des Binaries – nie dem Ton.

use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::engine::{EngineDsp, Io, Params};
use crate::jack::{
    AsyncClient, AudioIn, AudioOut, ClosureProcessHandler, Control, Jack, Port, ProcessScope,
};
use crate::macros_io::MacroControl;
use crate::pegel::Pegelbank;
use crate::tape_io::{Ring, TapeControl};
use crate::vban_io::{VbanControl, VbanRecvControl};
use crate::{BUSES, BUS_NAMES, STRIPS};

/// Prozess-Callback, boxed: der Typ steht im [`Lauf`], damit der Client dort
/// liegen kann, wo ihn die Abschaltfunktion erreicht.
type Callback = Box<dyn FnMut(&crate::jack::Client, &ProcessScope) -> Control + Send>;
/// Der aktivierte Client. Faellt er weg, deaktiviert JACK den Client und seine
/// Ports verschwinden aus dem Graphen.
type Aktiv = AsyncClient<(), ClosureProcessHandler<(), Callback>>;

/// Was der Aufrufer beim Start mitgeben kann.
#[derive(Clone, Debug)]
pub struct Konfig {
    /// JACK-Clientname (Vorgabe `crosspoint`); die Portnamen haengen daran.
    pub client_name: String,
    /// Adresse des HTTP-Adapters, z. B. `127.0.0.1:8770`.
    pub listen: String,
    /// Pfad der Preset-Datei (Autosave und Wiederherstellung).
    pub preset: PathBuf,
    /// Leerlaufabschaltung: keine Oberflaeche seit so vielen Millisekunden
    /// (`None` = nie). Im Fensterbetrieb ist das `None`, denn das Fenster ist
    /// die Oberflaeche und fragt den HTTP-Adapter nicht.
    pub leerlauf_ms: Option<u64>,
    /// Die Oberflaeche laeuft im **selben** Prozess (GTK, `crosspoint --features ui`).
    ///
    /// Dann ist der HTTP-Adapter nicht die Oberflaeche: fehlendes Polling sagt
    /// nichts ueber einen leeren Platz aus und darf die Engine weder beenden
    /// noch das Abraeumen uebernehmen. Das Aufraeumen haengt allein am Fenster
    /// (Schliessen, `abschalten`) - nie an einem Zeitgeber.
    pub oberflaeche_im_prozess: bool,
}

impl Konfig {
    /// Vorgaben wie im Binary: Client `crosspoint`, Port 8770, Preset im
    /// Konfigurationsverzeichnis. `CROSSPOINT_IDLE_MS` und
    /// `CROSSPOINT_KEEP_RUNNING` werden ausgewertet wie bisher.
    pub fn vorgabe() -> Konfig {
        Konfig {
            client_name: "crosspoint".to_string(),
            listen: "127.0.0.1:8770".to_string(),
            preset: crate::pfade::datei("preset.json"),
            leerlauf_ms: leerlauf_aus_env(),
            oberflaeche_im_prozess: false,
        }
    }

    /// Kein Leerlauf-Abschalten (Fensterbetrieb).
    ///
    /// Die Oberflaeche im selben Prozess fragt den HTTP-Adapter nicht; der
    /// Leerlauf-Waechter wuerde sonst nach `CROSSPOINT_IDLE_MS` die Engine
    /// beenden, obwohl das Fenster offen ist. Beides wird hier abgeschaltet:
    /// kein Zeitgeber entscheidet ueber Leben und Aufraeumen.
    pub fn dauerbetrieb(mut self) -> Konfig {
        self.leerlauf_ms = None;
        self.oberflaeche_im_prozess = true;
        self
    }
}

/// `CROSSPOINT_IDLE_MS` (Vorgabe 30 000), abgeschaltet durch
/// `CROSSPOINT_KEEP_RUNNING`.
fn leerlauf_aus_env() -> Option<u64> {
    if std::env::var("CROSSPOINT_KEEP_RUNNING").is_ok() {
        return None;
    }
    Some(
        std::env::var("CROSSPOINT_IDLE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30_000),
    )
}

/// Der laufende Mixer samt allem, was ihn bedient.
pub struct Lauf {
    pub params: Arc<Params>,
    /// Pegel je Streifen und Bus – der Audio-Thread schreibt, die Oberflaeche
    /// liest (kein JSON, kein Socket).
    pub pegel: Arc<Pegelbank>,
    pub tape: Arc<TapeControl>,
    pub vban: Arc<VbanControl>,
    pub vban_recv: Arc<VbanRecvControl>,
    pub makros: Arc<MacroControl>,
    pub devices: Arc<Mutex<crate::devices::DeviceMap>>,
    pub restore: Arc<Mutex<crate::restore::Ergebnis>>,
    pub preset: PathBuf,
    /// Ordner, in den der Tape-Schreiber seine WAV-Dateien legt.
    pub tape_ordner: PathBuf,
    pub sample_rate: u32,
    pub client_name: String,
    ende: Arc<AtomicBool>,
    osc: crate::osc::Halt,
    client: Mutex<Option<Aktiv>>,
    abgeschaltet: AtomicBool,
}

impl Lauf {
    /// Meldet, ob die Engine beendet ist (Leerlauf, Signal oder [`Self::abschalten`]).
    pub fn beendet(&self) -> bool {
        self.ende.load(Ordering::SeqCst)
    }

    /// Warteschleife fuer den Kopf ohne Fenster: blockiert, bis die Engine
    /// beendet ist. Die Aufraeumarbeit macht [`Self::abschalten`].
    pub fn warte_auf_ende(&self) {
        while !self.beendet() {
            thread::sleep(Duration::from_millis(250));
        }
    }

    /// Alles abbauen: Aufnahme schliessen, OSC anhalten, virtuelle Geraete
    /// entfernen, JACK-Client deaktivieren. Mehrfach aufrufbar.
    ///
    /// Reihenfolge (deterministisch, kein Zeitgeber):
    /// 1. Sperre setzen - ab hier legt niemand mehr Geraete an.
    /// 2. `wire down` ueber das Skript (der regulaere Weg mit Merkliste).
    /// 3. Sweep ohne Merkliste: entfernt **alle** `crosspoint_*`-Module und
    ///    -Knoten, auch wenn Schritt 2 fehlschlug oder das Skript fehlt.
    /// 4. Rest nachzaehlen und protokollieren.
    pub fn abschalten(&self) {
        if self.abgeschaltet.swap(true, Ordering::SeqCst) {
            return;
        }
        ABBAU.store(true, Ordering::SeqCst);
        self.ende.store(true, Ordering::SeqCst);
        self.tape
            .recording
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.osc.anhalten();
        wire("down");
        let bilanz = crate::aufraeumen::sweep("Fenster zu");
        // Der Schreiber-Thread finalisiert die offene Datei (WAV-Kopf mit der
        // richtigen Laenge). Er merkt das Merkmal binnen 4 ms; die Wartezeit
        // ist die Obergrenze, nicht die Regel.
        self.tape
            .shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
        thread::sleep(Duration::from_millis(120));
        self.pegel.zuruecksetzen();
        // Client deaktivieren: damit verschwinden die Ports aus dem Graphen.
        if let Ok(mut g) = self.client.lock() {
            g.take();
        }
        // Letzter Blick: was in Schritt 3 noch nicht weg war (z. B. ein Knoten,
        // den ein gerade entladenes Modul nachgeschoben hat), faellt hier.
        let (mut module, mut knoten) = crate::aufraeumen::reste();
        if module + knoten > 0 {
            eprintln!(
                "Aufraeumen unvollstaendig: {module} Module, {knoten} Knoten - zweiter Sweep"
            );
            crate::aufraeumen::sweep("Nachkontrolle");
            (module, knoten) = crate::aufraeumen::reste();
        }
        eprintln!("{}", bilanz.zeile("Fenster zu"));
        eprintln!("Engine beendet; Rest: {module} Module, {knoten} Knoten");
    }
}

impl Drop for Lauf {
    fn drop(&mut self) {
        self.abschalten();
    }
}

/* ==================================================================
Audio-Thread
================================================================== */

struct Rt {
    params: Arc<Params>,
    dsp: EngineDsp,
    /// Ports gehoeren (seit der Crate `jack`) uns; die Puffer kommen aus dem
    /// `ProcessScope` des Callbacks.
    ins: [[Option<Port<AudioIn>>; 2]; STRIPS],
    outs: [[Option<Port<AudioOut>>; 2]; BUSES],
    /// Ausgangspaare fuer empfangenes VBAN-Audio (`vban_L`/`vban_R`).
    vban_out: [Option<Port<AudioOut>>; 2],
    tape: Arc<TapeControl>,
    vban: Arc<VbanControl>,
    vban_ring: Arc<Ring>,
    vban_recv_ring: Arc<Ring>,
    pegel: Arc<Pegelbank>,
    /// Meter-Ballistik: gehaltener Spitzenwert je Streifen und Bus. Ohne Halt
    /// fiele die Anzeige in jedem Block ohne Sample auf Null und flackerte.
    halte_s: [f32; STRIPS],
    halte_b: [f32; BUSES],
    /// Blockpuffer fuer den VBAN-Eingang (fest, keine Allokation im Callback).
    vban_buf: Vec<f32>,
    vban_pos: usize,
    vban_len: usize,
    scratch: Vec<f32>,
}

impl Rt {
    /// Legt empfangenes VBAN-Audio auf `vban_L`/`vban_R`.
    ///
    /// Arbeitet blockweise: ein Ringblock (1024 Frames) wird gepuffert und ueber
    /// so viele Callbacks verteilt, wie noetig – so bleiben Blockgroesse des Rings
    /// und des JACK-Callbacks unabhaengig. Ist der Ring leer, wird der Rest des
    /// Puffers still geschrieben. Kein Lock, keine Allokation.
    fn vban_eingang(&mut self, ps: &ProcessScope, frames: usize) {
        // Je Port einzeln leihen, die Zeiger sind danach unabhaengig.
        let l = match self.vban_out[0].as_mut() {
            Some(p) => p.as_mut_slice(ps).as_mut_ptr(),
            None => return,
        };
        let r = match self.vban_out[1].as_mut() {
            Some(p) => p.as_mut_slice(ps).as_mut_ptr(),
            None => return,
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

    /// Spitzenwert eines Stereopaars im aktuellen Block.
    #[inline]
    fn spitze(links: *const f32, rechts: *const f32, frames: usize) -> f32 {
        let mut peak = 0.0f32;
        if links.is_null() {
            return peak;
        }
        unsafe {
            for i in 0..frames {
                let l = *links.add(i);
                let l = if l < 0.0 { -l } else { l };
                if l > peak {
                    peak = l;
                }
                if !rechts.is_null() {
                    let r = *rechts.add(i);
                    let r = if r < 0.0 { -r } else { r };
                    if r > peak {
                        peak = r;
                    }
                }
            }
        }
        peak
    }

    fn run(&mut self, ps: &ProcessScope) {
        let frames = ps.n_frames() as usize;
        let mut io = Io {
            ins: [[ptr::null(); 2]; STRIPS],
            outs: [[ptr::null_mut(); 2]; BUSES],
        };
        for s in 0..STRIPS {
            for c in 0..2 {
                if let Some(p) = self.ins[s][c].as_ref() {
                    io.ins[s][c] = p.as_slice(ps).as_ptr();
                }
            }
        }
        for b in 0..BUSES {
            for c in 0..2 {
                if let Some(p) = self.outs[b][c].as_mut() {
                    io.outs[b][c] = p.as_mut_slice(ps).as_mut_ptr();
                }
            }
        }
        // Pegel der Eingaenge: Spitze dieses Blocks mit Halt, direkt aus den
        // Puffern. Danach der DSP, danach die Ausgaenge - gemessen wird der
        // Block, den der Ton wirklich gesehen hat.
        let mut strip_pegel = [0.0f32; STRIPS];
        for (s, ziel) in strip_pegel.iter_mut().enumerate() {
            let peak = Self::spitze(io.ins[s][0], io.ins[s][1], frames);
            let alt = self.halte_s[s];
            let neu = if peak > alt { peak } else { alt * 0.72 };
            self.halte_s[s] = neu;
            *ziel = neu;
        }

        self.dsp.process(&self.params, &io, frames);

        let mut bus_pegel = [0.0f32; BUSES];
        for (b, ziel) in bus_pegel.iter_mut().enumerate() {
            let peak = Self::spitze(io.outs[b][0], io.outs[b][1], frames);
            let alt = self.halte_b[b];
            let neu = if peak > alt { peak } else { alt * 0.72 };
            self.halte_b[b] = neu;
            *ziel = neu;
        }
        self.pegel.schreibe_block(&strip_pegel, &bus_pegel);

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

        // Derselbe Block geht zusaetzlich in den VBAN-Ring, wenn VBAN aktiv ist.
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
        if self.vban_out[0].is_some() && self.vban_out[1].is_some() {
            self.vban_eingang(ps, frames);
        }
    }
}

/* ==================================================================
Start
================================================================== */

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: i32) {
    SHUTDOWN.store(true, Ordering::SeqCst);
}

extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}

/// SIGINT/SIGTERM nur als Merker: die Aufraeumarbeit macht der Aufpasser.
fn signale_abfangen() {
    unsafe {
        signal(2, on_signal as *const () as usize);
        signal(15, on_signal as *const () as usize);
    }
}

/// Anlegen und Abraeumen duerfen sich nie ueberholen: beide Wege sperren.
static WIRE_SPERRE: Mutex<()> = Mutex::new(());
/// Gesetzt, sobald abgeschaltet wird - ab dann wird nichts mehr angelegt.
static ABBAU: AtomicBool = AtomicBool::new(false);

/// Legt die virtuellen Knoten an bzw. raeumt sie ab. Damit haengen die Geraete
/// am Leben der Engine: laeuft die Engine, gibt es sie; wird sie beendet,
/// verschwinden sie wieder. CROSSPOINT_NO_WIRE=1 schaltet das ab.
///
/// Der Aufruf ist gesperrt ([`WIRE_SPERRE`]) und legt nach [`ABBAU`] nichts
/// mehr an: ein Aufpasser, der gerade `up` fahren will, kann das Abraeumen
/// sonst ueberholen - dann stehen die Geraete nach dem Schliessen wieder da.
pub fn wire(richtung: &str) {
    if std::env::var("CROSSPOINT_NO_WIRE").is_ok() {
        return;
    }
    let _sperre = WIRE_SPERRE.lock().unwrap_or_else(|e| e.into_inner());
    if richtung == "up" && ABBAU.load(Ordering::SeqCst) {
        eprintln!("wire up unterlassen: der Abbau laeuft");
        return;
    }
    // Reihenfolge: ausdruecklicher Pfad, neben dem Binary (Flatpak: /app/bin),
    // dann aufwaerts bis zur Repo-Wurzel (target/debug -> ../../../scripts),
    // zuletzt relativ zum Arbeitsverzeichnis.
    let mut kandidaten: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("CROSSPOINT_WIRE").filter(|p| !p.is_empty()) {
        kandidaten.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            kandidaten.push(dir.join("crosspoint-wire.sh"));
            let mut hoch = dir.to_path_buf();
            for _ in 0..5 {
                kandidaten.push(hoch.join("scripts").join("crosspoint-wire.sh"));
                if !hoch.pop() {
                    break;
                }
            }
        }
    }
    kandidaten.push(PathBuf::from("scripts/crosspoint-wire.sh"));

    let script = match kandidaten.iter().find(|p| p.is_file()) {
        Some(p) => p.clone(),
        None => {
            eprintln!(
                "wire {richtung}: crosspoint-wire.sh nicht gefunden \
                 (CROSSPOINT_WIRE setzen) - Geraete werden nur vom Sweep abgeraeumt"
            );
            return;
        }
    };
    match std::process::Command::new("bash")
        .arg(&script)
        .arg(richtung)
        .status()
    {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("wire {richtung}: {} endete mit {s}", script.display()),
        Err(e) => eprintln!("wire {richtung}: {} nicht startbar: {e}", script.display()),
    }
}

/// Engine hochfahren: Parameter, JACK-Client, Verdrahtung, Tape, VBAN, Makros,
/// HTTP, OSC, Effekt-Host, Wiederherstellung, Autosave, Aufpasser.
///
/// Der Aufruf kehrt zurueck, sobald alles laeuft; danach gehoert der
/// Hauptthread dem Aufrufer. Fehler beim Oeffnen des Audio-Clients kommen als
/// `Err` (die Meldung ist fertig formuliert), alles andere laeuft ohne den
/// jeweiligen Adapter weiter.
pub fn starte(konfig: Konfig) -> Result<Lauf, String> {
    signale_abfangen();
    // Einmalige Migration des Konfigurationsverzeichnisses, bevor irgendetwas
    // daraus gelesen wird (~/.config/vm8-linux -> ~/.config/crosspoint).
    if let Some(neu) = crate::pfade::migriere() {
        println!("Konfiguration uebernommen: {}", neu.display());
    }

    let params = Arc::new(Params::new(48_000.0, 512));
    let pegel = Arc::new(Pegelbank::neu());
    let ring = Arc::new(Ring::new(8, 2048));
    let tape = Arc::new(TapeControl::new(ring.clone(), 2048));
    let vban = Arc::new(VbanControl::new());
    let vban_ring = Arc::new(Ring::new(8, 2048));
    let vban_recv = Arc::new(VbanRecvControl::new());
    let vban_recv_ring = Arc::new(Ring::new(16, crate::vban_io::RECV_BLOCK_FRAMES));
    let macroctl = Arc::new(MacroControl::new());

    let mut rt = Rt {
        params: params.clone(),
        dsp: EngineDsp::new(2048),
        ins: std::array::from_fn(|_| [None, None]),
        outs: std::array::from_fn(|_| [None, None]),
        vban_out: [None, None],
        tape: tape.clone(),
        vban: vban.clone(),
        vban_ring: vban_ring.clone(),
        vban_recv_ring: vban_recv_ring.clone(),
        pegel: pegel.clone(),
        halte_s: [0.0; STRIPS],
        halte_b: [0.0; BUSES],
        vban_buf: vec![0.0f32; crate::vban_io::RECV_BLOCK_FRAMES * 2],
        vban_pos: 0,
        vban_len: 0,
        scratch: vec![0.0f32; 2048 * 2],
    };

    let jack = Jack::open(&konfig.client_name).map_err(|e| format!("JACK/PipeWire: {e}"))?;
    params.sample_rate.store(
        (jack.sample_rate as f32).to_bits(),
        std::sync::atomic::Ordering::Relaxed,
    );
    params
        .block
        .store(jack.block, std::sync::atomic::Ordering::Relaxed);
    let server_name = jack.name.clone();
    let sample_rate = jack.sample_rate;
    println!(
        "Client '{server_name}' verbunden: {sample_rate} Hz, Block {} Frames",
        jack.block
    );

    for s in 0..STRIPS {
        rt.ins[s] = [
            Some(jack.port_in(&format!("strip{}_L", s + 1)).expect("Port")),
            Some(jack.port_in(&format!("strip{}_R", s + 1)).expect("Port")),
        ];
    }
    for b in 0..BUSES {
        rt.outs[b] = [
            Some(
                jack.port_out(&format!("bus{}_L", BUS_NAMES[b]))
                    .expect("Port"),
            ),
            Some(
                jack.port_out(&format!("bus{}_R", BUS_NAMES[b]))
                    .expect("Port"),
            ),
        ];
    }
    // Zusatz-Ausgaenge fuer empfangenes VBAN-Audio. Sie werden wie ein
    // Hardware-Eingang per pw-link auf einen Streifen gelegt, z. B.
    //   pw-link crosspoint:vban_L crosspoint:strip1_L
    //   pw-link crosspoint:vban_R crosspoint:strip1_R
    rt.vban_out = [
        Some(jack.port_out("vban_L").expect("Port")),
        Some(jack.port_out("vban_R").expect("Port")),
    ];

    // Ab `activate` ruft JACK `run` je Block auf. Der Client wird im `Lauf`
    // gehalten: faellt er weg (Abschalten), verschwinden seine Ports.
    let callback: Callback = Box::new(move |_c: &crate::jack::Client, ps: &ProcessScope| {
        rt.run(ps);
        Control::Continue
    });
    let client: Aktiv = jack
        .activate(ClosureProcessHandler::new(callback))
        .map_err(|e| e)?;

    println!("\nPorts ({server_name}:<name>):");
    for s in 0..STRIPS {
        println!("  strip{}_L / strip{}_R", s + 1, s + 1);
    }
    for b in 0..BUSES {
        println!("  bus{}_L / bus{}_R", BUS_NAMES[b], BUS_NAMES[b]);
    }
    println!("  vban_L / vban_R (VBAN-Eingang, /api/vban/recv)");
    println!("\nVerdrahtung: scripts/crosspoint-wire.sh up\n");

    let tape_ordner = konfig.preset.with_file_name("tape");
    let _ = std::fs::create_dir_all(&tape_ordner);
    crate::tape_io::spawn_writer(tape.clone(), tape_ordner.clone(), sample_rate);

    crate::vban_io::spawn_sender(vban.clone(), vban_ring.clone(), sample_rate);
    crate::vban_io::spawn_empfaenger(vban_recv.clone(), vban_recv_ring.clone());

    // Macro-Kommandos aus der Oberflaeche auf die Parameter anwenden.
    {
        let params_mac = params.clone();
        let macro_worker = macroctl.clone();
        thread::spawn(move || loop {
            for cmd in macro_worker.drain() {
                apply_macro_command(&params_mac, &cmd);
            }
            thread::sleep(Duration::from_millis(25));
        });
    }

    let devices = Arc::new(Mutex::new(crate::devices::DeviceMap::load_default()));
    // Ergebnis der letzten Wiederherstellung; die API liest es, der Start
    // schreibt es hinein.
    let restore_ergebnis = Arc::new(Mutex::new(crate::restore::Ergebnis::default()));
    let preset_pfad = konfig.preset.clone();

    {
        let listen_owned = konfig.listen.clone();
        let params_ui = params.clone();
        let tape_ui = tape.clone();
        let vban_ui = vban.clone();
        let vban_recv_ui = vban_recv.clone();
        let macro_ui = macroctl.clone();
        let dev_ui = devices.clone();
        let restore_ui = restore_ergebnis.clone();
        let preset = konfig.preset.clone();
        thread::spawn(move || {
            if let Err(e) = crate::http::serve(
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

    // Selbstheilung vor dem Anlegen: Reste einer abgestuerzten oder frueheren
    // Instanz (kill -9 laesst Module und Knoten stehen) verschwinden hier,
    // bevor `wire up` dieselben Namen wieder belegt - sonst sammeln sich
    // Duplikate an, die niemand mehr zuordnen kann.
    ABBAU.store(false, Ordering::SeqCst);
    let reste = crate::aufraeumen::sweep("Start");
    if reste.gesamt() > 0 {
        eprintln!(
            "Reste der vorigen Instanz entfernt: {}",
            reste.zeile("Start")
        );
    }
    wire("up");

    // OSC-Eingang: `/strip/<n>/<param>` und `/bus/<n>/<param>` landen ueber
    // dieselbe Funktion wie `/api/set` in den Parametern (fuer MIDIMonster).
    let osc_halt = crate::osc::spawn(params.clone());
    // Effekt-Host: mod-host/Carla als betreuter Kindprozess (fehlt er, nur
    // eine Logzeile, die Engine laeuft weiter).
    crate::fx::spawn(params.clone());

    // Jetzt stehen die Busports im Graphen: gemerkte Zuordnungen anwenden.
    if crate::restore::deaktiviert() {
        println!("Wiederherstellen abgeschaltet (CROSSPOINT_NO_RESTORE)");
    } else {
        let ergebnis = crate::restore::beim_start(&params, &devices, &preset_pfad);
        if let Ok(mut letzte) = restore_ergebnis.lock() {
            *letzte = ergebnis;
        }
    }
    // Ab hier haelt der Autosave das Preset aktuell (2 s nach der letzten
    // Aenderung, im eigenen Thread).
    crate::restore::spawn_autosave(params.clone(), preset_pfad.clone());

    // Waechter: ein abgestecktes Geraet bleibt gemerkt; kommt es zurueck, wird
    // die Zuordnung ohne Neustart wieder gelegt.
    crate::wache::spawn_wache(devices.clone(), crate::inputs::mapping_path());

    let ende = Arc::new(AtomicBool::new(false));
    {
        let ende_w = ende.clone();
        let grenze = konfig
            .leerlauf_ms
            .filter(|_| !konfig.oberflaeche_im_prozess);
        thread::spawn(move || {
            // Aufpasser: verschwinden die virtuellen Geraete von aussen (z. B.
            // weil jemand aufgeraeumt hat), legt die Engine sie wieder an.
            //
            // Er legt **nur** an, nie ab, und hoert beim ersten Abbauzeichen
            // auf: das Abraeumen gehoert dem Fenster (abschalten), nicht einem
            // Zeitgeber. Ohne die Pruefung direkt vor `wire("up")` konnte ein
            // laufender Aufpasser die Geraete nach dem Schliessen wieder
            // anlegen - die Sperre in `wire` und [`ABBAU`] schliessen das.
            let mut letzte_pruefung = Instant::now();
            while !SHUTDOWN.load(Ordering::SeqCst) && !ende_w.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(250));
                // Nach dem Schlafen erneut pruefen: in den 250 ms kann der
                // Abbau begonnen haben.
                if SHUTDOWN.load(Ordering::SeqCst) || ende_w.load(Ordering::SeqCst) {
                    break;
                }
                if letzte_pruefung.elapsed() >= Duration::from_secs(5) {
                    letzte_pruefung = Instant::now();
                    if ende_w.load(Ordering::SeqCst) {
                        break;
                    }
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
                if let Some(grenze) = grenze {
                    if crate::http::leerlauf_ms() > grenze {
                        eprintln!("keine Oberflaeche seit {grenze} ms - beende");
                        break;
                    }
                }
            }
            ende_w.store(true, Ordering::SeqCst);
        });
    }

    Ok(Lauf {
        params,
        pegel,
        tape,
        vban,
        vban_recv,
        makros: macroctl,
        devices,
        restore: restore_ergebnis,
        preset: preset_pfad,
        tape_ordner,
        sample_rate,
        client_name: server_name,
        ende,
        osc: osc_halt,
        client: Mutex::new(Some(client)),
        abgeschaltet: AtomicBool::new(false),
    })
}

/// Wendet ein Makro-Kommando der Form `strip 0 gain -6` an.
pub fn apply_macro_command(params: &Params, cmd: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makro_setzt_einen_wert() {
        let p = Params::new(48_000.0, 512);
        apply_macro_command(&p, "strip 2 gain -6");
        let wert = f32::from_bits(p.strips[2].gain.load(Ordering::Relaxed));
        assert!((wert + 6.0).abs() < 1e-6);
    }

    #[test]
    fn makro_toggle_schaltet_um() {
        let p = Params::new(48_000.0, 512);
        apply_macro_command(&p, "strip 0 mute toggle");
        assert!(p.strips[0].mute.load(Ordering::Relaxed));
        apply_macro_command(&p, "strip 0 mute toggle");
        assert!(!p.strips[0].mute.load(Ordering::Relaxed));
    }

    #[test]
    fn unbekannte_kommandos_aendern_nichts() {
        let p = Params::new(48_000.0, 512);
        let vorher = p.to_json().to_json();
        for cmd in [
            "midi cc 1 2 3",
            "wait 500",
            "quatsch 0 gain 1",
            "strip 0 gain",
            "strip x gain 1",
            "strip 0 gain keine_zahl",
            "strip 99 gain 1",
        ] {
            apply_macro_command(&p, cmd);
        }
        assert_eq!(p.to_json().to_json(), vorher);
    }

    #[test]
    fn bus_makro_wirkt_auf_busse() {
        let p = Params::new(48_000.0, 512);
        apply_macro_command(&p, "bus 3 mute 1");
        assert!(p.buses[3].mute.load(Ordering::Relaxed));
    }

    /// Im Fensterbetrieb ist die Oberflaeche der Prozess selbst: weder ein
    /// Leerlauf-Zeitgeber noch fehlendes HTTP-Polling darf die Engine beenden.
    #[test]
    fn fensterbetrieb_hat_keinen_leerlauf() {
        let k = Konfig::vorgabe().dauerbetrieb();
        assert_eq!(k.leerlauf_ms, None);
        assert!(k.oberflaeche_im_prozess);
        // Die Vorgabe (Engine allein) haelt den Waechter dagegen bereit.
        let v = Konfig {
            oberflaeche_im_prozess: false,
            ..k
        };
        assert!(!v.oberflaeche_im_prozess);
    }
}
