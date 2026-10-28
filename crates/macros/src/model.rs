//! Datenmodell der Macro-Buttons: Tastenbelegung, Trigger-Arten und Aktionen.
//!
//! Aufbau wie im Vorbild-Fenster "MacroButtons": 80 Tasten, jede mit Name,
//! Zustand (0 = off, 1 = on, 2 = trigger, 3 = color), einer Trigger-Art und
//! einer Liste von Aktionen, die beim Ausloesen abgearbeitet werden.
//!
//! Das Modell beschreibt nur *was* passieren soll. Ausgefuehrt wird nichts:
//! die Engine gibt die Aktionen als `Vec<Action>` zurueck, der Hauptprozess
//! setzt sie um.

/// Anzahl der Tasten-Slots (wie im Original-Fenster).
pub const MAX_BUTTONS: usize = 80;

/// Toleranz fuer "Wert hat sich geaendert". Verhindert, dass Rundungsrauschen
/// eines kontinuierlichen Reglers dauernd Trigger ausloest.
pub const VALUE_EPS: f64 = 1e-9;

/// Betriebsart einer Taste (die "State"-Spalte im Original).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonState {
    /// 0: Taste ist abgeschaltet, Trigger werden ignoriert.
    Off,
    /// 1: rastend - erste Flanke ein, naechste aus.
    On,
    /// 2: tastenartig - loest bei jeder Flanke einmal aus.
    Trigger,
    /// 3: wie `On`, zusaetzlich als Farbe gemeldet.
    Color,
}

impl ButtonState {
    pub fn as_u8(self) -> u8 {
        match self {
            ButtonState::Off => 0,
            ButtonState::On => 1,
            ButtonState::Trigger => 2,
            ButtonState::Color => 3,
        }
    }

    pub fn from_u8(v: u8) -> Option<ButtonState> {
        match v {
            0 => Some(ButtonState::Off),
            1 => Some(ButtonState::On),
            2 => Some(ButtonState::Trigger),
            3 => Some(ButtonState::Color),
            _ => None,
        }
    }

    /// Name in der JSON-Datei. Zahlen werden ebenfalls gelesen (`"1"`), weil
    /// die State-Spalte im Original numerisch ist.
    pub fn as_str(self) -> &'static str {
        match self {
            ButtonState::Off => "off",
            ButtonState::On => "on",
            ButtonState::Trigger => "trigger",
            ButtonState::Color => "color",
        }
    }

    pub fn parse(s: &str) -> Option<ButtonState> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "aus" => Some(ButtonState::Off),
            "on" | "1" | "an" => Some(ButtonState::On),
            "trigger" | "2" => Some(ButtonState::Trigger),
            "color" | "colour" | "3" => Some(ButtonState::Color),
            _ => None,
        }
    }

    /// Rastende Betriebsarten behalten ihren Zustand zwischen den Frames.
    pub fn rastend(self) -> bool {
        matches!(self, ButtonState::On | ButtonState::Color)
    }
}

/// Wodurch eine Taste ausgeloest wird.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Beim Druecken (Flanke 0 -> 1).
    Press,
    /// Beim Loslassen (Flanke 1 -> 0).
    Release,
    /// Wenn sich ein kontinuierlicher Wert aendert.
    ValueChange,
}

impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Press => "press",
            Trigger::Release => "release",
            Trigger::ValueChange => "value_change",
        }
    }

    /// Liest `press`, `on press`, `on_press`, `value change` usw.
    pub fn parse(s: &str) -> Option<Trigger> {
        let roh: String = s
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '_' && *c != '-')
            .collect::<String>()
            .to_ascii_lowercase();
        let roh = roh.strip_prefix("on").unwrap_or(&roh).to_string();
        match roh.as_str() {
            "press" | "down" => Some(Trigger::Press),
            "release" | "up" => Some(Trigger::Release),
            "valuechange" | "change" | "value" => Some(Trigger::ValueChange),
            _ => None,
        }
    }

    /// Liegt fuer den Wechsel `vorher -> jetzt` eine Flanke vor?
    pub fn is_edge(self, vorher: Input, jetzt: Input) -> bool {
        match self {
            Trigger::Press => !vorher.down && jetzt.down,
            Trigger::Release => vorher.down && !jetzt.down,
            Trigger::ValueChange => (jetzt.value - vorher.value).abs() > VALUE_EPS,
        }
    }
}

