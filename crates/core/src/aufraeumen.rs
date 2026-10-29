//! Deterministisches Aufraeumen der virtuellen Geraete.
//!
//! Der regulaere Weg ist [`crate::lauf::wire`] mit `down`: das Skript entlaedt
//! die Module aus seiner Merkliste (`$XDG_RUNTIME_DIR/crosspoint/modules`).
//! Das allein reicht nicht, denn
//!
//!   * das Skript kann fehlen, scheitern oder - relativ aufgerufen - gar nicht
//!     gefunden werden,
//!   * nach einem Absturz oder `kill -9` gibt es keine Merkliste mehr,
//!   * ein zweiter Pfad (Aufpasser-Thread) kann Geraete wieder anlegen,
//!     waehrend das Skript sie gerade entfernt.
//!
//! Deshalb arbeitet der Sweep **ohne Merkliste**: er liest den Ist-Zustand aus
//! PulseAudio/PipeWire und entfernt alles, was `crosspoint_` heisst - Module
//! einzeln, Knoten einzeln, in zwei Runden. Fehler werden gemeldet, aber der
//! Sweep laeuft weiter; am Ende wird der Rest nachgezaehlt.
//!
//! Bewusst nicht angefasst wird der JACK-Client `crosspoint` selbst (ohne
//! Unterstrich): den haelt der Prozess, er verschwindet mit dem Client.

use std::process::Command;

/// Namensanfang aller Geraete, die zu Crosspoint gehoeren.
pub const PRAEFIX: &str = "crosspoint_";

/// Was der Sweep geschafft hat.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bilanz {
    pub module: usize,
    pub knoten: usize,
    pub fehler: Vec<String>,
}

impl Bilanz {
    pub fn gesamt(&self) -> usize {
        self.module + self.knoten
    }

    /// Eine Zeile fuer das Protokoll, z. B.
    /// `Aufraeumen (Fenster zu): 5 Module, 5 Knoten entfernt, 0 Fehler`.
    pub fn zeile(&self, anlass: &str) -> String {
        format!(
            "Aufraeumen ({anlass}): {} Module, {} Knoten entfernt, {} Fehler",
            self.module,
            self.knoten,
            self.fehler.len()
        )
    }
}

/// Module aus `pactl list short modules`, die zu Crosspoint gehoeren.
///
/// Zeilenformat: `Index\tName\tArgumente\t`. Erkannt wird am Argument
/// (`sink_name=crosspoint_in_1`, `source_name=crosspoint_mic_B1`), weil der
/// Modulname selbst generisch ist (`module-null-sink`). `vm8_` sind Geraete
/// der Vorgaengerversion, die sonst als Leiche liegen bleiben.
pub fn module_ids(ausgabe: &str) -> Vec<(u32, String)> {
    let mut ids = Vec::new();
    for zeile in ausgabe.lines() {
        let felder: Vec<&str> = zeile.split('\t').collect();
        if felder.len() < 2 {
            continue;
        }
        let Ok(id) = felder[0].trim().parse::<u32>() else {
            continue;
        };
        let rest = felder[1..].join(" ");
        if rest.contains(PRAEFIX) || rest.contains("vm8_") {
            ids.push((id, felder[1].to_string()));
        }
    }
    ids
}

