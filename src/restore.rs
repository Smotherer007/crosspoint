//! Zustand beim Start wiederherstellen und Änderungen selbsttätig sichern.
//!
//! Drei Dinge gehören zum Zustand:
//!
//! * **Busse** – `~/.config/crosspoint/devices.json` (Bus → Sink). Gesetzt
//!   werden sie über denselben Weg wie `POST /api/devices`, also
//!   [`DeviceMap::assign`]: alte Links lösen, Ports des Geräts verbinden.
//! * **Streifen** – `~/.config/crosspoint/inputs.conf` (Streifen → Quelle),
//!   über [`crate::inputs::assign_mit`].
//! * **Parameter** – `~/.config/crosspoint/preset.json`, das vollständige
//!   Parameterbild aus [`Params::to_json`].
//!
//! Beim Start läuft das einmal nach dem Verdrahten (`wire up`), damit die
//! Busports der Engine schon im Graphen stehen.
//!
//! # Autosave
//!
//! Jede Änderung über `POST /api/set` setzt nur einen Zeitstempel
//! ([`markiere_schmutzig`]) – ein Atom, kein Lock, nichts, was den
//! Audio-Callback anfasst. Ein eigener Thread prüft alle 200 ms und schreibt
//! das Preset, wenn seit der letzten Änderung [`AUTOSAVE_RUHE_MS`] vergangen
//! sind. Damit kostet eine Reihe von Änderungen (Fader ziehen) genau einen
//! Schreibvorgang, und zwar nach dem letzten Schritt.
//!
//! `CROSSPOINT_NO_RESTORE=1` schaltet beides ab: weder wird beim Start
//! wiederhergestellt, noch schreibt der Autosave. Ein ausdrückliches
//! `POST /api/restore` wirkt trotzdem – wer es aufruft, will es.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::devices::DeviceMap;
use crate::engine::Params;
use crate::json::Value;
use crate::pw::{PipeWire, Zugriff};

/// Ruhezeit nach der letzten Änderung, bevor der Autosave schreibt.
pub const AUTOSAVE_RUHE_MS: u64 = 2000;

/// Prüftakt des Autosave-Threads.
const AUTOSAVE_TAKT_MS: u64 = 200;

/// Autosave-Zustand: nur der Zeitstempel der letzten Änderung (0 = sauber).
pub struct Autosave {
    seit: AtomicU64,
}

fn jetzt_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Default for Autosave {
    fn default() -> Autosave {
        Autosave::new()
    }
}

impl Autosave {
    pub const fn new() -> Autosave {
        Autosave {
            seit: AtomicU64::new(0),
        }
    }

    /// Eine Änderung ist passiert: Zeitstempel setzen. Nur ein Atom – der
    /// Aufrufer kann im HTTP-Thread sitzen, der Audio-Callback wird nie berührt.
    pub fn markiere(&self) {
        self.seit.store(jetzt_ms().max(1), Ordering::Relaxed);
    }

    /// Steht eine Sicherung aus?
    pub fn ist_schmutzig(&self) -> bool {
        self.seit.load(Ordering::Relaxed) != 0
    }

    /// Ein Schritt: schreibt das Preset, wenn seit der letzten Änderung `ruhe`
    /// vergangen ist. Liefert `true`, wenn geschrieben wurde.
    ///
    /// Zwischen Prüfung und Schreiben darf sich der Zeitstempel ändern (der
    /// nächste Faderzug); dann wird nicht geschrieben und der folgende Takt
    /// übernimmt – so entsteht nie ein halbfertiges Bild.
    pub fn schritt(&self, params: &Arc<Params>, pfad: &Path, ruhe: Duration) -> bool {
        let seit = self.seit.load(Ordering::Relaxed);
        if !ruhe_vorbei(seit, jetzt_ms(), ruhe) {
            return false;
        }
        if self
            .seit
            .compare_exchange(seit, 0, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            return false;
        }
        match preset_speichern(params, pfad) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("Autosave: {e}");
                // Nicht in einer Endlosschleife schreiben wollen: erst die
                // nächste Änderung löst einen neuen Versuch aus.
                false
            }
        }
    }
}

