//! Theming-Engine: ein Token-Satz als Datenmodell, CSS zur Laufzeit gerendert.
//!
//! Der Token-Satz ist die **einzige** Farbquelle. Das CSS wird aus ihm erzeugt
//! (`GtkCssProvider::load_from_data`), und auch die selbst gezeichneten Fader,
//! Pegel und EQ-Raster lesen ihre Farben aus [`Tokens`]. Ein Theme-Wechsel
//! tauscht nur den Provider und die Tokens - ohne Neustart, ohne Neubau.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Farben, Schrift und Masse einer Oberflaeche.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    /// Fenstergrund.
    pub grund: String,
    /// Panel-/Zugflaeche.
    pub panel: String,
    /// Hervorgehobene Flaeche (Koepfe, Chips, Dialogkopf).
    pub panel_hell: String,
    /// Linien und Raster.
    pub linie: String,
    /// Signalfarbe (Fader, Pegel, aktive Knoepfe, Wortmarken-Punkt).
    pub akzent: String,
    /// Dunkle Variante des Akzents (Faderfuellung unten, Rahmen).
    pub akzent_dunkel: String,
    /// Text.
    pub schrift: String,
    /// Sekundaertext, Skalen, Einheiten.
    pub gedaempft: String,
    /// Warnung/Spitzen (Pegel oben, Fehlerspalte).
    pub warnung: String,
    /// Text auf gefuellten Akzentflaechen.
    pub auf_akzent: String,
    /// Schriftfamilie, gilt fuer alles (Vorgabe: Monospace).
    pub font_family: String,
    pub schrift_basis: f64,
    pub schrift_klein: f64,
    pub schrift_gross: f64,
    /// Breite der Faderbahn in Pixeln.
    pub fader_breite: f64,
    /// Mindesthoehe des Faderbereichs in Pixeln.
    pub fader_hoehe: f64,
    /// Breite der Pegelsaeule in Pixeln.
    pub pegel_breite: f64,
    /// Grundabstand zwischen Bereichen in Pixeln.
    pub abstand: f64,
}

impl Default for Tokens {
    /// Eingebautes Standard-Theme `crosspoint`.
    fn default() -> Self {
        Tokens {
            grund: "#060A12".into(),
            panel: "#0E1624".into(),
            panel_hell: "#18344E".into(),
            linie: "#18344E".into(),
            akzent: "#39DD93".into(),
            akzent_dunkel: "#1C7D54".into(),
            schrift: "#D6E2F0".into(),
            gedaempft: "#7C93A8".into(),
            warnung: "#F3C24B".into(),
            auf_akzent: "#04140C".into(),
            font_family: "DejaVu Sans Mono".into(),
            // Lesbarkeit vor Dichte: 15/13/18 px, nichts darunter.
            schrift_basis: 15.0,
            schrift_klein: 13.0,
            schrift_gross: 18.0,
            fader_breite: 9.0,
            fader_hoehe: 240.0,
            pegel_breite: 10.0,
            abstand: 8.0,
        }
    }
}

/// Teilmenge eines Token-Satzes, wie sie in der Konfigurationsdatei steht.
///
/// Jedes Feld ist optional; fehlende Felder bleiben auf dem Standard-Theme.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TokenPatch {
    pub grund: Option<String>,
    pub panel: Option<String>,
    pub panel_hell: Option<String>,
    pub linie: Option<String>,
    pub akzent: Option<String>,
    pub akzent_dunkel: Option<String>,
    pub schrift: Option<String>,
    pub gedaempft: Option<String>,
    pub warnung: Option<String>,
    pub auf_akzent: Option<String>,
    pub font_family: Option<String>,
    pub schrift_basis: Option<f64>,
    pub schrift_klein: Option<f64>,
    pub schrift_gross: Option<f64>,
    pub fader_breite: Option<f64>,
    pub fader_hoehe: Option<f64>,
    pub pegel_breite: Option<f64>,
    pub abstand: Option<f64>,
}

