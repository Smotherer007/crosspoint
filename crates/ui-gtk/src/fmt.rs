//! Zahlen- und Textformate der Oberflaeche, ohne GTK-Abhaengigkeit.
//!
//! Hier liegt alles, was sich ohne Fenster pruefen laesst: dB-Umrechnung,
//! Faderkennlinie und das Kuerzen von Beschriftungen. Die Tests stehen am Ende
//! der Datei und laufen ohne Display (`cargo test`).

/// Untere Anzeigegrenze. Alles darunter gilt als Stille und wird als -60 dB
/// gezeichnet (die Fader-Skala endet dort).
pub const DB_MIN: f32 = -60.0;
/// Obere Anzeigegrenze der Fader (entspricht dem Clamp in `engine::set_strip`).
pub const DB_MAX: f32 = 12.0;

/// Amplitudenfaktor -> Dezibel. 0 und winzige Werte ergeben [`DB_MIN`].
pub fn lin_to_db(v: f32) -> f32 {
    if !(v > 0.0) {
        return DB_MIN;
    }
    let db = 20.0 * v.log10();
    if db.is_finite() {
        db.max(DB_MIN)
    } else {
        DB_MIN
    }
}

/// Dezibel -> Amplitudenfaktor (Gegenrichtung zu [`lin_to_db`]).
#[allow(dead_code)]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Faderposition 0..1 fuer einen dB-Wert (0 unten, 1 oben).
pub fn fader_pos(db: f32) -> f32 {
    ((db.clamp(DB_MIN, DB_MAX) - DB_MIN) / (DB_MAX - DB_MIN)).clamp(0.0, 1.0)
}

/// Umkehrung von [`fader_pos`].
pub fn pos_db(pos: f32) -> f32 {
    DB_MIN + pos.clamp(0.0, 1.0) * (DB_MAX - DB_MIN)
}

/// Pegeltext mit einer Nachkommastelle; unterhalb der Skala `-oo`.
pub fn db_text(db: f32) -> String {
    if db <= DB_MIN + 0.05 {
        "-oo".to_string()
    } else {
        format!("{db:.1}")
    }
}

/// Rastet einen Faderwert auf 0 dB ein, laesst aber ueberall sonst 0,1-dB-Schritte.
///
/// * `|v| <= ZONE`         -> genau 0,0 (der Regler springt in die Raste)
/// * `vorher == 0.0` und   -> bleibt 0,0, bis man deutlich weiter zieht
///   `|v| < HYSTERESE`        (sonst zittert der Wert an der Raste)
/// * sonst                 -> auf 0,1 dB gerundet
///
/// Die Hysterese ist der Unterschied zwischen „rastet ein" und „klebt":
/// 0 dB ist leicht zu treffen, aber 0,1 dB daneben bleibt erreichbar.
pub fn einrasten(v: f32, vorher: f32) -> f32 {
    const ZONE: f32 = 0.35;
    const HYSTERESE: f32 = 0.65;
    if v.abs() <= ZONE || (vorher == 0.0 && v.abs() < HYSTERESE) {
        0.0
    } else {
        (v * 10.0).round() / 10.0
    }
}

/// Verstaerkungstext mit Vorzeichen (`+3.0`, `-6.5`, `0.0`).
pub fn gain_text(db: f32) -> String {
    if db.abs() < 0.05 {
        "0.0".to_string()
    } else {
        format!("{db:+.1}")
    }
}

/// Beschriftung auf `max` Zeichen kuerzen, letztes Zeichen wird `…`.
///
/// Gezaehlt werden Zeichen, nicht Bytes: Umlaute und CJK brechen nicht mitten
/// im Codepoint. `max == 0` ergibt einen leeren Text.
pub fn kurz(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let anzahl = text.chars().count();
    if anzahl <= max {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max - 1).collect();
    s.push('…');
    s
}

