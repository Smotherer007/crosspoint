//! Eingangszuordnung für die Streifen 1..5 (Hardware-Eingänge).
//!
//! Auf Windows sind das WDM/KS/ASIO-Geräte; hier sind es PipeWire-Knoten.
//! Wichtig: echte Aufnahmegeräte haben die Ports `capture_FL/FR`, die Monitore
//! von Sinks dagegen `monitor_FL/FR` – und der Monitor heißt in pactl
//! `<sink>.monitor`, die Ports liegen aber am Sink selbst.

use std::process::Command;

pub const MAX_HW_STRIPS: usize = 5;

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub name: String,
    pub is_monitor: bool,
}

/// Führt ein Kommando aus; im Test durch eine Attrappe ersetzt.
pub trait Runner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String>;
}

pub struct RealRunner;

impl Runner for RealRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
        let out = Command::new(program)
            .args(args)
            .output()
            .map_err(|e| format!("{program}: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        } else {
            Err(format!("{program}: {}", String::from_utf8_lossy(&out.stderr)))
        }
    }
}

/// Portnamen für einen Quellknoten (FL, FR).
pub fn ports_for(source: &str) -> (String, String) {
    let basis = source.strip_suffix(".monitor").unwrap_or(source);
    if source.ends_with(".monitor") {
        (format!("{basis}:monitor_FL"), format!("{basis}:monitor_FR"))
    } else {
        (format!("{basis}:capture_FL"), format!("{basis}:capture_FR"))
    }
}

/// Streifen 1..5 → Portnamen des Mixers.
pub fn strip_ports(strip: usize) -> Result<(String, String, String, String), String> {
    if strip == 0 || strip > MAX_HW_STRIPS {
        return Err(format!("Streifen {strip} hat keinen Hardware-Eingang (1..{MAX_HW_STRIPS})"));
    }
    Ok((
        format!("vm8:strip{strip}_L"),
        format!("vm8:strip{strip}_R"),
        format!("vm8:strip{strip}_L"),
        format!("vm8:strip{strip}_R"),
    ))
}

/// Verfügbare Quellen aus `pactl list short sources`, ohne die eigenen Knoten.
pub fn list_sources(runner: &dyn Runner) -> Result<Vec<Source>, String> {
    let text = runner.run("pactl", &["list", "short", "sources"])?;
    let mut out = Vec::new();
    for line in text.lines() {
        let name = match line.split_whitespace().nth(1) {
            Some(n) => n.to_string(),
            None => continue,
        };
        if name.starts_with("vm8_") {
            continue; // eigene Ausgänge nicht als Eingang anbieten
        }
        out.push(Source {
            is_monitor: name.ends_with(".monitor"),
            name,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    Ok(out)
}

/// Alle Ausgangsports einer Quelle, so wie PipeWire sie gerade anbietet.
/// Nötig, weil z. B. das Corsair-Headset-Mikrofon nur `capture_MONO` hat -
/// ein blindes `capture_FL`/`capture_FR` schlägt dort fehl und der Streifen
/// bleibt stumm.
pub fn source_ports(runner: &dyn Runner, source: &str) -> Vec<String> {
    let basis = source.strip_suffix(".monitor").unwrap_or(source);
    let text = runner.run("pw-link", &["-o"]).unwrap_or_default();
    let mut ports: Vec<String> = text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|p| p.starts_with(&format!("{basis}:")))
        .collect();
    ports.sort();
    ports.dedup();
    ports
}

/// Verbindet Quellports mit den beiden Streifen-Eingängen. Mono-Quellen
/// werden auf beide Seiten gelegt, Stereo-Quellen paarweise.
pub fn link_source(runner: &dyn Runner, source: &str, in_l: &str, in_r: &str) -> Result<(), String> {
    let ports = source_ports(runner, source);
    let find = |suffix: &str| {
        ports
            .iter()
            .find(|p| p.ends_with(suffix))
            .cloned()
    };
    if let (Some(fl), Some(fr)) = (find(":capture_FL"), find(":capture_FR")) {
        runner.run("pw-link", &[&fl, in_l])?;
        runner.run("pw-link", &[&fr, in_r])?;
        return Ok(());
    }
    if let (Some(fl), Some(fr)) = (find(":monitor_FL"), find(":monitor_FR")) {
        runner.run("pw-link", &[&fl, in_l])?;
        runner.run("pw-link", &[&fr, in_r])?;
        return Ok(());
    }
    match ports.len() {
        0 => Err(format!("Quelle {source} hat keinen Ausgangsport")),
        1 => {
            runner.run("pw-link", &[&ports[0], in_l])?;
            runner.run("pw-link", &[&ports[0], in_r])?;
            Ok(())
        }
        _ => {
            runner.run("pw-link", &[&ports[0], in_l])?;
            runner.run("pw-link", &[&ports[1], in_r])?;
            Ok(())
        }
    }
}

/// Merkliste: welche Quelle an welchem Streifen hängt (überlebt Neustarts).
fn mapping_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".config/vm8-linux/inputs.conf")
}

pub fn mapping() -> [Option<String>; MAX_HW_STRIPS] {
    let mut out: [Option<String>; MAX_HW_STRIPS] = Default::default();
    if let Ok(text) = std::fs::read_to_string(mapping_path()) {
        for line in text.lines() {
            if let Some((n, src)) = line.split_once('=') {
                if let Ok(i) = n.trim().parse::<usize>() {
                    if (1..=MAX_HW_STRIPS).contains(&i) && !src.trim().is_empty() {
                        out[i - 1] = Some(src.trim().to_string());
                    }
                }
            }
        }
    }
    out
}

