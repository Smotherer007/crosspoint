//! crosspoint-engine – Einstiegspunkt ohne Fenster: JACK-Client, Parameter,
//! Steuer-API.
//!
//! Der Ablauf selbst steht seit dem Zusammenschluss in
//! [`crosspoint::lauf`]: dieselbe Funktion startet auch das Binary
//! `crosspoint`, dort mit Fenster im Hauptthread. Hier bleibt nur, was ein
//! Kopfloser braucht – Argumente lesen, starten, auf das Ende warten.

fn main() {
    let mut konfig = crosspoint::lauf::Konfig::vorgabe();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--client" => konfig.client_name = args.next().unwrap_or(konfig.client_name),
            "--listen" => konfig.listen = args.next().unwrap_or(konfig.listen),
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

    let lauf = match crosspoint::lauf::starte(konfig) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // Warten, bis der Aufpasser das Ende meldet (SIGINT/SIGTERM oder
    // Leerlauf). Danach raeumt `Lauf` beim Fallenlassen auf: OSC anhalten,
    // virtuelle Geraete entfernen, JACK-Client deaktivieren.
    lauf.warte_auf_ende();
    eprintln!("beende: virtuelle Geraete werden entfernt");
    drop(lauf);
}
