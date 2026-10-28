//! Audio-Frames des Sub-Protokolls 0x00: Header plus interleaved PCM-Daten.
//!
//! Ein Frame haelt seine Nutzdaten als rohe Bytes in Little-Endian-Ordnung, wie
//! sie auf der Leitung stehen. Die typisierten Helfer (`push_i16`, `push_f32`,
//! `samples_i16`, `samples_f32`) pruefen, dass der Sample-Typ zum Format im
//! Header passt, damit kein Frame mit widerspruechlichen Angaben entsteht.

use crate::error::VbanError;
use crate::header::{
    AudioFormat, Codec, Header, SampleRate, SubProtocol, HEADER_LEN, OFFSET_SAMPLES,
};

/// Ein VBAN-Audio-Frame (Header + PCM-Nutzdaten).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    header: Header,
    data: Vec<u8>,
}

impl AudioFrame {
    /// Neuer Frame fuer 16-Bit-Integer-PCM (Format-Code 1).
    pub fn pcm16(
        stream_name: impl Into<String>,
        sample_rate: SampleRate,
        channels: u16,
    ) -> Result<Self, VbanError> {
        Self::with_format(stream_name, sample_rate, channels, AudioFormat::I16)
    }

    /// Neuer Frame fuer 32-Bit-Float-PCM (Format-Code 4).
    pub fn float32(
        stream_name: impl Into<String>,
        sample_rate: SampleRate,
        channels: u16,
    ) -> Result<Self, VbanError> {
        Self::with_format(stream_name, sample_rate, channels, AudioFormat::F32)
    }

    /// Neuer Frame mit explizitem Format (alle ungepackten Formate erlaubt).
    pub fn with_format(
        stream_name: impl Into<String>,
        sample_rate: SampleRate,
        channels: u16,
        format: AudioFormat,
    ) -> Result<Self, VbanError> {
        if format.bytes_per_sample().is_none() {
            return Err(VbanError::UnsupportedFormat(format));
        }
        let header = Header::audio_pcm(stream_name, sample_rate, format, channels);
        // Frueh pruefen, damit der Fehler beim Aufrufer und nicht erst beim Senden landet.
        header.pack()?;
        Ok(Self {
            header,
            data: Vec::new(),
        })
    }

    /// Header (lesend).
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Header (veraenderbar); der Aufrufer ist fuer gueltige Werte verantwortlich.
    pub fn header_mut(&mut self) -> &mut Header {
        &mut self.header
    }

    /// Aktueller Frame-Zaehler.
    pub fn frame_counter(&self) -> u32 {
        self.header.frame_counter
    }

    /// Setzt den Frame-Zaehler auf einen beliebigen Wert.
    pub fn set_frame_counter(&mut self, counter: u32) {
        self.header.frame_counter = counter;
    }

    /// Erhoeht den Frame-Zaehler um eins und gibt den neuen Wert zurueck.
    /// Ueberlauf ist gewollt: `u32::MAX` -> `0` (Wrap-around).
    pub fn advance_counter(&mut self) -> u32 {
        self.header.frame_counter = self.header.frame_counter.wrapping_add(1);
        self.header.frame_counter
    }

    /// Setzt den Zaehler um `n` Frames weiter (ebenfalls mit Wrap-around).
    pub fn advance_counter_by(&mut self, n: u32) -> u32 {
        self.header.frame_counter = self.header.frame_counter.wrapping_add(n);
        self.header.frame_counter
    }

    /// Anzahl der Sample-Frames (Samples pro Kanal), nicht der Einzelwerte.
    pub fn sample_frames(&self) -> usize {
        let bps = self.header.format.bytes_per_sample().unwrap_or(1).max(1);
        let block = bps * self.header.channels as usize;
        if block == 0 {
            0
        } else {
            self.data.len() / block
        }
    }

    /// Gesamtzahl der Einzelwerte (Sample-Frames * Kanaele).
    pub fn sample_count(&self) -> usize {
        self.data.len() / self.header.format.bytes_per_sample().unwrap_or(1).max(1)
    }

    /// Rohdaten, wie sie auf der Leitung stehen.
    pub fn payload(&self) -> &[u8] {
        &self.data
    }

