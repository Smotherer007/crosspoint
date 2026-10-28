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
        let (fl, fr) = ports_for(src);
        runner.run("pw-link", &[&fl, &in_l])?;
        runner.run("pw-link", &[&fr, &in_r])?;
    }
    Ok(())
}

/// Zustand als JSON für die Oberfläche.
pub fn status_json(runner: &dyn Runner) -> String {
    let quellen = list_sources(runner).unwrap_or_default();
    let eintraege: Vec<String> = quellen
        .iter()
        .map(|s| {
            format!(
                "{{\"name\":{},\"monitor\":{}}}",
                crate::json::quote(&s.name),
                s.is_monitor
            )
        })
        .collect();
    format!(
        "{{\"sources\":[{}],\"strips\":{}}}",
        eintraege.join(","),
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
