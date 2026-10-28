//! VBAN-Header: Kodieren (pack) und Dekodieren (unpack) der 28 Byte, die jedes
//! VBAN-Paket einleiten.
//!
//! Feldbelegung nach der oeffentlichen VBAN-Spezifikation von VB-Audio
//! (VBANProtocol_Specifications.pdf):
//!
//! | Offset | Groesse | Feld |
//! |-------:|--------:|------|
//! | 0..4   | 4  | `VBAN` (ASCII) |
//! | 4      | 1  | Bits 0..4 Sample-Rate-Index, Bits 5..7 Sub-Protokoll |
//! | 5      | 1  | Samples pro Frame minus 1 |
//! | 6      | 1  | Kanaele minus 1 |
//! | 7      | 1  | Bits 0..2 Datentyp, Bit 3 reserviert, Bits 4..7 Codec |
//! | 8..24  | 16 | Stream-Name (NUL-gefuellt) |
//! | 24..28 | 4  | Frame-Zaehler (`u32` Little Endian) |
//!
//! Ab Byte 28 folgen die Nutzdaten.

use crate::error::VbanError;

/// Magic der ersten vier Bytes.
pub const MAGIC: [u8; 4] = *b"VBAN";
/// Laenge des Headers in Bytes; die Nutzdaten beginnen direkt danach.
pub const HEADER_LEN: usize = 28;
/// Groesse des Stream-Namen-Feldes (NUL-gefuellt).
pub const STREAM_NAME_LEN: usize = 16;
/// Maximal nutzbare Zeichen im Stream-Namen (Feld minus NUL-Abschluss).
pub const MAX_STREAM_NAME: usize = STREAM_NAME_LEN - 1;

/// Offset des Bytes 4 (Sub-Protokoll + Sample-Rate-Index).
const OFFSET_FORMAT_SR: usize = 4;
/// Offset des Feldes "Samples pro Frame minus 1".
pub const OFFSET_SAMPLES: usize = 5;
/// Offset des Feldes "Kanaele minus 1".
const OFFSET_CHANNELS: usize = 6;
/// Offset des Bytes 7 (Datentyp + Codec).
const OFFSET_FORMAT_BITS: usize = 7;
/// Offset des Stream-Namen-Feldes.
const OFFSET_STREAM_NAME: usize = 8;
/// Offset des Frame-Zaehlers.
const OFFSET_FRAME_COUNTER: usize = 24;

/// Sub-Protokoll, in Bits 5..7 von Byte 4.
///
/// Die Codes sind die **maskierten** Bytewerte (`byte & 0xE0`), so wie sie in
/// der Spezifikation angegeben sind: 0x00, 0x20, 0x40, 0x60.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubProtocol {
    /// 0x00 – Audio (PCM).
    Audio,
    /// 0x20 – serieller Bytestrom.
    Serial,
    /// 0x40 – Text (UTF-8).
    Text,
    /// 0x60 – Service/User.
    Service,
}

impl SubProtocol {
    /// Maskiertes Sub-Protokoll-Byte (Bits 5..7).
    pub fn code(self) -> u8 {
        match self {
            SubProtocol::Audio => 0x00,
            SubProtocol::Serial => 0x20,
            SubProtocol::Text => 0x40,
            SubProtocol::Service => 0x60,
        }
    }

    /// Bildet das **ganze** Byte 4 auf die Enum ab.
    ///
    /// Die unteren fuenf Bit gehoeren zum Sample-Rate-Index und werden hier
    /// maskiert (0xE0). Eine Maske 0xF0 waere falsch: ab Rate-Index 16
    /// (44100 Hz) steht Bit 4 und wuerde das Sub-Protokoll verfaelschen.
    pub fn from_code(byte: u8) -> Result<Self, VbanError> {
        let sub = byte & 0xE0;
        match sub {
            0x00 => Ok(SubProtocol::Audio),
            0x20 => Ok(SubProtocol::Serial),
            0x40 => Ok(SubProtocol::Text),
            0x60 => Ok(SubProtocol::Service),
            _ => Err(VbanError::UnknownSubProtocol(sub)),
        }
    }
}

