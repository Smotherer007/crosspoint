//! RIFF/WAVE-Kopf und Sample-Kodierung.
//!
//! Layout (kanonisch, 44 Byte, ohne Zusatz-Chunks):
//!
//! ```text
//!  0  "RIFF" |  4  riff_size (u32)   |  8  "WAVE"
//! 12  "fmt " | 16  fmt_size = 16     | 20  format_tag (1 = PCM, 3 = IEEE float)
//! 22  kanaele (u16)                  | 24  samplerate (u32)
//! 28  byte_rate = rate * block_align | 32  block_align (u16)
//! 34  bits_per_sample (u16)          | 36  "data" | 40 data_size (u32)
//! ```
//!
//! `riff_size` = Dateigroesse - 8 = 36 + data_size (+ 1 Pad-Byte, falls
//! data_size ungerade ist; RIFF-Chunks muessen wortausgerichtet sein).

use crate::Format;

/// Laenge des WAV-Kopfes in Byte.
pub const HEADER_LEN: usize = 44;
/// Groesse des `fmt `-Chunks (ohne Chunk-Kopf).
const FMT_CHUNK_LEN: u32 = 16;

/// Block-Align: Bytes eines Frames (alle Kanaele).
pub const fn block_align(format: Format, channels: u16) -> u16 {
    channels * format.bits_per_sample() / 8
}

