//! JACK-Anbindung über die Crate `jack` (RustAudio/rust-jack).
//!
//! Vorher stand hier das ABI von Hand: `extern "C"`-Deklarationen, eigene
//! `Client`/`Port`-Zeiger, `jack::buffer()` über `jack_port_get_buffer` und ein
//! Linker-Argument auf `lib:jack.so.0` (siehe `build.rs`). Eingesetzt wird
//! jetzt die gepflegte Crate: sie lädt `libjack.so.0` zur Laufzeit
//! (`dynamic_loading`, Vorgabe der Crate), Ports sind typisiert
//! (`Port<AudioIn>`, `Port<AudioOut>`), und die Audiopuffer kommen als
//! `&[f32]`/`&mut [f32]` aus dem [`ProcessScope`] statt als rohe Zeiger.
//!
//! Ports und Callback-Semantik bleiben unverändert: dieselben Portnamen
//! (`strip1_L`, `busA1_L`, `vban_L` …), ein Prozess-Callback je Block, keine
//! Allokation darin.
//!
//! **Bauen:** `jack-sys` fragt auch beim dynamischen Laden `pkg-config` nach
//! `jack`. Auf Systemen mit PipeWire gibt es kein `jack.pc`; deshalb liegt in
//! `packaging/pkgconfig/jack.pc` ein Ersatz, den `.cargo/config.toml` über
//! `PKG_CONFIG_PATH` bekannt macht. Er nennt keine Bibliothek – gelinkt wird
//! nichts, geladen wird zur Laufzeit.

pub use jack::contrib::ClosureProcessHandler;
pub use jack::{AudioIn, AudioOut, Client, ClientOptions, Control, Port, ProcessScope};

pub type Nframes = u32;

/// Ein offener JACK-Client (noch nicht aktiviert).
pub struct Jack {
    client: Client,
    /// Name, unter dem der Server den Client führt (`crosspoint` oder der von
    /// `--client`).
    pub name: String,
    pub sample_rate: u32,
    pub block: u32,
}

impl Jack {
    /// Öffnet den Client. `NO_START_SERVER`: ohne laufenden Server (PipeWire)
    /// wird nicht heimlich ein `jackd` gestartet.
    pub fn open(name: &str) -> Result<Jack, String> {
        let (client, _status) = Client::new(name, ClientOptions::NO_START_SERVER)
            .map_err(|e| format!("jack_client_open fehlgeschlagen – läuft PipeWire? ({e})"))?;
        let got = client.name().to_string();
        let sample_rate = client.sample_rate();
        let block = client.buffer_size();
        Ok(Jack {
            client,
            name: got,
            sample_rate,
            block,
        })
    }

    /// Eingangsport (`32 bit float mono audio`) anlegen.
    pub fn port_in(&self, name: &str) -> Result<Port<AudioIn>, String> {
        self.client
            .register_port(name, AudioIn::default())
            .map_err(|e| format!("Port {name} nicht angelegt: {e}"))
    }

    /// Ausgangsport anlegen.
    pub fn port_out(&self, name: &str) -> Result<Port<AudioOut>, String> {
        self.client
            .register_port(name, AudioOut::default())
            .map_err(|e| format!("Port {name} nicht angelegt: {e}"))
    }

    /// Zwei Ports verbinden (vollqualifizierte Namen, z. B.
    /// `crosspoint:busA1_L` → `alsa_output…:playback_FL`).
    pub fn connect(&self, src: &str, dst: &str) -> Result<(), String> {
        self.client
            .connect_ports_by_name(src, dst)
            .map_err(|e| format!("connect {src}→{dst}: {e}"))
    }

    /// Client aktivieren: ab hier ruft JACK `handler` je Block auf.
    ///
    /// Der zurückgegebene [`jack::AsyncClient`] hält den Client am Leben; wird
    /// er fallen gelassen, wird der Client deaktiviert und geschlossen.
    pub fn activate<F>(
        self,
        handler: ClosureProcessHandler<(), F>,
    ) -> Result<jack::AsyncClient<(), ClosureProcessHandler<(), F>>, String>
    where
        F: FnMut(&Client, &ProcessScope) -> Control + Send + 'static,
    {
        self.client
            .activate_async((), handler)
            .map_err(|e| format!("jack_activate fehlgeschlagen: {e}"))
    }
}