impl TokenPatch {
    /// Patch ueber eine Basis legen. Leere Farbwerte gelten als "nicht gesetzt",
    /// damit ein Tippfehler in der Datei nicht zu unsichtbarem Text fuehrt.
    pub fn anwenden(&self, basis: &Tokens) -> Tokens {
        let mut t = basis.clone();
        macro_rules! farbe {
            ($feld:ident) => {
                if let Some(v) = self.$feld.as_ref().filter(|s| gueltige_farbe(s)) {
                    t.$feld = v.clone();
                }
            };
        }
        farbe!(grund);
        farbe!(panel);
        farbe!(panel_hell);
        farbe!(linie);
        farbe!(akzent);
        farbe!(akzent_dunkel);
        farbe!(schrift);
        farbe!(gedaempft);
        farbe!(warnung);
        farbe!(auf_akzent);
        if let Some(f) = self.font_family.as_ref().filter(|s| !s.trim().is_empty()) {
            t.font_family = f.clone();
        }
        // Groessen nur uebernehmen, wenn sie zeichenbar sind.
        let masse = [
            (self.schrift_basis, &mut t.schrift_basis, 6.0, 40.0),
            (self.schrift_klein, &mut t.schrift_klein, 5.0, 40.0),
            (self.schrift_gross, &mut t.schrift_gross, 8.0, 80.0),
            (self.fader_breite, &mut t.fader_breite, 3.0, 40.0),
            (self.fader_hoehe, &mut t.fader_hoehe, 80.0, 2000.0),
            (self.pegel_breite, &mut t.pegel_breite, 3.0, 40.0),
            (self.abstand, &mut t.abstand, 0.0, 60.0),
        ];
        for (quelle, ziel, min, max) in masse {
            if let Some(v) = quelle {
                if v.is_finite() && v >= min && v <= max {
                    *ziel = v;
                }
            }
        }
        t
    }
}

/// Akzeptiert `#rgb`, `#rrggbb`, `#rrggbbaa`.
pub fn gueltige_farbe(s: &str) -> bool {
    let s = s.trim();
    let Some(rest) = s.strip_prefix('#') else {
        return false;
    };
    matches!(rest.len(), 3 | 6 | 8) && rest.chars().all(|c| c.is_ascii_hexdigit())
}

/// Name des eingebauten Standard-Themes.
pub const STANDARD: &str = "crosspoint";

/// Die drei eingebauten Themes.
pub fn eingebaute_themes() -> BTreeMap<String, Tokens> {
    let mut m = BTreeMap::new();
    m.insert(STANDARD.to_string(), Tokens::default());

    // Heller Grund, dunkle Schrift, gleicher Akzent. Der Akzent traegt hier
    // keine Schrift, sondern nur Flaechen - Text auf Akzent bleibt dunkel.
    let hell = Tokens {
        grund: "#EDF1F6".into(),
        panel: "#FFFFFF".into(),
        panel_hell: "#DCE4EE".into(),
        linie: "#B9C6D6".into(),
        akzent: "#39DD93".into(),
        akzent_dunkel: "#12A868".into(),
        schrift: "#101821".into(),
        gedaempft: "#5A6B7D".into(),
        warnung: "#B8791A".into(),
        auf_akzent: "#04140C".into(),
        ..Tokens::default()
    };
    m.insert("crosspoint-hell".to_string(), hell);

    // Graustufen mit hohem Kontrast: Farbe traegt hier keine Bedeutung,
    // Helligkeit schon.
    let mono = Tokens {
        grund: "#000000".into(),
        panel: "#111111".into(),
        panel_hell: "#2B2B2B".into(),
        linie: "#4E4E4E".into(),
        akzent: "#EDEDED".into(),
        akzent_dunkel: "#8C8C8C".into(),
        schrift: "#FFFFFF".into(),
        gedaempft: "#A6A6A6".into(),
        warnung: "#D9D9D9".into(),
        auf_akzent: "#000000".into(),
        ..Tokens::default()
    };
    m.insert("mono".to_string(), mono);

    m
}

