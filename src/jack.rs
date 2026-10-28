//! JACK-Anbindung über die von PipeWire bereitgestellte ABI.
//!
//! Auf Bazzite gibt es weder JACK-Header noch ein `libjack.so`-Symlink; die
//! Funktionen werden hier deklariert und gegen `/usr/lib64/pipewire-0.3/jack/
//! libjack.so.0` gelinkt (siehe build.rs).

use std::ffi::{c_char, c_int, c_ulong, c_void, CStr, CString};
use std::ptr;

pub type Nframes = u32;

#[repr(C)]
pub struct Client {
    _p: [u8; 0],
}
#[repr(C)]
pub struct Port {
    _p: [u8; 0],
}

const NO_START_SERVER: c_int = 0x1;
const IS_INPUT: c_ulong = 0x1;
const IS_OUTPUT: c_ulong = 0x2;
const PORT_TYPE: &str = "32 bit float mono audio";

pub type ProcessCb = extern "C" fn(Nframes, *mut c_void) -> c_int;

extern "C" {
    fn jack_client_open(name: *const c_char, opts: c_int, status: *mut c_int, ...) -> *mut Client;
    fn jack_client_close(c: *mut Client) -> c_int;
    fn jack_get_client_name(c: *mut Client) -> *const c_char;
    fn jack_get_sample_rate(c: *mut Client) -> Nframes;
    fn jack_get_buffer_size(c: *mut Client) -> Nframes;
    fn jack_set_process_callback(c: *mut Client, cb: ProcessCb, arg: *mut c_void) -> c_int;
    fn jack_activate(c: *mut Client) -> c_int;
    fn jack_deactivate(c: *mut Client) -> c_int;
    fn jack_port_register(
        c: *mut Client,
        name: *const c_char,
        ty: *const c_char,
        flags: c_ulong,
        bufsize: c_ulong,
    ) -> *mut Port;
    fn jack_port_get_buffer(p: *mut Port, n: Nframes) -> *mut c_void;
    fn jack_connect(c: *mut Client, src: *const c_char, dst: *const c_char) -> c_int;
}

pub struct Jack {
    client: *mut Client,
    pub name: String,
    pub sample_rate: u32,
    pub block: u32,
}

impl Jack {
    pub fn open(name: &str, cb: ProcessCb, arg: *mut c_void) -> Result<Jack, String> {
        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let mut status: c_int = 0;
        let client = unsafe { jack_client_open(cname.as_ptr(), NO_START_SERVER, &mut status) };
        if client.is_null() {
            return Err(format!("jack_client_open fehlgeschlagen (status={status}) – läuft PipeWire?"));
        }
        unsafe {
            if jack_set_process_callback(client, cb, arg) != 0 {
                return Err("jack_set_process_callback fehlgeschlagen".into());
            }
        }
        let got = unsafe {
            CStr::from_ptr(jack_get_client_name(client))
                .to_string_lossy()
                .into_owned()
        };
        let sr = unsafe { jack_get_sample_rate(client) };
        let bs = unsafe { jack_get_buffer_size(client) };
        Ok(Jack { client, name: got, sample_rate: sr, block: bs })
    }

    pub fn port_in(&self, name: &str) -> Result<*mut Port, String> {
        self.port(name, IS_INPUT)
    }

    pub fn port_out(&self, name: &str) -> Result<*mut Port, String> {
        self.port(name, IS_OUTPUT)
    }

    fn port(&self, name: &str, flags: c_ulong) -> Result<*mut Port, String> {
        let n = CString::new(name).map_err(|e| e.to_string())?;
        let t = CString::new(PORT_TYPE).unwrap();
        let p = unsafe { jack_port_register(self.client, n.as_ptr(), t.as_ptr(), flags, 0) };
        if p.is_null() {
            Err(format!("Port {name} nicht angelegt"))
        } else {
            Ok(p)
        }
    }

    pub fn activate(&self) -> Result<(), String> {
        let rc = unsafe { jack_activate(self.client) };
        if rc == 0 { Ok(()) } else { Err(format!("jack_activate rc={rc}")) }
    }

    pub fn connect(&self, src: &str, dst: &str) -> Result<(), String> {
        let s = CString::new(src).map_err(|e| e.to_string())?;
        let d = CString::new(dst).map_err(|e| e.to_string())?;
        let rc = unsafe { jack_connect(self.client, s.as_ptr(), d.as_ptr()) };
        if rc == 0 { Ok(()) } else { Err(format!("connect {src}→{dst} rc={rc}")) }
    }
}

impl Drop for Jack {
    fn drop(&mut self) {
        if !self.client.is_null() {
            unsafe {
                jack_deactivate(self.client);
                jack_client_close(self.client);
            }
            self.client = ptr::null_mut();
        }
    }
}

/// Audiopuffer eines Ports als veränderbare Scheibe.
///
/// # Safety
/// Nur aus dem JACK-Prozess-Callback aufrufen; der Puffer gehört dann dem
/// Audio-Thread und ist für `nframes` gültig.
pub unsafe fn buffer(p: *mut Port, nframes: Nframes) -> &'static mut [f32] {
    unsafe {
        let raw = jack_port_get_buffer(p, nframes) as *mut f32;
        std::slice::from_raw_parts_mut(raw, nframes as usize)
    }
}