/// Knoten aus `pw-cli list-objects Node`, die zu Crosspoint gehoeren.
///
/// Format je Objekt:
/// ```text
///     id 46, type PipeWire:Interface:Node/3
///         node.name = "crosspoint_in_1"
/// ```
/// Die Id steht im Kopf des Blocks, der Name irgendwo darunter.
pub fn knoten_ids(ausgabe: &str) -> Vec<(u32, String)> {
    let mut ids = Vec::new();
    let mut aktuell: Option<u32> = None;
    let mut treffer = false;
    let abschliessen =
        |ids: &mut Vec<(u32, String)>, id: Option<u32>, treffer: bool, name: &str| {
            if let (Some(id), true) = (id, treffer) {
                ids.push((id, name.to_string()));
            }
        };
    let mut name = String::new();
    for zeile in ausgabe.lines() {
        let z = zeile.trim();
        if z.starts_with("id ") && z.contains(", type ") {
            // Neuer Block: den vorigen abschliessen.
            abschliessen(&mut ids, aktuell, treffer, &name);
            let id = z
                .trim_start_matches("id ")
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .parse::<u32>()
                .ok();
            aktuell = id;
            treffer = false;
            name.clear();
            continue;
        }
        if let Some(wert) = z.strip_prefix("node.name = ") {
            let wert = wert.trim().trim_matches('"');
            if wert.starts_with(PRAEFIX) {
                treffer = true;
                name = wert.to_string();
            }
        }
    }
    abschliessen(&mut ids, aktuell, treffer, &name);
    ids
}

fn ausfuehren(programm: &str, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new(programm)
        .args(args)
        .output()
        .map_err(|e| format!("{programm} nicht ausfuehrbar: {e}"))
}

/// Ist-Bestand: Anzahl (Module, Knoten) mit `crosspoint_` im Namen.
///
/// Liest frisch aus PulseAudio/PipeWire. Nicht verfuegbare Werkzeuge zaehlen
/// als 0 - die Aussage "nichts mehr da" waere sonst nicht pruefbar.
pub fn reste() -> (usize, usize) {
    let module = ausfuehren("pactl", &["list", "short", "modules"])
        .map(|o| module_ids(&String::from_utf8_lossy(&o.stdout)).len())
        .unwrap_or(0);
    let knoten = ausfuehren("pw-cli", &["list-objects", "Node"])
        .map(|o| knoten_ids(&String::from_utf8_lossy(&o.stdout)).len())
        .unwrap_or(0);
    (module, knoten)
}

