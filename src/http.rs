//! Steuer-API über HTTP (JSON) und ein WebSocket für die Pegelanzeige.
//! Parameteränderungen gehen als POST an `/api/set`, den Zustand liefert
//! `/api/state`. Die Pegel schickt `/ws/levels` 40×/s von sich aus (ein
//! eigener Thread, nie der Audio-Callback); `/api/levels` bleibt als
//! HTTP-Abfrage und als Rückfallebene erhalten.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::engine::Params;
use crate::json::{self, Value};
use crate::devices::{DeviceMap, RealRunner};
use crate::macros_io::MacroControl;
use crate::tape_io::TapeControl;
use crate::vban_io::{VbanControl, VbanRecvControl};
use std::sync::Mutex;

const INDEX_HTML: &str = include_str!("../web/index.html");

pub fn serve(
    listen: String,
    params: Arc<Params>,
    preset: PathBuf,
    tape: Arc<TapeControl>,
    vban: Arc<VbanControl>,
    vban_recv: Arc<VbanRecvControl>,
    macros: Arc<MacroControl>,
    devices: Arc<Mutex<DeviceMap>>,
) -> io::Result<()> {
    let listener = TcpListener::bind(&listen)?;
    println!("UI: http://{listen}/  (Pegel-WebSocket: ws://{listen}/ws/levels)");
    let hub = Arc::new(WsHub::new());
    spawn_levels_broadcast(params.clone(), hub.clone());
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let params = params.clone();
        let preset = preset.clone();
        let tape = tape.clone();
        let vban = vban.clone();
        let vban_recv = vban_recv.clone();
        let macros = macros.clone();
        let devices = devices.clone();
        let hub = hub.clone();
        thread::spawn(move || {
            let _ = handle(
                &params,
                &preset,
                &tape,
                &vban,
                &vban_recv,
                &macros,
                &devices,
                &hub,
                &mut stream,
            );
        });
    }
    Ok(())
}