fn save_mapping(strip: usize, source: Option<&str>) {
    let mut m = mapping();
    m[strip - 1] = source.map(|s| s.to_string());
    let mut text = String::new();
    for (i, eintrag) in m.iter().enumerate() {
        if let Some(s) = eintrag {
            text.push_str(&format!("{}={}\n", i + 1, s));
        }
    }
    let p = mapping_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, text);
}

/// Verbindet einen Streifen mit einer Quelle (bzw. löst die Verbindung).
pub fn assign(
    runner: &dyn Runner,
    strip: usize,
    source: Option<&str>,
) -> Result<(), String> {
    let (in_l, in_r, _, _) = strip_ports(strip)?;

    // Alte Verbindungen lösen, egal wer dranhängt: alle Quellen abklemmen.
    let quellen = list_sources(runner).unwrap_or_default();
    for s in &quellen {
        let (fl, fr) = ports_for(&s.name);
        let _ = runner.run("pw-link", &["-d", &fl, &in_l]);
        let _ = runner.run("pw-link", &["-d", &fr, &in_r]);
    }

    if let Some(src) = source {
        link_source(runner, src, &in_l, &in_r)?;
    }
    save_mapping(strip, source);
    Ok(())
}

/// Zustand als JSON für die Oberfläche.
pub fn status_json(runner: &dyn Runner) -> String {
    let quellen = list_sources(runner).unwrap_or_default();
    let eintraege: Vec<String> = quellen
        .iter()
        .map(|s| {
            let zuordnung = mapping()
                .iter()
                .position(|m| m.as_deref() == Some(s.name.as_str()))
                .map(|i| (i + 1).to_string())
                .unwrap_or_else(|| "null".to_string());
            format!(
                "{{\"name\":{},\"monitor\":{},\"strip\":{}}}",
                crate::json::quote(&s.name),
                s.is_monitor,
                zuordnung
            )
        })
        .collect();
    let binds: Vec<String> = mapping()
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            m.as_ref()
                .map(|s| format!("{{\"strip\":{},\"source\":{}}}", i + 1, crate::json::quote(s)))
        })
        .collect();
    format!(
        "{{\"sources\":[{}],\"assigned\":[{}],\"strips\":{}}}",
        eintraege.join(","),
        binds.join(","),
        MAX_HW_STRIPS
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        ausgabe: String,
        aufrufe: RefCell<Vec<String>>,
    }

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[&str]) -> Result<String, String> {
            self.aufrufe
                .borrow_mut()
                .push(format!("{program} {}", args.join(" ")));
            if program == "pactl" {
                Ok(self.ausgabe.clone())
            } else if args.contains(&"-o") {
                // Portliste, wie pw-link -o sie liefert
                Ok("alsa_input.usb-mic:capture_FL\n\
                    alsa_input.usb-mic:capture_FR\n\
                    alsa_output.hdmi:monitor_FL\n\
                    alsa_output.hdmi:monitor_FR\n"
                    .to_string())
            } else {
                Ok(String::new())
            }
        }
    }

    fn fake() -> Fake {
        Fake {
            ausgabe: "52\talsa_input.usb-mic\tPipeWire\tfloat32le 2ch 48000Hz\tRUNNING\n\
                      53\talsa_output.hdmi.monitor\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n"
                .to_string(),
            aufrufe: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn quellen_werden_gelesen_und_eigene_gefiltert() {
        let f = fake();
        let q = list_sources(&f).unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].name, "alsa_input.usb-mic");
        assert!(!q[0].is_monitor);
        assert!(q[1].is_monitor);
    }

    #[test]
    fn eigene_vm8_knoten_werden_ausgeblendet() {
        let f = Fake {
            ausgabe: "60\tvm8_in_1.monitor\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n".to_string(),
            aufrufe: RefCell::new(Vec::new()),
        };
        assert!(list_sources(&f).unwrap().is_empty());
    }

    #[test]
    fn portnamen_unterscheiden_quelle_und_monitor() {
        assert_eq!(
            ports_for("alsa_input.usb-mic"),
            ("alsa_input.usb-mic:capture_FL".into(), "alsa_input.usb-mic:capture_FR".into())
        );
        assert_eq!(
            ports_for("alsa_output.hdmi.monitor"),
            ("alsa_output.hdmi:monitor_FL".into(), "alsa_output.hdmi:monitor_FR".into())
        );
    }

    #[test]
    fn zulaessige_streifen() {
        assert!(strip_ports(1).is_ok());
        assert!(strip_ports(5).is_ok());
        assert!(strip_ports(0).is_err());
        assert!(strip_ports(6).is_err());
    }

    #[test]
    fn assign_verbindet_und_loest_alte_verbindung() {
        let f = fake();
        assign(&f, 2, Some("alsa_input.usb-mic")).unwrap();
        let aufrufe = f.aufrufe.borrow().join("\n");
        assert!(aufrufe.contains("pw-link alsa_input.usb-mic:capture_FL vm8:strip2_L"));
        assert!(aufrufe.contains("pw-link alsa_input.usb-mic:capture_FR vm8:strip2_R"));
        assert!(aufrufe.contains("pw-link -d alsa_output.hdmi:monitor_FL vm8:strip2_L"));
    }

    #[test]
    fn assign_ohne_quelle_loest_nur() {
        let f = fake();
        assign(&f, 3, None).unwrap();
        let aufrufe = f.aufrufe.borrow().join("\n");
        assert!(!aufrufe.contains("pw-link alsa_input"));
        assert!(aufrufe.contains("pw-link -d"));
    }
}