/// Frequenz eines EQ-Bandes lesbar machen (1000 -> "1.0k", 100 -> "100").
pub fn hz_text(hz: f32) -> String {
    if hz >= 1000.0 {
        format!("{:.1}k", hz / 1000.0)
    } else {
        format!("{:.0}", hz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn einheit_ist_null_db() {
        assert!((db_to_lin(0.0) - 1.0).abs() < 1e-6);
        assert!(lin_to_db(1.0).abs() < 1e-6);
    }

    #[test]
    fn halbe_amplitude_ist_minus_sechs_db() {
        // 0.5 entspricht -6.0206 dB; die Anzeige rundet auf eine Stelle.
        assert!((db_to_lin(-6.0206) - 0.5).abs() < 1e-4);
        assert!((lin_to_db(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(db_text(lin_to_db(0.5)), "-6.0");
    }

    #[test]
    fn stille_bleibt_an_der_untergrenze() {
        assert_eq!(lin_to_db(0.0), DB_MIN);
        assert_eq!(lin_to_db(-1.0), DB_MIN);
        assert_eq!(db_text(DB_MIN), "-oo");
        assert_eq!(db_text(-59.96), "-oo");
    }

    #[test]
    fn faderkennlinie_ist_umkehrbar() {
        for db in [-60.0, -36.0, -12.0, 0.0, 6.0, 12.0] {
            let pos = fader_pos(db);
            assert!((pos_db(pos) - db).abs() < 1e-3, "db={db} pos={pos}");
            assert!((0.0..=1.0).contains(&pos));
        }
        assert_eq!(fader_pos(-120.0), 0.0);
        assert_eq!(fader_pos(40.0), 1.0);
    }

    #[test]
    fn beschriftung_bleibt_unter_der_grenze() {
        assert_eq!(kurz("HW In 1", 24), "HW In 1");
        assert_eq!(kurz("HW In 1", 7), "HW In 1");
        assert_eq!(kurz("HW In 1", 4), "HW …");
        assert_eq!(kurz("", 4), "");
        assert_eq!(kurz("irgendwas", 0), "");
    }

    #[test]
    fn beschriftung_zaehlt_zeichen_nicht_bytes() {
        // Umlaute sind zwei Bytes; die Kuerzung darf nicht mitten im Zeichen
        // landen (sonst 0xFF im Text).
        let s = kurz("Mikrofon Größe", 8);
        assert_eq!(s, "Mikrofo…");
        assert_eq!(s.chars().count(), 8);
        assert!(s.is_char_boundary(s.len()));
        assert_eq!(kurz("Größe", 5), "Größe");
    }

    #[test]
    fn gain_text_hat_vorzeichen() {
        assert_eq!(gain_text(0.0), "0.0");
        assert_eq!(gain_text(-0.02), "0.0");
        assert_eq!(gain_text(3.0), "+3.0");
        assert_eq!(gain_text(-6.25), "-6.2");
    }

    #[test]
    fn frequenz_wird_gekuerzt() {
        assert_eq!(hz_text(100.0), "100");
        assert_eq!(hz_text(1000.0), "1.0k");
        assert_eq!(hz_text(12500.0), "12.5k");
    }
}

#[cfg(test)]
mod rast_tests {
    use super::einrasten;

    #[test]
    fn einrasten_trifft_die_null() {
        assert_eq!(einrasten(0.2, 3.0), 0.0);
        assert_eq!(einrasten(-0.3, -6.0), 0.0);
        assert_eq!(einrasten(0.34, 1.0), 0.0);
    }

    #[test]
    fn einrasten_haelt_die_null_fest() {
        // einmal auf 0, dann bleibt es 0 bis deutlich weiter gezogen wird
        assert_eq!(einrasten(0.5, 0.0), 0.0);
        assert_eq!(einrasten(-0.64, 0.0), 0.0);
        assert_ne!(einrasten(0.7, 0.0), 0.0);
    }

    #[test]
    fn einrasten_laesst_feine_schritte_zu() {
        assert_eq!(einrasten(0.1, 5.0), 0.0); // 0,1 dB liegt in der Raste
        assert_eq!(einrasten(0.8, 5.0), 0.8);
        assert_eq!(einrasten(1.23, 5.0), 1.2);
        assert_eq!(einrasten(-6.44, 5.0), -6.4);
        assert_eq!(einrasten(-11.98, 5.0), -12.0);
    }
}
