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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

/// Wie lange beim Start auf ein Gerät gewartet wird, dessen Ports erst beim
/// Aufwecken erscheinen (USB-Mikrofon nach dem Einstecken).
pub const START_GEDULD: Duration = Duration::from_secs(3);

/// Abstand zwischen zwei Versuchen beim Start.
pub const START_TAKT: Duration = Duration::from_millis(200);

// ---------------------------------------------------------------------------
// Ergebnis
// ---------------------------------------------------------------------------

/// Ergebnis eines Teils der Wiederherstellung.
///
/// `fehlt` ist **kein Fehler**: das Gerät ist nur gerade nicht da, die
/// Zuordnung bleibt gespeichert und der Wächter legt sie später an. `fehler`
/// sammelt, was wirklich schiefgegangen ist (Ports, PipeWire, Rechte).
#[derive(Clone, Debug, Default)]
pub struct Teil {
    /// Tatsächlich gesetzte Zuordnungen.
    pub gesetzt: usize,
    /// Gemerkt, aber Gerät nicht angeschlossen.
    pub fehlt: Vec<String>,
    /// Echte Fehler.
    pub fehler: Vec<String>,
}

impl Teil {
    fn aufnehmen(&mut self, was: &str, e: String) {
        self.fehler.push(format!("{was}: {e}"));
    }
}