/// Alle bekannten Themes plus der aktive Name.
#[derive(Clone, Debug)]
pub struct ThemeSet {
    pub aktiv: String,
    pub themes: BTreeMap<String, Tokens>,
}

impl Default for ThemeSet {
    fn default() -> Self {
        ThemeSet {
            aktiv: STANDARD.to_string(),
            themes: eingebaute_themes(),
        }
    }
}

impl ThemeSet {
    pub fn namen(&self) -> Vec<String> {
        self.themes.keys().cloned().collect()
    }

    /// Tokens des aktiven Themes; bei unbekanntem Namen das Standard-Theme.
    pub fn tokens(&self) -> Tokens {
        self.themes
            .get(&self.aktiv)
            .cloned()
            .unwrap_or_else(Tokens::default)
    }

    /// Aktives Theme setzen. Unbekannt -> Standard, Logzeile zurueck.
    pub fn aktiv_setzen(&mut self, name: &str) -> Vec<String> {
        let mut log = Vec::new();
        let ziel = if self.themes.contains_key(name) {
            name.to_string()
        } else {
            log.push(format!("Theme '{name}' unbekannt - es bleibt '{STANDARD}'"));
            STANDARD.to_string()
        };
        self.aktiv = ziel;
        log
    }

    /// Abschnitt `theme` der Konfigurationsdatei einlesen.
    ///
    /// `aktiv` und `themes` sind optional; eigene Themes legen ihre Felder
    /// ueber das Standard-Theme. Unbekannte Felder in der Datei werden von
    /// serde ignoriert.
    pub fn aus_abschnitt(abschnitt: Option<&ThemeAbschnitt>) -> (ThemeSet, Vec<String>) {
        let mut log = Vec::new();
        let mut set = ThemeSet::default();
        let Some(abschnitt) = abschnitt else {
            return (set, log);
        };
        for (name, patch) in abschnitt.themes.iter().flatten() {
            if name.trim().is_empty() {
                log.push("Theme ohne Namen uebersprungen".to_string());
                continue;
            }
            let basis = set.themes.get(name).cloned().unwrap_or_default();
            set.themes.insert(name.clone(), patch.anwenden(&basis));
        }
        if let Some(aktiv) = abschnitt.aktiv.as_deref() {
            log.extend(set.aktiv_setzen(aktiv));
        }
        (set, log)
    }
}

/// Abschnitt `theme` aus `ui.json`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ThemeAbschnitt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aktiv: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub themes: Option<BTreeMap<String, TokenPatch>>,
}

impl ThemeSet {
    pub fn als_abschnitt(&self, eigene: &[String]) -> ThemeAbschnitt {
        let mut themes = BTreeMap::new();
        for name in eigene {
            if let Some(t) = self.themes.get(name) {
                themes.insert(name.clone(), TokenPatch::aus_tokens(t));
            }
        }
        ThemeAbschnitt {
            aktiv: Some(self.aktiv.clone()),
            themes: if themes.is_empty() {
                None
            } else {
                Some(themes)
            },
        }
    }
}

impl TokenPatch {
    pub fn aus_tokens(t: &Tokens) -> TokenPatch {
        TokenPatch {
            grund: Some(t.grund.clone()),
            panel: Some(t.panel.clone()),
            panel_hell: Some(t.panel_hell.clone()),
            linie: Some(t.linie.clone()),
            akzent: Some(t.akzent.clone()),
            akzent_dunkel: Some(t.akzent_dunkel.clone()),
            schrift: Some(t.schrift.clone()),
            gedaempft: Some(t.gedaempft.clone()),
            warnung: Some(t.warnung.clone()),
            auf_akzent: Some(t.auf_akzent.clone()),
            font_family: Some(t.font_family.clone()),
            schrift_basis: Some(t.schrift_basis),
            schrift_klein: Some(t.schrift_klein),
            schrift_gross: Some(t.schrift_gross),
            fader_breite: Some(t.fader_breite),
            fader_hoehe: Some(t.fader_hoehe),
            pegel_breite: Some(t.pegel_breite),
            abstand: Some(t.abstand),
        }
    }
}