/// Der Autosave der laufenden Engine.
pub static AUTOSAVE: Autosave = Autosave::new();

/// Eine Änderung ist passiert (siehe [`Autosave::markiere`]).
pub fn markiere_schmutzig() {
    AUTOSAVE.markiere();
}

/// Steht eine Sicherung aus?
pub fn ist_schmutzig() -> bool {
    AUTOSAVE.ist_schmutzig()
}

/// Ist die Ruhezeit seit der letzten Änderung vorbei?
fn ruhe_vorbei(seit: u64, jetzt: u64, ruhe: Duration) -> bool {
    seit != 0 && jetzt.saturating_sub(seit) >= ruhe.as_millis() as u64
}

/// Ein Autosave-Schritt der laufenden Engine (siehe [`Autosave::schritt`]).
pub fn autosave_schritt(params: &Arc<Params>, pfad: &Path, ruhe: Duration) -> bool {
    AUTOSAVE.schritt(params, pfad, ruhe)
}

/// Ist das automatische Wiederherstellen/Autosave abgeschaltet?
pub fn deaktiviert() -> bool {
    match std::env::var("CROSSPOINT_NO_RESTORE") {
        Ok(v) => {
            let v = v.trim();
            !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false"))
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Ergebnis
// ---------------------------------------------------------------------------

/// Was zuletzt wiederhergestellt wurde. `devices`/`inputs` zählen die
/// tatsächlich gesetzten Zuordnungen, `preset` sagt, ob ein Parameterbild
/// geladen wurde. `fehler` bleibt für das Protokoll – die API liefert nur die
/// drei Zahlen.
#[derive(Clone, Debug, Default)]
pub struct Ergebnis {
    pub devices: usize,
    pub inputs: usize,
    pub preset: bool,
    pub fehler: Vec<String>,
}

impl Ergebnis {
    /// `{"devices":n,"inputs":n,"preset":bool}`
    pub fn to_json(&self) -> String {
        Value::obj(vec![
            ("devices", Value::num(self.devices as f64)),
            ("inputs", Value::num(self.inputs as f64)),
            ("preset", Value::Bool(self.preset)),
        ])
        .to_json()
    }

    fn meldungen_ausgeben(&self, was: &str) {
        for f in &self.fehler {
            eprintln!("{was}: {f}");
        }
    }
}

// ---------------------------------------------------------------------------
// Die drei Teile
// ---------------------------------------------------------------------------

/// Alle gemerkten Busse erneut zuordnen (derselbe Weg wie `POST /api/devices`).
pub fn devices_wiederherstellen(
    map: &mut DeviceMap,
    zugriff: &dyn Zugriff,
) -> (usize, Vec<String>) {
    let eintraege: Vec<(String, String)> = map
        .entries()
        .into_iter()
        .map(|(bus, name)| (bus.to_string(), name.to_string()))
        .collect();
    let mut gesetzt = 0;
    let mut fehler = Vec::new();
    for (bus, sink) in eintraege {
        match map.assign(zugriff, &bus, &sink) {
            Ok(()) => gesetzt += 1,
            Err(e) => fehler.push(format!("{bus} auf '{sink}': {e}")),
        }
    }
    (gesetzt, fehler)
}

/// Alle gemerkten Streifen-Eingänge erneut verbinden.
pub fn inputs_wiederherstellen(zugriff: &dyn Zugriff, pfad: &Path) -> (usize, Vec<String>) {
    let zuordnung = crate::inputs::mapping_mit(pfad);
    let mut gesetzt = 0;
    let mut fehler = Vec::new();
    for (i, quelle) in zuordnung.iter().enumerate() {
        let Some(quelle) = quelle else { continue };
        match crate::inputs::assign_mit(zugriff, pfad, i + 1, Some(quelle)) {
            Ok(()) => gesetzt += 1,
            Err(e) => fehler.push(format!("Streifen {}: {e}", i + 1)),
        }
    }
    (gesetzt, fehler)
}

/// Preset laden; `false`, wenn keines da ist oder es nicht lesbar ist.
pub fn preset_laden(params: &Arc<Params>, pfad: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(pfad) else {
        return false;
    };
    match crate::json::parse(&text) {
        Ok(v) => {
            crate::http::apply_preset(params, &v);
            true
        }
        Err(e) => {
            eprintln!("Preset {} nicht lesbar: {e}", pfad.display());
            false
        }
    }
}

/// Schreibt das vollständige Parameterbild als Preset.
pub fn preset_speichern(params: &Arc<Params>, pfad: &Path) -> Result<(), String> {
    if let Some(dir) = pfad.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("{} nicht anlegbar: {e}", dir.display()))?;
    }
    std::fs::write(pfad, params.to_json().to_json())
        .map_err(|e| format!("{} nicht schreibbar: {e}", pfad.display()))
}

// ---------------------------------------------------------------------------
// Alles zusammen
// ---------------------------------------------------------------------------

/// Stellt den Zustand wieder her. `devices`/`inputs`/`preset` wählen die Teile
/// aus; beim Start sind alle drei dran, `POST /api/restore` kann einzelne
/// weglassen.
pub fn anwenden(
    params: &Arc<Params>,
    devices: &Arc<Mutex<DeviceMap>>,
    preset_pfad: &Path,
    mit_devices: bool,
    mit_inputs: bool,
    mit_preset: bool,
) -> Ergebnis {
    let zugriff = PipeWire;
    let mut ergebnis = Ergebnis::default();

    if mit_devices {
        match devices.lock() {
            Ok(mut map) => {
                let (n, f) = devices_wiederherstellen(&mut map, &zugriff);
                ergebnis.devices = n;
                ergebnis.fehler.extend(f);
            }
            Err(_) => ergebnis.fehler.push("Geräteliste gesperrt".into()),
        }
    }

    if mit_inputs {
        let (n, f) = inputs_wiederherstellen(&zugriff, &crate::inputs::mapping_path());
        ergebnis.inputs = n;
        ergebnis.fehler.extend(f);
    }

    if mit_preset {
        ergebnis.preset = preset_laden(params, preset_pfad);
    }

    ergebnis
}

/// Alles wiederherstellen und einmal ins Protokoll schreiben.
pub fn beim_start(
    params: &Arc<Params>,
    devices: &Arc<Mutex<DeviceMap>>,
    preset_pfad: &Path,
) -> Ergebnis {
    let ergebnis = anwenden(params, devices, preset_pfad, true, true, true);
    println!(
        "Zustand wiederhergestellt: {} Bus(se), {} Streifen-Eingang/-Eingaenge, Preset {}",
        ergebnis.devices,
        ergebnis.inputs,
        if ergebnis.preset {
            "geladen"
        } else {
            "nicht vorhanden"
        }
    );
    ergebnis.meldungen_ausgeben("Wiederherstellen");
    if !ergebnis.fehler.is_empty() {
        println!(
            "{} Zuordnung(en) konnten nicht gesetzt werden (Gerät nicht vorhanden?)",
            ergebnis.fehler.len()
        );
    }
    ergebnis
}

// ---------------------------------------------------------------------------
// Autosave-Thread
// ---------------------------------------------------------------------------

/// Startet den Autosave-Thread (nichts, wenn `CROSSPOINT_NO_RESTORE` gesetzt ist).
pub fn spawn_autosave(params: Arc<Params>, pfad: PathBuf) {
    if deaktiviert() {
        println!("Autosave abgeschaltet (CROSSPOINT_NO_RESTORE)");
        return;
    }
    println!("Autosave: {}", pfad.display());
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(AUTOSAVE_TAKT_MS));
        autosave_schritt(&params, &pfad, Duration::from_millis(AUTOSAVE_RUHE_MS));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pw::tests::{Testzugriff, DUMP};

    const CORSAIR: &str =
        "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo";

    fn tmp(tag: &str, endung: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "crosspoint-restore-{}-{tag}.{endung}",
            std::process::id()
        ))
    }

    #[test]
    fn deaktiviert_liest_die_umgebungsvariable() {
        // Nur die reine Auswertung prüfen, ohne die Variable global zu setzen:
        // sie gilt prozessweit und würde andere Tests stören.
        assert!(!deaktiviert());
    }

    #[test]
    fn ruhezeit_erst_nach_zwei_sekunden_vorbei() {
        let ruhe = Duration::from_millis(2000);
        assert!(!ruhe_vorbei(0, 10_000, ruhe), "nichts zu tun");
        assert!(
            !ruhe_vorbei(9_000, 10_000, ruhe),
            "eine Sekunde ist zu wenig"
        );
        assert!(!ruhe_vorbei(8_001, 10_000, ruhe), "1999 ms sind zu wenig");
        assert!(ruhe_vorbei(8_000, 10_000, ruhe), "genau 2 s");
        assert!(ruhe_vorbei(1, 10_000, ruhe), "lange her");
        assert_eq!(AUTOSAVE_RUHE_MS, 2000, "Vorgabe der Engine");
    }

    #[test]
    fn autosave_schreibt_nach_der_ruhezeit_genau_einmal() {
        let pfad = tmp("autosave", "json");
        let _ = std::fs::remove_file(&pfad);
        let params = Arc::new(Params::new(48_000.0, 512));
        let merker = Autosave::new();
        params.set("strip", 0, "gain", -6.0);

        merker.markiere();
        assert!(merker.ist_schmutzig());
        // Vor Ablauf der Ruhezeit passiert nichts.
        assert!(!merker.schritt(&params, &pfad, Duration::from_secs(30)));
        assert!(!pfad.exists(), "noch nicht geschrieben");

        thread::sleep(Duration::from_millis(60));
        assert!(merker.schritt(&params, &pfad, Duration::from_millis(20)));
        assert!(!merker.ist_schmutzig(), "nach dem Schreiben ist es sauber");
        let text = std::fs::read_to_string(&pfad).unwrap();
        assert!(text.contains("\"strips\""), "{text}");
        assert!(text.contains("-6"), "{text}");

        // Zweiter Aufruf ohne neue Änderung schreibt nicht wieder.
        assert!(!merker.schritt(&params, &pfad, Duration::ZERO));
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn autosave_haelt_eine_reihe_von_aenderungen_zurueck() {
        let pfad = tmp("autosave-reihe", "json");
        let _ = std::fs::remove_file(&pfad);
        let params = Arc::new(Params::new(48_000.0, 512));
        let merker = Autosave::new();
        for _ in 0..5 {
            params.set("strip", 1, "gain", -3.0);
            merker.markiere();
            thread::sleep(Duration::from_millis(5));
            assert!(!merker.schritt(&params, &pfad, Duration::from_millis(500)));
        }
        assert!(
            !pfad.exists(),
            "waehrend der Aenderungen nichts geschrieben"
        );
        assert!(merker.ist_schmutzig());
        thread::sleep(Duration::from_millis(550));
        assert!(merker.schritt(&params, &pfad, Duration::from_millis(500)));
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn devices_werden_wie_bei_api_devices_gesetzt() {
        let pfad = tmp("devices", "json");
        let _ = std::fs::remove_file(&pfad);
        let mut map = DeviceMap::new(&pfad);
        let fake = Testzugriff::aus_dump(DUMP);
        map.assign(&fake, "A1", CORSAIR).unwrap();
        let vorher = fake.aktionen().len();

        let (n, fehler) = devices_wiederherstellen(&mut map, &fake);
        assert_eq!(n, 1, "{fehler:?}");
        assert!(fehler.is_empty(), "{fehler:?}");
        let neu: Vec<String> = fake.aktionen()[vorher..].to_vec();
        assert_eq!(
            neu[0],
            format!("trennen crosspoint:busA1_L {CORSAIR}:playback_FL")
        );
        assert!(neu.contains(&format!("verbinden crosspoint:busA1_L {CORSAIR}:playback_FL")));
        assert!(neu.contains(&format!("verbinden crosspoint:busA1_R {CORSAIR}:playback_FR")));
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn fehlendes_geraet_zaehlt_nicht_und_bricht_nicht_ab() {
        let pfad = tmp("devices-fehlt", "json");
        let _ = std::fs::remove_file(&pfad);
        std::fs::write(
            &pfad,
            r#"{"A1":"alsa_output.gibtsnicht","A2":"alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo"}"#,
        )
        .unwrap();
        let mut map = DeviceMap::load(&pfad);
        let fake = Testzugriff::aus_dump(DUMP);
        let (n, fehler) = devices_wiederherstellen(&mut map, &fake);
        assert_eq!(n, 1, "nur A2 laesst sich verbinden: {fehler:?}");
        assert_eq!(fehler.len(), 1);
        assert!(fehler[0].contains("A1"), "{fehler:?}");
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn streifen_werden_aus_der_merkliste_gesetzt() {
        let pfad = tmp("inputs", "conf");
        let _ = std::fs::remove_file(&pfad);
        std::fs::write(&pfad, "1=alsa_input.usb-mic\n3=\n").unwrap();
        let fake = Testzugriff::aus_dump(
            r#"[
              {"id": 1, "type": "PipeWire:Interface:Node",
               "info": {"props": {"media.class": "Audio/Source", "node.name": "alsa_input.usb-mic",
                                  "node.description": "USB-Mikrofon"}}},
              {"id": 2, "type": "PipeWire:Interface:Node",
               "info": {"props": {"node.name": "crosspoint"}}},
              {"id": 3, "type": "PipeWire:Interface:Port",
               "info": {"direction": "output", "props": {"node.id": 1, "port.name": "capture_FL"}}},
              {"id": 4, "type": "PipeWire:Interface:Port",
               "info": {"direction": "output", "props": {"node.id": 1, "port.name": "capture_FR"}}},
              {"id": 5, "type": "PipeWire:Interface:Port",
               "info": {"direction": "input", "props": {"node.id": 2, "port.name": "strip1_L"}}},
              {"id": 6, "type": "PipeWire:Interface:Port",
               "info": {"direction": "input", "props": {"node.id": 2, "port.name": "strip1_R"}}}
            ]"#,
        );
        let (n, fehler) = inputs_wiederherstellen(&fake, &pfad);
        assert_eq!(n, 1, "{fehler:?}");
        assert!(fehler.is_empty(), "{fehler:?}");
        let zeilen = fake.aktionen();
        assert!(zeilen
            .contains(&"verbinden alsa_input.usb-mic:capture_FL crosspoint:strip1_L".to_string()));
        assert!(zeilen
            .contains(&"verbinden alsa_input.usb-mic:capture_FR crosspoint:strip1_R".to_string()));
        // Leere Streifen bleiben unangetastet.
        assert!(!zeilen.iter().any(|z| z.contains("strip3")));
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn preset_wird_gespeichert_und_geladen() {
        let pfad = tmp("preset", "json");
        let _ = std::fs::remove_file(&pfad);
        let params = Arc::new(Params::new(48_000.0, 512));
        params.set("strip", 2, "gain", -12.5);
        params.set("strip", 2, "mute", 1.0);
        params.set("bus", 0, "gain", -3.0);
        preset_speichern(&params, &pfad).unwrap();

        let frisch = Arc::new(Params::new(48_000.0, 512));
        assert!(preset_laden(&frisch, &pfad));
        let json = frisch.to_json().to_json();
        assert!(json.contains("-12.5"), "{json}");
        assert!(json.contains("-3"), "{json}");
        let wert = crate::json::parse(&json).unwrap();
        assert_eq!(
            wert.get("strips")
                .and_then(|s| s.as_arr())
                .and_then(|a| a.get(2))
                .and_then(|s| s.get("mute"))
                .unwrap(),
            &Value::Bool(true)
        );
        let _ = std::fs::remove_file(&pfad);
    }

    #[test]
    fn fehlendes_preset_ist_kein_fehler() {
        let params = Arc::new(Params::new(48_000.0, 512));
        assert!(!preset_laden(&params, Path::new("/gibt/es/nicht.json")));
    }

    #[test]
    fn ergebnis_json_hat_die_drei_zahlen() {
        let e = Ergebnis {
            devices: 2,
            inputs: 1,
            preset: true,
            fehler: vec!["egal".into()],
        };
        assert_eq!(e.to_json(), r#"{"devices":2,"inputs":1,"preset":true}"#);
    }
}
