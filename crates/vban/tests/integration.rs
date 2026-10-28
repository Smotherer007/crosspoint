//! Integrationstests von aussen (nur ueber die oeffentliche API).
//!
//! Damit ist belegt, dass die spaetere Nutzung als Workspace-Member ohne
//! crate-interne Tricks funktioniert.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use vban::{AudioFrame, AudioFormat, Receiver, Sender, SubProtocol, VbanError, HEADER_LEN};

#[test]
fn oeffentliche_api_frame_roundtrip() {
    let mut frame = AudioFrame::pcm16("Integration", vban::SampleRate::Hz48000, 2).unwrap();
    assert_eq!(frame.header().sub_protocol, SubProtocol::Audio);
    assert_eq!(frame.header().format, AudioFormat::I16);
    frame.push_i16(&[1, 2, 3, 4]).unwrap();
    frame.advance_counter();

    let bytes = frame.encode().unwrap();
    assert_eq!(&bytes[0..4], b"VBAN");
    // Byte 4: Sub-Protokoll Audio (0) in Bits 5..7, Rate 48000 = Index 3.
    assert_eq!(bytes[4], 0x03);
    // Byte 5: 2 Sample-Frames je Kanal -> 1.
    assert_eq!(bytes[5], 0x01);
    assert_eq!(bytes[6], 0x01, "2 Kanaele minus eins");
    assert_eq!(bytes[7], 0x01, "Datentyp I16, Codec PCM");
    assert_eq!(&bytes[8..19], b"Integration");
    assert_eq!(bytes[19], 0, "Namensfeld ist NUL-gefuellt");
    assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 1);
    assert_eq!(bytes.len(), HEADER_LEN + 8);

    let back = AudioFrame::parse(&bytes).unwrap();
    assert_eq!(back.header(), frame.header());
    assert_eq!(back.samples_i16().unwrap(), vec![1, 2, 3, 4]);
}

#[test]
fn fehlertexte_sind_verstaendlich() {
    let err = AudioFrame::parse(b"VBAN").unwrap_err();
    let text = err.to_string();
    assert!(text.contains("zu kurz"), "unerwartet: {text}");
    assert!(matches!(err, VbanError::TooShort { got: 4, need: 28 }));
    assert!(std::error::Error::source(&err).is_none());
}

#[test]
fn udp_ende_zu_ende() {
    let mut rx = Receiver::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    rx.set_read_timeout(Some(Duration::from_millis(1000))).unwrap();
    let target = rx.local_addr().unwrap();

    let tx = Sender::bind_iface(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, target).unwrap();
    assert_eq!(tx.target(), target);

    let mut frame = AudioFrame::float32("E2E", vban::SampleRate::Hz96000, 1).unwrap();
    frame.push_f32(&[0.25, -0.25]).unwrap();
    let sent = tx.send_frame(&frame).unwrap();
    assert_eq!(sent, HEADER_LEN + 8);

    let (got, _src) = rx.recv_frame().unwrap().expect("Paket erwartet");
    assert_eq!(got.header(), frame.header());
    assert_eq!(got.samples_f32().unwrap(), vec![0.25, -0.25]);
}