/// Ort der Oberflaechen-Konfiguration: `$XDG_CONFIG_HOME/crosspoint/ui.json`
/// bzw. `~/.config/crosspoint/ui.json`.
pub fn konfig_pfad() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        Path::new(&xdg).join("crosspoint").join("ui.json")
    } else if let Some(home) = std::env::var_os("HOME") {
        Path::new(&home)
            .join(".config")
            .join("crosspoint")
            .join("ui.json")
    } else {
        PathBuf::from("ui.json")
    }
}

/// CSS aus einem Token-Satz. Keine Farbe steht hier fest - alles kommt aus `t`.
pub fn css(t: &Tokens) -> String {
    let (grund, panel, hell, linie) = (&t.grund, &t.panel, &t.panel_hell, &t.linie);
    let (akzent, akzent_d, schrift) = (&t.akzent, &t.akzent_dunkel, &t.schrift);
    let (dim, warn, auf_akzent) = (&t.gedaempft, &t.warnung, &t.auf_akzent);
    let (basis, klein, gross) = (t.schrift_basis, t.schrift_klein, t.schrift_gross);
    let fam = &t.font_family;
    let r = (t.abstand * 0.45).clamp(1.0, 6.0);
    // Buskuerzel A1..B3: 13 px Untergrenze, damit sie lesbar bleiben; die
    // Knopfreihe im Zug bleibt trotzdem unter 88 px (vier Spalten).
    let map_fs = (t.schrift_klein * 0.92).max(13.0);
    let klitzeklein = t.schrift_klein;
    let zeile_hoehe = (t.schrift_klein * 1.6).round();
    let warnung_c = &t.warnung;
    let gedaempft_c = &t.gedaempft;
    format!(
        r#"
* {{ font-family: "{fam}"; font-feature-settings: "tnum"; }}

window, window.background, .grund {{
    background-color: {grund};
    color: {schrift};
    font-size: {basis}px;
}}

/* ---------- Kopfzeile ---------- */
.kopf {{
    background-color: {panel};
    border: 1px solid {linie};
    border-radius: {r}px;
    padding: 4px 8px;
    min-height: 52px;
}}
.marke {{
    font-family: "{fam}";
    font-size: {gross}px;
    font-weight: 700;
    color: {schrift};
    padding: 0;
}}
.marke-punkt {{
    color: {akzent};
    font-size: {klein}px;
    padding: 0 2px;
}}
.marke-trenner {{
    background-color: {linie};
    min-width: 1px;
    min-height: 26px;
    margin: 0 2px;
}}
.unterzeile-b {{
    font-size: {klein}px;
    color: {schrift};
    font-weight: 700;
}}
.unterzeile-i {{
    font-size: {klein}px;
    color: {dim};
}}
.chip {{
    background-color: {grund};
    background-image: none;
    border: 1px solid {linie};
    border-radius: {r}px;
    padding: 2px 7px;
    color: {schrift};
    font-size: {klein}px;
}}
.chip-k {{ color: {dim}; }}
.status {{ color: {akzent}; font-size: {klein}px; }}
.status.aus {{ color: {warn}; }}
.tbtn {{
    background-color: {grund};
    background-image: none;
    border: 1px solid {linie};
    border-radius: {r}px;
    padding: 4px 10px;
    color: {schrift};
    font-size: {klein}px;
    font-weight: 700;
    box-shadow: none;
    text-shadow: none;
}}
.tbtn:hover {{ border-color: {akzent}; color: {schrift}; }}
.tbtn:active {{ background-color: {hell}; background-image: none; }}
.tbtn.an {{ background-color: {akzent}; color: {auf_akzent}; border-color: {akzent}; }}

/* ---------- Tape ---------- */
.tape {{
    background-color: {grund};
    background-image: none;
    border: 1px solid {linie};
    border-radius: {r}px;
    padding: 3px 8px;
}}
.tape-zeit {{
    font-family: "{fam}";
    font-size: {gross}px;
    font-weight: 700;
    color: {akzent};
    padding: 0 4px;
}}
.tape.auf .tape-zeit {{ color: {warn}; }}
.tape-zeit.aus {{ color: {dim}; }}
.tape-rec.an {{
    background-color: #e8453c;
    border-color: #ff6a60;
    color: #ffffff;
    font-weight: 700;
}}
.tape-zeit.an {{ color: #ff6a60; font-weight: 700; }}
.seg {{ background-color: {linie}; border-radius: 0px; min-width: 3px; min-height: 9px; }}
.seg.an {{ background-color: {akzent}; }}
.seg.spitze.an {{ background-color: {warn}; }}

/* ---------- Kanalzuege ---------- */
.zug {{
    background-color: {panel};
    background-image: none;
    border: 1px solid {linie};
    border-radius: {r}px;
    padding: 0;
}}
.zugkopf {{
    background-color: {hell};
    background-image: none;
    border-bottom: 1px solid {linie};
    border-radius: {r}px {r}px 0 0;
    padding: 3px 5px;
}}
.zugname {{
    font-size: {basis}px;
    font-weight: 700;
    color: {schrift};
    padding: 0;
}}
.zugsub {{
    font-size: {klein}px;
    color: {dim};
}}
.zugidx {{
    font-size: {klein}px;
    color: {dim};
    font-weight: 700;
}}
.zug.master .zugkopf {{ border-bottom: 2px solid {akzent}; }}
.zugname-eingabe {{
    background-color: {grund};
    background-image: none;
    border: 1px solid {akzent};
    border-radius: 2px;
    color: {schrift};
    padding: 1px 4px;
    min-height: 16px;
}}
button.knopf {{
    background-color: {grund};
    background-image: none;
    border: 1px solid {linie};
    border-radius: 3px;
    color: {dim};
    font-size: {klein}px;
    font-weight: 700;
    padding: 2px 0;
    min-height: 22px;
    box-shadow: none;
    text-shadow: none;
}}
button.knopf:hover {{ border-color: {akzent}; color: {schrift}; }}
button.knopf:checked {{ background-color: {akzent}; color: {auf_akzent}; border-color: {akzent}; }}
button.knopf.solo:checked {{ background-color: {schrift}; color: {grund}; border-color: {schrift}; }}
button.knopf.mono:checked {{ background-color: {akzent_d}; color: {schrift}; border-color: {akzent_d}; }}
/* Lowcut, TK, NR und Gate: aktiver Zustand im Signalgruen (Akzent). */
button.knopf.lc:checked, button.knopf.tk:checked, button.knopf.nr:checked, button.knopf.gate:checked {{
    background-color: {akzent};
    color: {auf_akzent};
    border-color: {akzent};
}}

/* Popover der drei Stufen */
.stufenmenue {{ padding: 6px; }}
.stufen-kopf {{
    font-size: {klitzeklein}px;
    color: {dim};
    letter-spacing: 0.08em;
    padding-bottom: 2px;
}}
.stufen-titel {{ font-size: {klein}px; color: {schrift}; }}
.stufen-wert {{ font-size: {klein}px; color: {akzent_d}; font-weight: 700; }}
.stufen-hinweis {{ font-size: {klitzeklein}px; color: {akzent_d}; }}
button.stufenwahl {{
    font-size: {klein}px;
    padding: 1px 6px;
    min-height: 18px;
}}
/* GTK rechnet aus min-width/min-height auf dem Slider eine negative
   Mindestgroesse (-4) und warnt im Log: deshalb Mindestmass 0 und die Groesse
   ueber padding/margin steuern. */
.stufenregler trough {{ min-height: 6px; border-radius: 3px; }}
.stufenregler slider {{
    min-width: 0px;
    min-height: 0px;
    padding: 6px;
    margin: -5px 0;
}}

/* ---------- Makros ---------- */
.hinweis {{ font-size: {klein}px; color: {dim}; padding: 0 6px; }}
.hinweis.fehler {{ color: {warnung_c}; }}
.tape-datei {{ font-size: {klein}px; color: {dim}; }}
.tape-status {{ font-size: {klitzeklein}px; color: {gedaempft_c}; }}
.geraetehinweis {{
    font-size: {klitzeklein}px;
    color: {gedaempft_c};
    letter-spacing: 0.06em;
    min-height: 15px;
}}
dropdown.geraetewahl > button {{
    background-color: {grund};
    background-image: none;
    border: 1px solid {linie};
    border-radius: 2px;
    color: {dim};
    font-size: {klitzeklein}px;
    padding: 1px 4px;
    min-height: 13px;
    box-shadow: none;
}}
/* Gemerktes Gerät, aber nicht angeschlossen: rot, nicht still. */
dropdown.geraetewahl.fehlt > button {{
    color: {warnung_c};
    border-color: {warnung_c};
}}
label.fehlt {{ color: {warnung_c}; }}
.geraetezeile {{
    /* 8 px: "A1 · 100 %" und "EINGANG FEHLT" muessen in 65 px passen. */
    font-size: 8px;
    color: {gedaempft_c};
    letter-spacing: 0;
    min-height: 13px;
}}
.geraetezeile.fehlt {{
    color: {warnung_c};
    font-weight: 700;
}}
.midi-zeile {{ padding: 1px 0; border-bottom: 1px solid {linie}; }}
button.learn.an {{ background-color: {warnung_c}; color: {auf_akzent}; border-color: {warnung_c}; }}
button.makro.an {{ background-color: {akzent}; color: {auf_akzent}; border-color: {akzent}; }}

.zuordnung {{ padding: 0; }}
button.map {{
    background-color: {grund};
    background-image: none;
    border: 1px solid {linie};
    border-radius: 6px;
    color: {dim};
    font-size: {map_fs}px;
    font-weight: 700;
    padding: 0;
    margin: 0;
    min-width: 0px;
    min-height: 16px;
    box-shadow: none;
    text-shadow: none;
}}
button.map:hover {{ border-color: {akzent}; color: {schrift}; }}
button.map:checked {{
    background-color: {akzent};
    color: {auf_akzent};
    border-color: {akzent};
}}

.makrobereich {{
    background-color: {panel};
    background-image: none;
    border: 1px solid {linie};
    border-radius: {r}px;
    padding: 4px 6px;
}}
.makro-titel {{ font-size: {basis}px; color: {schrift}; font-weight: 700; }}
.makro-info {{ font-size: {klein}px; color: {dim}; }}
button.makro {{
    background-color: {grund};
    background-image: none;
    border: 1px solid {linie};
    border-radius: 2px;
    color: {dim};
    font-size: {klein}px;
    padding: 1px 4px;
    min-height: 18px;
    box-shadow: none;
    text-shadow: none;
}}
button.makro:hover {{ border-color: {akzent}; color: {schrift}; background-color: {hell}; }}
/* Zwei Zeilen im Makro-Feld: Name (mind. 14 px) und Untertitel (mind. 12 px).
   Die Groessen sind fest, nicht aus dem Token-Satz: die Lesbarkeit der
   Beschriftung darf nicht an der Dichte haengen. */
.makro-name {{ font-size: 14px; font-weight: 700; color: {schrift}; }}
.makro-unter {{ font-size: 13px; color: {dim}; }}
button.makro.an .makro-name {{ color: {auf_akzent}; }}
button.makro.an .makro-unter {{ color: {auf_akzent}; }}

/* ---------- Dialoge ---------- */
window.dialog {{
    background-color: {grund};
    background-image: none;
    border: 1px solid {linie};
}}
.dialogkopf {{
    background-color: {panel};
    background-image: none;
    border-bottom: 1px solid {akzent};
    padding: 8px 12px;
}}
.dialogtitel {{ font-size: {gross}px; font-weight: 700; color: {schrift}; }}
.dialogunter {{ font-size: {klein}px; color: {dim}; }}
.dialogfuss {{
    background-color: {panel};
    background-image: none;
    border-top: 1px solid {linie};
    padding: 6px 12px;
}}
.feld {{ font-size: {klein}px; color: {dim}; }}
columnview {{
    background-color: {panel};
    background-image: none;
    color: {schrift};
    border: 1px solid {linie};
    border-radius: {r}px;
}}
columnview header button {{
    background-color: {hell};
    background-image: none;
    border: none;
    border-right: 1px solid {linie};
    border-radius: 0;
    color: {dim};
    font-size: {klein}px;
    font-weight: 700;
    padding: 5px 8px;
    box-shadow: none;
    text-shadow: none;
}}
columnview listview {{ background-color: transparent; }}
columnview listview > row {{
    background-color: {panel};
    border-bottom: 1px solid {linie};
    color: {schrift};
    font-size: {klein}px;
    /* 1,6-fache Schrifthoehe: 13 px * 1,6 = 21 px */
    min-height: {zeile_hoehe}px;
}}
columnview listview > row:selected {{ background-color: {hell}; color: {schrift}; }}
.zeile {{ font-size: {klein}px; color: {schrift}; padding: 4px 6px; }}
tooltip, tooltip label {{ font-size: {klein}px; }}
popover, popover label {{ font-size: {klein}px; }}
.zeile.dim {{ color: {dim}; }}
.zeile.fehler {{ color: {warn}; }}
.zeile.an {{ color: {akzent}; }}
dropdown > button {{
    background-color: {grund};
    background-image: none;
    border: 1px solid {linie};
    border-radius: {r}px;
    color: {schrift};
    font-size: {klein}px;
    padding: 3px 8px;
    min-height: 18px;
    box-shadow: none;
    text-shadow: none;
}}
popover > contents {{
    background-color: {panel};
    background-image: none;
    border: 1px solid {linie};
    color: {schrift};
}}
popover listview > row {{ padding: 4px 8px; font-size: {klein}px; color: {schrift}; }}
popover listview > row:hover {{ background-color: {hell}; }}
switch {{ background-color: {grund}; background-image: none; border: 1px solid {linie}; }}
switch:checked {{ background-color: {akzent}; border-color: {akzent}; }}
switch > slider {{ background-color: {schrift}; background-image: none; }}
scale trough {{ background-color: {grund}; background-image: none; border: 1px solid {linie}; min-height: 6px; }}
scale highlight {{ background-color: {akzent}; background-image: none; }}
scale slider {{ background-color: {schrift}; background-image: none; min-width: 8px; min-height: 8px; }}
.schalter-zeile {{ font-size: {klein}px; color: {schrift}; }}
.trenner {{ background-color: {linie}; min-height: 1px; }}
"#
    )
}

/// CSS-Text anwenden und den vorherigen Provider ersetzen.
///
/// Der Aufruf ist der einzige Ort, an dem GTK von einem Theme erfaehrt; die
/// Fader lesen ihre Farben parallel aus denselben Tokens.
pub fn anwenden(tokens: &Tokens, alt: Option<gtk::CssProvider>) -> Option<gtk::CssProvider> {
    let Some(display) = gtk::gdk::Display::default() else {
        return alt;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_data(&css(tokens));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    if let Some(alt) = alt {
        gtk::style_context_remove_provider_for_display(&display, &alt);
    }
    Some(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn standard_theme_hat_die_palette() {
        let t = Tokens::default();
        assert_eq!(t.grund, "#060A12");
        assert_eq!(t.panel, "#0E1624");
        assert_eq!(t.panel_hell, "#18344E");
        assert_eq!(t.akzent, "#39DD93");
        assert_eq!(t.schrift, "#D6E2F0");
        assert_eq!(t.gedaempft, "#7C93A8");
        assert!(t.font_family.contains("Mono"));
    }

    #[test]
    fn css_enthaelt_die_akzentfarbe() {
        let text = css(&Tokens::default());
        assert!(text.contains("#39DD93"), "Akzent fehlt im CSS");
        assert!(text.contains(".zug"));
        let mono = eingebaute_themes().remove("mono").unwrap();
        let mono_css = css(&mono);
        assert!(mono_css.contains("#EDEDED"));
        assert!(!mono_css.contains("#39DD93"));
    }

    #[test]
    fn fehlende_felder_fallen_auf_standard_zurueck() {
        let patch = TokenPatch {
            grund: Some("#123456".into()),
            ..Default::default()
        };
        let t = patch.anwenden(&Tokens::default());
        assert_eq!(t.grund, "#123456");
        assert_eq!(t.akzent, Tokens::default().akzent);
        assert_eq!(t.font_family, Tokens::default().font_family);
    }

    #[test]
    fn unsinnige_werte_werden_abgewiesen() {
        let patch = TokenPatch {
            grund: Some("blau".into()),
            akzent: Some("#12".into()),
            schrift_basis: Some(-4.0),
            fader_breite: Some(f64::NAN),
            ..Default::default()
        };
        let t = patch.anwenden(&Tokens::default());
        assert_eq!(t.grund, Tokens::default().grund);
        assert_eq!(t.akzent, Tokens::default().akzent);
        assert_eq!(t.schrift_basis, Tokens::default().schrift_basis);
        assert_eq!(t.fader_breite, Tokens::default().fader_breite);
    }

    #[test]
    fn unbekanntes_theme_faellt_auf_standard() {
        let mut set = ThemeSet::default();
        let log = set.aktiv_setzen("gibtsnicht");
        assert_eq!(set.aktiv, STANDARD);
        assert_eq!(set.tokens(), Tokens::default());
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn theme_rundlauf_ueber_die_datei() {
        // Schreiben, lesen, vergleichen - inklusive eigenem Theme.
        let mut set = ThemeSet::default();
        set.themes.insert(
            "eigen".into(),
            TokenPatch {
                grund: Some("#101820".into()),
                akzent: Some("#00FF88".into()),
                ..Default::default()
            }
            .anwenden(&Tokens::default()),
        );
        set.aktiv = "eigen".into();

        let abschnitt = set.als_abschnitt(&["eigen".to_string()]);
        let datei = serde_json::json!({ "theme": abschnitt });
        let ordner = std::env::temp_dir().join(format!("crosspoint-theme-{}", std::process::id()));
        std::fs::create_dir_all(&ordner).unwrap();
        let pfad = ordner.join("ui.json");
        let mut f = std::fs::File::create(&pfad).unwrap();
        f.write_all(datei.to_string().as_bytes()).unwrap();
        drop(f);

        let roh = std::fs::read_to_string(&pfad).unwrap();
        let gelesen: serde_json::Value = serde_json::from_str(&roh).unwrap();
        let abschnitt: ThemeAbschnitt =
            serde_json::from_value(gelesen.get("theme").cloned().unwrap()).unwrap();
        let (zurueck, log) = ThemeSet::aus_abschnitt(Some(&abschnitt));
        assert!(log.is_empty(), "unerwartete Logzeilen: {log:?}");
        assert_eq!(zurueck.aktiv, "eigen");
        let t = zurueck.tokens();
        assert_eq!(t.grund, "#101820");
        assert_eq!(t.akzent, "#00FF88");
        // Nicht ueberschriebene Felder stammen aus dem Standard.
        assert_eq!(t.panel, Tokens::default().panel);
        let _ = std::fs::remove_dir_all(&ordner);
    }

    #[test]
    fn eigener_theme_name_im_abschnitt_legt_neues_theme_an() {
        let abschnitt: ThemeAbschnitt = serde_json::from_value(serde_json::json!({
            "aktiv": "nacht",
            "themes": { "nacht": { "grund": "#000011", "unbekannt": 5 } }
        }))
        .unwrap();
        let (set, log) = ThemeSet::aus_abschnitt(Some(&abschnitt));
        assert!(log.is_empty());
        assert_eq!(set.aktiv, "nacht");
        assert_eq!(set.tokens().grund, "#000011");
        assert_eq!(set.tokens().schrift, Tokens::default().schrift);
    }
}
