//! VBAN-Header: Kodieren (pack) und Dekodieren (unpack) der 28 Byte, die jedes
//! VBAN-Paket einleiten.
//!
//! Die Feldbelegung folgt der oeffentlichen VBAN-Spezifikation von VB-Audio
//! (siehe Moduldoku in `lib.rs` fuer die Tabelle und die getroffenen Annahmen).

use crate::error::VbanError;

/// Magic der ersten vier Bytes.
pub const MAGIC: [u8; 4] = *b"VBAN";
/// Laenge des Headers in Bytes; die Nutzdaten beginnen direkt danach.
pub const HEADER_LEN: usize = 28;
/// Groesse des Stream-Namen-Feldes (NUL-gefuellt).
pub const STREAM_NAME_LEN: usize = 16;
/// Maximal nutzbare Zeichen im Stream-Namen (Feld minus NUL-Abschluss).
pub const MAX_STREAM_NAME: usize = STREAM_NAME_LEN - 1;

/// Sub-Protokoll (Byte 20). Die unteren vier Bit sind in der Spezifikation
/// unbenutzt und werden hier beim Kodieren auf 0 gesetzt, beim Dekodieren
/// ignoriert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubProtocol {
    /// 0x00 – PCM-Audiodaten.
    Audio,
    /// 0x10 – serieller Bytestrom.
    Serial,
    /// 0x20 – MIDI.
    Midi,
    /// 0x40 – Text (UTF-8).
    Chat,
    /// 0x60 – RT-Paket (Parameter/Telemetrie).
    RtPacket,
    /// 0x80 – Service/User.
    Service,
}

impl SubProtocol {
    /// Wert des Sub-Protokoll-Bytes.
    pub fn code(self) -> u8 {
        match self {
            SubProtocol::Audio => 0x00,
            SubProtocol::Serial => 0x10,
            SubProtocol::Midi => 0x20,
            SubProtocol::Chat => 0x40,
            SubProtocol::RtPacket => 0x60,
            SubProtocol::Service => 0x80,
        }
    }

    /// Bildet das Sub-Protokoll-Byte auf die Enum ab.
    ///
    /// ANNAHME: Die unteren vier Bit sind reserviert (0) und werden hier
    /// ignoriert, damit Pakete anderer Implementierungen nicht abgelehnt werden.
    pub fn from_code(byte: u8) -> Result<Self, VbanError> {
        match byte & 0xF0 {
            0x00 => Ok(SubProtocol::Audio),
            0x10 => Ok(SubProtocol::Serial),
            0x20 => Ok(SubProtocol::Midi),
            0x40 => Ok(SubProtocol::Chat),
            0x60 => Ok(SubProtocol::RtPacket),
            0x80 => Ok(SubProtocol::Service),
            _ => Err(VbanError::UnknownSubProtocol(byte)),
        }
    }
}

/// Abtastrate, kodiert in Bits 0..=2 von Format_SR (Byte 21).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleRate {
    Hz6000,
    Hz12000,
    Hz24000,
    Hz48000,
    Hz96000,
    Hz192000,
    Hz384000,
    Hz768000,
}

impl SampleRate {
    /// Alle Codes in der Reihenfolge der Spezifikation (Code = Index).
    pub const ALL: [SampleRate; 8] = [
        SampleRate::Hz6000,
        SampleRate::Hz12000,
        SampleRate::Hz24000,
        SampleRate::Hz48000,
        SampleRate::Hz96000,
        SampleRate::Hz192000,
        SampleRate::Hz384000,
        SampleRate::Hz768000,
    ];

    /// Code 0..=7 fuer Format_SR.
    pub fn code(self) -> u8 {
        match self {
            SampleRate::Hz6000 => 0,
            SampleRate::Hz12000 => 1,
            SampleRate::Hz24000 => 2,
            SampleRate::Hz48000 => 3,
            SampleRate::Hz96000 => 4,
            SampleRate::Hz192000 => 5,
            SampleRate::Hz384000 => 6,
            SampleRate::Hz768000 => 7,
        }
    }

    /// Abtastrate in Hertz.
    pub fn hz(self) -> u32 {
        match self {
            SampleRate::Hz6000 => 6_000,
            SampleRate::Hz12000 => 12_000,
            SampleRate::Hz24000 => 24_000,
            SampleRate::Hz48000 => 48_000,
            SampleRate::Hz96000 => 96_000,
            SampleRate::Hz192000 => 192_000,
            SampleRate::Hz384000 => 384_000,
            // ANNAHME: Code 7 ist in neueren Fassungen 768 kHz. Aeltere
            // Fassungen fuehren ihn als "reserved" – dann ist dieser Wert falsch.
            SampleRate::Hz768000 => 768_000,
        }
    }