/// Baut den 44-Byte-Kopf. `data_len` ist die Nutzlast des `data`-Chunks in Byte,
/// `pad` sagt, ob nach der Nutzlast ein Fuellbyte steht (nur bei ungerader Laenge).
pub fn build_header(
    format: Format,
    sample_rate: u32,
    channels: u16,
    data_len: u32,
    pad: bool,
) -> [u8; HEADER_LEN] {
    let bps = format.bits_per_sample();
    let align = block_align(format, channels);
    let byte_rate = sample_rate.saturating_mul(align as u32);
    let riff_size = 36u32
        .saturating_add(data_len)
        .saturating_add(if pad { 1 } else { 0 });

    let mut h = [0u8; HEADER_LEN];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&riff_size.to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&FMT_CHUNK_LEN.to_le_bytes());
    h[20..22].copy_from_slice(&format.format_tag().to_le_bytes());
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&align.to_le_bytes());
    h[34..36].copy_from_slice(&bps.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

/// Prueft, ob `data_len` (+ Pad) noch in ein u32-Feld des RIFF-Kopfes passt.
pub const fn data_len_fits(data_len: u64, pad: bool) -> bool {
    data_len + if pad { 1 } else { 0 } <= (u32::MAX - 36) as u64
}

/// Kodiert interleaved f32-Samples in `dst` (klein-endian) und liefert
/// (geschriebene Bytes, Peak als Betragsmaximum der *Eingabe*).
///
/// Der Peak wird vor der Klemmung/Quantisierung gemessen: Werte ueber 1.0
/// zeigen Uebersteuerung an. Nicht-endliche Samples (NaN/Inf) werden als 0.0
/// geschrieben und beeinflussen den Peak nicht.
///
/// Keine Allokation: `dst` muss mindestens
/// `src.len() * bytes_per_sample(format)` Byte gross sein.
pub fn encode(format: Format, src: &[f32], dst: &mut [u8]) -> (usize, f32) {
    let mut peak = 0.0f32;
    match format {
        Format::Float32 => {
            for (i, s) in src.iter().enumerate() {
                let v = if s.is_finite() { *s } else { 0.0 };
                let a = v.abs();
                if a > peak {
                    peak = a;
                }
                dst[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            (src.len() * 4, peak)
        }
        Format::Pcm24 => {
            // 24-bit signed, Skalenfaktor 2^23 - 1: [-1.0, 1.0] bildet auf
            // [-8388607, 8388607] ab, also immer innerhalb des Wertebereichs.
            const SCALE: f32 = 8_388_607.0;
            for (i, s) in src.iter().enumerate() {
                let raw = if s.is_finite() { *s } else { 0.0 };
                let a = raw.abs();
                if a > peak {
                    peak = a;
                }
                let v = raw.clamp(-1.0, 1.0);
                let q = (v * SCALE).round() as i32;
                let b = q.to_le_bytes();
                dst[i * 3] = b[0];
                dst[i * 3 + 1] = b[1];
                dst[i * 3 + 2] = b[2];
            }
            (src.len() * 3, peak)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(b: &[u8], o: usize) -> u16 {
        u16::from_le_bytes([b[o], b[o + 1]])
    }
    fn u32_at(b: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    }

    #[test]
    fn kopf_pcm24_stereo() {
        // 48000 Hz, 2 Kanaele, 24 bit, 100 Frames = 600 Byte Nutzlast
        let data = 100u32 * 2 * 3;
        let h = build_header(Format::Pcm24, 48_000, 2, data, false);

        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(&h[12..16], b"fmt ");
        assert_eq!(&h[36..40], b"data");
        assert_eq!(u32_at(&h, 16), 16, "fmt-Chunk soll 16 Byte sein");
        assert_eq!(u16_at(&h, 20), 1, "PCM muss Format-Tag 1 haben");
        assert_eq!(u16_at(&h, 22), 2);
        assert_eq!(u32_at(&h, 24), 48_000);
        assert_eq!(u16_at(&h, 32), 6, "Block-Align = 2 * 3");
        assert_eq!(u32_at(&h, 28), 48_000 * 6, "byte_rate");
        assert_eq!(u16_at(&h, 34), 24);
        assert_eq!(u32_at(&h, 40), data);
        assert_eq!(u32_at(&h, 4), 36 + 600, "riff_size = Dateigroesse - 8");
        assert_eq!(HEADER_LEN as u32 + data, u32_at(&h, 4) + 8);
    }

    #[test]
    fn kopf_float32_quad() {
        let h = build_header(Format::Float32, 44_100, 4, 0, false);
        assert_eq!(u16_at(&h, 20), 3, "IEEE float muss Format-Tag 3 haben");
        assert_eq!(u16_at(&h, 32), 16, "Block-Align = 4 * 4");
        assert_eq!(u16_at(&h, 34), 32);
        assert_eq!(u32_at(&h, 28), 44_100 * 16);
        assert_eq!(u32_at(&h, 4), 36, "leerer data-Chunk: riff_size = 36");
    }

    #[test]
    fn ungerade_nutzlast_bekommt_pad() {
        let h = build_header(Format::Pcm24, 16_000, 1, 15, true);
        assert_eq!(u32_at(&h, 40), 15);
        assert_eq!(u32_at(&h, 4), 36 + 15 + 1);
        assert_eq!(u32_at(&h, 4) % 2, 0, "riff_size muss gerade sein");
    }

    #[test]
    fn encode_pcm24_werte() {
        let src = [0.0f32, 1.0, -1.0];
        let mut dst = [0u8; 9];
        let (n, peak) = encode(Format::Pcm24, &src, &mut dst);
        assert_eq!(n, 9);
        assert_eq!(peak, 1.0);
        assert_eq!(&dst[0..3], &[0x00, 0x00, 0x00], "0.0");
        assert_eq!(&dst[3..6], &[0xFF, 0xFF, 0x7F], "+Vollaussteuerung");
        assert_eq!(&dst[6..9], &[0x01, 0x00, 0x80], "-Vollaussteuerung");
    }

    #[test]
    fn encode_klemmt_und_ignoriert_nan() {
        let src = [f32::NAN, 2.0, -2.0, f32::INFINITY];
        let mut dst = [0u8; 12];
        let (_, peak) = encode(Format::Pcm24, &src, &mut dst);
        assert_eq!(&dst[0..3], &[0, 0, 0], "NaN -> 0");
        assert_eq!(&dst[3..6], &[0xFF, 0xFF, 0x7F], "2.0 geklemmt auf +max");
        assert_eq!(&dst[6..9], &[0x01, 0x00, 0x80], "-2.0 geklemmt auf -max");
        assert_eq!(&dst[9..12], &[0, 0, 0], "Inf -> 0");
        assert_eq!(peak, 2.0, "Peak ist der Eingabe-Spitzenwert vor der Klemmung");
    }

    #[test]
    fn encode_float32_roundtrip() {
        let src = [0.5f32, -0.25, 0.0];
        let mut dst = [0u8; 12];
        let (n, peak) = encode(Format::Float32, &src, &mut dst);
        assert_eq!(n, 12);
        assert_eq!(peak, 0.5);
        for (i, s) in src.iter().enumerate() {
            let v = f32::from_le_bytes([dst[i * 4], dst[i * 4 + 1], dst[i * 4 + 2], dst[i * 4 + 3]]);
            assert_eq!(v, *s);
        }
    }

    #[test]
    fn grosse_laengen_passen_nicht_mehr_in_u32() {
        assert!(data_len_fits(512 * 1024 * 1024, false));
        assert!(!data_len_fits(u32::MAX as u64, false));
    }
}