/// Was zuletzt wiederhergestellt wurde. `devices`/`inputs` zählen die
/// tatsächlich gesetzten Zuordnungen, `preset` sagt, ob ein Parameterbild
/// geladen wurde. `fehlend` nennt, was auf ein Gerät wartet, `fehler` bleibt
/// für das Protokoll – die API liefert nur die drei Zahlen.
#[derive(Clone, Debug, Default)]
pub struct Ergebnis {
    pub devices: usize,
    pub inputs: usize,
    pub preset: bool,
    pub fehlend: Vec<String>,
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
///
/// Ohne Wartezeit – für `POST /api/restore` und Tests.
pub fn devices_wiederherstellen(map: &mut DeviceMap, zugriff: &dyn Zugriff) -> Teil {
    devices_wiederherstellen_mit(map, zugriff, Duration::ZERO, START_TAKT)
}

/// Wie [`devices_wiederherstellen`], aber mit Wiederholung: bis `geduld`
/// vergeht, wird alle `takt` erneut geprüft, ob ein fehlendes Gerät auftaucht.
///
/// Beim Start ist das wichtig: nach dem Einstecken eines USB-Mikrofons
/// erscheinen die Ports erst, wenn das Gerät aufgeweckt ist. Erst danach wird
/// „fehlt“ gemeldet – und der Eintrag bleibt in jedem Fall in `devices.json`.
pub fn devices_wiederherstellen_mit(
    map: &mut DeviceMap,
    zugriff: &dyn Zugriff,
    geduld: Duration,
    takt: Duration,
) -> Teil {
    let mut teil = Teil::default();
    let mut offen: Vec<(String, String)> = map
        .entries()
        .into_iter()
        .map(|(bus, name)| (bus.to_string(), name.to_string()))
        .collect();
    let frist = Instant::now() + geduld;
    let mut grund = "abgesteckt?".to_string();

    loop {
        let graph = zugriff.graph();
        if let Err(e) = &graph {
            grund = format!("PipeWire nicht erreichbar: {e}");
        }
        let mut weiter = Vec::new();
        for (bus, name) in std::mem::take(&mut offen) {
            let da = graph
                .as_ref()
                .map(|g| g.knoten(&name).is_some())
                .unwrap_or(false);
            if !da {
                weiter.push((bus, name));
                continue;
            }
            match map.assign(zugriff, &bus, &name) {
                Ok(()) => teil.gesetzt += 1,
                Err(e) => teil.aufnehmen(&format!("Bus {bus} auf '{name}'"), e),
            }
        }
        if weiter.is_empty() {
            return teil;
        }
        if Instant::now() >= frist {
            for (bus, name) in weiter {
                teil.fehlt.push(format!(
                    "Bus {bus}: '{name}' nicht angeschlossen ({grund}) – Zuordnung bleibt gespeichert"
                ));
            }
            return teil;
        }
        let rest = frist.saturating_duration_since(Instant::now());
        std::thread::sleep(takt.min(rest).max(Duration::from_millis(1)));
        offen = weiter;
    }
}

/// Alle gemerkten Streifen-Eingänge erneut verbinden.
pub fn inputs_wiederherstellen(zugriff: &dyn Zugriff, pfad: &Path) -> Teil {
    inputs_wiederherstellen_mit(zugriff, pfad, Duration::ZERO, START_TAKT)
}

/// Wie [`inputs_wiederherstellen`], aber mit Wiederholung (siehe
/// [`devices_wiederherstellen_mit`]).
pub fn inputs_wiederherstellen_mit(
    zugriff: &dyn Zugriff,
    pfad: &Path,
    geduld: Duration,
    takt: Duration,
) -> Teil {
    let mut teil = Teil::default();
    let frist = Instant::now() + geduld;
    let mut offen: Vec<(usize, String)> = crate::inputs::mapping_mit(pfad)
        .iter()
        .enumerate()
        .filter_map(|(i, q)| q.as_ref().map(|name| (i + 1, name.clone())))
        .collect();
    let mut grund = "abgesteckt?".to_string();

    loop {
        let graph = zugriff.graph();
        if let Err(e) = &graph {
            grund = format!("PipeWire nicht erreichbar: {e}");
        }
        let mut weiter = Vec::new();
        for (strip, name) in std::mem::take(&mut offen) {
            let da = graph
                .as_ref()
                .map(|g| g.knoten(&name).is_some())
                .unwrap_or(false);
            if !da {
                weiter.push((strip, name));
                continue;
            }
            match crate::inputs::assign_mit(zugriff, pfad, strip, Some(&name)) {
                Ok(()) => teil.gesetzt += 1,
                Err(e) => teil.aufnehmen(&format!("Streifen {strip} auf '{name}'"), e),
            }
        }
        if weiter.is_empty() {
            return teil;
        }
        if Instant::now() >= frist {
            for (strip, name) in weiter {
                teil.fehlt.push(format!(
                    "Streifen {strip}: '{name}' nicht angeschlossen ({grund}) – Zuordnung bleibt gespeichert"
                ));
            }
            return teil;
        }
        let rest = frist.saturating_duration_since(Instant::now());
        std::thread::sleep(takt.min(rest).max(Duration::from_millis(1)));
        offen = weiter;
    }
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
    anwenden_mit_geduld(
        params,
        devices,
        preset_pfad,
        mit_devices,
        mit_inputs,
        mit_preset,
        Duration::ZERO,
    )
}

/// Wie [`anwenden`], aber mit Wartezeit für Geräte, die noch auftauchen
/// ([`START_GEDULD`] beim Start, `Duration::ZERO` für `POST /api/restore`).
#[allow(clippy::too_many_arguments)]
pub fn anwenden_mit_geduld(
    params: &Arc<Params>,
    devices: &Arc<Mutex<DeviceMap>>,
    preset_pfad: &Path,
    mit_devices: bool,
    mit_inputs: bool,
    mit_preset: bool,
    geduld: Duration,
) -> Ergebnis {
    let zugriff = PipeWire;
    let mut ergebnis = Ergebnis::default();

    if mit_devices {
        match devices.lock() {
            Ok(mut map) => {
                let teil = devices_wiederherstellen_mit(&mut map, &zugriff, geduld, START_TAKT);
                ergebnis.devices = teil.gesetzt;
                ergebnis.fehlend.extend(teil.fehlt);
                ergebnis.fehler.extend(teil.fehler);
            }
            Err(_) => ergebnis.fehler.push("Geräteliste gesperrt".into()),
        }
    }

    if mit_inputs {
        let teil = inputs_wiederherstellen_mit(
            &zugriff,
            &crate::inputs::mapping_path(),
            geduld,
            START_TAKT,
        );
        ergebnis.inputs = teil.gesetzt;
        ergebnis.fehlend.extend(teil.fehlt);
        ergebnis.fehler.extend(teil.fehler);
    }

    if mit_preset {
        ergebnis.preset = preset_laden(params, preset_pfad);
    }

    ergebnis
}

/// Alles wiederherstellen und einmal ins Protokoll schreiben.
///
/// Ein fehlendes Gerät ist hier **kein Fehler**: es wird als „nicht
/// angeschlossen“ gemeldet, die Zuordnung bleibt gespeichert, und der Wächter
/// legt sie an, sobald das Gerät wieder da ist.
pub fn beim_start(
    params: &Arc<Params>,
    devices: &Arc<Mutex<DeviceMap>>,
    preset_pfad: &Path,
) -> Ergebnis {
    let ergebnis =
        anwenden_mit_geduld(params, devices, preset_pfad, true, true, true, START_GEDULD);
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
    if !ergebnis.fehlend.is_empty() {
        println!(
            "{} Zuordnung(en) warten auf ein Geraet (nicht angeschlossen):",
            ergebnis.fehlend.len()
        );
        for zeile in &ergebnis.fehlend {
            println!("  {zeile}");
        }
        println!("Die Zuordnung bleibt gespeichert und greift, sobald das Geraet wieder da ist.");
    }
    if !ergebnis.fehler.is_empty() {
        println!(
            "{} Zuordnung(en) konnten nicht gesetzt werden",
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
    use crate::pw::Graph;

    const CORSAIR: &str =
        "alsa_output.usb-Corsair_Corsair_HS80_RGB_USB_Gaming_Headset-00.analog-stereo";

    fn tmp(tag: &str, endung: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "crosspoint-restore-{}-{tag}.{endung}",
            std::process::id()
        ))
    }

    /// Nur der Mixer mit den Streifenports – kein Gerät.
    const LEER: &str = r#"[
      {"id": 2, "type": "PipeWire:Interface:Node",
       "info": {"props": {"node.name": "crosspoint"}}},
      {"id": 5, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 2, "port.name": "strip1_L"}}},
      {"id": 6, "type": "PipeWire:Interface:Port",
       "info": {"direction": "input", "props": {"node.id": 2, "port.name": "strip1_R"}}}
    ]"#;

    /// Wie [`LEER`], aber mit dem Mikrofon (noch ohne Link).
    const GEPLUGGT: &str = r#"[
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
    ]"#;

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

        let teil = devices_wiederherstellen(&mut map, &fake);
        assert_eq!(teil.gesetzt, 1, "{:?}", teil.fehler);
        assert!(teil.fehler.is_empty(), "{:?}", teil.fehler);
        assert!(teil.fehlt.is_empty(), "{:?}", teil.fehlt);
        let neu: Vec<String> = fake.aktionen()[vorher..].to_vec();
        assert_eq!(
            neu[0],
            format!("trennen crosspoint:busA1_L {CORSAIR}:playback_FL")
        );
        assert!(neu.contains(&format!(
            "verbinden crosspoint:busA1_L {CORSAIR}:playback_FL"
        )));
        assert!(neu.contains(&format!(
            "verbinden crosspoint:busA1_R {CORSAIR}:playback_FR"
        )));
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
        let teil = devices_wiederherstellen(&mut map, &fake);
        assert_eq!(teil.gesetzt, 1, "nur A2 laesst sich verbinden: {teil:?}");
        // Fehlendes Geraet ist kein Fehler, sondern ein Zustand.
        assert!(teil.fehler.is_empty(), "{:?}", teil.fehler);
        assert_eq!(teil.fehlt.len(), 1, "{:?}", teil.fehlt);
        assert!(teil.fehlt[0].contains("A1"), "{:?}", teil.fehlt);
        assert!(teil.fehlt[0].contains("abgesteckt"), "{:?}", teil.fehlt);
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
        let teil = inputs_wiederherstellen(&fake, &pfad);
        assert_eq!(teil.gesetzt, 1, "{:?}", teil.fehler);
        assert!(teil.fehler.is_empty(), "{:?}", teil.fehler);
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
            fehlend: vec!["Streifen 1: 'x' nicht angeschlossen".into()],
            fehler: vec!["egal".into()],
        };
        assert_eq!(e.to_json(), r#"{"devices":2,"inputs":1,"preset":true}"#);
    }

    #[test]
    fn fehlendes_geraet_bleibt_in_der_datei_und_ist_kein_fehler() {
        let pfad = tmp("bleibt", "json");
        let _ = std::fs::remove_file(&pfad);
        let inhalt = r#"{"A1":"alsa_output.gibtsnicht"}"#;
        std::fs::write(&pfad, inhalt).unwrap();
        let mut map = DeviceMap::load(&pfad);

        let teil = devices_wiederherstellen(&mut map, &Testzugriff::aus_dump(DUMP));
        assert_eq!(teil.gesetzt, 0);
        assert!(teil.fehler.is_empty(), "kein Fehler: {:?}", teil.fehler);
        assert_eq!(teil.fehlt.len(), 1);
        // Der Eintrag steht unverändert in der Datei – nichts verworfen.
        assert_eq!(std::fs::read_to_string(&pfad).unwrap(), inhalt);
        assert_eq!(map.get("A1"), Some("alsa_output.gibtsnicht"));

        // Dasselbe für einen Streifen.
        let conf = tmp("bleibt", "conf");
        let _ = std::fs::remove_file(&conf);
        let zeile = "1=alsa_input.gibtsnicht\n";
        std::fs::write(&conf, zeile).unwrap();
        let teil = inputs_wiederherstellen(&Testzugriff::aus_dump(DUMP), &conf);
        assert_eq!(teil.gesetzt, 0);
        assert!(teil.fehler.is_empty(), "{:?}", teil.fehler);
        assert_eq!(teil.fehlt.len(), 1, "{:?}", teil.fehlt);
        assert!(teil.fehlt[0].contains("Streifen 1"), "{:?}", teil.fehlt);
        assert!(teil.fehlt[0].contains("abgesteckt"), "{:?}", teil.fehlt);
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), zeile);
        let _ = std::fs::remove_file(&pfad);
        let _ = std::fs::remove_file(&conf);
    }

    #[test]
    fn beim_start_wird_bis_drei_sekunden_erneut_geprueft() {
        // Das Gerät erscheint erst nach dem dritten Versuch („aufgeweckt“).
        let conf = tmp("geduld", "conf");
        let _ = std::fs::remove_file(&conf);
        std::fs::write(&conf, "1=alsa_input.usb-mic\n").unwrap();

        let zugriff = crate::pw::tests::Wandelzugriff::neu(Graph::parse(LEER).unwrap());
        let zaehler = std::cell::Cell::new(0);
        // Ein Zugriff, der beim dritten Aufruf das Gerät „ansteckt“.
        struct Spaet<'a> {
            inner: &'a crate::pw::tests::Wandelzugriff,
            runden: &'a std::cell::Cell<u32>,
            gepluggter: Graph,
        }
        impl crate::pw::Zugriff for Spaet<'_> {
            fn graph(&self) -> Result<Graph, String> {
                let n = self.runden.get() + 1;
                self.runden.set(n);
                if n >= 3 {
                    self.inner.setzen(self.gepluggter.clone());
                }
                self.inner.graph()
            }
            fn verbinden(&self, von: &str, nach: &str) -> Result<(), String> {
                self.inner.verbinden(von, nach)
            }
            fn trennen(&self, von: &str, nach: &str) -> Result<(), String> {
                self.inner.trennen(von, nach)
            }
        }
        let spaet = Spaet {
            inner: &zugriff,
            runden: &zaehler,
            gepluggter: Graph::parse(GEPLUGGT).unwrap(),
        };
        let teil = inputs_wiederherstellen_mit(
            &spaet,
            &conf,
            Duration::from_millis(400),
            Duration::from_millis(10),
        );
        assert_eq!(teil.gesetzt, 1, "{teil:?}");
        assert!(teil.fehlt.is_empty(), "{teil:?}");
        assert!(teil.fehler.is_empty(), "{teil:?}");
        assert!(zaehler.get() >= 3, "mehrfach geprueft: {}", zaehler.get());
        assert_eq!(
            std::fs::read_to_string(&conf).unwrap(),
            "1=alsa_input.usb-mic\n"
        );
        let _ = std::fs::remove_file(&conf);
    }

    #[test]
    fn ohne_geduld_wird_ein_fehlendes_geraet_sofort_gemeldet() {
        let conf = tmp("ohne-geduld", "conf");
        let _ = std::fs::remove_file(&conf);
        std::fs::write(&conf, "1=alsa_input.usb-mic\n").unwrap();
        let beginn = std::time::Instant::now();
        let teil = inputs_wiederherstellen(&Testzugriff::aus_dump(DUMP), &conf);
        assert!(beginn.elapsed() < Duration::from_millis(500), "kein Warten");
        assert_eq!(teil.fehlt.len(), 1, "{teil:?}");
        let _ = std::fs::remove_file(&conf);
    }
}