    /// Prueft, dass `len` Samples ganzzahlig auf die Kanaele verteilbar sind.
    fn check_channel_multiple(&self, len: usize) -> Result<(), VbanError> {
        let ch = self.header.channels;
        if ch == 0 || len % ch as usize != 0 {
            return Err(VbanError::OddSampleCount {
                samples: len,
                channels: ch,
            });
        }
        Ok(())
    }

    /// Haengt 16-Bit-Samples an (interleaved). Nur fuer [`AudioFormat::I16`].
    pub fn push_i16(&mut self, samples: &[i16]) -> Result<(), VbanError> {
        if self.header.format != AudioFormat::I16 {
            return Err(VbanError::FormatMismatch {
                expected: AudioFormat::I16,
                got: self.header.format,
            });
        }
        self.check_channel_multiple(samples.len())?;
        self.data.reserve(samples.len() * 2);
        for s in samples {
            self.data.extend_from_slice(&s.to_le_bytes());
        }
        Ok(())
    }

    /// Haengt 32-Bit-Float-Samples an (interleaved, IEEE-754 Little Endian).
    pub fn push_f32(&mut self, samples: &[f32]) -> Result<(), VbanError> {
        if self.header.format != AudioFormat::F32 {
            return Err(VbanError::FormatMismatch {
                expected: AudioFormat::F32,
                got: self.header.format,
            });
        }
        self.check_channel_multiple(samples.len())?;
        self.data.reserve(samples.len() * 4);
        for s in samples {
            self.data.extend_from_slice(&s.to_le_bytes());
        }
        Ok(())
    }

    /// Liest die Nutzdaten als `i16` (nur bei [`AudioFormat::I16`]).
    pub fn samples_i16(&self) -> Result<Vec<i16>, VbanError> {
        if self.header.format != AudioFormat::I16 {
            return Err(VbanError::FormatMismatch {
                expected: AudioFormat::I16,
                got: self.header.format,
            });
        }
        Ok(self
            .data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect())
    }