/// Abtastrate, kodiert in Bits 0..4 von Byte 4 (Index 0..=20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleRate {
    Hz6000,
    Hz12000,
    Hz24000,
    Hz48000,
    Hz96000,
    Hz192000,
    Hz384000,
    Hz8000,
    Hz16000,
    Hz32000,
    Hz64000,
    Hz128000,
    Hz256000,
    Hz512000,
    Hz11025,
    Hz22050,
    Hz44100,
    Hz88200,
    Hz176400,
    Hz352800,
    Hz705600,
}

impl SampleRate {
    /// Alle Codes in der Reihenfolge der Spezifikation (Code = Index).
    pub const ALL: [SampleRate; 21] = [
        SampleRate::Hz6000,
        SampleRate::Hz12000,
        SampleRate::Hz24000,
        SampleRate::Hz48000,
        SampleRate::Hz96000,
        SampleRate::Hz192000,
        SampleRate::Hz384000,
        SampleRate::Hz8000,
        SampleRate::Hz16000,
        SampleRate::Hz32000,
        SampleRate::Hz64000,
        SampleRate::Hz128000,
        SampleRate::Hz256000,
        SampleRate::Hz512000,
        SampleRate::Hz11025,
        SampleRate::Hz22050,
        SampleRate::Hz44100,
        SampleRate::Hz88200,
        SampleRate::Hz176400,
        SampleRate::Hz352800,
        SampleRate::Hz705600,
    ];

    /// Index 0..=20 fuer Bits 0..4 von Byte 4.
    pub fn code(self) -> u8 {
        match self {
            SampleRate::Hz6000 => 0,
            SampleRate::Hz12000 => 1,
            SampleRate::Hz24000 => 2,
            SampleRate::Hz48000 => 3,
            SampleRate::Hz96000 => 4,
            SampleRate::Hz192000 => 5,
            SampleRate::Hz384000 => 6,
            SampleRate::Hz8000 => 7,
            SampleRate::Hz16000 => 8,
            SampleRate::Hz32000 => 9,
            SampleRate::Hz64000 => 10,
            SampleRate::Hz128000 => 11,
            SampleRate::Hz256000 => 12,
            SampleRate::Hz512000 => 13,
            SampleRate::Hz11025 => 14,
            SampleRate::Hz22050 => 15,
            SampleRate::Hz44100 => 16,
            SampleRate::Hz88200 => 17,
            SampleRate::Hz176400 => 18,
            SampleRate::Hz352800 => 19,
            SampleRate::Hz705600 => 20,
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
            SampleRate::Hz8000 => 8_000,
            SampleRate::Hz16000 => 16_000,
            SampleRate::Hz32000 => 32_000,
            SampleRate::Hz64000 => 64_000,
            SampleRate::Hz128000 => 128_000,
            SampleRate::Hz256000 => 256_000,
            SampleRate::Hz512000 => 512_000,
            SampleRate::Hz11025 => 11_025,
            SampleRate::Hz22050 => 22_050,
            SampleRate::Hz44100 => 44_100,
            SampleRate::Hz88200 => 88_200,
            SampleRate::Hz176400 => 176_400,
            SampleRate::Hz352800 => 352_800,
            SampleRate::Hz705600 => 705_600,
        }
    }

    /// Index -> Enum. Codes 0..=20 sind belegt, alles andere ist ein Fehler.
    pub fn from_code(code: u8) -> Result<Self, VbanError> {
        SampleRate::ALL
            .get(code as usize)
            .copied()
            .ok_or(VbanError::UnknownSampleRateCode(code))
    }

    /// Hertz -> Enum (nur die 21 spezifizierten Raten).
    pub fn from_hz(hz: u32) -> Result<Self, VbanError> {
        SampleRate::ALL
            .iter()
            .copied()
            .find(|r| r.hz() == hz)
            .ok_or(VbanError::UnknownSampleRateHertz(hz))
    }
}

