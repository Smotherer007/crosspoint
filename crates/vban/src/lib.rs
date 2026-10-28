//! # vban – VBAN-Over-UDP
//!
//! Eigenstaendiges Crate (Rust 2021, nur `std`) fuer das VBAN-Protokoll von
//! VB-Audio: Header kodieren/dekodieren, Audio-Frames bauen und ueber UDP
//! senden/empfangen. Bewusst **keine** Abhaengigkeit auf `vm8` oder andere
//! Workspace-Typen – spaetere Integration als Workspace-Member ist damit
//! unkritisch.
//!
//! ## Header-Belegung (was belegt ist)
//!
//! Grundlage ist die oeffentliche VBAN-Protokollspezifikation von VB-Audio.
//! Header = **28 Byte**, Little Endian, Nutzdaten folgen direkt danach:
//!
//! | Offset | Groesse | Feld | Belegung |
//! |-------:|--------:|------|----------|
//! | 0      | 4  | Magic | `VBAN` (ASCII) |
//! | 4      | 16 | Stream-Name | ASCII, NUL-gefuellt, max. 15 Zeichen + NUL |
//! | 20     | 1  | Sub-Protokoll | 0x00 Audio, 0x10 Serial, 0x20 MIDI, 0x40 Chat/Text, 0x60 RT-Packet, 0x80 Service/User |
//! | 21     | 1  | Format_SR | Bits 0..=2 Sample-Rate-Code, Bits 3..=5 Format-Code, Bits 6..=7 unbenutzt |
//! | 22     | 1  | Channels-1 | Kanalzahl minus eins (1..=256 Kanäle) |
//! | 23     | 1  | Codec | 0x00 = PCM/unkomprimiert |
//! | 24     | 4  | Frame-Zaehler | `u32` Little Endian, laeuft modulo 2^32 ueber |
//!
//! Sample-Rate-Codes (Bits 0..=2): 0 = 6000, 1 = 12000, 2 = 24000, 3 = 48000,
//! 4 = 96000, 5 = 192000, 6 = 384000, 7 = 768000 Hz.
//!
//! Format-Codes (Bits 3..=5): 0 = 8 Bit uint, 1 = 16 Bit int, 2 = 24 Bit int,
//! 3 = 32 Bit int, 4 = 32 Bit float, 5 = 64 Bit float, 6 = 12 Bit int (gepackt),
//! 7 = 10 Bit int (gepackt). Integer- und Float-Samples stehen Little Endian und
//! sind bei Float **nicht** auf [-1, 1] normiert, sondern rohe IEEE-754-Bits.
//!
//! ## Annahmen und Grenzen (bewusst dokumentiert, nicht geraten)
//!
//! 1. **Reservebyte:** Die Aufgabe nennt zusaetzlich "1 Byte reserviert" am
//!    Header-Ende. Die oeffentliche Spezifikation hat dieses Byte **nicht** –
//!    dort beginnt nach dem Frame-Zaehler (Offset 24..28) bereits der Payload.
//!    `Header` fuehrt das Feld `reserved` (muss 0 sein), es wird aber nicht auf
//!    die Leitung geschrieben und `HEADER_LEN` bleibt 28, damit echte
//!    Gegenstellen (Voicemeeter, vban-cmd) die Pakete verstehen. Ein 29-Byte-
//!    Header waere nicht interoperabel.
//! 2. **Sample-Rate-Code 7 = 768 kHz** ist eine Annahme: aeltere Fassungen der
//!    Spezifikation fuehren Code 7 als "reserved". Wird Code 7 gesendet und die
//!    Gegenstelle kennt ihn nicht, ist die Rate dort undefiniert.
//! 3. **Bits 6..=7 von Format_SR** werden beim Schreiben auf 0 gesetzt und beim
//!    Lesen ignoriert (Annahme: unbenutzt/reserviert). Ein Paket mit gesetzten
//!    Bits wird daher nicht abgelehnt.
//! 4. **Codec 0x00 = PCM** ist implementiert. Alle anderen Codec-Werte
//!    (komprimierte VBAN-Codecs) werden mit `UnsupportedCodec` abgelehnt statt
//!    interpretiert – ihre Belegung ist hier nicht belegt.
//! 5. **12-/10-Bit-Formate** sind gepackt (kein ganzzahliges Bytes-pro-Sample)
//!    und nicht implementiert; sie werden nur erkannt und abgelehnt.
//! 6. **Stream-Name** wird auf 1..=15 druckbare ASCII-Zeichen geprueft
//!    (NUL-Abschluss noetig). Beim Lesen werden auch 16 Zeichen ohne NUL
//!    akzeptiert, weil manche Sender das Feld voll ausfuellen.
//! 7. **Untere 4 Bit des Sub-Protokoll-Bytes** werden beim Lesen ignoriert
//!    (Annahme: reserviert), beim Schreiben auf 0 gesetzt.
//! 8. **Kein Payload-Framing**: das Sub-Protokoll Audio hat laut Spezifikation
//!    keinen eigenen Unterkopf; die Samples beginnen bei Offset 28. RT-Packet
//!    und Text-Frames werden zwar im Header erkannt, ihr Payload aber nicht
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