/// Eingangssignal einer Taste in einem Frame.
///
/// `down` ist der Tastendruck bzw. die MIDI-Note, `value` ein kontinuierlicher
/// Wert (z. B. MIDI-CC) fuer `Trigger::ValueChange`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Input {
    pub down: bool,
    pub value: f64,
}

impl Input {
    pub fn idle() -> Input {
        Input {
            down: false,
            value: 0.0,
        }
    }
    pub fn gedrueckt() -> Input {
        Input {
            down: true,
            value: 1.0,
        }
    }
    pub fn wert(v: f64) -> Input {
        Input {
            down: v >= 0.5,
            value: v,
        }
    }
}

impl Default for Input {
    fn default() -> Self {
        Input::idle()
    }
}

/// Zielparameter einer Aktion, z. B. `strip` / `0` / `mute`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamKey {
    pub target: String,
    pub index: u32,
    pub param: String,
}

impl ParamKey {
    pub fn new(target: &str, index: u32, param: &str) -> ParamKey {
        ParamKey {
            target: target.to_string(),
            index,
            param: param.to_string(),
        }
    }
}

/// Eine einzelne Aktion. Die Engine gibt sie zurueck, sie fuehrt sie nicht aus.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Parameter setzen: `target`/`index`/`param` wie in der Steuer-API des
    /// Haupt-Crates (`strip`/`bus`/`recorder`, `gain`, `mute`, ...).
    SetParam {
        target: String,
        index: u32,
        param: String,
        value: f64,
    },
    /// MIDI-Note senden. `channel` 1..=16, `note` 0..=127, `velocity` 0..=127.
    MidiNote { channel: u8, note: u8, velocity: u8 },
    /// Warten. Wird als Aktion ausgegeben; die Engine blockiert nicht.
    Wait { ms: u32 },
    /// Parameter umschalten. `to` ist der Zielwert, wenn die Engine den
    /// aktuellen Wert kennt (`None` = der Ausfuehrende entscheidet).
    Toggle {
        target: String,
        index: u32,
        param: String,
        to: Option<f64>,
    },
}

impl Action {
    pub fn set(target: &str, index: u32, param: &str, value: f64) -> Action {
        Action::SetParam {
            target: target.to_string(),
            index,
            param: param.to_string(),
            value,
        }
    }

    pub fn midi(channel: u8, note: u8, velocity: u8) -> Action {
        Action::MidiNote {
            channel,
            note,
            velocity,
        }
    }

    pub fn wait(ms: u32) -> Action {
        Action::Wait { ms }
    }

    pub fn toggle(target: &str, index: u32, param: &str) -> Action {
        Action::Toggle {
            target: target.to_string(),
            index,
            param: param.to_string(),
            to: None,
        }
    }

    /// Zielparameter, falls die Aktion einen setzt oder umschaltet. Grundlage
    /// der Konfliktloesung: zwei Aktionen auf denselben Parameter im selben
    /// Frame widersprechen sich.
    pub fn param_key(&self) -> Option<ParamKey> {
        match self {
            Action::SetParam {
                target,
                index,
                param,
                ..
            } => Some(ParamKey::new(target, *index, param)),
            Action::Toggle {
                target,
                index,
                param,
                ..
            } => Some(ParamKey::new(target, *index, param)),
            Action::MidiNote { .. } | Action::Wait { .. } => None,
        }
    }

