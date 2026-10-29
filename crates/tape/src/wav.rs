//! Wave-Format: was `hound` wissen muss, und die Sample-Kodierung davor.
//!
//! Der RIFF-Kopf und das Nachpatchen der Größen beim Schließen macht `hound`
//! ([`hound::WavWriter`]); hier steht nur noch, welches `WavSpec` zu einem
//! [`Format`] gehört, wie ein f32-Sample auf 24 bit quantisiert wird und
//! welche Längen in ein u32-Feld des Kopfes passen.
//!
//! **Kopf-Layout:** `hound` schreibt für mehr als zwei Kanäle oder mehr als
//! 16 bit `WAVE_FORMAT_EXTENSIBLE` (Format-Tag 0xFFFE, 40-Byte-`fmt`-Chunk,
//! `data` ab Byte 68), sonst das klassische 44-Byte-Layout. Beides ist
//! standardkonform und wird von DAWs, `ffmpeg` und dem Voicemeeter-Import
//! gelesen; die Kanalzahl steht in beiden Fällen an Offset 22, die
//! `data`-Länge wird über die Chunk-Kette gefunden (siehe `tests/wav_check.py`).

use crate::Format;

/// Block-Align: Bytes eines Frames (alle Kanäle).
pub const fn block_align(format: Format, channels: u16) -> u16 {
    channels * format.bits_per_sample() / 8
}

/// Sample-Rate, Kanalzahl und Bittiefe als `hound`-Spezifikation.
pub fn spec(format: Format, sample_rate: u32, channels: u16) -> hound::WavSpec {
    hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: format.bits_per_sample(),
        sample_format: match format {
            Format::Pcm24 => hound::SampleFormat::Int,
            Format::Float32 => hound::SampleFormat::Float,
        },
    }
}

/// Ein f32-Sample als 24-bit-PCM-Wert.
///
/// Skalenfaktor 2^23 − 1: `[-1.0, 1.0]` bildet auf `[-8388607, 8388607]` ab,
/// also immer innerhalb des Wertebereichs. Werte darüber werden geklemmt,
/// nicht-endliche (NaN/Inf) werden 0.
pub fn pcm24(v: f32) -> i32 {
    const SCALE: f32 = 8_388_607.0;
    if !v.is_finite() {
        return 0;
    }
    (v.clamp(-1.0, 1.0) * SCALE).round() as i32
}

