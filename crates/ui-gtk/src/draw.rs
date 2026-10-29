//! Zeichenhelfer auf `GtkSnapshot`.
//!
//! Alles nimmt Farben als Parameter - hier steht keine Farbe fest, die Quelle
//! ist immer der Token-Satz (`theme::Tokens`). Dadurch zeichnen Fader, Pegel
//! und EQ-Raster im jeweils aktiven Theme.

use gtk::gdk;
use gtk::graphene;
use gtk::gsk;
use gtk::pango;
use gtk::prelude::*;

/// `#rrggbb` (auch `#rgb`, `#rrggbbaa`) -> RGBA. Unbekanntes wird sichtbar
/// grau statt unsichtbar transparent.
pub fn farbe(hex: &str) -> gdk::RGBA {
    gdk::RGBA::parse(hex.trim()).unwrap_or(gdk::RGBA::new(0.5, 0.5, 0.5, 1.0))
}

/// Kopie mit anderer Deckkraft.
pub fn alpha(c: &gdk::RGBA, a: f32) -> gdk::RGBA {
    let mut c = *c;
    c.set_alpha(a.clamp(0.0, 1.0));
    c
}

pub fn rechteck(x: f32, y: f32, w: f32, h: f32) -> graphene::Rect {
    graphene::Rect::new(x, y, w.max(0.0), h.max(0.0))
}

/// Gefuellte Flaeche (scharfe Kanten).
pub fn flaeche(snap: &gtk::Snapshot, r: graphene::Rect, c: &gdk::RGBA) {
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return;
    }
    snap.append_color(c, &r);
}

/// Gefuellte Flaeche mit runden Ecken.
pub fn flaeche_rund(snap: &gtk::Snapshot, r: graphene::Rect, radius: f32, c: &gdk::RGBA) {
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return;
    }
    let rr = gsk::RoundedRect::from_rect(r, radius.max(0.0));
    snap.push_rounded_clip(&rr);
    snap.append_color(c, &r);
    snap.pop();
}

/// Rahmen (nur Linie) mit runden Ecken.
pub fn rahmen(snap: &gtk::Snapshot, r: graphene::Rect, radius: f32, breite: f32, c: &gdk::RGBA) {
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return;
    }
    let rr = gsk::RoundedRect::from_rect(r, radius.max(0.0));
    snap.append_border(&rr, &[breite; 4], &[*c; 4]);
}

/// Waagerechte Linie.
pub fn waagerecht(snap: &gtk::Snapshot, x: f32, y: f32, w: f32, d: f32, c: &gdk::RGBA) {
    flaeche(snap, rechteck(x, y, w, d), c);
}

/// Senkrechte Linie.
pub fn senkrecht(snap: &gtk::Snapshot, x: f32, y: f32, h: f32, d: f32, c: &gdk::RGBA) {
    flaeche(snap, rechteck(x, y, d, h), c);
}

/// Text zeichnen. `breite == 0` bedeutet: keine Umbruchbreite, kein Kuerzen.
///
/// Die Groesse ist in Geraetepixeln (absolute Pango-Groesse), damit die
/// Monospace-Zeilen exakt zu den gezeichneten Kaesten passen.
#[allow(clippy::too_many_arguments)]
pub fn text(
    snap: &gtk::Snapshot,
    widget: &impl IsA<gtk::Widget>,
    s: &str,
    x: f32,
    y: f32,
    breite: f32,
    groesse: f32,
    fett: bool,
    familie: &str,
    c: &gdk::RGBA,
) {
    let layout = widget.create_pango_layout(Some(s));
    let mut fd = pango::FontDescription::new();
    fd.set_family(familie);
    fd.set_absolute_size(groesse as f64 * pango::SCALE as f64);
    fd.set_weight(if fett {
        pango::Weight::Bold
    } else {
        pango::Weight::Normal
    });
    layout.set_font_description(Some(&fd));
    if breite > 0.0 {
        layout.set_width((breite * pango::SCALE as f32) as i32);
        layout.set_ellipsize(pango::EllipsizeMode::End);
    }
    snap.save();
    snap.translate(&graphene::Point::new(x, y));
    snap.append_layout(&layout, c);
    snap.restore();
}

/// Wie [`text`], aber rechtsbuendig innerhalb `[x, x+breite]`.
#[allow(clippy::too_many_arguments)]
pub fn text_rechts(
    snap: &gtk::Snapshot,
    widget: &impl IsA<gtk::Widget>,
    s: &str,
    x: f32,
    y: f32,
    breite: f32,
    groesse: f32,
    fett: bool,
    familie: &str,
    c: &gdk::RGBA,
) {
    let layout = widget.create_pango_layout(Some(s));
    let mut fd = pango::FontDescription::new();
    fd.set_family(familie);
    fd.set_absolute_size(groesse as f64 * pango::SCALE as f64);
    fd.set_weight(if fett {
        pango::Weight::Bold
    } else {
        pango::Weight::Normal
    });
    layout.set_font_description(Some(&fd));
    layout.set_width((breite * pango::SCALE as f32) as i32);
    layout.set_ellipsize(pango::EllipsizeMode::End);
    layout.set_alignment(pango::Alignment::Right);
    snap.save();
    snap.translate(&graphene::Point::new(x, y));
    snap.append_layout(&layout, c);
    snap.restore();
}

/// Wie [`text`], aber mittig.
#[allow(clippy::too_many_arguments)]
pub fn text_mitte(
    snap: &gtk::Snapshot,
    widget: &impl IsA<gtk::Widget>,
    s: &str,
    x: f32,
    y: f32,
    breite: f32,
    groesse: f32,
    fett: bool,
    familie: &str,
    c: &gdk::RGBA,
) {
    let layout = widget.create_pango_layout(Some(s));
    let mut fd = pango::FontDescription::new();
    fd.set_family(familie);
    fd.set_absolute_size(groesse as f64 * pango::SCALE as f64);
    fd.set_weight(if fett {
        pango::Weight::Bold
    } else {
        pango::Weight::Normal
    });
    layout.set_font_description(Some(&fd));
    layout.set_width((breite * pango::SCALE as f32) as i32);
    layout.set_alignment(pango::Alignment::Center);
    snap.save();
    snap.translate(&graphene::Point::new(x, y));
    snap.append_layout(&layout, c);
    snap.restore();
}