    /// Kurzbeschreibung fuer Protokoll und Fehlermeldungen.
    pub fn kurz(&self) -> String {
        match self {
            Action::SetParam {
                target,
                index,
                param,
                value,
            } => {
                format!("set {target}[{index}].{param}={value}")
            }
            Action::MidiNote {
                channel,
                note,
                velocity,
            } => {
                format!("midi ch{channel} n{note} v{velocity}")
            }
            Action::Wait { ms } => format!("wait {ms}ms"),
            Action::Toggle {
                target,
                index,
                param,
                to,
            } => match to {
                Some(v) => format!("toggle {target}[{index}].{param}->{v}"),
                None => format!("toggle {target}[{index}].{param}"),
            },
        }
    }
}

/// Eine Taste: Belegung, kein Laufzeitzustand. Latch- und Puls-Zustand haelt
/// die Engine, damit dieselbe Belegung mehrfach verwendbar bleibt.
#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    pub name: String,
    pub state: ButtonState,
    pub trigger: Trigger,
    pub actions: Vec<Action>,
    /// Optionale Verknuepfung zum GUI-Knopf (Slot 0..=255).
    pub gui: Option<u16>,
}

impl Button {
    /// Standardbelegung fuer Slot `index` (0-basiert): aus, Trigger auf Druck.
    pub fn standard(index: usize) -> Button {
        Button {
            name: format!("Makro {}", index + 1),
            state: ButtonState::Off,
            trigger: Trigger::Press,
            actions: Vec::new(),
            gui: None,
        }
    }

    pub fn neu(name: &str, state: ButtonState, trigger: Trigger, actions: Vec<Action>) -> Button {
        Button {
            name: name.to_string(),
            state,
            trigger,
            actions,
            gui: None,
        }
    }

    pub fn mit_gui(mut self, slot: u16) -> Button {
        self.gui = Some(slot);
        self
    }

    /// Prueft Wertebereiche. Wird beim Laden aus JSON aufgerufen, damit eine
    /// kaputte Datei nicht erst im MIDI-Aufruf auffaellt.
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Tastenname ist leer".to_string());
        }
        if let Some(g) = self.gui {
            if g > 255 {
                return Err(format!("GUI-Slot {g} liegt ausserhalb 0..=255"));
            }
        }
        for a in &self.actions {
            match a {
                Action::MidiNote {
                    channel,
                    note,
                    velocity,
                } => {
                    if !(1..=16).contains(channel) {
                        return Err(format!("MIDI-Kanal {channel} liegt ausserhalb 1..=16"));
                    }
                    if *note > 127 {
                        return Err(format!("MIDI-Note {note} liegt ausserhalb 0..=127"));
                    }
                    if *velocity > 127 {
                        return Err(format!("MIDI-Velocity {velocity} liegt ausserhalb 0..=127"));
                    }
                }
                Action::SetParam {
                    target,
                    param,
                    value,
                    ..
                } => {
                    if target.trim().is_empty() || param.trim().is_empty() {
                        return Err("Aktion ohne Ziel oder Parameter".to_string());
                    }
                    if !value.is_finite() {
                        return Err(format!("Wert {value} ist nicht endlich"));
                    }
                }
                Action::Toggle { target, param, .. } => {
                    if target.trim().is_empty() || param.trim().is_empty() {
                        return Err("Aktion ohne Ziel oder Parameter".to_string());
                    }
                }
                Action::Wait { .. } => {}
            }
        }
        Ok(())
    }
}

/// Fehler beim Belegen eines Slots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BankError {
    /// Slot-Nummer ausserhalb 0..MAX_BUTTONS.
    SlotAusserhalb(usize),
    /// Mehr als `MAX_BUTTONS` Tasten.
    ZuViele(usize),
}

impl std::fmt::Display for BankError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BankError::SlotAusserhalb(i) => {
                write!(f, "Slot {i} liegt ausserhalb 0..{MAX_BUTTONS}")
            }
            BankError::ZuViele(n) => write!(f, "{n} Tasten, erlaubt sind hoechstens {MAX_BUTTONS}"),
        }
    }
}

impl std::error::Error for BankError {}