    /// Code -> Enum. Alle acht Codes sind belegt, ein Fehler kann daher nur
    /// durch einen Wert > 7 entstehen (defensiv).
    pub fn from_code(code: u8) -> Result<Self, VbanError> {
        SampleRate::ALL
            .get(code as usize)
            .copied()
            .ok_or(VbanError::UnknownSampleRateCode(code))
    }

    /// Hertz -> Enum (nur die acht spezifizierten Raten).
    pub fn from_hz(hz: u32) -> Result<Self, VbanError> {
        SampleRate::ALL
            .iter()
            .copied()
            .find(|r| r.hz() == hz)
            .ok_or(VbanError::UnknownSampleRateHertz(hz))
    }
}

/// Sample-Format, kodiert in Bits 3..=5 von Format_SR (Byte 21).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    /// 0 – 8 Bit unsigned integer.
    U8,
    /// 1 – 16 Bit signed integer (Little Endian).
    I16,
    /// 2 – 24 Bit signed integer (Little Endian).
    I24,
    /// 3 – 32 Bit signed integer (Little Endian).
    I32,
    /// 4 – 32 Bit IEEE-754 float (Little Endian).
    F32,
    /// 5 – 64 Bit IEEE-754 float (Little Endian).
    F64,
    /// 6 – 12 Bit signed integer, zwei Samples in drei Bytes gepackt.
    I12,
    /// 7 – 10 Bit signed integer, gepackt.
    I10,
}

impl AudioFormat {
    /// Format-Code fuer Format_SR.
    pub fn code(self) -> u8 {
        match self {
            AudioFormat::U8 => 0,
            AudioFormat::I16 => 1,
            AudioFormat::I24 => 2,
            AudioFormat::I32 => 3,
            AudioFormat::F32 => 4,
            AudioFormat::F64 => 5,
            AudioFormat::I12 => 6,
            AudioFormat::I10 => 7,
        }
    }

    /// Code -> Enum.
    pub fn from_code(code: u8) -> Result<Self, VbanError> {
        match code {
            0 => Ok(AudioFormat::U8),
            1 => Ok(AudioFormat::I16),
            2 => Ok(AudioFormat::I24),
            3 => Ok(AudioFormat::I32),
            4 => Ok(AudioFormat::F32),
            5 => Ok(AudioFormat::F64),
            6 => Ok(AudioFormat::I12),
            7 => Ok(AudioFormat::I10),
            _ => Err(VbanError::UnknownFormatCode(code)),
        }
    }

    /// Bytes pro Sample bei ungepackten Formaten.
    ///
    /// Die gepackten Formate I12/I10 haben kein ganzzahliges Bytes-pro-Sample
    /// und liefern `None` – sie sind in diesem Crate nicht implementiert.
    pub fn bytes_per_sample(self) -> Option<usize> {
        match self {
            AudioFormat::U8 => Some(1),
            AudioFormat::I16 => Some(2),
            AudioFormat::I24 => Some(3),
            AudioFormat::I32 => Some(4),
            AudioFormat::F32 => Some(4),
            AudioFormat::F64 => Some(8),
            AudioFormat::I12 | AudioFormat::I10 => None,
        }
    }
}

/// Codec (Byte 23). Bei Sub-Protokoll Audio ist 0x00 unkomprimiertes PCM.
///
/// ANNAHME: Andere Werte (komprimierte VBAN-Codecs) sind nicht implementiert und
/// werden mit [`VbanError::UnsupportedCodec`] abgelehnt, statt geraten zu werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// 0x00 – unkomprimiert.
    Pcm,
}

impl Codec {
    /// Codec-Byte.
    pub fn code(self) -> u8 {
        match self {
            Codec::Pcm => 0x00,
        }
    }

    /// Code -> Enum; alles ausser 0x00 ist nicht implementiert.
    pub fn from_code(byte: u8) -> Result<Self, VbanError> {
        match byte {
            0x00 => Ok(Codec::Pcm),
            other => Err(VbanError::UnsupportedCodec(other)),
        }
    }
}

