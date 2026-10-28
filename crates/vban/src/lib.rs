//! # vban – VBAN-Over-UDP
//!
//! Eigenstaendiges Crate (Rust 2021, nur `std`) fuer das VBAN-Protokoll von
//! VB-Audio: Header kodieren/dekodieren, Audio-Frames bauen und ueber UDP
//! senden/empfangen. Bewusst **keine** Abhaengigkeit auf `crosspoint` oder andere
//! Workspace-Typen – spaetere Integration als Workspace-Member ist damit
//! unkritisch.
//!
//! ## Header-Belegung (VBAN-Spezifikation)
//!
//! Grundlage ist die oeffentliche VBAN-Protokollspezifikation von VB-Audio
//! (VBANProtocol_Specifications.pdf). Header = **28 Byte**, Little Endian,
//! Nutzdaten folgen direkt danach:
//!
//! | Offset | Groesse | Feld | Belegung |
//! |-------:|--------:|------|----------|
//! | 0      | 4  | Magic | `VBAN` (ASCII) |
//! | 4      | 1  | Format_SR | Bits 0..4 Sample-Rate-Index, Bits 5..7 Sub-Protokoll (0x00 Audio, 0x20 Serial, 0x40 Text, 0x60 Service) |
//! | 5      | 1  | NUMSAMPLES | Samples pro Frame minus 1 (setzt `AudioFrame::encode`) |
//! | 6      | 1  | Channels-1 | Kanalzahl minus eins (1..=256 Kanaele) |
//! | 7      | 1  | Format_Bits | Bits 0..2 Datentyp, Bit 3 reserviert, Bits 4..7 Codec (0 = PCM) |
//! | 8      | 16 | Stream-Name | ASCII, NUL-gefuellt, max. 15 Zeichen + NUL |
//! | 24     | 4  | Frame-Zaehler | `u32` Little Endian, laeuft modulo 2^32 ueber |
//!
//! Sample-Rate-Indizes (Bits 0..4 von Byte 4): 0 = 6000, 1 = 12000, 2 = 24000,
//! 3 = 48000, 4 = 96000, 5 = 192000, 6 = 384000, 7 = 8000, 8 = 16000,
//! 9 = 32000, 10 = 64000, 11 = 128000, 12 = 256000, 13 = 512000, 14 = 11025,
//! 15 = 22050, 16 = 44100, 17 = 88200, 18 = 176400, 19 = 352800, 20 = 705600 Hz.
//!
//! Datentyp-Codes (Bits 0..2 von Byte 7): 0 = 8 Bit uint, 1 = 16 Bit int,
//! 2 = 24 Bit int, 3 = 32 Bit int, 4 = 32 Bit float, 5 = 64 Bit float,
//! 6 = 12 Bit int (gepackt), 7 = 10 Bit int (gepackt). Integer- und
//! Float-Samples stehen Little Endian und sind bei Float **nicht** auf [-1, 1]
//! normiert, sondern rohe IEEE-754-Bits.
//!
//! ## Annahmen und Grenzen (bewusst dokumentiert, nicht geraten)
//!
//! 1. **Ein Header ohne Nutzdaten** kann Byte 5 nicht sinnvoll fuellen; `pack`
//!    schreibt dort 0 und erst `AudioFrame::encode` den echten Wert
//!    (Samples pro Kanal minus 1). Das Feld ist **8 Bit** breit, also sind nur
//!    1..=256 Samples pro Kanal exakt darstellbar; laengere Frames werden beim
//!    Schreiben auf 255 gesaettigt, und der Empfaenger leitet die Laenge aus
//!    den Nutzdaten ab.
//! 2. **Bit 3 von Byte 7** ist laut Spezifikation reserviert. Es wird beim
//!    Schreiben auf 0 gesetzt und beim Lesen ignoriert (kein Ablehnen fremder
//!    Sender).
//! 3. **Bits 5..7 von Byte 4** (Sub-Protokoll) werden mit 0xE0 maskiert. Eine
//!    Maske 0xF0 waere falsch: ab Rate-Index 16 (44100 Hz) ist Bit 4 gesetzt
//!    und gehoert noch zur Abtastrate.
//! 4. **Codec 0x0 = PCM** ist implementiert. Alle anderen Codec-Werte
//!    (komprimierte VBAN-Codecs) werden mit `UnsupportedCodec` abgelehnt statt
//!    interpretiert – ihre Belegung ist hier nicht belegt.
//! 5. **12-/10-Bit-Formate** sind gepackt (kein ganzzahliges Bytes-pro-Sample)
//!    und nicht implementiert; sie werden nur erkannt und abgelehnt.
//! 6. **Stream-Name** wird auf 1..=15 druckbare ASCII-Zeichen geprueft
//!    (NUL-Abschluss noetig). Beim Lesen werden auch 16 Zeichen ohne NUL
//!    akzeptiert, weil manche Sender das Feld voll ausfuellen.
//! 7. **Sub-Protokolle** sind nur die vier spezifizierten implementiert
//!    (Audio, Serial, Text, Service); die uebrigen Belegungen der oberen drei
//!    Bit werden als `UnknownSubProtocol` abgelehnt.
//! 8. **Kein Payload-Framing**: das Sub-Protokoll Audio hat laut Spezifikation
//!    keinen eigenen Unterkopf; die Samples beginnen bei Offset 28. Text- und
//!    Serial-Frames werden zwar im Header erkannt, ihr Payload aber nicht
//!    interpretiert.
//!
//! ## Beispiel
//!
//! ```
//! use vban::{AudioFrame, SampleRate};
//!
//! let mut frame = AudioFrame::pcm16("MeinStream", SampleRate::Hz48000, 2)?;
//! frame.push_i16(&[0, 0, 100, -100])?;
//! frame.advance_counter();
//! let bytes = frame.encode()?;
//! assert_eq!(&bytes[0..4], b"VBAN");
//! assert_eq!(bytes.len(), 28 + 8);
//!
//! let back = AudioFrame::parse(&bytes)?;
//! assert_eq!(back, frame);
//! # Ok::<(), vban::VbanError>(())
//! ```

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod audio;
pub mod error;
pub mod header;
pub mod socket;

pub use crate::audio::AudioFrame;
pub use crate::error::VbanError;
pub use crate::header::{
    AudioFormat, Codec, Header, SampleRate, SubProtocol, HEADER_LEN, MAGIC, MAX_STREAM_NAME,
    STREAM_NAME_LEN,
};
pub use crate::socket::{Receiver, Sender, DEFAULT_PORT, DEFAULT_STREAM_NAME};