/// Die Tastenbelegung: immer genau `MAX_BUTTONS` Slots, damit GUI- und
/// MIDI-Indizes stabil bleiben.
#[derive(Clone, Debug, PartialEq)]
pub struct Bank {
    buttons: Vec<Button>,
}

impl Bank {
    /// Leere Belegung: 80 Slots, alle aus.
    pub fn new() -> Bank {
        Bank {
            buttons: (0..MAX_BUTTONS).map(Button::standard).collect(),
        }
    }

    /// Belegung aus einer Liste bauen. Kuerzere Listen werden mit
    /// Standardtasten aufgefuellt, laengere abgelehnt.
    pub fn from_buttons(mut buttons: Vec<Button>) -> Result<Bank, BankError> {
        if buttons.len() > MAX_BUTTONS {
            return Err(BankError::ZuViele(buttons.len()));
        }
        while buttons.len() < MAX_BUTTONS {
            buttons.push(Button::standard(buttons.len()));
        }
        Ok(Bank { buttons })
    }

    pub fn len(&self) -> usize {
        self.buttons.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buttons.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&Button> {
        self.buttons.get(index)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut Button> {
        self.buttons.get_mut(index)
    }

    pub fn set(&mut self, index: usize, button: Button) -> Result<(), BankError> {
        match self.buttons.get_mut(index) {
            Some(slot) => {
                *slot = button;
                Ok(())
            }
            None => Err(BankError::SlotAusserhalb(index)),
        }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Button> {
        self.buttons.iter()
    }

    /// Alle Tasten pruefen.
    pub fn validate(&self) -> Result<(), String> {
        for (i, b) in self.buttons.iter().enumerate() {
            b.validate().map_err(|e| format!("Taste {i}: {e}"))?;
        }
        Ok(())
    }
}

impl Default for Bank {
    fn default() -> Self {
        Bank::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_und_trigger_lesen() {
        assert_eq!(ButtonState::parse("2"), Some(ButtonState::Trigger));
        assert_eq!(ButtonState::parse("Color"), Some(ButtonState::Color));
        assert_eq!(ButtonState::from_u8(3), Some(ButtonState::Color));
        assert_eq!(ButtonState::from_u8(4), None);
        assert_eq!(Trigger::parse("on press"), Some(Trigger::Press));
        assert_eq!(Trigger::parse("On_Release"), Some(Trigger::Release));
        assert_eq!(Trigger::parse("value change"), Some(Trigger::ValueChange));
        assert_eq!(Trigger::parse("quatsch"), None);
    }

    #[test]
    fn flanken_erkannt() {
        let los = Input::idle();
        let druck = Input::gedrueckt();
        assert!(Trigger::Press.is_edge(los, druck));
        assert!(!Trigger::Press.is_edge(druck, druck));
        assert!(Trigger::Release.is_edge(druck, los));
        assert!(!Trigger::Release.is_edge(los, los));
        assert!(Trigger::ValueChange.is_edge(Input::wert(0.0), Input::wert(0.5)));
        assert!(!Trigger::ValueChange.is_edge(Input::wert(0.5), Input::wert(0.5)));
    }

    #[test]
    fn standardbank_hat_80_slots() {
        let bank = Bank::new();
        assert_eq!(bank.len(), MAX_BUTTONS);
        assert_eq!(bank.get(79).unwrap().name, "Makro 80");
        assert!(bank.get(80).is_none());
    }

    #[test]
    fn validate_weist_kaputte_midi_werte_ab() {
        let b = Button::neu(
            "x",
            ButtonState::On,
            Trigger::Press,
            vec![Action::midi(17, 60, 100)],
        );
        assert!(b.validate().is_err());
        let b = Button::neu(
            "x",
            ButtonState::On,
            Trigger::Press,
            vec![Action::midi(1, 128, 100)],
        );
        assert!(b.validate().is_err());
        let b = Button::neu(
            "x",
            ButtonState::On,
            Trigger::Press,
            vec![Action::midi(16, 127, 127)],
        );
        assert!(b.validate().is_ok());
    }
}