fn handle(
    params: &Arc<Params>,
    preset: &PathBuf,
    tape: &Arc<TapeControl>,
    vban: &Arc<VbanControl>,
    vban_recv: &Arc<VbanRecvControl>,
    macros: &Arc<MacroControl>,
    devices: &Arc<Mutex<DeviceMap>>,
    hub: &Arc<WsHub>,
    stream: &mut TcpStream,
) -> io::Result<()> {
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();

    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 32 * 1024 {
            return Ok(());
        }
        match stream.read(&mut b) {
            Ok(0) => return Ok(()),
            Ok(_) => head.push(b[0]),
            Err(_) => return Ok(()),
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let mut parts = text.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let lower = text.to_lowercase();
    let clen: usize = lower
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; clen];
    if clen > 0 {
        stream.read_exact(&mut body).ok();
    }
    let body = String::from_utf8_lossy(&body).to_string();

    // Pegel-Abo: hier endet HTTP, ab jetzt laufen WebSocket-Frames.
    if method == "GET" && path.split('?').next().unwrap_or("") == "/ws/levels" {
        return ws_levels(params, hub, stream, &text);
    }

    match (method.as_str(), path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            reply(stream, "200 OK", "text/html; charset=utf-8", INDEX_HTML.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/state") => {
            let json = params.to_json().to_json();
            reply(stream, "200 OK", "application/json", json.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/levels") => {
            let json = params.levels_json();
            reply(stream, "200 OK", "application/json", json.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/set") => {
            let out = match json::parse(&body) {
                Err(e) => format!("{{\"error\":{}}}", json::quote(&e)),
                Ok(v) => {
                    let target = v.get("target").and_then(|x| match x {
                        Value::Str(s) => Some(s.clone()),
                        _ => None,
                    });
                    let index = v.get("index").and_then(|x| x.as_f64()).unwrap_or(0.0) as usize;
                    let param = v.get("param").and_then(|x| match x {
                        Value::Str(s) => Some(s.clone()),
                        _ => None,
                    });
                    let value = v.get("value").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
                    match (target, param) {
                        (Some(t), Some(p)) if params.set(&t, index, &p, value) => {
                            "{\"ok\":true}".to_string()
                        }
                        (t, p) => format!(
                            "{{\"error\":{}}}",
                            json::quote(&format!("unbekannt: {t:?}/{index}/{p:?}"))
                        ),
                    }
                }
            };
            reply(stream, "200 OK", "application/json", out.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/vban/recv") => {
            let body = vban_recv.status_json();
            reply(stream, "200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/vban/recv") => {
            let v = json::parse(&body).unwrap_or(Value::Null);
            let mut fehler: Option<String> = None;
            // Reihenfolge: erst Konfiguration, dann schalten – so startet ein
            // {enabled:true, port:6981} direkt auf dem neuen Port.
            if let Some(p) = v.get("port").and_then(|x| x.as_f64()) {
                if let Err(e) = vban_recv.set_port(p as u32) {
                    fehler = Some(e);
                }
            }
            if fehler.is_none() {
                if let Some(s) = v.get("stream").and_then(|x| match x {
                    Value::Str(t) => Some(t.clone()),
                    _ => None,
                }) {
                    if let Err(e) = vban_recv.set_stream(&s) {
                        fehler = Some(e);
                    }
                }
            }
            if fehler.is_none() {
                if let Some(an) = v.get("enabled").and_then(|x| x.as_bool()) {
                    vban_recv.set_enabled(an);
                }
            }
            let out = match fehler {
                Some(e) => format!("{{\"error\":{}}}", json::quote(&e)),
                None => vban_recv.status_json(),
            };
            reply(stream, "200 OK", "application/json", out.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/vban") => {
            let body = vban.status_json();
            reply(stream, "200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/vban") => {
            let v = json::parse(&body).unwrap_or(Value::Null);
            let s = |k: &str| {
                v.get(k).and_then(|x| match x {
                    Value::Str(t) => Some(t.clone()),
                    _ => None,
                })
            };
            let action = s("action").unwrap_or_default();
            let host = s("host").unwrap_or_else(|| "127.0.0.1".into());
            let port = v.get("port").and_then(|x| x.as_f64()).unwrap_or(6980.0) as u32;
            let stream_name = s("stream").unwrap_or_else(|| "vm8busA1".into());
            let out = match action.as_str() {
                "start" => match vban.set(&host, port, &stream_name) {
                    Ok(()) => {
                        vban.set_enabled(true);
                        vban.status_json()
                    }
                    Err(e) => format!("{{\"error\":{}}}", json::quote(&e)),
                },
                "stop" => {
                    vban.set_enabled(false);
                    vban.status_json()
                }
                _ => format!("{{\"error\":{}}}", json::quote("action start|stop")),
            };
            reply(stream, "200 OK", "application/json", out.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/macros") => {
            let body = macros.status_json();
            reply(stream, "200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/macros") => {
            let v = json::parse(&body).unwrap_or(Value::Null);
            let action = v
                .get("action")
                .and_then(|x| match x {
                    Value::Str(t) => Some(t.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let index = v.get("index").and_then(|x| x.as_f64()).unwrap_or(0.0) as usize;
            let res: Result<(), String> = match action.as_str() {
                "press" => macros.press(index),
                "release" => macros.release(index),
                "save" => macros.save(),
                "load" => macros.load(),
                _ => Err("action press|release|save|load".into()),
            };
            match res {
                Ok(()) => {
                    let body = macros.status_json();
                    reply(stream, "200 OK", "application/json", body.as_bytes())
                }
                Err(e) => {
                    let body = format!("{{\"error\":{}}}", json::quote(&e));
                    reply(stream, "400 Bad Request", "application/json", body.as_bytes())
                }
            }
        }
        ("GET", p) if p.starts_with("/api/eq") => {
            let strip: usize = p
                .split("strip=")
                .nth(1)
                .and_then(|s| s.split(&['&', ' '][..]).next())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            match params.eq_json(strip) {
                Some(v) => {
                    let body = v.to_json();
                    reply(stream, "200 OK", "application/json", body.as_bytes())
                }
                None => reply(stream, "404 Not Found", "application/json", b"{\"error\":\"strip\"}"),
            }
        }
        ("GET", p) if p.starts_with("/api/inputs") => {
            let body = crate::inputs::status_json(&crate::inputs::RealRunner);
            reply(stream, "200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/inputs") => {
            let v = json::parse(&body).unwrap_or(Value::Null);
            let strip = v.get("strip").and_then(|x| x.as_f64()).unwrap_or(0.0) as usize;
            let quelle = v
                .get("source")
                .and_then(|x| match x {
                    Value::Str(s) if !s.is_empty() => Some(s.clone()),
                    _ => None,
                });
            let runner = crate::inputs::RealRunner;
            let out = match crate::inputs::assign(&runner, strip, quelle.as_deref()) {
                Ok(()) => crate::inputs::status_json(&runner),
                Err(e) => format!("{{\"error\":{}}}", json::quote(&e)),
            };
            reply(stream, "200 OK", "application/json", out.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/devices") => {
            let body = match devices.lock() {
                Ok(d) => d.status_json(&RealRunner),
                Err(_) => "{\"error\":\"gesperrt\"}".to_string(),
            };
            reply(stream, "200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/devices") => {
            let v = json::parse(&body).unwrap_or(Value::Null);
            let bus = v
                .get("bus")
                .and_then(|x| match x {
                    Value::Str(t) => Some(t.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let sink_name = v
                .get("sink")
                .and_then(|x| match x {
                    Value::Str(t) => Some(t.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let runner = RealRunner;
            let ergebnis = (|| -> Result<(), String> {
                let sinks = crate::devices::list_sinks(&runner)?;
                let sink = sinks
                    .iter()
                    .find(|s| s.name == sink_name)
                    .ok_or_else(|| format!("Sink {sink_name} nicht gefunden"))?;
                let mut map = devices.lock().map_err(|_| "gesperrt".to_string())?;
                if sink_name.is_empty() {
                    map.clear(&runner, &bus)
                } else {
                    map.assign(&runner, &bus, sink)?;
                    map.save()
                }
            })();
            let body = match ergebnis {
                Ok(()) => match devices.lock() {
                    Ok(d) => d.status_json(&runner),
                    Err(_) => "{\"error\":\"gesperrt\"}".to_string(),
                },
                Err(e) => format!("{{\"error\":{}}}", json::quote(&e)),
            };
            reply(stream, "200 OK", "application/json", body.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/tape") => {
            let body = format!(
                "{{\"recording\":{},\"status\":{}}}",
                tape.recording.load(std::sync::atomic::Ordering::Relaxed),
                json::quote(&tape.status())
            );
            reply(stream, "200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/tape") => {
            let action = json::parse(&body)
                .ok()
                .and_then(|v| v.get("action").and_then(|a| match a {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                }))
                .unwrap_or_default();
            match action.as_str() {
                "start" => {
                    tape.recording.store(true, std::sync::atomic::Ordering::Relaxed);
                    tape.set_status("start angefordert");
                    reply(stream, "200 OK", "application/json", b"{\"ok\":true}")
                }
                "stop" => {
                    tape.recording.store(false, std::sync::atomic::Ordering::Relaxed);
                    reply(stream, "200 OK", "application/json", b"{\"ok\":true}")
                }
                _ => reply(stream, "400 Bad Request", "text/plain", b"action start|stop"),
            }
        }
        ("GET", p) if p.starts_with("/api/preset") => match fs::read_to_string(preset) {
            Ok(s) => reply(stream, "200 OK", "application/json", s.as_bytes()),
            Err(e) => reply(stream, "404 Not Found", "text/plain", e.to_string().as_bytes()),
        },
        ("POST", p) if p.starts_with("/api/preset") => {
            let action = json::parse(&body)
                .ok()
                .and_then(|v| v.get("action").and_then(|a| match a {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                }))
                .unwrap_or_default();
            match action.as_str() {
                "save" => {
                    if let Some(dir) = preset.parent() {
                        let _ = fs::create_dir_all(dir);
                    }
                    let text = params.to_json().to_json();
                    match fs::write(preset, text) {
                        Ok(()) => reply(stream, "200 OK", "application/json", b"{\"ok\":true}"),
                        Err(e) => reply(
                            stream,
                            "500 Internal Server Error",
                            "text/plain",
                            e.to_string().as_bytes(),
                        ),
                    }
                }
                "load" => match fs::read_to_string(preset) {
                    Ok(text) => match json::parse(&text) {
                        Ok(v) => {
                            apply_preset(params, &v);
                            reply(stream, "200 OK", "application/json", b"{\"ok\":true}")
                        }
                        Err(e) => reply(
                            stream,
                            "400 Bad Request",
                            "text/plain",
                            e.to_string().as_bytes(),
                        ),
                    },
                    Err(e) => reply(stream, "404 Not Found", "text/plain", e.to_string().as_bytes()),
                },
                _ => reply(stream, "400 Bad Request", "text/plain", b"action save|load"),
            }
        }
        _ => reply(stream, "404 Not Found", "text/plain", b"nicht gefunden"),
    }
}

fn reply(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

/// Preset anwenden: alle im JSON vorhandenen Felder werden gesetzt.
pub fn apply_preset(params: &Arc<Params>, v: &Value) {
    if let Some(strips) = v.get("strips").and_then(|x| x.as_arr()) {
        for (i, s) in strips.iter().enumerate() {
            for key in ["gain", "pan"] {
                if let Some(x) = s.get(key).and_then(|x| x.as_f64()) {
                    params.set("strip", i, key, x as f32);
                }
            }
            for key in ["mute", "solo", "mono", "eq", "comp", "gate", "denoiser"] {
                if let Some(b) = s.get(key).and_then(|x| x.as_bool()) {
                    params.set("strip", i, key, if b { 1.0 } else { 0.0 });
                }
            }
            if let Some(sends) = s.get("sends").and_then(|x| x.as_arr()) {
                for (b, sv) in sends.iter().enumerate() {
                    if let Some(x) = sv.as_f64() {
                        params.set("strip", i, &format!("send{b}"), x as f32);
                    }
                }
            }
        }
    }
    if let Some(buses) = v.get("buses").and_then(|x| x.as_arr()) {
        for (i, b) in buses.iter().enumerate() {
            if let Some(x) = b.get("gain").and_then(|x| x.as_f64()) {
                params.set("bus", i, "gain", x as f32);
            }
            if let Some(x) = b.get("delay_ms").and_then(|x| x.as_f64()) {
                params.set("bus", i, "delay", x as f32);
            }
            if let Some(x) = b.get("trim").and_then(|x| x.as_f64()) {
                params.set("bus", i, "trim", x as f32);
            }
            for key in ["mute", "mono", "eq", "limiter"] {
                if let Some(x) = b.get(key).and_then(|x| x.as_bool()) {
                    params.set("bus", i, key, if x { 1.0 } else { 0.0 });
                }
            }
        }
    }
}

/* ==================================================================
   WebSocket für die Pegelanzeige
   ------------------------------------------------------------------
   Der Audio-Thread rechnet weiter nur in seine Atomics; ein eigener
   Thread baut alle 25 ms (40 Hz) den JSON-Text und reicht ihn an die
   verbundenen Clients weiter. Jeder Client hat einen eigenen
   Sende-Puffer (2 Frames): hängt ein Browser, wird der nächste Frame
   verworfen statt den Verteiler zu blockieren. Fällt ein Client weg,
   endet sein Thread und räumt seinen Eintrag selbst aus der Liste.
   ================================================================== */

/// Verbundene Pegel-Abonnenten. Wert ist der Sende-Puffer des Clients.
pub struct WsHub {
    clients: Mutex<HashMap<u64, SyncSender<Arc<String>>>>,
    next_id: AtomicU64,
    frames: AtomicU64,
    running: AtomicBool,
}

impl WsHub {
    pub fn new() -> WsHub {
        WsHub {
            clients: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            frames: AtomicU64::new(0),
            running: AtomicBool::new(true),
        }
    }

    fn register(&self, tx: SyncSender<Arc<String>>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut m) = self.clients.lock() {
            m.insert(id, tx);
        }
        id
    }

    fn unregister(&self, id: u64) {
        if let Ok(mut m) = self.clients.lock() {
            m.remove(&id);
        }
    }

    /// Zahl der gerade verbundenen Clients.
    pub fn count(&self) -> usize {
        self.clients.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Zahl der bislang an alle Clients verteilten Frames (Diagnose).
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    fn broadcast(&self, msg: Arc<String>) {
        let mut m = match self.clients.lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        // retain räumt tote Clients mit auf, ohne dass jemand anders sperren muss
        m.retain(|_, tx| match tx.try_send(msg.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => true,
            Err(TrySendError::Disconnected(_)) => false,
        });
        if !m.is_empty() {
            self.frames.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Verteiler-Thread: 40 Frames pro Sekunde, nur solange jemand zuhört.
pub fn spawn_levels_broadcast(params: Arc<Params>, hub: Arc<WsHub>) {
    thread::spawn(move || {
        let period = Duration::from_millis(25);
        while hub.running.load(Ordering::Relaxed) {
            let start = Instant::now();
            if hub.count() > 0 {
                hub.broadcast(Arc::new(params.levels_json()));
            }
            let elapsed = start.elapsed();
            if elapsed < period {
                thread::sleep(period - elapsed);
            }
        }
    });
}

fn ws_levels(
    params: &Arc<Params>,
    hub: &Arc<WsHub>,
    stream: &mut TcpStream,
    head: &str,
) -> io::Result<()> {
    let key = match header_value(head, "sec-websocket-key") {
        Some(k) => k,
        None => {
            return reply(stream, "400 Bad Request", "text/plain", b"Sec-WebSocket-Key fehlt");
        }
    };
    let accept = ws_accept(&key);
    let antwort = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nCache-Control: no-store\r\n\r\n"
    );
    stream.write_all(antwort.as_bytes())?;
    stream.flush()?;
    stream.set_nodelay(true).ok();

    // Kurzer Lese-Timeout: der Thread schreibt neue Frames und liest
    // zwischendurch, ob der Client noch da ist (Ping/Close/EOF).
    stream.set_read_timeout(Some(Duration::from_millis(4))).ok();

    let (tx, rx) = sync_channel::<Arc<String>>(2);
    let id = hub.register(tx);
    // Erstes Bild sofort, damit die Anzeige nicht auf den Takt wartet.
    let mut pending: Option<Arc<String>> = Some(Arc::new(params.levels_json()));
    let mut rb: Vec<u8> = Vec::with_capacity(1024);
    let mut buf = [0u8; 4096];

    let ergebnis = (|| -> io::Result<()> {
        loop {
            // Alle wartenden Frames einsammeln – nur der neueste zählt,
            // Pegel sind Wegwerfdaten.
            for msg in rx.try_iter() {
                pending = Some(msg);
            }
            if let Some(m) = pending.take() {
                ws_send(stream, 0x1, m.as_bytes())?;
            }
            match stream.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    if !ws_consume(stream, &mut rb, &buf[..n])? {
                        return Ok(());
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut => {}
                Err(_) => return Ok(()),
            }
        }
    })();

    hub.unregister(id);
    ergebnis
}

fn header_value(head: &str, name: &str) -> Option<String> {
    head.lines()
        .skip(1)
        .find_map(|l| match l.split_once(':') {
            Some((k, v)) if k.trim().eq_ignore_ascii_case(name) => Some(v.trim().to_string()),
            _ => None,
        })
}

/// RFC 6455: base64(sha1(key + GUID))
fn ws_accept(key: &str) -> String {
    const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    base64(&sha1(format!("{key}{GUID}").as_bytes()))
}

/// Ein Frame vom Server (unmaskiert).
fn ws_send(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut head = Vec::with_capacity(10);
    head.push(0x80 | (opcode & 0x0F));
    let n = payload.len();
    if n < 126 {
        head.push(n as u8);
    } else if n <= 0xFFFF {
        head.push(126);
        head.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        head.push(127);
        head.extend_from_slice(&(n as u64).to_be_bytes());
    }
    stream.write_all(&head)?;
    stream.write_all(payload)?;
    stream.flush()
}

/// Frames vom Client auswerten. `false` heißt: Verbindung ist zu.
fn ws_consume(stream: &mut TcpStream, rb: &mut Vec<u8>, chunk: &[u8]) -> io::Result<bool> {
    rb.extend_from_slice(chunk);
    loop {
        if rb.len() < 2 {
            return Ok(true);
        }
        let opcode = rb[0] & 0x0F;
        let maskiert = rb[1] & 0x80 != 0;
        let mut len = (rb[1] & 0x7F) as usize;
        let mut off = 2;
        if len == 126 {
            if rb.len() < off + 2 {
                return Ok(true);
            }
            len = u16::from_be_bytes([rb[off], rb[off + 1]]) as usize;
            off += 2;
        } else if len == 127 {
            if rb.len() < off + 8 {
                return Ok(true);
            }
            let mut a = [0u8; 8];
            a.copy_from_slice(&rb[off..off + 8]);
            len = u64::from_be_bytes(a) as usize;
            off += 8;
        }
        let maske = if maskiert {
            if rb.len() < off + 4 {
                return Ok(true);
            }
            let m = [rb[off], rb[off + 1], rb[off + 2], rb[off + 3]];
            off += 4;
            Some(m)
        } else {
            None
        };
        if len > 64 * 1024 {
            return Ok(false); // unplausibel groß – Verbindung aufgeben
        }
        if rb.len() < off + len {
            return Ok(true);
        }
        let mut payload: Vec<u8> = rb[off..off + len].to_vec();
        if let Some(m) = maske {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= m[i % 4];
            }
        }
        rb.drain(..off + len);
        match opcode {
            0x8 => return Ok(false),                       // Close
            0x9 => ws_send(stream, 0xA, &payload)?,        // Ping -> Pong
            _ => {}                                        // Text/Pong: egal
        }
    }
}

/// SHA-1 (FIPS 180-4), reicht für den Handshake.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for i in 0..80 {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[i]);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for c in data.chunks(3) {
        let b1 = *c.get(1).unwrap_or(&0);
        let b2 = *c.get(2).unwrap_or(&0);
        let n = ((c[0] as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if c.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_bekannte_werte() {
        let hex = |d: [u8; 20]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(sha1(b"The quick brown fox jumps over the lazy dog")),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
        // Länge genau an der Padding-Grenze
        let lang = vec![b'a'; 64];
        assert_eq!(hex(sha1(&lang)), "0098ba824b5c16427bd7a1122a5a442a25ec644d");
    }

    #[test]
    fn base64_bekannte_werte() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn handshake_accept_wie_rfc6455() {
        // Beispiel aus RFC 6455, Abschnitt 1.3
        assert_eq!(ws_accept("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn header_werden_erkannt() {
        let head = "GET /ws/levels HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nSec-WebSocket-Key: ABC\r\n\r\n";
        assert_eq!(header_value(head, "sec-websocket-key").as_deref(), Some("ABC"));
        assert_eq!(header_value(head, "fehlt"), None);
    }
}