/// Der VBAN-Header (28 Byte).
///
/// Die Felder sind oeffentlich, damit ein Aufrufer sie direkt setzen kann; die
/// Pruefung erfolgt gesammelt in [`Header::pack`], nicht im Konstruktor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Stream-Name, 1..=15 druckbare ASCII-Zeichen (Feld ist NUL-gefuellt).
    pub stream_name: String,
    /// Sub-Protokoll (Byte 20).
    pub sub_protocol: SubProtocol,
    /// Abtastrate, Bits 0..=2 von Byte 21.
    pub sample_rate: SampleRate,
    /// Sample-Format, Bits 3..=5 von Byte 21.
    pub format: AudioFormat,
    /// Kanalzahl 1..=256; auf der Leitung steht `channels - 1`.
    pub channels: u16,
    /// Codec (Byte 23).
    pub codec: Codec,
    /// Frame-Zaehler (Byte 24..=27, Little Endian), laeuft modulo 2^32 ueber.
    pub frame_counter: u32,
    /// Reserve-/Auffuellbyte.
    ///
    /// ANNAHME/ABWEICHUNG: Die oeffentliche Spezifikation beschreibt den Header
    /// mit 28 Byte – nach dem Frame-Zaehler folgen bereits die Nutzdaten. Das in
    /// der Aufgabe genannte Reservebyte existiert dort nicht. Das Feld bleibt
    /// deshalb im Struct (muss 0 sein), wird aber **nicht** auf die Leitung
    /// geschrieben; [`HEADER_LEN`] bleibt 28, damit echte VBAN-Gegenstellen
    /// (Voicemeeter u. a.) die Pakete verstehen.
    pub reserved: u8,
}

impl Header {
    /// Header mit den Pflichtwerten fuer einen Audio-Stream (noch ohne Daten).
    pub fn audio_pcm(
        stream_name: impl Into<String>,
        sample_rate: SampleRate,
        format: AudioFormat,
        channels: u16,
    ) -> Self {
        Header {
            stream_name: stream_name.into(),
            sub_protocol: SubProtocol::Audio,
            sample_rate,
            format,
            channels,
            codec: Codec::Pcm,
            frame_counter: 0,
            reserved: 0,
        }
    }

    /// Prueft Name, Kanalzahl und Reservebyte.
    fn validate(&self) -> Result<(), VbanError> {
        let name = self.stream_name.as_bytes();
        if name.is_empty()
            || name.len() > MAX_STREAM_NAME
            || !name.iter().all(|b| (0x20..=0x7E).contains(b))
        {
            return Err(VbanError::InvalidStreamName(self.stream_name.clone()));
        }
        if self.channels == 0 || self.channels > 256 {
            return Err(VbanError::InvalidChannels(self.channels));
        }
        if self.reserved != 0 {
            return Err(VbanError::ReservedNotZero(self.reserved));
        }
        Ok(())
    }

    /// Serialisiert den Header in seine 28 Byte.
    pub fn pack(&self) -> Result<[u8; HEADER_LEN], VbanError> {
        self.validate()?;
        let mut buf = [0u8; HEADER_LEN];
        buf[0..4].copy_from_slice(&MAGIC);
        // Rest des Namensfeldes bleibt 0 (NUL-Auffuellung).
        let name = self.stream_name.as_bytes();
        buf[4..4 + name.len()].copy_from_slice(name);
        buf[20] = self.sub_protocol.code();
        // Bits 6 und 7 bleiben 0 (in der Spezifikation unbenutzt).
        buf[21] = (self.format.code() << 3) | (self.sample_rate.code() & 0x07);
        buf[22] = (self.channels - 1) as u8;
        buf[23] = self.codec.code();
        buf[24..28].copy_from_slice(&self.frame_counter.to_le_bytes());
        Ok(buf)
    }

    /// Liest den Header aus `bytes` und gibt ihn samt Restpuffer (Nutzdaten)
    /// zurueck.
    ///
    /// Geprueft werden Laenge, Magic, Sub-Protokoll, Rate-/Format-Code und
    /// Codec. Bits 6..7 von Format_SR werden ignoriert (ANNAHME: reserviert).
    pub fn unpack(bytes: &[u8]) -> Result<(Header, &[u8]), VbanError> {
        if bytes.len() < HEADER_LEN {
            return Err(VbanError::TooShort {
                got: bytes.len(),
                need: HEADER_LEN,
            });
        }
        let magic: [u8; 4] = [bytes[0], bytes[1], bytes[2], bytes[3]];
        if magic != MAGIC {
            return Err(VbanError::BadMagic(magic));
        }

        let raw_name = &bytes[4..4 + STREAM_NAME_LEN];
        let end = raw_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(STREAM_NAME_LEN);
        let stream_name = std::str::from_utf8(&raw_name[..end])
            .map_err(|_| VbanError::InvalidStreamName(String::from("<kein UTF-8>")))?
            .to_string();

        let sub_protocol = SubProtocol::from_code(bytes[20])?;
        let format_sr = bytes[21];
        let sample_rate = SampleRate::from_code(format_sr & 0x07)?;
        let format = AudioFormat::from_code((format_sr >> 3) & 0x07)?;
        // Byte 22 speichert die Kanalzahl minus eins.
        let channels = u16::from(bytes[22]) + 1;
        let codec = Codec::from_code(bytes[23])?;
        let frame_counter = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);

