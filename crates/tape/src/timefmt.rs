//! Zeitstempel ohne externe Crates.
//!
//! Rechnet aus dem UNIX-Zeitstempel (UTC) ein Datum nach dem
//! zivilen Kalender um (`days_from_civil` nach Howard Hinnant).
//! Bewusst ohne `chrono`/`time`, damit das Crate abhaengigkeitsfrei bleibt.

use std::time::{SystemTime, UNIX_EPOCH};

/// Liefert (Sekunden seit UNIX_EPOCH, Zeitstempel "YYYYMMDD-HHMMSS").
///
/// Der Zeitstempel ist UTC und dateisystemfreundlich (keine Doppelpunkte).
pub fn now_epoch_and_stamp() -> (u64, String) {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (epoch, stamp_from_epoch(epoch))
}

/// "YYYYMMDD-HHMMSS" (UTC), z. B. "20261028-140312".
pub fn stamp_from_epoch(epoch: u64) -> String {
    let (y, mo, d, h, mi, s) = parts(epoch);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// "YYYY-MM-DDTHH:MM:SSZ" (UTC) fuer die Metadaten.
pub fn iso_from_epoch(epoch: u64) -> String {
    let (y, mo, d, h, mi, s) = parts(epoch);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// (Jahr, Monat, Tag, Stunde, Minute, Sekunde) in UTC.
fn parts(epoch: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (epoch / 86_400) as i64;
    let secs = (epoch % 86_400) as u32;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// Tage seit 1970-01-01 -> (Jahr, Monat, Tag). Gueltig fuer alle Jahre ab 1970.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Verschiebung des Epochen-Bezugspunkts auf den 1. Maerz 0000.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // Tag innerhalb der Era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153; // Monat, gezaehlt ab Maerz
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoche_ist_1970() {
        assert_eq!(iso_from_epoch(0), "1970-01-01T00:00:00Z");
        assert_eq!(stamp_from_epoch(0), "19700101-000000");
    }

    #[test]
    fn bekannter_zeitpunkt() {
        // 2024-02-29T12:34:56Z (Schaltjahr)
        assert_eq!(iso_from_epoch(1_709_210_096), "2024-02-29T12:34:56Z");
        assert_eq!(stamp_from_epoch(1_709_210_096), "20240229-123456");
    }

    #[test]
    fn zeitstempel_ist_dateisystemfreundlich() {
        let (epoch, stamp) = now_epoch_and_stamp();
        assert!(epoch > 1_600_000_000, "Uhr unplausibel: {epoch}");
        assert_eq!(stamp.len(), 15);
        assert!(!stamp.contains(':'));
    }
}