/// Raeumt alle virtuellen Geraete ab - unabhaengig von der Merkliste.
///
/// Reihenfolge: Module entladen (das entfernt die meisten Knoten mit), dann
/// uebrig gebliebene Knoten einzeln zerstoeren. Zwei Runden, weil ein Modul
/// beim Entladen noch einen Knoten nachschieben kann. Fehler werden gesammelt
/// und gemeldet, brechen den Sweep aber nicht ab.
pub fn sweep(anlass: &str) -> Bilanz {
    let mut b = Bilanz::default();
    // Doppelte Nennungen in zwei Runden vermeiden: was schon entfernt ist,
    // taucht in der zweiten Runde nicht mehr auf.
    for _runde in 0..2 {
        let module = ausfuehren("pactl", &["list", "short", "modules"])
            .map(|o| module_ids(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default();
        for (id, name) in module {
            let id_text = id.to_string();
            match ausfuehren("pactl", &["unload-module", &id_text]) {
                Ok(o) if o.status.success() => {
                    b.module += 1;
                    eprintln!("  Modul {id} entladen ({name})");
                }
                Ok(o) => b.fehler.push(format!(
                    "pactl unload-module {id}: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => b.fehler.push(e),
            }
        }

        let knoten = ausfuehren("pw-cli", &["list-objects", "Node"])
            .map(|o| knoten_ids(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or_default();
        for (id, name) in knoten {
            let id_text = id.to_string();
            match ausfuehren("pw-cli", &["destroy", &id_text]) {
                Ok(o) if o.status.success() => {
                    b.knoten += 1;
                    eprintln!("  Knoten {id} zerstoert ({name})");
                }
                Ok(o) => b.fehler.push(format!(
                    "pw-cli destroy {id} ({name}): {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                )),
                Err(e) => b.fehler.push(e),
            }
        }
    }

    let (module, knoten) = reste();
    for f in &b.fehler {
        eprintln!("  Fehler beim Aufraeumen: {f}");
    }
    eprintln!(
        "{}; Rest: {module} Module, {knoten} Knoten",
        b.zeile(anlass)
    );
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Echte Ausgabe von `pactl list short modules` (gekuerzt).
    const MODULE: &str = "27\tmodule-also-pcm\t\t\n\
32\tmodule-null-sink\tsink_name=crosspoint_in_1 sink_properties=device.description=CROSSPOINT-Input-1\t\n\
33\tmodule-null-sink\tsink_name=crosspoint_out_B1 sink_properties=device.description=CROSSPOINT-Output-B1\t\n\
34\tmodule-remap-source\tmaster=crosspoint_in_1.monitor source_name=crosspoint_mic_in_1\t\n\
35\tmodule-null-sink\tsink_name=vm8_in_1\t\n\
36\tmodule-loopback\tsource=alsa_input.pci-0000_00_1f.3.analog-stereo sink=alsa_output.pci-0000_03_00.1.hdmi-stereo\t\n";

    #[test]
    fn module_werden_am_argument_erkannt() {
        let ids = module_ids(MODULE);
        assert_eq!(
            ids.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![32, 33, 34, 35],
            "nur crosspoint_- und vm8_-Module, keine fremden"
        );
    }

    #[test]
    fn fremde_module_bleiben_unberuehrt() {
        let ids = module_ids(MODULE);
        assert!(!ids.iter().any(|(i, _)| *i == 27 || *i == 36));
    }

    #[test]
    fn kaputte_zeilen_werden_uebersprungen() {
        let roh = "\nkeine tabelle\nx\tmodule-null-sink\tsink_name=crosspoint_x\n\tmodule-null-sink\tsink_name=crosspoint_y\n";
        // Zeile ohne Zahl ist keine Modulzeile; die leere Id ebenso.
        assert!(module_ids(roh).is_empty());
    }

    /// Echte Ausgabe von `pw-cli list-objects Node` (gekuerzt).
    const KNOTEN: &str = "\tid 29, type PipeWire:Interface:Node/3\n\
 \t\tobject.serial = \"29\"\n\
 \t\tnode.name = \"Dummy-Driver\"\n\
\tid 46, type PipeWire:Interface:Node/3\n\
 \t\tobject.serial = \"46\"\n\
 \t\tnode.name = \"crosspoint_in_1\"\n\
\tid 47, type PipeWire:Interface:Node/3\n\
 \t\tobject.serial = \"47\"\n\
 \t\tnode.name = \"crosspoint_mic_B1\"\n\
\tid 48, type PipeWire:Interface:Node/3\n\
 \t\tobject.serial = \"48\"\n\
 \t\tnode.name = \"alsa_output.pci-0000_03_00.1.hdmi-stereo\"\n\
\tid 49, type PipeWire:Interface:Node/3\n\
 \t\tobject.serial = \"49\"\n\
 \t\tnode.name = \"crosspoint\"\n";

    #[test]
    fn knoten_werden_am_namen_erkannt() {
        let ids = knoten_ids(KNOTEN);
        assert_eq!(
            ids,
            vec![
                (46, "crosspoint_in_1".to_string()),
                (47, "crosspoint_mic_B1".to_string())
            ]
        );
    }

    #[test]
    fn eigener_client_und_fremde_knoten_bleiben() {
        let ids = knoten_ids(KNOTEN);
        // Der JACK-Client heisst `crosspoint` (ohne Unterstrich) und gehoert
        // dem Prozess - ihn darf der Sweep nicht zerstoeren.
        assert!(!ids.iter().any(|(i, _)| *i == 49 || *i == 29 || *i == 48));
    }

    #[test]
    fn bilanz_zaehlt_und_berichtet() {
        let b = Bilanz {
            module: 5,
            knoten: 8,
            fehler: vec!["x".into()],
        };
        assert_eq!(b.gesamt(), 13);
        assert_eq!(
            b.zeile("Fenster zu"),
            "Aufraeumen (Fenster zu): 5 Module, 8 Knoten entfernt, 1 Fehler"
        );
    }
}
