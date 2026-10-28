//! Duenner UDP-Layer auf `std::net::UdpSocket`.
//!
//! Kein eigener Thread, keine Puffer-Queue: der Aufrufer entscheidet, wann
//! gesendet bzw. empfangen wird. Beide Typen erlauben das Binden an ein
//! bestimmtes Interface (IP) und das Setzen eines Timeouts; damit laesst sich
//! der Empfaenger ohne Blockade in eine bestehende Schleife einhaengen.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

use crate::audio::AudioFrame;
use crate::error::VbanError;
use crate::header::HEADER_LEN;

/// Standard-Port der VBAN-Spezifikation.
pub const DEFAULT_PORT: u16 = 6980;

/// Standard-Stream-Name von Voicemeeter (nur als Hinweis, nicht erzwungen).
pub const DEFAULT_STREAM_NAME: &str = "Command1";

/// Empfangspuffergroesse; deckt die ueblichen MTU-Grenzen ab.
pub const DEFAULT_BUFFER_SIZE: usize = 2048;

/// UDP-Sender fuer VBAN-Pakete.
#[derive(Debug)]
pub struct Sender {
    socket: UdpSocket,
    target: SocketAddr,
}

impl Sender {
    /// Bindet auf `0.0.0.0:0` (beliebiger Port, alle Interfaces) und merkt sich
    /// das Ziel.
    pub fn new(target: SocketAddr) -> io::Result<Self> {
        Self::bind_iface(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0, target)
    }

    /// Bindet explizit an ein Interface (IP) und einen lokalen Port.
    ///
    /// Fuer Broadcast-Ziele muss zusaetzlich [`Sender::set_broadcast`] aktiviert
    /// werden.
    pub fn bind_iface(local_ip: IpAddr, local_port: u16, target: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::new(local_ip, local_port))?;
        Ok(Self { socket, target })
    }

    /// Bindet an ein Interface und nutzt den VBAN-Standardport als Zielport.
    pub fn to_iface(local_ip: IpAddr, target_ip: IpAddr) -> io::Result<Self> {
        Self::bind_iface(local_ip, 0, SocketAddr::new(target_ip, DEFAULT_PORT))
    }

    /// Lokale Adresse (nach dem Binden auslesbar, z. B. fuer Tests).
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Zieladresse.
    pub fn target(&self) -> SocketAddr {
        self.target
    }

    /// Erlaubt/verbietet Broadcast auf diesem Socket.
    pub fn set_broadcast(&self, on: bool) -> io::Result<()> {
        self.socket.set_broadcast(on)
    }

    /// Sendet einen fertig kodierten Puffer (z. B. aus [`AudioFrame::encode`]).
    pub fn send_bytes(&self, bytes: &[u8]) -> io::Result<usize> {
        self.socket.send_to(bytes, self.target)
    }

    /// Prueft den Puffer minimal (Magic + Laenge) und sendet ihn dann.
    pub fn send_raw(&self, bytes: &[u8]) -> Result<usize, VbanError> {
        if bytes.len() < HEADER_LEN {
            return Err(VbanError::TooShort {
                got: bytes.len(),
                need: HEADER_LEN,
            });
        }
        if bytes[0..4] != crate::header::MAGIC {
            let mut magic = [0u8; 4];
            magic.copy_from_slice(&bytes[0..4]);
            return Err(VbanError::BadMagic(magic));
        }
        Ok(self.socket.send_to(bytes, self.target)?)
    }

    /// Kodiert den Frame (inkl. Validierung des Headers) und sendet ihn.
    pub fn send_frame(&self, frame: &AudioFrame) -> Result<usize, VbanError> {
        let bytes = frame.encode()?;
        Ok(self.socket.send_to(&bytes, self.target)?)
    }
}

/// UDP-Empfaenger fuer VBAN-Pakete mit wiederverwendetem Puffer.
#[derive(Debug)]
pub struct Receiver {
    socket: UdpSocket,
    buf: Vec<u8>,
}

impl Receiver {
    /// Bindet auf die angegebene Adresse (Port 0 = vom System vergeben).
    pub fn bind(addr: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(addr)?;
        Ok(Self {
            socket,
            buf: vec![0u8; DEFAULT_BUFFER_SIZE],
        })
    }

    /// Bindet an ein bestimmtes Interface und einen Port.
    pub fn bind_iface(ip: IpAddr, port: u16) -> io::Result<Self> {
        Self::bind(SocketAddr::new(ip, port))
    }