    /// Liest die Nutzdaten als `f32` (nur bei [`AudioFormat::F32`]).
    pub fn samples_f32(&self) -> Result<Vec<f32>, VbanError> {
        if self.header.format != AudioFormat::F32 {
            return Err(VbanError::FormatMismatch {
                expected: AudioFormat::F32,
                got: self.header.format,
            });
        }
        Ok(self
            .data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
    }

    /// Serialisiert Header + Nutzdaten zu einem UDP-tauglichen Puffer.
    ///
    /// Byte 5 ("Samples pro Frame minus 1") setzt der Header auf 0, weil nur
    /// der Frame die Nutzdatenlaenge kennt – hier wird der echte Wert
    /// eingetragen. Das Feld ist 8 Bit breit; Frames mit mehr als 256 Samples
    /// pro Kanal werden auf 255 gesaettigt (die tatsaechliche Laenge ergibt
    /// sich fuer den Empfaenger aus den Nutzdaten).
    pub fn encode(&self) -> Result<Vec<u8>, VbanError> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.data.len());
        out.extend_from_slice(&self.header.pack()?);
        out[OFFSET_SAMPLES] = self.samples_byte();
        out.extend_from_slice(&self.data);
        Ok(out)
    }

    /// Byte 5 des Headers: Samples pro Kanal minus 1, auf 8 Bit begrenzt.
    fn samples_byte(&self) -> u8 {
        self.sample_frames().saturating_sub(1).min(255) as u8
    }

    /// Parst ein empfangenes Paket. Nur Sub-Protokoll Audio und Codec PCM.
    pub fn parse(bytes: &[u8]) -> Result<Self, VbanError> {
        let (header, payload) = Header::unpack(bytes)?;
        if header.sub_protocol != SubProtocol::Audio {
            return Err(VbanError::WrongSubProtocol {
                expected: SubProtocol::Audio,
                got: header.sub_protocol,
            });
        }
        if header.codec != Codec::Pcm {
            return Err(VbanError::UnsupportedCodec(header.codec.code()));
        }
        let fmt = header.format;
        let bps = fmt
            .bytes_per_sample()
            .ok_or(VbanError::UnsupportedFormat(fmt))?;
        let block = bps * header.channels as usize;
        if block == 0 || payload.len() % block != 0 {
            return Err(VbanError::BadPayloadLength {
                len: payload.len(),
                block,
            });
        }
        Ok(Self {
            header,
            data: payload.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::VbanError;

    fn pcm_frame() -> AudioFrame {
        let mut f = AudioFrame::pcm16("Mikrofon", SampleRate::Hz48000, 2).expect("Frame");
        f.push_i16(&[1000, -1000, 2000, -2000]).expect("push");
        f
    }

    #[test]
    fn pcm16_roundtrip_ueber_die_leitung() {
        let frame = pcm_frame();
        let bytes = frame.encode().expect("encode");
        assert_eq!(bytes.len(), HEADER_LEN + 8);
        let back = AudioFrame::parse(&bytes).expect("parse");
        assert_eq!(back, frame);
        assert_eq!(back.samples_i16().unwrap(), vec![1000, -1000, 2000, -2000]);
        assert_eq!(back.sample_frames(), 2);
        assert_eq!(back.sample_count(), 4);
    }

    #[test]
    fn pcm16_grenzwerte() {
        let mut f = AudioFrame::pcm16("Grenzen", SampleRate::Hz96000, 1).unwrap();
        f.push_i16(&[i16::MIN, -1, 0, 1, i16::MAX]).unwrap();
        let back = AudioFrame::parse(&f.encode().unwrap()).unwrap();
        assert_eq!(
            back.samples_i16().unwrap(),
            vec![i16::MIN, -1, 0, 1, i16::MAX]
        );
    }

    #[test]
    fn float32_roundtrip() {
        let mut f = AudioFrame::float32("Float", SampleRate::Hz192000, 2).unwrap();
        f.push_f32(&[0.0, 0.5, -0.5, 1.0]).unwrap();
        let bytes = f.encode().unwrap();
        let back = AudioFrame::parse(&bytes).unwrap();
        assert_eq!(back.header().format, AudioFormat::F32);
        assert_eq!(back.samples_f32().unwrap(), vec![0.0, 0.5, -0.5, 1.0]);
        assert_eq!(back, f);
    }

    #[test]
    fn float32_bits_bleiben_unveraendert() {
        let mut f = AudioFrame::float32("Bits", SampleRate::Hz48000, 1).unwrap();
        let samples = [f32::NEG_INFINITY, -0.0, f32::MIN_POSITIVE, 1234.5678];
        f.push_f32(&samples).unwrap();
        let back = AudioFrame::parse(&f.encode().unwrap()).unwrap();
        let got = back.samples_f32().unwrap();
        for (a, b) in samples.iter().zip(got.iter()) {
            assert_eq!(a.to_bits(), b.to_bits());
        }
    }

    #[test]
    fn interleaving_ist_l_r_l_r() {
        let mut f = AudioFrame::pcm16("Inter", SampleRate::Hz48000, 2).unwrap();
        f.push_i16(&[1, 2, 3, 4]).unwrap();
        let data = f.payload();
        assert_eq!(
            data,
            &[
                1u8, 0, // L
                2, 0, // R
                3, 0, // L
                4, 0, // R
            ]
        );
    }

    #[test]
    fn zaehler_laeuft_ueber() {
        let mut f = pcm_frame();
        f.set_frame_counter(u32::MAX);
        assert_eq!(f.advance_counter(), 0);
        assert_eq!(f.frame_counter(), 0);
        assert_eq!(f.advance_counter(), 1);

        f.set_frame_counter(u32::MAX);
        let back = AudioFrame::parse(&f.encode().unwrap()).unwrap();
        assert_eq!(back.frame_counter(), u32::MAX);
    }

    #[test]
    fn zaehler_ueber_leerem_rahmen_ueber_wrap() {
        // Wrap-around auch bei vielen Schritten (2^32 + 3).
        let mut f = pcm_frame();
        f.set_frame_counter(0xFFFF_FFFE);
        assert_eq!(f.advance_counter_by(3), 1);
        assert_eq!(f.advance_counter_by(u32::MAX), 0);
    }

    #[test]
    fn falscher_sample_typ_wird_abgelehnt() {
        let mut f = AudioFrame::pcm16("Typ", SampleRate::Hz48000, 1).unwrap();
        assert!(matches!(
            f.push_f32(&[0.5]),
            Err(VbanError::FormatMismatch {
                expected: AudioFormat::F32,
                got: AudioFormat::I16
            })
        ));
        let mut g = AudioFrame::float32("Typ", SampleRate::Hz48000, 1).unwrap();
        assert!(matches!(
            g.push_i16(&[1]),
            Err(VbanError::FormatMismatch {
                expected: AudioFormat::I16,
                got: AudioFormat::F32
            })
        ));
        assert!(matches!(
            f.samples_f32(),
            Err(VbanError::FormatMismatch { .. })
        ));
    }

    #[test]
    fn sample_anzahl_muss_zur_kanalzahl_passen() {
        let mut f = AudioFrame::pcm16("Kanaele", SampleRate::Hz48000, 2).unwrap();
        assert!(matches!(
            f.push_i16(&[1, 2, 3]),
            Err(VbanError::OddSampleCount {
                samples: 3,
                channels: 2
            })
        ));
        assert!(f.payload().is_empty(), "nichts darf halb geschrieben sein");
    }

    #[test]
    fn leere_frames_sind_gueltig() {
        let f = AudioFrame::pcm16("Leer", SampleRate::Hz48000, 2).unwrap();
        let bytes = f.encode().unwrap();
        assert_eq!(bytes.len(), HEADER_LEN);
        let back = AudioFrame::parse(&bytes).unwrap();
        assert_eq!(back.sample_frames(), 0);
        assert!(back.payload().is_empty());
    }

    #[test]
    fn nutzdaten_ohne_vielfaches_wird_abgelehnt() {
        let frames = pcm_frame();
        let mut bytes = frames.encode().unwrap();
        bytes.push(0xAA); // ein Byte zu viel fuer 2 Kanaele * 2 Byte
        match AudioFrame::parse(&bytes) {
            Err(VbanError::BadPayloadLength { len, block }) => {
                assert_eq!(len, 9);
                assert_eq!(block, 4);
            }
            other => panic!("falsches Ergebnis: {other:?}"),
        }
    }

    #[test]
    fn nur_audio_sub_protokoll_wird_geparst() {
        let mut f = pcm_frame();
        f.header_mut().sub_protocol = SubProtocol::Text;
        let bytes = f.encode().unwrap();
        assert!(matches!(
            AudioFrame::parse(&bytes),
            Err(VbanError::WrongSubProtocol {
                expected: SubProtocol::Audio,
                got: SubProtocol::Text
            })
        ));
    }

    #[test]
    fn byte_5_traegt_samples_pro_kanal_minus_eins() {
        // Zwei Stereo-Frames -> 2 Samples pro Kanal -> Byte 5 = 1.
        let f = pcm_frame();
        let bytes = f.encode().expect("encode");
        assert_eq!(bytes[5], 1);

        // Ein Sample pro Kanal -> 0.
        let mut mono = AudioFrame::float32("Mono", SampleRate::Hz48000, 1).unwrap();
        mono.push_f32(&[0.5]).unwrap();
        assert_eq!(mono.encode().unwrap()[5], 0);

        // 256 Samples pro Kanal passen exakt (Feld ist 8 Bit).
        let mut viel = AudioFrame::float32("Voll", SampleRate::Hz48000, 1).unwrap();
        viel.push_f32(&vec![0.0; 256]).unwrap();
        assert_eq!(viel.encode().unwrap()[5], 255);

        // Mehr als 256 werden gesaettigt; die Laenge steht in den Nutzdaten.
        let mut mehr = AudioFrame::float32("Mehr", SampleRate::Hz48000, 1).unwrap();
        mehr.push_f32(&vec![0.0; 300]).unwrap();
        let bytes = mehr.encode().unwrap();
        assert_eq!(bytes[5], 255);
        assert_eq!(AudioFrame::parse(&bytes).unwrap().sample_frames(), 300);
    }

    #[test]
    fn zu_kurzes_paket_wird_abgelehnt() {
        let bytes = pcm_frame().encode().unwrap();
        assert!(matches!(
            AudioFrame::parse(&bytes[..HEADER_LEN - 1]),
            Err(VbanError::TooShort { .. })
        ));
    }

    #[test]
    fn gepackte_formate_werden_nicht_erzeugt() {
        assert!(matches!(
            AudioFrame::with_format("X", SampleRate::Hz48000, 2, AudioFormat::I12),
            Err(VbanError::UnsupportedFormat(AudioFormat::I12))
        ));
    }

    #[test]
    fn nicht_ascii_name_wird_abgelehnt() {
        let mut f = pcm_frame();
        f.header_mut().stream_name = String::from("Mikro");
        assert!(f.encode().is_ok());
        f.header_mut().stream_name = String::from("Mikrö");
        assert!(matches!(f.encode(), Err(VbanError::InvalidStreamName(_))));
    }
}