/// Ein f32-Sample als IEEE-float32-Wert (nicht-endliche werden 0).
pub fn float32(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// Prueft, ob `data_len` noch in ein u32-Feld des RIFF-Kopfes passt.
pub const fn data_len_fits(data_len: u64) -> bool {
    data_len <= (u32::MAX - 36) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Schreibt zwei Samples und liest die Datei als Bytes zurück.
    fn datei(format: Format, channels: u16, samples: &[f32]) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!(
            "tape-wav-{}-{}-{channels}",
            std::process::id(),
            format.name()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let pfad = dir.join("probe.wav");
        let mut w = hound::WavWriter::create(&pfad, spec(format, 48_000, channels)).unwrap();
        for &s in samples {
            match format {
                Format::Pcm24 => w.write_sample(pcm24(s)).unwrap(),
                Format::Float32 => w.write_sample(float32(s)).unwrap(),
            }
        }
        w.finalize().unwrap();
        let mut b = Vec::new();
        std::fs::File::open(&pfad)
            .unwrap()
            .read_to_end(&mut b)
            .unwrap();
        b
    }

    fn u16_at(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes([b[o], b[o + 1]])
    }
    fn u32_at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }

    /// (Offset des data-Chunks, Länge) aus der Chunk-Kette.
    fn data_chunk(b: &[u8]) -> (usize, usize) {
        let mut i = 12usize;
        while i + 8 <= b.len() {
            let len = u32_at(b, i + 4) as usize;
            if &b[i..i + 4] == b"data" {
                return (i + 8, len);
            }
            i += 8 + len + (len & 1);
        }
        panic!("kein data-Chunk");
    }

    #[test]
    fn kopf_pcm24_stereo() {
        // 100 Frames * 2 Kanäle * 3 Byte = 600 Byte Nutzlast
        let b = datei(Format::Pcm24, 2, &vec![0.0f32; 200]);
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(&b[8..12], b"WAVE");
        assert_eq!(&b[12..16], b"fmt ");
        assert_eq!(u16_at(&b, 22), 2, "Kanalzahl");
        assert_eq!(u32_at(&b, 24), 48_000, "Samplerate");
        assert_eq!(u16_at(&b, 32), 6, "Block-Align = 2 * 3");
        assert_eq!(u32_at(&b, 28), 48_000 * 6, "byte_rate");
        assert_eq!(u16_at(&b, 34), 24, "Bittiefe");
        let (off, len) = data_chunk(&b);
        assert_eq!(len, 600);
        assert_eq!(off + len, b.len(), "Kopf-Groessen passen zur Datei");
        assert_eq!(u32_at(&b, 4) as usize + 8, b.len(), "riff_size");
        // 24 bit ist mehr als 16: hound schreibt WAVE_FORMAT_EXTENSIBLE.
        assert_eq!(u16_at(&b, 20), 0xFFFE, "Format-Tag EXTENSIBLE");
    }

    #[test]
    fn kopf_pcm24_quad_ist_extensible() {
        let b = datei(Format::Pcm24, 4, &vec![0.0f32; 4]);
        assert_eq!(u16_at(&b, 22), 4);
        assert_eq!(u16_at(&b, 20), 0xFFFE);
        let (off, len) = data_chunk(&b);
        assert_eq!(len, 4 * 3);
        assert_eq!(off, 68, "40-Byte-fmt-Chunk");
    }

    #[test]
    fn kopf_float32() {
        let b = datei(Format::Float32, 2, &[0.5, -0.25]);
        assert_eq!(u16_at(&b, 34), 32, "Bittiefe");
        assert_eq!(u16_at(&b, 32), 8, "Block-Align = 2 * 4");
        assert_eq!(u32_at(&b, 28), 48_000 * 8);
        let (_, len) = data_chunk(&b);
        assert_eq!(len, 8);
    }

    #[test]
    fn pcm24_werte() {
        assert_eq!(pcm24(0.0), 0);
        assert_eq!(pcm24(1.0), 8_388_607, "+Vollaussteuerung");
        assert_eq!(pcm24(-1.0), -8_388_607, "-Vollaussteuerung");
        assert_eq!(pcm24(0.5), 4_194_304, "halbe Aussteuerung");
        assert_eq!(pcm24(2.0), 8_388_607, "geklemmt");
        assert_eq!(pcm24(-2.0), -8_388_607, "geklemmt");
        assert_eq!(pcm24(f32::NAN), 0);
        assert_eq!(pcm24(f32::INFINITY), 0);
    }

    #[test]
    fn pcm24_bytes_klein_endian() {
        let b = datei(Format::Pcm24, 1, &[1.0, -1.0, 0.0]);
        let (off, _) = data_chunk(&b);
        assert_eq!(&b[off..off + 3], &[0xFF, 0xFF, 0x7F]);
        assert_eq!(&b[off + 3..off + 6], &[0x01, 0x00, 0x80]);
        assert_eq!(&b[off + 6..off + 9], &[0x00, 0x00, 0x00]);
    }

    #[test]
    fn float32_roundtrip_und_nan() {
        let b = datei(Format::Float32, 1, &[0.5, -0.25, f32::NAN]);
        let (off, _) = data_chunk(&b);
        let lies = |i: usize| {
            f32::from_le_bytes([
                b[off + i * 4],
                b[off + i * 4 + 1],
                b[off + i * 4 + 2],
                b[off + i * 4 + 3],
            ])
        };
        assert_eq!(lies(0), 0.5);
        assert_eq!(lies(1), -0.25);
        assert_eq!(lies(2), 0.0, "NaN wird 0");
    }

    #[test]
    fn block_align_und_grenzen() {
        assert_eq!(block_align(Format::Pcm24, 2), 6);
        assert_eq!(block_align(Format::Float32, 4), 16);
        assert!(data_len_fits(512 * 1024 * 1024));
        assert!(!data_len_fits(u32::MAX as u64));
    }
}