        let header = Header {
            stream_name,
            sub_protocol,
            sample_rate,
            format,
            channels,
            codec,
            frame_counter,
            reserved: 0,
        };
        Ok((header, &bytes[HEADER_LEN..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::VbanError;

    fn demo() -> Header {
        Header {
            stream_name: String::from("VBAN Test"),
            sub_protocol: SubProtocol::Audio,
            sample_rate: SampleRate::Hz48000,
            format: AudioFormat::I16,
            channels: 2,
            codec: Codec::Pcm,
            frame_counter: 0x0102_0304,
            reserved: 0,
        }
    }

    #[test]
    fn header_hat_28_byte_und_feste_offsets() {
        let bytes = demo().pack().expect("pack");
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(&bytes[0..4], b"VBAN");
        assert_eq!(&bytes[4..13], b"VBAN Test");
        assert_eq!(&bytes[13..20], &[0u8; 7], "Namensfeld muss NUL-gefuellt sein");
        assert_eq!(bytes[20], 0x00, "Audio");
        assert_eq!(bytes[21], 0x0B, "I16 (1<<3) | Rate 48000 (3)");
        assert_eq!(bytes[22], 1, "2 Kanaele -> gespeichert wird 1");
        assert_eq!(bytes[23], 0x00, "PCM");
        assert_eq!(&bytes[24..28], &[0x04, 0x03, 0x02, 0x01], "LE-Zaehler");
    }

    #[test]
    fn header_roundtrip() {
        let original = demo();
        let bytes = original.pack().expect("pack");
        let (parsed, rest) = Header::unpack(&bytes).expect("unpack");
        assert_eq!(parsed, original);
        assert!(rest.is_empty());
    }

    #[test]
    fn header_zu_kurz_wird_abgelehnt() {
        let bytes = demo().pack().expect("pack");
        for len in [0usize, 4, 27] {
            let err = Header::unpack(&bytes[..len]).expect_err("muss fehlschlagen");
            match err {
                VbanError::TooShort { got, need } => {
                    assert_eq!(got, len);
                    assert_eq!(need, HEADER_LEN);
                }
                other => panic!("falscher Fehler: {other:?}"),
            }
        }
    }

    #[test]
    fn falsches_magic_wird_abgelehnt() {
        let mut bytes = demo().pack().expect("pack");
        bytes[0..4].copy_from_slice(b"VBAM");
        match Header::unpack(&bytes).expect_err("muss fehlschlagen") {
            VbanError::BadMagic(m) => assert_eq!(&m, b"VBAM"),
            other => panic!("falscher Fehler: {other:?}"),
        }
    }

    #[test]
    fn unbekanntes_sub_protokoll_wird_abgelehnt() {
        let mut bytes = demo().pack().expect("pack");
        bytes[20] = 0x30;
        assert!(matches!(
            Header::unpack(&bytes),
            Err(VbanError::UnknownSubProtocol(0x30))
        ));
    }

    #[test]
    fn reservierte_bits_im_sub_protokoll_werden_ignoriert() {
        let mut bytes = demo().pack().expect("pack");
        bytes[20] = 0x0A; // Audio + untere Bits gesetzt
        let (h, _) = Header::unpack(&bytes).expect("unpack");
        assert_eq!(h.sub_protocol, SubProtocol::Audio);
    }

    #[test]
    fn alle_sub_protokolle_kodieren_und_dekodieren() {
        for (sp, code) in [
            (SubProtocol::Audio, 0x00),
            (SubProtocol::Serial, 0x10),
            (SubProtocol::Midi, 0x20),
            (SubProtocol::Chat, 0x40),
            (SubProtocol::RtPacket, 0x60),
            (SubProtocol::Service, 0x80),
        ] {
            assert_eq!(sp.code(), code);
            assert_eq!(SubProtocol::from_code(code).unwrap(), sp);
        }
    }

    #[test]
    fn alle_abtastraten_kodieren_und_dekodieren() {
        for (idx, rate) in SampleRate::ALL.iter().enumerate() {
            assert_eq!(rate.code() as usize, idx);
            assert_eq!(SampleRate::from_code(rate.code()).unwrap(), *rate);
            assert_eq!(SampleRate::from_hz(rate.hz()).unwrap(), *rate);
        }
        assert_eq!(SampleRate::Hz48000.hz(), 48_000);
        assert_eq!(SampleRate::Hz768000.code(), 7);
        assert!(matches!(
            SampleRate::from_hz(44_100),
            Err(VbanError::UnknownSampleRateHertz(44_100))
        ));
        assert!(matches!(
            SampleRate::from_code(9),
            Err(VbanError::UnknownSampleRateCode(9))
        ));
    }

    #[test]
    fn kanalzahl_grenzen() {
        let mut h = demo();
        h.channels = 256;
        let bytes = h.pack().expect("256 Kanaele sind erlaubt");
        assert_eq!(bytes[22], 255);
        let (parsed, _) = Header::unpack(&bytes).unwrap();
        assert_eq!(parsed.channels, 256);

        h.channels = 257;
        assert!(matches!(h.pack(), Err(VbanError::InvalidChannels(257))));
        h.channels = 0;
        assert!(matches!(h.pack(), Err(VbanError::InvalidChannels(0))));
    }

    #[test]
    fn stream_name_wird_geprueft() {
        let mut h = demo();
        h.stream_name = String::new();
        assert!(matches!(h.pack(), Err(VbanError::InvalidStreamName(_))));

        h.stream_name = String::from("1234567890123456"); // 16 Zeichen
        assert!(matches!(h.pack(), Err(VbanError::InvalidStreamName(_))));

        h.stream_name = String::from("123456789012345"); // 15 Zeichen
        let bytes = h.pack().expect("15 Zeichen passen");
        assert_eq!(&bytes[4..20], b"123456789012345\0");

        h.stream_name = String::from("Stream1\n");
        assert!(matches!(h.pack(), Err(VbanError::InvalidStreamName(_))));
    }

    #[test]
    fn namensfeld_ohne_nul_wird_vollstaendig_gelesen() {
        let mut bytes = demo().pack().expect("pack");
        bytes[4..20].copy_from_slice(b"ABCDEFGHIJKLMNOP");
        let (h, _) = Header::unpack(&bytes).unwrap();
        assert_eq!(h.stream_name, "ABCDEFGHIJKLMNOP");
    }

    #[test]
    fn reservebyte_muss_null_sein() {
        let mut h = demo();
        h.reserved = 1;
        assert!(matches!(h.pack(), Err(VbanError::ReservedNotZero(1))));
    }

    #[test]
    fn codec_ausser_pcm_wird_abgelehnt() {
        let mut bytes = demo().pack().expect("pack");
        bytes[23] = 0x02;
        assert!(matches!(
            Header::unpack(&bytes),
            Err(VbanError::UnsupportedCodec(0x02))
        ));
        assert_eq!(Codec::Pcm.code(), 0);
    }

    #[test]
    fn format_bits_werden_richtig_zerlegt() {
        for fmt in [
            AudioFormat::U8,
            AudioFormat::I16,
            AudioFormat::I24,
            AudioFormat::I32,
            AudioFormat::F32,
            AudioFormat::F64,
            AudioFormat::I12,
            AudioFormat::I10,
        ] {
            let mut h = demo();
            h.format = fmt;
            h.sample_rate = SampleRate::Hz96000;
            let bytes = h.pack().unwrap();
            assert_eq!(bytes[21], (fmt.code() << 3) | 4);
            let (parsed, _) = Header::unpack(&bytes).unwrap();
            assert_eq!(parsed.format, fmt);
            assert_eq!(parsed.sample_rate, SampleRate::Hz96000);
        }
    }

    #[test]
    fn bytes_pro_sample() {
        assert_eq!(AudioFormat::U8.bytes_per_sample(), Some(1));
        assert_eq!(AudioFormat::I16.bytes_per_sample(), Some(2));
        assert_eq!(AudioFormat::I24.bytes_per_sample(), Some(3));
        assert_eq!(AudioFormat::F32.bytes_per_sample(), Some(4));
        assert_eq!(AudioFormat::F64.bytes_per_sample(), Some(8));
        assert_eq!(AudioFormat::I12.bytes_per_sample(), None);
        assert_eq!(AudioFormat::I10.bytes_per_sample(), None);
    }

    #[test]
    fn nutzdaten_bleiben_beim_unpack_erhalten() {
        let mut bytes = demo().pack().unwrap().to_vec();
        bytes.extend_from_slice(&[1, 2, 3, 4]);
        let (_, rest) = Header::unpack(&bytes).unwrap();
        assert_eq!(rest, &[1, 2, 3, 4]);
    }
}