    /// Bindet an alle Interfaces auf dem VBAN-Standardport.
    pub fn bind_default_port() -> io::Result<Self> {
        Self::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            DEFAULT_PORT,
        ))
    }

    /// Setzt die Puffergroesse fuer eingehende Datagramme.
    pub fn with_buffer_size(mut self, size: usize) -> Self {
        self.buf = vec![0u8; size.max(HEADER_LEN)];
        self
    }

    /// Lokale Adresse inkl. tatsaechlichem Port.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Setzt das Lesetimeout (`None` = blockierend).
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.socket.set_read_timeout(timeout)
    }

    /// Empfaengt ein Datagramm und gibt Laenge und Absender zurueck; ein
    /// Timeout wird als [`io::ErrorKind::WouldBlock`]/`TimedOut` durchgereicht.
    pub fn recv_raw(&mut self) -> io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(&mut self.buf)
    }

    /// Empfaengt und parst einen Audio-Frame.
    ///
    /// `Ok(None)` bedeutet "Timeout, kein Paket" – das ist der Normalfall in
    /// einer Abfrageschleife und kein Fehler. Falsche oder fremde Datagramme
    /// liefern dagegen `Err`.
    pub fn recv_frame(&mut self) -> Result<Option<(AudioFrame, SocketAddr)>, VbanError> {
        let (n, src) = match self.socket.recv_from(&mut self.buf) {
            Ok(v) => v,
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                return Ok(None)
            }
            Err(e) => return Err(VbanError::Io(e)),
        };
        let frame = AudioFrame::parse(&self.buf[..n])?;
        Ok(Some((frame, src)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::SampleRate;
    use std::time::Instant;

    fn loopback_frame() -> AudioFrame {
        let mut f = AudioFrame::pcm16("Loopback", SampleRate::Hz48000, 2).expect("Frame");
        f.push_i16(&[100, -100, 200, -200]).expect("push");
        f.set_frame_counter(4711);
        f
    }

    #[test]
    fn udp_loopback_ueber_127_0_0_1() {
        let mut rx = Receiver::bind("127.0.0.1:0".parse().unwrap()).expect("bind rx");
        rx.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("timeout");
        let addr = rx.local_addr().expect("local_addr");

        let tx = Sender::bind_iface(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            0,
            addr, // Ziel = Empfaenger
        )
        .expect("bind tx");

        let frame = loopback_frame();
        let sent = tx.send_frame(&frame).expect("send");
        assert_eq!(sent, HEADER_LEN + 8);

        let (got, src) = rx
            .recv_frame()
            .expect("recv")
            .expect("Paket erwartet, Timeout bekommen");
        assert_eq!(got, frame);
        assert_eq!(got.samples_i16().unwrap(), vec![100, -100, 200, -200]);
        assert_eq!(got.frame_counter(), 4711);
        assert_eq!(src.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn timeout_liefert_none_statt_fehler() {
        let mut rx = Receiver::bind("127.0.0.1:0".parse().unwrap()).expect("bind");
        rx.set_read_timeout(Some(Duration::from_millis(80)))
            .expect("timeout");
        let start = Instant::now();
        let result = rx.recv_frame().expect("Timeout ist kein Fehler");
        let elapsed = start.elapsed();
        assert!(result.is_none());
        assert!(
            elapsed >= Duration::from_millis(50) && elapsed < Duration::from_secs(2),
            "unerwartete Wartezeit: {elapsed:?}"
        );
    }

    #[test]
    fn fremdes_paket_wird_als_fehler_gemeldet() {
        let mut rx = Receiver::bind("127.0.0.1:0".parse().unwrap()).expect("bind");
        rx.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("timeout");
        let addr = rx.local_addr().unwrap();
        let tx = Sender::new(addr).expect("bind tx");
        tx.send_bytes(b"kein VBAN").expect("send");

        match rx.recv_frame() {
            Err(VbanError::TooShort { got, need }) => {
                assert_eq!(got, 9);
                assert_eq!(need, HEADER_LEN);
            }
            other => panic!("falsches Ergebnis: {other:?}"),
        }
    }

    #[test]
    fn send_raw_prueft_magic_und_laenge() {
        let tx = Sender::new("127.0.0.1:6980".parse().unwrap()).expect("bind");
        assert!(matches!(
            tx.send_raw(b"kurz"),
            Err(VbanError::TooShort { got: 4, need: 28 })
        ));
        let mut bytes = loopback_frame().encode().unwrap();
        bytes[0] = b'X';
        assert!(matches!(tx.send_raw(&bytes), Err(VbanError::BadMagic(_))));
    }

    #[test]
    fn mehrere_frames_mit_zaehler_wrap_ueber_socket() {
        let mut rx = Receiver::bind("127.0.0.1:0".parse().unwrap()).expect("bind");
        rx.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("timeout");
        let tx = Sender::new(rx.local_addr().unwrap()).expect("bind tx");

        let mut frame = loopback_frame();
        frame.set_frame_counter(u32::MAX);
        tx.send_frame(&frame).expect("send 1");

        let mut nxt = frame.clone();
        nxt.advance_counter();
        tx.send_frame(&nxt).expect("send 2");

        let (first, _) = rx.recv_frame().unwrap().expect("erstes Paket");
        let (second, _) = rx.recv_frame().unwrap().expect("zweites Paket");
        assert_eq!(first.frame_counter(), u32::MAX);
        assert_eq!(second.frame_counter(), 0);
    }
}