/// Sample-Format, kodiert in Bits 0..2 von Byte 7.
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
    /// Datentyp-Code 0..=7 fuer Bits 0..2 von Byte 7.
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

    /// Code (0..=7) -> Enum.
    pub fn from_code(code: u8) -> Result<Self, VbanError> {
        match code & 0x07 {
            0 => Ok(AudioFormat::U8),
            1 => Ok(AudioFormat::I16),
            2 => Ok(AudioFormat::I24),
            3 => Ok(AudioFormat::I32),
            4 => Ok(AudioFormat::F32),
            5 => Ok(AudioFormat::F64),
            6 => Ok(AudioFormat::I12),
            _ => Ok(AudioFormat::I10),
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

/// Codec, kodiert in Bits 4..7 von Byte 7 (Nibble-Wert, nicht das ganze Byte).
///
/// ANNAHME: Andere Werte (komprimierte VBAN-Codecs) sind nicht implementiert und
/// werden mit [`VbanError::UnsupportedCodec`] abgelehnt, statt geraten zu werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// 0 – unkomprimiert (PCM).
    Pcm,
}

impl Codec {
    /// Nibble-Wert 0..=15 fuer Bits 4..7 von Byte 7.
    pub fn code(self) -> u8 {
        match self {
            Codec::Pcm => 0x00,
        }
    }

    /// Nibble-Wert (`byte7 >> 4`) -> Enum; alles ausser 0x0 ist nicht
    /// implementiert.
    pub fn from_code(nibble: u8) -> Result<Self, VbanError> {
        match nibble & 0x0F {
            0x00 => Ok(Codec::Pcm),
            other => Err(VbanError::UnsupportedCodec(other)),
        }
    }
}

/// Der VBAN-Header (28 Byte).
///
/// Die Felder sind oeffentlich, damit ein Aufrufer sie direkt setzen kann; die
/// Pruefung erfolgt gesammelt in [`Header::pack`], nicht im Konstruktor.
///
/// Das Feld "Samples pro Frame minus 1" (Byte 5) ist Teil des Structs bewusst
/// **nicht**: es folgt aus der Nutzdatenlaenge und wird von
/// [`crate::AudioFrame::encode`] gesetzt. [`Header::pack`] schreibt dort 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Stream-Name, 1..=15 druckbare ASCII-Zeichen (Feld ist NUL-gefuellt).
    pub stream_name: String,
    /// Sub-Protokoll (Bits 5..7 von Byte 4).
    pub sub_protocol: SubProtocol,
    /// Abtastrate (Bits 0..4 von Byte 4).
    pub sample_rate: SampleRate,
    /// Sample-Format (Bits 0..2 von Byte 7).
    pub format: AudioFormat,
    /// Kanalzahl 1..=256; auf der Leitung steht `channels - 1`.
    pub channels: u16,
    /// Codec (Bits 4..7 von Byte 7).
    pub codec: Codec,
    /// Frame-Zaehler (Byte 24..=27, Little Endian), laeuft modulo 2^32 ueber.
    pub frame_counter: u32,
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
        }
    }

    /// Prueft Name und Kanalzahl.
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
        Ok(())
    }

    /// Serialisiert den Header in seine 28 Byte.
    ///
    /// Byte 5 ("Samples pro Frame minus 1") wird mit 0 geschrieben; den echten
    /// Wert traegt [`crate::AudioFrame::encode`] nach, weil nur der Frame die
    /// Nutzdatenlaenge kennt.
    pub fn pack(&self) -> Result<[u8; HEADER_LEN], VbanError> {
        self.validate()?;
        let mut buf = [0u8; HEADER_LEN];
        buf[0..4].copy_from_slice(&MAGIC);
        // Byte 4: Bits 5..7 Sub-Protokoll, Bits 0..4 Sample-Rate-Index.
        buf[OFFSET_FORMAT_SR] =
            (self.sub_protocol.code() & 0xE0) | (self.sample_rate.code() & 0x1F);
        // Byte 5: wird von AudioFrame::encode gesetzt.
        buf[OFFSET_SAMPLES] = 0;
        // Byte 6: Kanalzahl minus eins.
        buf[OFFSET_CHANNELS] = (self.channels - 1) as u8;
        // Byte 7: Bits 4..7 Codec, Bit 3 reserviert (0), Bits 0..2 Datentyp.
        buf[OFFSET_FORMAT_BITS] = (self.codec.code() << 4) | (self.format.code() & 0x07);
        // Rest des Namensfeldes bleibt 0 (NUL-Auffuellung).
        let name = self.stream_name.as_bytes();
        buf[OFFSET_STREAM_NAME..OFFSET_STREAM_NAME + name.len()].copy_from_slice(name);
        buf[OFFSET_FRAME_COUNTER..OFFSET_FRAME_COUNTER + 4]
            .copy_from_slice(&self.frame_counter.to_le_bytes());
        Ok(buf)
    }

    /// Liest den Header aus `bytes` und gibt ihn samt Restpuffer (Nutzdaten)
    /// zurueck.
    ///
    /// Geprueft werden Laenge, Magic, Sub-Protokoll, Rate-/Format-Code und
    /// Codec. Bit 3 von Byte 7 ist laut Spezifikation reserviert und wird beim
    /// Lesen ignoriert; Byte 5 (Samples pro Frame) wird nicht ausgewertet, weil
    /// die Laenge bereits aus den Nutzdaten folgt.
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

        let raw_name = &bytes[OFFSET_STREAM_NAME..OFFSET_STREAM_NAME + STREAM_NAME_LEN];
        let end = raw_name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(STREAM_NAME_LEN);
        let stream_name = std::str::from_utf8(&raw_name[..end])
            .map_err(|_| VbanError::InvalidStreamName(String::from("<kein UTF-8>")))?
            .to_string();

        // Byte 4 traegt beides: from_code maskiert selbst, from_code der Rate
        // bekommt den Index.
        let format_sr = bytes[OFFSET_FORMAT_SR];
        let sub_protocol = SubProtocol::from_code(format_sr)?;
        let sample_rate = SampleRate::from_code(format_sr & 0x1F)?;
        // Byte 7: Datentyp ist der Index, Codec der Nibble-Wert.
        let format_bits = bytes[OFFSET_FORMAT_BITS];
        let format = AudioFormat::from_code(format_bits & 0x07)?;
        let codec = Codec::from_code(format_bits >> 4)?;
        // Byte 6 speichert die Kanalzahl minus eins.
        let channels = u16::from(bytes[OFFSET_CHANNELS]) + 1;
        let frame_counter = u32::from_le_bytes([
            bytes[OFFSET_FRAME_COUNTER],
            bytes[OFFSET_FRAME_COUNTER + 1],
            bytes[OFFSET_FRAME_COUNTER + 2],
            bytes[OFFSET_FRAME_COUNTER + 3],
        ]);

        let header = Header {
            stream_name,
            sub_protocol,
            sample_rate,
            format,
            channels,
            codec,
            frame_counter,
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
        }
    }

    #[test]
    fn header_hat_28_byte_und_feste_offsets() {
        let bytes = demo().pack().expect("pack");
        assert_eq!(bytes.len(), HEADER_LEN);
        assert_eq!(&bytes[0..4], b"VBAN");
        // Byte 4: Sub-Protokoll 0 (Audio) in Bits 5..7, Rate 48000 = Index 3.
        assert_eq!(bytes[4], 0x03, "Audio (0x00) | Rate-Index 3");
        assert_eq!(bytes[5], 0x00, "Samples pro Frame setzt erst encode()");
        assert_eq!(bytes[6], 1, "2 Kanaele -> gespeichert wird 1");
        assert_eq!(bytes[7], 0x01, "Datentyp I16 (1), Codec PCM (0)");
        assert_eq!(&bytes[8..17], b"VBAN Test");
        assert_eq!(
            &bytes[17..24],
            &[0u8; 7],
            "Namensfeld muss NUL-gefuellt sein"
        );
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
        // Bits 5..7 = 100 ist in der Spezifikation nicht belegt.
        let mut bytes = demo().pack().expect("pack");
        bytes[4] = 0x80;
        assert!(matches!(
            Header::unpack(&bytes),
            Err(VbanError::UnknownSubProtocol(0x80))
        ));
    }

    #[test]
    fn sub_protokoll_teilt_byte_4_mit_dem_rate_index() {
        // Text (0x40) mit 44100 Hz (Index 16): Bit 4 gehoert der Rate und darf
        // das Sub-Protokoll nicht verfaelschen.
        let mut bytes = demo().pack().expect("pack");
        bytes[4] = 0x40 | 16;
        let (h, _) = Header::unpack(&bytes).expect("unpack");
        assert_eq!(h.sub_protocol, SubProtocol::Text);
        assert_eq!(h.sample_rate, SampleRate::Hz44100);

        // Gegenprobe: auch hohe Rate-Indizes bleiben ein Audio-Header.
        let mut h = demo();
        h.sample_rate = SampleRate::Hz705600;
        let bytes = h.pack().expect("pack");
        assert_eq!(bytes[4], 20, "Index 20 in den unteren fuenf Bits");
        assert_eq!(Header::unpack(&bytes).unwrap().0, h);
    }

    #[test]
    fn alle_sub_protokolle_kodieren_und_dekodieren() {
        for (sp, code) in [
            (SubProtocol::Audio, 0x00),
            (SubProtocol::Serial, 0x20),
            (SubProtocol::Text, 0x40),
            (SubProtocol::Service, 0x60),
        ] {
            assert_eq!(sp.code(), code);
            assert_eq!(SubProtocol::from_code(code).unwrap(), sp);
            // Mit gesetztem Rate-Index bleibt die Zuordnung dieselbe.
            assert_eq!(SubProtocol::from_code(code | 0x1F).unwrap(), sp);
        }
    }

    #[test]
    fn alle_abtastraten_kodieren_und_dekodieren() {
        for (idx, rate) in SampleRate::ALL.iter().enumerate() {
            assert_eq!(rate.code() as usize, idx, "Code = Index der Tabelle");
            assert_eq!(SampleRate::from_code(rate.code()).unwrap(), *rate);
            assert_eq!(SampleRate::from_hz(rate.hz()).unwrap(), *rate);
        }
        assert_eq!(SampleRate::Hz48000.hz(), 48_000);
        assert_eq!(SampleRate::Hz48000.code(), 3);
        assert_eq!(SampleRate::Hz44100.code(), 16);
        assert_eq!(SampleRate::Hz8000.code(), 7);
        assert!(matches!(
            SampleRate::from_hz(50_000),
            Err(VbanError::UnknownSampleRateHertz(50_000))
        ));
        assert!(matches!(
            SampleRate::from_code(21),
            Err(VbanError::UnknownSampleRateCode(21))
        ));
    }

    #[test]
    fn kanalzahl_grenzen() {
        let mut h = demo();
        h.channels = 256;
        let bytes = h.pack().expect("256 Kanaele sind erlaubt");
        assert_eq!(bytes[6], 255);
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
        assert_eq!(&bytes[8..24], b"123456789012345\0");

        h.stream_name = String::from("Stream1\n");
        assert!(matches!(h.pack(), Err(VbanError::InvalidStreamName(_))));
    }

    #[test]
    fn namensfeld_ohne_nul_wird_vollstaendig_gelesen() {
        let mut bytes = demo().pack().expect("pack");
        bytes[8..24].copy_from_slice(b"ABCDEFGHIJKLMNOP");
        let (h, _) = Header::unpack(&bytes).unwrap();
        assert_eq!(h.stream_name, "ABCDEFGHIJKLMNOP");
    }

    #[test]
    fn reserviertes_bit_im_datentyp_byte_wird_ignoriert() {
        // Bit 3 von Byte 7 ist laut Spezifikation reserviert; ein Sender, der
        // es setzt, soll nicht abgelehnt werden.
        let mut bytes = demo().pack().expect("pack");
        bytes[7] |= 0x08;
        let (h, _) = Header::unpack(&bytes).expect("unpack");
        assert_eq!(h.format, AudioFormat::I16);
        assert_eq!(h.codec, Codec::Pcm);
    }

    #[test]
    fn codec_ausser_pcm_wird_abgelehnt() {
        // Codec ist der Nibble in Bits 4..7; 0x2 ist nicht implementiert.
        let mut bytes = demo().pack().expect("pack");
        bytes[7] = (0x02 << 4) | 0x01;
        match Header::unpack(&bytes) {
            Err(VbanError::UnsupportedCodec(0x02)) => {}
            other => panic!("falsches Ergebnis: {other:?}"),
        }
        assert_eq!(Codec::Pcm.code(), 0);
        assert_eq!(Codec::from_code(0).unwrap(), Codec::Pcm);
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
            assert_eq!(bytes[4], 4, "Sample-Rate-Index 4 steht in Byte 4");
            assert_eq!(bytes[7], fmt.code(), "Datentyp in Bits 0..2 von Byte 7");
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
