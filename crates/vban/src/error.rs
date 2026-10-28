//! Fehlertypen des VBAN-Crates.
//!
//! Alle Fehler tragen den abgelehnten Wert mit, damit ein Aufrufer die Ursache
//! ohne erneutes Parsen nachvollziehen kann. `std::io::Error` wird ueber
//! [`VbanError::Io`] durchgereicht, sodass Sender/Empfaenger nur einen
//! Fehlertyp nach aussen zeigen.

use std::fmt;
use std::io;

use crate::header::{AudioFormat, SubProtocol};

/// Fehler beim Kodieren, Dekodieren oder Versenden von VBAN-Paketen.
#[derive(Debug)]
pub enum VbanError {
    /// Der Puffer ist kuerzer als der VBAN-Header.
    TooShort { got: usize, need: usize },
    /// Die ersten vier Bytes sind nicht `VBAN`.
    BadMagic([u8; 4]),
    /// Sub-Protokoll-Byte entspricht keinem bekannten Wert.
    UnknownSubProtocol(u8),
    /// Sample-Rate-Code (Bits 0..=2 von Format_SR) ist unbekannt.
    UnknownSampleRateCode(u8),
    /// Die gewuenschte Abtastrate hat keinen Code in der Spezifikation.
    UnknownSampleRateHertz(u32),
    /// Format-Code (Bits 3..=5 von Format_SR) ist unbekannt.
    UnknownFormatCode(u8),
    /// Format ist definiert, aber (noch) nicht implementiert (gepackte 12/10 Bit).
    UnsupportedFormat(AudioFormat),
    /// Codec-Byte ist nicht 0x00 (PCM); komprimierte Codecs sind nicht implementiert.
    UnsupportedCodec(u8),
    /// Stream-Name ist leer, zu lang oder enthaelt Nicht-ASCII.
    InvalidStreamName(String),
    /// Kanalzahl ausserhalb 1..=256.
    InvalidChannels(u16),
    /// Das Reserve-Byte ist ungleich 0 (wird nicht auf die Leitung geschrieben).
    ReservedNotZero(u8),
    /// Nutzdaten passen nicht zum Format (kein Vielfaches von Kanal * Sample-Bytes).
    BadPayloadLength { len: usize, block: usize },
    /// Paket gehoert zu einem anderen Sub-Protokoll als erwartet.
    WrongSubProtocol {
        expected: SubProtocol,
        got: SubProtocol,
    },
    /// Sample-Typ passt nicht zum Format des Frames (z. B. f32 in PCM-16-Frame).
    FormatMismatch {
        expected: AudioFormat,
        got: AudioFormat,
    },
    /// Anzahl uebergebener Samples ist kein Vielfaches der Kanalzahl.
    OddSampleCount { samples: usize, channels: u16 },
    /// Fehler der Socket-Schicht.
    Io(io::Error),
}

impl fmt::Display for VbanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VbanError::TooShort { got, need } => {
                write!(f, "Paket zu kurz: {got} Bytes, mindestens {need} noetig")
            }
            VbanError::BadMagic(b) => write!(f, "kein VBAN-Paket, Magic = {b:?}"),
            VbanError::UnknownSubProtocol(b) => {
                write!(f, "unbekanntes Sub-Protokoll 0x{b:02X}")
            }
            VbanError::UnknownSampleRateCode(c) => {
                write!(f, "unbekannter Sample-Rate-Code {c} (Bits 0..=2 von Format_SR)")
            }
            VbanError::UnknownSampleRateHertz(hz) => {
                write!(f, "Abtastrate {hz} Hz ist im VBAN-Code nicht vorgesehen")
            }
            VbanError::UnknownFormatCode(c) => {
                write!(f, "unbekannter Format-Code {c} (Bits 3..=5 von Format_SR)")
            }
            VbanError::UnsupportedFormat(fmt) => {
                write!(f, "Format {fmt:?} ist nicht implementiert")
            }
            VbanError::UnsupportedCodec(b) => {
                write!(f, "Codec 0x{b:02X} wird nicht unterstuetzt (nur 0x00 = PCM)")
            }
            VbanError::InvalidStreamName(name) => {
                write!(f, "ungueltiger Stream-Name {name:?} (1..=15 druckbare ASCII-Zeichen)")
            }
            VbanError::InvalidChannels(n) => {
                write!(f, "ungueltige Kanalzahl {n} (erlaubt 1..=256)")
            }
            VbanError::ReservedNotZero(v) => {
                write!(f, "Reserve-Byte muss 0 sein, ist aber {v}")
            }
            VbanError::BadPayloadLength { len, block } => write!(
                f,
                "Nutzdatenlaenge {len} Bytes ist kein Vielfaches von {block} Bytes pro Sample-Frame"
            ),
            VbanError::WrongSubProtocol { expected, got } => {
                write!(f, "Sub-Protokoll {got:?} statt {expected:?}")
            }
            VbanError::FormatMismatch { expected, got } => {
                write!(f, "Sample-Format {got:?} statt {expected:?}")
            }
            VbanError::OddSampleCount { samples, channels } => write!(
                f,
                "{samples} Samples sind kein Vielfaches der Kanalzahl {channels}"
            ),
            VbanError::Io(e) => write!(f, "Socket-Fehler: {e}"),
        }
    }
}

impl std::error::Error for VbanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VbanError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for VbanError {
    fn from(e: io::Error) -> Self {
        VbanError::Io(e)
    }
}
