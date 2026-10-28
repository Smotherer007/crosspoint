//! Kleiner JSON-Schreiber und -Leser, im Crate selbst.
//!
//! Warum eigener Code: das Projekt baut ohne Netz und ohne Fremd-Crates, damit
//! die Persistenz der Tastenbelegung offline reproduzierbar bleibt. Der Umfang
//! ist bewusst klein: Objekt, Array, Zeichenkette, Zahl, Wahrheitswert, null.
//! Kommentare, Tiefenbegrenzung und Unicode-Surrogatpaare werden nicht
//! unterstuetzt; `\uXXXX` wird als einzelnes Zeichen gelesen.

/// Ein JSON-Wert. Objekte behalten die Einfuegereihenfolge, damit die
/// geschriebene Datei bei gleichem Inhalt byte-gleich bleibt.
#[derive(Clone, Debug, PartialEq)]
pub enum Wert {
    Null,
    Wahr(bool),
    Zahl(f64),
    Text(String),
    Liste(Vec<Wert>),
    Objekt(Vec<(String, Wert)>),
}

impl Wert {
    /// Objekt aus Schluessel/Wert-Paaren bauen.
    pub fn objekt(paare: Vec<(&str, Wert)>) -> Wert {
        Wert::Objekt(paare.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn text(s: impl Into<String>) -> Wert {
        Wert::Text(s.into())
    }

    pub fn zahl(v: f64) -> Wert {
        Wert::Zahl(v)
    }

    /// Feld eines Objekts; `None` bei fehlendem Schluessel oder falschem Typ.
    pub fn get(&self, schluessel: &str) -> Option<&Wert> {
        match self {
            Wert::Objekt(paare) => paare.iter().find(|(k, _)| k == schluessel).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn als_text(&self) -> Option<&str> {
        match self {
            Wert::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn als_zahl(&self) -> Option<f64> {
        match self {
            Wert::Zahl(n) => Some(*n),
            Wert::Wahr(b) => Some(if *b { 1.0 } else { 0.0 }),
            // Zahlen als Text werden akzeptiert, damit handgeschriebene Dateien
            // ({"ms":"250"}) nicht scheitern.
            Wert::Text(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn als_liste(&self) -> Option<&Vec<Wert>> {
        match self {
            Wert::Liste(l) => Some(l),
            _ => None,
        }
    }

    pub fn to_json(&self) -> String {
        let mut aus = String::new();
        self.schreiben(&mut aus);
        aus
    }

    fn schreiben(&self, aus: &mut String) {
        match self {
            Wert::Null => aus.push_str("null"),
            Wert::Wahr(b) => aus.push_str(if *b { "true" } else { "false" }),
            Wert::Zahl(n) => {
                if !n.is_finite() {
                    // NaN und Unendlich sind in JSON nicht darstellbar.
                    aus.push('0');
                } else if n.fract() == 0.0 && n.abs() < 1e15 {
                    aus.push_str(&format!("{}", *n as i64));
                } else {
                    // Rusts kuerzeste Darstellung ist zugleich wieder einlesbar.
                    aus.push_str(&format!("{n}"));
                }
            }
            Wert::Text(s) => aus.push_str(&zitat(s)),
            Wert::Liste(l) => {
                aus.push('[');
                for (i, v) in l.iter().enumerate() {
                    if i > 0 {
                        aus.push(',');
                    }
                    v.schreiben(aus);
                }
                aus.push(']');
            }
            Wert::Objekt(paare) => {
                aus.push('{');
                for (i, (k, v)) in paare.iter().enumerate() {
                    if i > 0 {
                        aus.push(',');
                    }
                    aus.push_str(&zitat(k));
                    aus.push(':');
                    v.schreiben(aus);
                }
                aus.push('}');
            }
        }
    }
}

/// Zeichenkette in Anfuehrungszeichen setzen und maskieren.
pub fn zitat(s: &str) -> String {
    let mut aus = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => aus.push_str("\\\""),
            '\\' => aus.push_str("\\\\"),
            '\n' => aus.push_str("\\n"),
            '\r' => aus.push_str("\\r"),
            '\t' => aus.push_str("\\t"),
            c if (c as u32) < 0x20 => aus.push_str(&format!("\\u{:04x}", c as u32)),
            c => aus.push(c),
        }
    }
    aus.push('"');
    aus
}

/// Text einlesen. Die Fehlermeldungen nennen die Zeichenposition, weil die
/// Datei von Hand gepflegt wird.
pub fn parse(text: &str) -> Result<Wert, String> {
    let zeichen: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let wert = wert_lesen(&zeichen, &mut i)?;
    leerraum(&zeichen, &mut i);
    if i != zeichen.len() {
        return Err(format!("unerwarteter Rest ab Zeichen {i}"));
    }
    Ok(wert)
}

fn leerraum(z: &[char], i: &mut usize) {
    while *i < z.len() && z[*i].is_whitespace() {
        *i += 1;
    }
}

fn wert_lesen(z: &[char], i: &mut usize) -> Result<Wert, String> {
    leerraum(z, i);
    match z.get(*i) {
        None => Err("unerwartetes Ende der Eingabe".to_string()),
        Some('{') => {
            *i += 1;
            let mut paare = Vec::new();
            leerraum(z, i);
            if z.get(*i) == Some(&'}') {
                *i += 1;
                return Ok(Wert::Objekt(paare));
            }
            loop {
                leerraum(z, i);
                let schluessel = text_lesen(z, i)?;
                leerraum(z, i);
                if z.get(*i) != Some(&':') {
                    return Err(format!("':' erwartet bei Zeichen {i}"));
                }
                *i += 1;
                paare.push((schluessel, wert_lesen(z, i)?));
                leerraum(z, i);
                match z.get(*i) {
                    Some(',') => *i += 1,
                    Some('}') => {
                        *i += 1;
                        return Ok(Wert::Objekt(paare));
                    }
                    _ => return Err(format!("',' oder '}}' erwartet bei Zeichen {i}")),
                }
            }
        }
        Some('[') => {
            *i += 1;
            let mut liste = Vec::new();
            leerraum(z, i);
            if z.get(*i) == Some(&']') {
                *i += 1;
                return Ok(Wert::Liste(liste));
            }
            loop {
                liste.push(wert_lesen(z, i)?);
                leerraum(z, i);
                match z.get(*i) {
                    Some(',') => *i += 1,
                    Some(']') => {
                        *i += 1;
                        return Ok(Wert::Liste(liste));
                    }
                    _ => return Err(format!("',' oder ']' erwartet bei Zeichen {i}")),
                }
            }
        }
        Some('"') => Ok(Wert::Text(text_lesen(z, i)?)),
        Some('t') => wort(z, i, "true", Wert::Wahr(true)),
        Some('f') => wort(z, i, "false", Wert::Wahr(false)),
        Some('n') => wort(z, i, "null", Wert::Null),
        Some(_) => {
            let start = *i;
            while *i < z.len() && (z[*i].is_ascii_digit() || "+-.eE".contains(z[*i])) {
                *i += 1;
            }
            let roh: String = z[start..*i].iter().collect();
            roh.parse::<f64>()
                .map(Wert::Zahl)
                .map_err(|_| format!("keine Zahl bei Zeichen {start}: {roh}"))
        }
    }
}

fn wort(z: &[char], i: &mut usize, wort: &str, wert: Wert) -> Result<Wert, String> {
    let muster: Vec<char> = wort.chars().collect();
    if z.len() >= *i + muster.len() && z[*i..*i + muster.len()] == muster[..] {
        *i += muster.len();
        Ok(wert)
    } else {
        Err(format!("'{wort}' erwartet bei Zeichen {i}"))
    }
}

fn text_lesen(z: &[char], i: &mut usize) -> Result<String, String> {
    if z.get(*i) != Some(&'"') {
        return Err(format!("'\"' erwartet bei Zeichen {i}"));
    }
    *i += 1;
    let mut aus = String::new();
    while *i < z.len() {
        match z[*i] {
            '"' => {
                *i += 1;
                return Ok(aus);
            }
            '\\' => {
                *i += 1;
                match z.get(*i) {
                    Some('n') => aus.push('\n'),
                    Some('r') => aus.push('\r'),
                    Some('t') => aus.push('\t'),
                    Some('b') => aus.push('\u{8}'),
                    Some('f') => aus.push('\u{c}'),
                    Some('u') => {
                        let hex: String = z.get(*i + 1..*i + 5).unwrap_or(&[]).iter().collect();
                        if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                        {
                            aus.push(c);
                        }
                        *i += 4;
                    }
                    Some(c) => aus.push(*c),
                    None => return Err("Escape am Ende der Eingabe".to_string()),
                }
                *i += 1;
            }
            c => {
                aus.push(c);
                *i += 1;
            }
        }
    }
    Err("Zeichenkette nicht geschlossen".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rundlauf_verschachtelt() {
        let w = Wert::objekt(vec![
            ("name", Wert::text("Intro")),
            ("wert", Wert::zahl(1.5)),
            (
                "liste",
                Wert::Liste(vec![Wert::Wahr(true), Wert::Null, Wert::zahl(-3.0)]),
            ),
        ]);
        let zurueck = parse(&w.to_json()).unwrap();
        assert_eq!(zurueck, w);
    }

    #[test]
    fn texte_werden_maskiert() {
        let s = "Zeile1\n\"zitat\"\\pfad";
        assert_eq!(parse(&zitat(s)).unwrap(), Wert::Text(s.to_string()));
    }

    #[test]
    fn ganzzahlen_bleiben_ganzzahlen() {
        assert_eq!(Wert::zahl(80.0).to_json(), "80");
        assert_eq!(Wert::zahl(-12.5).to_json(), "-12.5");
        // 0.1 muss exakt zurueckkommen, nicht als 0.100
        let zurueck = parse(&Wert::zahl(0.1).to_json()).unwrap();
        assert_eq!(zurueck.als_zahl(), Some(0.1));
    }

    #[test]
    fn kaputte_eingabe_meldet_fehler() {
        assert!(parse("{\"a\":}").is_err());
        assert!(parse("[1,2").is_err());
        assert!(parse("\"offen").is_err());
    }
}
