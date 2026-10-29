//! Steuer-API über HTTP (JSON) und ein WebSocket für die Pegelanzeige.
//! Parameteränderungen gehen als POST an `/api/set`, den Zustand liefert
//! `/api/state`. Die Pegel schickt `/ws/levels` im 60-Hz-Takt von sich aus,
//! aber nur bei neuen Daten des Audio-Callbacks (ein eigener Thread, nie der
//! Callback selbst); `/api/levels` bleibt als HTTP-Abfrage und als
//! Rückfallebene erhalten.
//!
//! **Server:** `tiny_http` (synchron, ein Thread je Anfrage, kein tokio) und
//! `tungstenite` für den WebSocket. Der Eigenbau davor (Kopf Zeile für Zeile
//! lesen, `Content-Length` selbst auswerten, SHA-1/Base64 für den Handshake,
//! Frames von Hand kodieren) ist ersetzt – der fehlerhafte Handshake (Key aus
//! kleingeschriebenen Headern) war einer der drei schweren Fehler.
//!
//! Die Wege (`route`) sind eine reine Funktion aus Methode, Pfad und Rumpf auf
//! eine [`Antwort`]; das ist der Teil, den die Tests ohne Socket prüfen.

use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tiny_http::{Header, Method, Request, Response, Server, StatusCode};
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::{Message, Role, WebSocket};

use crate::devices::DeviceMap;
use crate::engine::Params;
use crate::json::{self, Value};
use crate::macros_io::MacroControl;
use crate::pw::PipeWire;
use crate::restore::Ergebnis;
use crate::tape_io::TapeControl;
use crate::vban_io::{VbanControl, VbanRecvControl};

const INDEX_HTML: &str = include_str!("../../../web/index.html");

/// Obergrenze für den Rumpf einer Anfrage. JSON-Kommandos sind winzig; alles
/// darüber ist ein Fehler oder ein Angriff und wird abgeschnitten.
const MAX_BODY: u64 = 64 * 1024;

/// Wartezeit eines Pegel-Abos zwischen zwei Frames (danach: Verbindung prüfen).
const WS_WARTEN: Duration = Duration::from_millis(50);

/// Alles, was eine Anfrage braucht. Wird je Anfrage geklont (nur `Arc`s und
/// ein Pfad), damit der Handler-Thread sie besitzen kann.
#[derive(Clone)]
struct Zustand {
    params: Arc<Params>,
    preset: PathBuf,
    tape: Arc<TapeControl>,
    vban: Arc<VbanControl>,
    vban_recv: Arc<VbanRecvControl>,
    macros: Arc<MacroControl>,
    devices: Arc<Mutex<DeviceMap>>,
    restore: Arc<Mutex<Ergebnis>>,
}

pub fn serve(
    listen: String,
    params: Arc<Params>,
    preset: PathBuf,
    tape: Arc<TapeControl>,
    vban: Arc<VbanControl>,
    vban_recv: Arc<VbanRecvControl>,
    macros: Arc<MacroControl>,
    devices: Arc<Mutex<DeviceMap>>,
    restore: Arc<Mutex<Ergebnis>>,
) -> io::Result<()> {
    let server = Server::http(&listen)
        .map_err(|e| io::Error::new(io::ErrorKind::AddrNotAvailable, e.to_string()))?;
    println!("UI: http://{listen}/  (Pegel-WebSocket: ws://{listen}/ws/levels)");
    let hub = Arc::new(WsHub::new());
    spawn_levels_broadcast(params.clone(), hub.clone());
    let zustand = Zustand {
        params,
        preset,
        tape,
        vban,
        vban_recv,
        macros,
        devices,
        restore,
    };
    for request in server.incoming_requests() {
        let zustand = zustand.clone();
        let hub = hub.clone();
        thread::spawn(move || {
            let _ = behandle(request, &zustand, &hub);
        });
    }
    Ok(())
}

/// Eine fertige Antwort: Status, Content-Type, Rumpf.
struct Antwort {
    status: u16,
    ctype: &'static str,
    body: Vec<u8>,
}

impl Antwort {
    fn neu(status: &str, ctype: &'static str, body: impl Into<Vec<u8>>) -> Antwort {
        Antwort {
            status: status_zahl(status),
            ctype,
            body: body.into(),
        }
    }
}

/// "200 OK" -> 200.
fn status_zahl(status: &str) -> u16 {
    status
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(500)
}

fn reply(status: &str, ctype: &'static str, body: &[u8]) -> Antwort {
    Antwort::neu(status, ctype, body.to_vec())
}

