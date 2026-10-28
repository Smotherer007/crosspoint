//! Steuer-API über HTTP (JSON). Bewusst ohne WebSocket: die Oberfläche fragt
//! 20×/s `/api/levels` ab, Parameteränderungen gehen als POST an `/api/set`.

use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use crate::engine::Params;
use crate::json::{self, Value};
use crate::devices::{DeviceMap, RealRunner};
use crate::macros_io::MacroControl;
use crate::tape_io::TapeControl;
use crate::vban_io::VbanControl;
use std::sync::Mutex;

const INDEX_HTML: &str = include_str!("../web/index.html");

pub fn serve(
    listen: String,
    params: Arc<Params>,
    preset: PathBuf,
    tape: Arc<TapeControl>,
    vban: Arc<VbanControl>,
    macros: Arc<MacroControl>,
    devices: Arc<Mutex<DeviceMap>>,
) -> io::Result<()> {
    let listener = TcpListener::bind(&listen)?;
    println!("UI: http://{listen}/");
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let params = params.clone();
        let preset = preset.clone();
        let tape = tape.clone();
        let vban = vban.clone();
        let macros = macros.clone();
        let devices = devices.clone();
        thread::spawn(move || {
            let _ = handle(&params, &preset, &tape, &vban, &macros, &devices, &mut stream);
        });
    }
    Ok(())
}

fn handle(
    params: &Arc<Params>,
    preset: &PathBuf,
    tape: &Arc<TapeControl>,
    vban: &Arc<VbanControl>,
    macros: &Arc<MacroControl>,
    devices: &Arc<Mutex<DeviceMap>>,
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