/// Eine Anfrage bedienen: WebSocket abzweigen, sonst `route` und antworten.
fn behandle(mut request: Request, zustand: &Zustand, hub: &Arc<WsHub>) -> io::Result<()> {
    setze_anfrage();
    let methode = match request.method() {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Head => "HEAD",
        Method::Put => "PUT",
        Method::Delete => "DELETE",
        _ => "ANDERE",
    };
    // Der Pfad enthält die Abfrage (`?strip=0`, `?monitors=1`), wie bisher.
    let pfad = request.url().to_string();

    // Pegel-Abo: hier endet HTTP, ab jetzt laufen WebSocket-Frames.
    if methode == "GET" && pfad.split('?').next().unwrap_or("") == "/ws/levels" {
        return ws_levels(request, hub);
    }

    let mut rumpf = Vec::new();
    let _ = request.as_reader().take(MAX_BODY).read_to_end(&mut rumpf);
    let rumpf = String::from_utf8_lossy(&rumpf).to_string();
    antworte(request, route(zustand, methode, &pfad, &rumpf))
}

/// Antwort über tiny_http ausliefern (setzt `Content-Length` selbst).
fn antworte(request: Request, a: Antwort) -> io::Result<()> {
    let mut antwort = Response::from_data(a.body).with_status_code(StatusCode(a.status));
    if let Ok(h) = Header::from_bytes(&b"Content-Type"[..], a.ctype.as_bytes()) {
        antwort.add_header(h);
    }
    if let Ok(h) = Header::from_bytes(&b"Cache-Control"[..], &b"no-store"[..]) {
        antwort.add_header(h);
    }
    request
        .respond(antwort)
        .map_err(|e| io::Error::other(e.to_string()))
}

/// Wege der Steuer-API. Reine Funktion: gleiche Eingabe, gleiche Antwort.
#[allow(clippy::too_many_lines)]
fn route(zustand: &Zustand, method: &str, path: &str, body: &str) -> Antwort {
    let params = &zustand.params;
    let preset = &zustand.preset;
    let tape = &zustand.tape;
    let vban = &zustand.vban;
    let vban_recv = &zustand.vban_recv;
    let macros = &zustand.macros;
    let devices = &zustand.devices;
    let restore = &zustand.restore;

    match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => {
            reply("200 OK", "text/html; charset=utf-8", INDEX_HTML.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/state") => {
            let json = params.to_json().to_json();
            reply("200 OK", "application/json", json.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/levels") => {
            let json = params.levels_json();
            reply("200 OK", "application/json", json.as_bytes())
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
                        // Anzeigename: Text statt Zahl (target strip|bus,
                        // param label). Der leere Text löscht den Namen.
                        (Some(t), Some(p)) if p == "label" => {
                            let text = v.get("value").and_then(|x| match x {
                                Value::Str(s) => Some(s.clone()),
                                _ => None,
                            });
                            match text {
                                Some(s) if params.set_text(&t, index, &p, &s) => {
                                    crate::restore::markiere_schmutzig();
                                    "{\"ok\":true}".to_string()
                                }
                                Some(_) => format!(
                                    "{{\"error\":{}}}",
                                    json::quote(&format!("label abgelehnt: {t}/{index}"))
                                ),
                                None => format!(
                                    "{{\"error\":{}}}",
                                    json::quote("label braucht \"value\" als Text")
                                ),
                            }
                        }
                        (Some(t), Some(p)) if params.set(&t, index, &p, value) => {
                            // Nur merken, geschrieben wird im Autosave-Thread:
                            // kein Lock, keine Datei-Arbeit im Anfragepfad.
                            crate::restore::markiere_schmutzig();
                            "{\"ok\":true}".to_string()
                        }
                        (t, p) => format!(
                            "{{\"error\":{}}}",
                            json::quote(&format!("unbekannt: {t:?}/{index}/{p:?}"))
                        ),
                    }
                }
            };
            reply("200 OK", "application/json", out.as_bytes())
        }
        // Muss vor dem allgemeinen /api/vban-Zweig stehen (Praefix-Match).
        ("GET", p) if p.starts_with("/api/vban/config") => {
            let sr = f32::from_bits(params.sample_rate.load(Ordering::Relaxed)) as u32;
            let body = crate::vban_io::konfig_json(&vban, &vban_recv, sr);
            reply("200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/vban/config") => {
            let v = json::parse(&body).unwrap_or(Value::Null);
            let action = v
                .get("action")
                .and_then(|x| match x {
                    Value::Str(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let ergebnis = match action.as_str() {
                // Reset setzt nur den Laufzeitzustand zurueck; die gesicherte
                // Datei bleibt, damit "Load Config" sie danach zurueckholen
                // kann. Save/Load arbeiten auf der Datei.
                "reset" => {
                    crate::vban_io::setze_standard(&vban, &vban_recv);
                    Ok(())
                }
                "save" => crate::vban_io::speichere_konfig(&vban, &vban_recv),
                "load" => crate::vban_io::lade_konfig(&vban, &vban_recv),
                _ => crate::vban_io::wende_konfig_an(&vban, &vban_recv, &v),
            };
            let sr = f32::from_bits(params.sample_rate.load(Ordering::Relaxed)) as u32;
            match ergebnis {
                Ok(()) => {
                    let body = crate::vban_io::konfig_json(&vban, &vban_recv, sr);
                    reply("200 OK", "application/json", body.as_bytes())
                }
                Err(e) => {
                    let body = format!("{{\"error\":{}}}", json::quote(&e));
                    reply("200 OK", "application/json", body.as_bytes())
                }
            }
        }
        ("GET", p) if p.starts_with("/api/vban/recv") => {
            let body = vban_recv.status_json();
            reply("200 OK", "application/json", body.as_bytes())
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
            reply("200 OK", "application/json", out.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/vban") => {
            let body = vban.status_json();
            reply("200 OK", "application/json", body.as_bytes())
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
            let stream_name = s("stream").unwrap_or_else(|| "crosspointbusA1".into());
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
            reply("200 OK", "application/json", out.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/macros") => {
            let body = macros.status_json();
            reply("200 OK", "application/json", body.as_bytes())
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
                    reply("200 OK", "application/json", body.as_bytes())
                }
                Err(e) => {
                    let body = format!("{{\"error\":{}}}", json::quote(&e));
                    reply("400 Bad Request", "application/json", body.as_bytes())
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
                    reply("200 OK", "application/json", body.as_bytes())
                }
                None => reply(
                    "404 Not Found",
                    "application/json",
                    b"{\"error\":\"strip\"}",
                ),
            }
        }
        ("GET", p) if p.starts_with("/api/inputs") => {
            let mit = p.contains("monitors=1");
            let body = crate::inputs::status_json(&PipeWire, mit);
            reply("200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/inputs") => {
            let v = json::parse(&body).unwrap_or(Value::Null);
            let strip = v.get("strip").and_then(|x| x.as_f64()).unwrap_or(0.0) as usize;
            let quelle = v.get("source").and_then(|x| match x {
                Value::Str(s) if !s.is_empty() => Some(s.clone()),
                _ => None,
            });
            let mit = p.contains("monitors=1");
            let out = match crate::inputs::assign(&PipeWire, strip, quelle.as_deref()) {
                Ok(()) => crate::inputs::status_json(&PipeWire, mit),
                Err(e) => format!("{{\"error\":{}}}", json::quote(&e)),
            };
            reply("200 OK", "application/json", out.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/devices") => {
            let mit = p.contains("monitors=1");
            let body = match devices.lock() {
                Ok(d) => d.status_json(&PipeWire, mit),
                Err(_) => "{\"error\":\"gesperrt\"}".to_string(),
            };
            reply("200 OK", "application/json", body.as_bytes())
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
            let ergebnis = (|| -> Result<(), String> {
                let mut map = devices.lock().map_err(|_| "gesperrt".to_string())?;
                // Abwaehlen: leeren Namen zuerst behandeln, sonst scheitert die
                // Sink-Suche und die Verbindung bleibt bestehen.
                if sink_name.is_empty() {
                    map.clear(&PipeWire, &bus)
                } else {
                    map.assign(&PipeWire, &bus, &sink_name)
                }
            })();
            let mit = p.contains("monitors=1");
            let body = match ergebnis {
                Ok(()) => match devices.lock() {
                    Ok(d) => d.status_json(&PipeWire, mit),
                    Err(_) => "{\"error\":\"gesperrt\"}".to_string(),
                },
                Err(e) => format!("{{\"error\":{}}}", json::quote(&e)),
            };
            reply("200 OK", "application/json", body.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/restore") => {
            let body = match restore.lock() {
                Ok(e) => e.to_json(),
                Err(_) => "{\"devices\":0,\"inputs\":0,\"preset\":false}".to_string(),
            };
            reply("200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/restore") => {
            let v = json::parse(&body).unwrap_or(Value::Null);
            // Ohne Angabe wird alles wiederhergestellt; ein Feld auf false
            // laesst genau diesen Teil aus.
            let an = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(true);
            let ergebnis = crate::restore::anwenden(
                params,
                devices,
                preset,
                an("devices"),
                an("inputs"),
                an("preset"),
            );
            if let Ok(mut letzte) = restore.lock() {
                *letzte = ergebnis.clone();
            }
            reply("200 OK", "application/json", ergebnis.to_json().as_bytes())
        }
        // Effekt-Host (mod-host): Ketten, Bypass und Host-Status.
        ("GET", p) if p.starts_with("/api/fx") => {
            let body = crate::fx::api_json().to_json();
            reply("200 OK", "application/json", body.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/autostart") => {
            let body = crate::autostart::status_json();
            reply("200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/autostart") => {
            let wunsch = json::parse(&body)
                .ok()
                .and_then(|v| v.get("enabled").and_then(|x| x.as_bool()));
            let out = match wunsch {
                Some(an) => match crate::autostart::set(an) {
                    Ok(_) => crate::autostart::status_json(),
                    Err(e) => format!("{{\"error\":{}}}", json::quote(&e)),
                },
                None => format!(
                    "{{\"error\":{}}}",
                    json::quote("\"enabled\" fehlt (true|false)")
                ),
            };
            reply("200 OK", "application/json", out.as_bytes())
        }
        ("GET", p) if p.starts_with("/api/tape") => {
            let body = format!(
                "{{\"recording\":{},\"status\":{}}}",
                tape.recording.load(std::sync::atomic::Ordering::Relaxed),
                json::quote(&tape.status())
            );
            reply("200 OK", "application/json", body.as_bytes())
        }
        ("POST", p) if p.starts_with("/api/tape") => {
            let action = json::parse(&body)
                .ok()
                .and_then(|v| {
                    v.get("action").and_then(|a| match a {
                        Value::Str(s) => Some(s.clone()),
                        _ => None,
                    })
                })
                .unwrap_or_default();
            match action.as_str() {
                "start" => {
                    tape.recording
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    tape.set_status("start angefordert");
                    reply("200 OK", "application/json", b"{\"ok\":true}")
                }
                "stop" => {
                    tape.recording
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    reply("200 OK", "application/json", b"{\"ok\":true}")
                }
                _ => reply("400 Bad Request", "text/plain", b"action start|stop"),
            }
        }
        ("GET", p) if p.starts_with("/api/preset") => match fs::read_to_string(preset) {
            Ok(s) => reply("200 OK", "application/json", s.as_bytes()),
            Err(e) => reply("404 Not Found", "text/plain", e.to_string().as_bytes()),
        },
        ("POST", p) if p.starts_with("/api/preset") => {
            let action = json::parse(&body)
                .ok()
                .and_then(|v| {
                    v.get("action").and_then(|a| match a {
                        Value::Str(s) => Some(s.clone()),
                        _ => None,
                    })
                })
                .unwrap_or_default();
            match action.as_str() {
                "save" => {
                    if let Some(dir) = preset.parent() {
                        let _ = fs::create_dir_all(dir);
                    }
                    let text = params.to_json().to_json();
                    match fs::write(preset, text) {
                        Ok(()) => reply("200 OK", "application/json", b"{\"ok\":true}"),
                        Err(e) => reply(
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
                            reply("200 OK", "application/json", b"{\"ok\":true}")
                        }
                        Err(e) => reply("400 Bad Request", "text/plain", e.to_string().as_bytes()),
                    },
                    Err(e) => reply("404 Not Found", "text/plain", e.to_string().as_bytes()),
                },
                _ => reply("400 Bad Request", "text/plain", b"action save|load"),
            }
        }
        _ => reply("404 Not Found", "text/plain", b"nicht gefunden"),
    }
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
            for key in [
                "mute", "solo", "mono", "eq", "comp", "gate", "denoiser", "nr_gate", "tk",
            ] {
                if let Some(b) = s.get(key).and_then(|x| x.as_bool()) {
                    params.set("strip", i, key, if b { 1.0 } else { 0.0 });
                }
            }
            // Zahlenwerte der neuen Stufen: Lowcut, TK, NR und Gate.
            for key in [
                "lowcut",
                "tk_schwelle",
                "tk_tiefe",
                "nr",
                "nr_schwelle",
                "nr_modus",
                "gate_schwelle",
                "gate_hold",
                "gate_tiefe",
                "gate_attack",
                "gate_release",
            ] {
                if let Some(x) = s.get(key).and_then(|x| x.as_f64()) {
                    params.set("strip", i, key, x as f32);
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

/// Zeitpunkt der letzten Anfrage (ms seit UNIX-Epoche) - die Engine beendet
/// sich, wenn die Oberflaeche laenger weg ist, damit die virtuellen Geraete
/// nicht als Leichen im System stehen bleiben.
pub static LETZTE_ANFRAGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn setze_anfrage() {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    LETZTE_ANFRAGE.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// Millisekunden seit der letzten Anfrage (u64::MAX, wenn noch keine kam).
pub fn leerlauf_ms() -> u64 {
    let jetzt = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let letzte = LETZTE_ANFRAGE.load(std::sync::atomic::Ordering::Relaxed);
    if letzte == 0 {
        return u64::MAX;
    }
    jetzt.saturating_sub(letzte)
}

/* ==================================================================
Pegel-Abo über WebSocket
------------------------------------------------------------------
Der Audio-Callback schreibt nur Atome und zählt am Blockende den
Frame-Zähler hoch. Der Sender-Thread läuft mit 60-Hz-Takt, schläft
aber nicht blind: er wacht am Frame-Zähler auf und sendet genau dann,
wenn es wirklich neue Pegel gibt (bei 1024 Frames @ 48 kHz sind das
~47 Bilder/s; Duplikate im 60-Hz-Takt wären verschenkte Arbeit).

Der JSON-Text entsteht einmal pro Takt in einem wiederverwendeten
Puffer und liegt dann als ein gemeinsamer `Arc<str>` im Hub. Jeder
Client-Thread hängt nur per Condvar daran und schreibt denselben Text
in seinen Socket – keine Kopie je Client, kein Puffer je Client, kein
Verteiler, der auf einen hängenden Browser wartet.

Die Frames selbst kodiert `tungstenite` (Text-Frame, FIN gesetzt,
unmaskiert vom Server); der Rumpf ist derselbe JSON-Text wie zuvor.
================================================================== */

const SENDER_HZ: u64 = 60;

struct SharedLevels {
    seq: u64,
    json: Arc<str>,
}

/// Gemeinsamer Pegel-Puffer für alle Abonnenten.
pub struct WsHub {
    shared: Mutex<SharedLevels>,
    cv: Condvar,
    next_id: AtomicU64,
    clients: AtomicUsize,
    frames: AtomicU64,
    running: AtomicBool,
}

impl WsHub {
    pub fn new() -> WsHub {
        WsHub {
            shared: Mutex::new(SharedLevels {
                seq: 0,
                json: Arc::from(r#"{"strips":[],"buses":[],"f":0}"#),
            }),
            cv: Condvar::new(),
            next_id: AtomicU64::new(1),
            clients: AtomicUsize::new(0),
            frames: AtomicU64::new(0),
            running: AtomicBool::new(true),
        }
    }

    fn register(&self) -> u64 {
        self.clients.fetch_add(1, Ordering::Relaxed);
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn unregister(&self) {
        self.clients.fetch_sub(1, Ordering::Relaxed);
    }

    /// Zahl der gerade verbundenen Clients.
    pub fn count(&self) -> usize {
        self.clients.load(Ordering::Relaxed)
    }

    /// Zahl der bislang veröffentlichten Frames (Diagnose).
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// Einen fertigen Text veröffentlichen: ein `Arc<str>` für alle Clients.
    fn publish(&self, json: Arc<str>) {
        {
            let mut s = match self.shared.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            s.seq = s.seq.wrapping_add(1);
            s.json = json;
        }
        if self.count() > 0 {
            self.frames.fetch_add(1, Ordering::Relaxed);
        }
        self.cv.notify_all();
    }

    fn latest(&self) -> (u64, Arc<str>) {
        match self.shared.lock() {
            Ok(s) => (s.seq, s.json.clone()),
            Err(_) => (0, Arc::from("{}")),
        }
    }

    /// Warten, bis ein Frame neuer als `nach` vorliegt (oder Timeout).
    fn warten(&self, nach: u64, timeout: Duration) -> Option<(u64, Arc<str>)> {
        let mut s = self.shared.lock().ok()?;
        let ende = Instant::now() + timeout;
        loop {
            if s.seq != nach {
                return Some((s.seq, s.json.clone()));
            }
            let rest = ende.checked_duration_since(Instant::now())?;
            if rest.is_zero() {
                return None;
            }
            let (neu, ergebnis) = self.cv.wait_timeout(s, rest).ok()?;
            s = neu;
            if ergebnis.timed_out() && s.seq == nach {
                return None;
            }
        }
    }
}

/// Verteiler-Thread: 60-Hz-Takt, aber getaktet am Frame-Zähler des
/// Audio-Callbacks – gesendet wird nur, wenn neue Pegel da sind.
pub fn spawn_levels_broadcast(params: Arc<Params>, hub: Arc<WsHub>) {
    thread::spawn(move || {
        let takt = Duration::from_nanos(1_000_000_000 / SENDER_HZ);
        let mut puffer = String::with_capacity(256);
        let mut letzter = params.frame_counter();
        let mut naechster = Instant::now();
        while hub.running.load(Ordering::Relaxed) {
            // Bis zu einem Takt auf neue Daten warten. Der Zähler springt am
            // Blockende (21,3 ms bei 1024 Frames), der Takt ist 16,7 ms — die
            // kurze Schlafzeit hält die Reaktion unter einer Millisekunde.
            let grenze = Instant::now() + takt;
            let mut jetzt_frames = params.frame_counter();
            while jetzt_frames == letzter && Instant::now() < grenze {
                thread::sleep(Duration::from_micros(500));
                jetzt_frames = params.frame_counter();
            }
            if jetzt_frames != letzter {
                letzter = jetzt_frames;
                if hub.count() > 0 {
                    // Genau ein Text je Takt, in den vorhandenen Puffer.
                    params.levels_into(&mut puffer);
                    hub.publish(Arc::from(puffer.as_str()));
                }
            }
            // Takt halten, ohne zu driften.
            naechster += takt;
            let jetzt = Instant::now();
            if naechster > jetzt {
                thread::sleep(naechster - jetzt);
            } else {
                naechster = jetzt;
            }
        }
    });
}

/// Ein Pegel-Abo: Handshake über `tungstenite`, danach nur senden.
///
/// Gelesen wird bewusst nicht: die Oberfläche schickt auf diesem Socket
/// nichts. Ein geschlossener Browser fällt dadurch auf, dass das Senden
/// fehlschlägt (RST) – dann endet der Thread. Das hält den 60-Hz-Takt frei
/// von blockierenden Leseversuchen.
fn ws_levels(request: Request, hub: &Arc<WsHub>) -> io::Result<()> {
    let schluessel = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Sec-WebSocket-Key"))
        .map(|h| h.value.as_str().to_string());
    let Some(schluessel) = schluessel else {
        return antworte(
            request,
            reply("400 Bad Request", "text/plain", b"Sec-WebSocket-Key fehlt"),
        );
    };
    // RFC 6455: base64(sha1(key + GUID)) – rechnet `tungstenite`.
    let accept = derive_accept_key(schluessel.as_bytes());
    let mut antwort = Response::empty(StatusCode(101));
    for (name, wert) in [
        ("Upgrade", "websocket"),
        ("Connection", "Upgrade"),
        ("Sec-WebSocket-Accept", accept.as_str()),
    ] {
        if let Ok(h) = Header::from_bytes(name.as_bytes(), wert.as_bytes()) {
            antwort.add_header(h);
        }
    }
    // Ab hier gehört die Verbindung dem WebSocket.
    let strom = request.upgrade("websocket", antwort);
    let mut ws = WebSocket::from_raw_socket(strom, Role::Server, None);

    hub.register();
    let (mut seq, mut frame) = hub.latest();
    // Erster Durchlauf schickt den aktuellen Stand sofort.
    let mut gesendet = u64::MAX;
    let ergebnis = (|| -> io::Result<()> {
        while hub.running.load(Ordering::Relaxed) {
            if seq != gesendet {
                ws.send(Message::text(frame.to_string()))
                    .map_err(|e| io::Error::other(e.to_string()))?;
                gesendet = seq;
            }
            // Auf den nächsten Frame warten statt zu pollen.
            if let Some((s, f)) = hub.warten(seq, WS_WARTEN) {
                seq = s;
                frame = f;
            }
        }
        Ok(())
    })();

    hub.unregister();
    ergebnis
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    fn zustand() -> Zustand {
        let ctl = Arc::new(TapeControl::new(
            Arc::new(crate::tape_io::Ring::new(2, 64)),
            64,
        ));
        Zustand {
            params: Arc::new(Params::new(48_000.0, 512)),
            preset: std::env::temp_dir().join("crosspoint-test-preset.json"),
            tape: ctl,
            vban: Arc::new(VbanControl::new()),
            vban_recv: Arc::new(VbanRecvControl::new()),
            macros: Arc::new(MacroControl::new()),
            devices: Arc::new(Mutex::new(DeviceMap::new(
                std::env::temp_dir().join("crosspoint-test-devices.json"),
            ))),
            restore: Arc::new(Mutex::new(Ergebnis::default())),
        }
    }

    fn text(a: &Antwort) -> String {
        String::from_utf8_lossy(&a.body).to_string()
    }

    #[test]
    fn zustand_wird_byte_gleich_ausgeliefert() {
        let z = zustand();
        let a = route(&z, "GET", "/api/state", "");
        assert_eq!(a.status, 200);
        assert_eq!(a.ctype, "application/json");
        assert_eq!(text(&a), z.params.to_json().to_json());
        assert!(text(&a).starts_with(r#"{"strips":["#));
    }

    #[test]
    fn pegel_und_eq_wege() {
        let z = zustand();
        let a = route(&z, "GET", "/api/levels", "");
        assert_eq!(text(&a), z.params.levels_json());
        let a = route(&z, "GET", "/api/eq?strip=0", "");
        assert!(text(&a).contains(r#""cells""#));
        let a = route(&z, "GET", "/api/eq?strip=99", "");
        assert_eq!(a.status, 404);
        assert_eq!(text(&a), r#"{"error":"strip"}"#);
    }

    #[test]
    fn setzen_pruefen_melden() {
        let z = zustand();
        let a = route(
            &z,
            "POST",
            "/api/set",
            r#"{"target":"strip","index":0,"param":"gain","value":-6}"#,
        );
        assert_eq!(a.status, 200);
        assert_eq!(text(&a), r#"{"ok":true}"#);
        assert_eq!(
            z.params.to_json().get("strips").unwrap().as_arr().unwrap()[0]
                .get("gain")
                .unwrap()
                .as_f64(),
            Some(-6.0)
        );

        let a = route(&z, "POST", "/api/set", r#"{"target":"strip","index":0}"#);
        assert!(text(&a).contains("unbekannt"), "{}", text(&a));

        let a = route(&z, "POST", "/api/set", "{\"target\":");
        assert_eq!(a.status, 200);
        assert!(text(&a).starts_with(r#"{"error":"#));
    }

    #[test]
    fn anzeigename_als_text() {
        let z = zustand();
        let a = route(
            &z,
            "POST",
            "/api/set",
            r#"{"target":"strip","index":1,"param":"label","value":"Gesang"}"#,
        );
        assert_eq!(text(&a), r#"{"ok":true}"#);
        assert_eq!(z.params.label("strip", 1).as_deref(), Some("Gesang"));
    }

    #[test]
    fn tape_und_vban_wege() {
        let z = zustand();
        assert_eq!(
            text(&route(&z, "GET", "/api/tape", "")),
            r#"{"recording":false,"status":"idle"}"#
        );
        assert_eq!(
            text(&route(&z, "POST", "/api/tape", r#"{"action":"start"}"#)),
            r#"{"ok":true}"#
        );
        assert_eq!(
            text(&route(&z, "GET", "/api/tape", "")),
            r#"{"recording":true,"status":"start angefordert"}"#
        );
        let a = route(&z, "POST", "/api/tape", r#"{"action":"quatsch"}"#);
        assert_eq!(a.status, 400);
        assert_eq!(text(&a), "action start|stop");

        let a = route(&z, "GET", "/api/vban", "");
        assert_eq!(a.status, 200);
        assert!(text(&a).starts_with(r#"{"enabled":false"#));
        let a = route(&z, "POST", "/api/vban", r#"{"action":"nope"}"#);
        assert_eq!(text(&a), r#"{"error":"action start|stop"}"#);
    }

    #[test]
    fn startseite_und_unbekannter_pfad() {
        let z = zustand();
        let a = route(&z, "GET", "/", "");
        assert_eq!(a.status, 200);
        assert_eq!(a.ctype, "text/html; charset=utf-8");
        assert_eq!(a.body.len(), INDEX_HTML.len());
        let a = route(&z, "GET", "/gibtsnicht", "");
        assert_eq!(a.status, 404);
        assert_eq!(text(&a), "nicht gefunden");
    }

    #[test]
    fn status_und_accept_schluessel() {
        assert_eq!(status_zahl("200 OK"), 200);
        assert_eq!(status_zahl("404 Not Found"), 404);
        assert_eq!(status_zahl("kaputt"), 500);
        // Beispiel aus RFC 6455, Abschnitt 1.3
        assert_eq!(
            derive_accept_key(b"dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn hub_haelt_genau_einen_frame_fuer_alle() {
        let hub = WsHub::new();
        let (seq0, _) = hub.latest();
        assert_eq!(hub.count(), 0);
        hub.register();
        assert_eq!(hub.count(), 1);

        hub.publish(Arc::from("{\"a\":1}"));
        let (seq1, t1) = hub.latest();
        assert_eq!(seq1, seq0 + 1);
        assert_eq!(&*t1, "{\"a\":1}");
        assert_eq!(hub.frames(), 1);

        // Der naechste Frame ersetzt den alten: es gibt keinen zweiten Puffer.
        hub.publish(Arc::from("{\"a\":2}"));
        let (seq2, t2) = hub.latest();
        assert_eq!(seq2, seq1 + 1);
        assert_eq!(&*t2, "{\"a\":2}");
        assert_eq!(hub.frames(), 2);

        hub.unregister();
        assert_eq!(hub.count(), 0);
    }

    #[test]
    fn hub_weckt_wartenden_client() {
        let hub = Arc::new(WsHub::new());
        let (seq, _) = hub.latest();
        let h = hub.clone();
        let schreiber = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            h.register();
            h.publish(Arc::from("{\"b\":7}"));
        });
        let start = Instant::now();
        let got = hub.warten(seq, Duration::from_secs(2)).expect("Frame");
        schreiber.join().unwrap();
        let (neu, text) = got;
        assert!(neu > seq);
        assert_eq!(&*text, "{\"b\":7}");
        // Condvar statt Pollen: die Wartezeit ist die Schlafzeit des Schreibers.
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn warten_laeuft_in_den_timeout() {
        let hub = WsHub::new();
        let (seq, _) = hub.latest();
        let start = Instant::now();
        assert!(hub.warten(seq, Duration::from_millis(40)).is_none());
        assert!(
            start.elapsed() >= Duration::from_millis(30),
            "{:?}",
            start.elapsed()
        );
    }

    // ---- echter Socket: HTTP-Antwort und WebSocket-Frame -------------------

    /// Startet den Server auf einem freien Port und liefert (Adresse, Hub).
    fn server() -> (String, Arc<WsHub>) {
        let server = Server::http("127.0.0.1:0").expect("Port");
        let addr = match server.server_addr() {
            tiny_http::ListenAddr::IP(a) => a,
            _ => panic!("kein TCP-Port"),
        };
        let hub = Arc::new(WsHub::new());
        let zustand = zustand();
        let h = hub.clone();
        thread::spawn(move || {
            for request in server.incoming_requests() {
                let z = zustand.clone();
                let hh = h.clone();
                thread::spawn(move || {
                    let _ = behandle(request, &z, &hh);
                });
            }
        });
        (format!("127.0.0.1:{}", addr.port()), hub)
    }

    fn anfrage(addr: &str, roh: &str) -> String {
        let mut s = TcpStream::connect(addr).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(5))).ok();
        s.write_all(roh.as_bytes()).expect("schreiben");
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        String::from_utf8_lossy(&out).to_string()
    }

    #[test]
    fn http_ueber_echten_socket() {
        let (addr, _hub) = server();
        let roh = format!("GET /api/state HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        let antwort = anfrage(&addr, &roh);
        assert!(antwort.starts_with("HTTP/1.1 200 OK"), "{antwort}");
        assert!(
            antwort
                .to_lowercase()
                .contains("content-type: application/json"),
            "{antwort}"
        );
        let (_, rumpf) = antwort.split_once("\r\n\r\n").expect("Kopf/Rumpf");
        let wert = crate::json::parse(rumpf).expect("JSON");
        assert!(wert.get("strips").is_some());
    }

    #[test]
    fn websocket_handshake_und_text_frame() {
        let (addr, hub) = server();
        let mut s = TcpStream::connect(&addr).expect("connect");
        s.set_read_timeout(Some(Duration::from_secs(5))).ok();
        s.write_all(
            format!(
                "GET /ws/levels HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                 Sec-WebSocket-Version: 13\r\n\r\n"
            )
            .as_bytes(),
        )
        .expect("handshake");

        let mut puffer = Vec::new();
        let mut b = [0u8; 4096];
        while !puffer.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut b).expect("kopf");
            assert!(n > 0, "Verbindung zu frueh zu");
            puffer.extend_from_slice(&b[..n]);
        }
        let kopf = String::from_utf8_lossy(&puffer).to_string();
        assert!(kopf.starts_with("HTTP/1.1 101"), "{kopf}");
        assert!(
            kopf.to_lowercase()
                .contains("sec-websocket-accept: s3pplmbitxaq9kygzzhzrbk+xoo="),
            "{kopf}"
        );

        // Ein neuer Stand muss als Text-Frame ankommen.
        hub.register(); // zaehlt fuer `publish` als Client
        hub.publish(Arc::from("{\"strips\":[],\"buses\":[],\"f\":1}"));

        // Frame lesen (FIN+Text = 0x81, Laenge, Rumpf) - vom Server unmaskiert.
        let mut kopf2 = [0u8; 2];
        s.read_exact(&mut kopf2).expect("frame-kopf");
        assert_eq!(kopf2[0], 0x81, "Text-Frame mit FIN erwartet");
        let mut laenge = (kopf2[1] & 0x7F) as usize;
        assert_eq!(kopf2[1] & 0x80, 0, "Server darf nicht maskieren");
        if laenge == 126 {
            let mut ext = [0u8; 2];
            s.read_exact(&mut ext).expect("laenge");
            laenge = u16::from_be_bytes(ext) as usize;
        }
        let mut rumpf = vec![0u8; laenge];
        s.read_exact(&mut rumpf).expect("rumpf");
        assert_eq!(
            String::from_utf8_lossy(&rumpf),
            "{\"strips\":[],\"buses\":[],\"f\":1}"
        );
    }
}
