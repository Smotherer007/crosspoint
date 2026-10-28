//! Auswertung der Tastenbelegung: welche Taste feuert in diesem Frame, und
//! welche Aktionen ergeben sich daraus.
//!
//! Reine Logik. Die Engine hat keine Uhr und keine Seiteneffekte:
//!
//! * die Frame-Zeit kommt von aussen (`tick(now_ms, inputs)`),
//! * Wartezeiten werden nur als `Action::Wait` ausgegeben,
//! * MIDI-Noten und Parameteraenderungen werden nur beschrieben.
//!
//! Drei Eigenschaften, die im Betrieb wichtig sind:
//!
//! **Flanken statt Pegel.** Eine Taste feuert nur beim Wechsel des Eingangs.
//! Wer denselben Eingang zweimal meldet, bekommt nichts - das ist der
//! Wiederholschutz gegen doppelt ausgefuehrte Aktionen.
//!
//! **Konfliktloesung.** Feuern in einem Frame mehrere Tasten auf denselben
//! Parameter, gewinnt die Taste mit dem hoeheren Slot-Index; die frueheren
//! Aktionen werden verworfen und als `Conflict` gemeldet, damit die Oberflaeche
//! die Doppelbelegung anzeigen kann. Innerhalb *einer* Taste bleibt die
//! Reihenfolge erhalten (setzen, warten, erneut setzen ist Absicht).
//!
//! **Reset.** `reset()` setzt Flanken, Rastzustand, Wiederholsperre und den
//! Parameterspiegel zurueck - z. B. nach dem Laden einer anderen Belegung.

use crate::model::{Action, Bank, ButtonState, Input, ParamKey, MAX_BUTTONS};

/// Ein Konflikt: zwei Tasten wollten im selben Frame denselben Parameter
/// setzen. `kept` bleibt, `dropped` faellt weg (beides Slot-Indizes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub key: ParamKey,
    pub kept: usize,
    pub dropped: usize,
}

/// Die Engine.
pub struct Engine {
    bank: Bank,
    /// Eingang des letzten Frames je Taste.
    prev: Vec<Input>,
    /// Rastzustand fuer `On`/`Color`.
    latched: Vec<bool>,
    /// Taste hat in diesem Frame ausgeloest (`Trigger`-Betriebsart).
    pulse: Vec<bool>,
    /// Zeitpunkt des letzten Feuerns je Taste (nur fuer die Wiederholsperre).
    last_fire: Vec<Option<u64>>,
    /// Mindestabstand zwischen zwei Ausloesungen derselben Taste in ms.
    repeat_guard_ms: u64,
    /// Zuletzt gemeldete Frame-Zeit.
    now_ms: u64,
    /// Spiegel der Parameterwerte, die der Hauptprozess zuletzt gesetzt hat.
    /// Grundlage fuer `Action::Toggle` und fuer Anzeigen.
    shadow: Vec<(ParamKey, f64)>,
    conflicts: Vec<Conflict>,
    fired: Vec<usize>,
    frames: u64,
    fires: u64,
}

impl Engine {
    pub fn new(bank: Bank) -> Engine {
        let n = bank.len();
        Engine {
            bank,
            prev: vec![Input::idle(); n],
            latched: vec![false; n],
            pulse: vec![false; n],
            last_fire: vec![None; n],
            repeat_guard_ms: 0,
            now_ms: 0,
            shadow: Vec::new(),
            conflicts: Vec::new(),
            fired: Vec::new(),
            frames: 0,
            fires: 0,
        }
    }

    /// Belegung laden und Engine bauen.
    pub fn load(text: &str) -> Result<Engine, String> {
        let bank = crate::persist::bank_from_json(text)?;
        Ok(Engine::new(bank))
    }

    pub fn bank(&self) -> &Bank {
        &self.bank
    }

    pub fn bank_mut(&mut self) -> &mut Bank {
        &mut self.bank
    }

    /// Belegung tauschen. Setzt den Laufzeitzustand zurueck, weil die alten
    /// Flanken zu den neuen Tasten nicht mehr passen.
    pub fn set_bank(&mut self, bank: Bank) {
        let n = bank.len();
        self.bank = bank;
        self.prev = vec![Input::idle(); n];
        self.latched = vec![false; n];
        self.pulse = vec![false; n];
        self.last_fire = vec![None; n];
        self.conflicts.clear();
        self.fired.clear();
    }

    pub fn repeat_guard_ms(&self) -> u64 {
        self.repeat_guard_ms
    }

    /// Mindestabstand in ms, in dem dieselbe Taste erneut feuern darf.
    /// 0 = keine Sperre (Standard). Eine durch die Sperre verschluckte Flanke
    /// gilt als verbraucht und feuert nicht nach.
    pub fn set_repeat_guard_ms(&mut self, ms: u64) {
        self.repeat_guard_ms = ms;
    }

    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn fires(&self) -> u64 {
        self.fires
    }

    /// Tasten, die im letzten Frame ausgeloest haben.
    pub fn fired(&self) -> &[usize] {
        &self.fired
    }

    /// Konflikte des letzten Frames.
    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }

    /// Rastzustand einer Taste (nur `On`/`Color` werden je wahr).
    pub fn is_latched(&self, index: usize) -> bool {
        self.latched.get(index).copied().unwrap_or(false)
    }

    /// Hat die Taste im letzten Frame getastet (`Trigger`-Betriebsart)?
    pub fn is_pulsing(&self, index: usize) -> bool {
        self.pulse.get(index).copied().unwrap_or(false)
    }

    /// Anzeigezustand: was die Oberflaeche fuer diese Taste malen soll.
    pub fn display_state(&self, index: usize) -> Option<ButtonState> {
        let konfig = self.bank.get(index)?.state;
        let aktiv = if konfig.rastend() {
            self.is_latched(index)
        } else {
            self.is_pulsing(index)
        };
        Some(if aktiv {
            konfig
        } else {
            ButtonState::Off
        })
    }

    /// Kennt die Engine den aktuellen Wert eines Parameters?
    pub fn known_param(&self, key: &ParamKey) -> Option<f64> {
        self.shadow.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
    }

    /// Parameterspiegel setzen. Der Hauptprozess ruft das nach jeder
    /// ausgefuehrten Aenderung auf, damit `Toggle` den Zielwert kennt.
    pub fn set_param(&mut self, target: &str, index: u32, param: &str, value: f64) {
        let key = ParamKey::new(target, index, param);
        if let Some(eintrag) = self.shadow.iter_mut().find(|(k, _)| *k == key) {
            eintrag.1 = value;
        } else {
            self.shadow.push((key, value));
        }
    }

    /// Aktion im Spiegel nachziehen, ohne sie auszufuehren. Praktisch fuer den
    /// Aufrufer, der die Aktionen ohnehin gleich abarbeitet, und fuer Tests.
    pub fn apply(&mut self, action: &Action) {
        match action {
            Action::SetParam { target, index, param, value } => {
                self.set_param(target, *index, param, *value)
            }
            Action::Toggle { target, index, param, to: Some(v) } => {
                self.set_param(target, *index, param, *v)
            }
            Action::Toggle { .. } | Action::MidiNote { .. } | Action::Wait { .. } => {}
        }
    }

    /// `Toggle` in einen konkreten Zielwert aufloesen: kennt die Engine den
    /// aktuellen Wert, wird umgeschaltet (>0,5 gilt als ein), sonst bleibt
    /// `to` offen und der Ausfuehrende entscheidet.
    fn toggle_aufloesen(&self, action: &Action) -> Action {
        match action {
            Action::Toggle { target, index, param, .. } => {
                let key = ParamKey::new(target, *index, param);
                let to = self
                    .known_param(&key)
                    .map(|v| if v >= 0.5 { 0.0 } else { 1.0 });
                Action::Toggle {
                    target: target.clone(),
                    index: *index,
                    param: param.clone(),
                    to,
                }
            }
            andere => andere.clone(),
        }
    }

    /// Einen Frame auswerten.
    ///
    /// `inputs` ist je Slot ein `Input`; fehlende Slots gelten als
    /// unveraendert. Rueckgabe sind die auszufuehrenden Aktionen in
    /// Slot-Reihenfolge, bereits konfliktbereinigt.
    pub fn tick(&mut self, now_ms: u64, inputs: &[Input]) -> Vec<Action> {
        self.now_ms = now_ms;
        self.frames = self.frames.saturating_add(1);
        self.conflicts.clear();
        self.fired.clear();
        for p in self.pulse.iter_mut() {
            *p = false;
        }

        let mut roh: Vec<(usize, Action)> = Vec::new();

        for i in 0..self.bank.len() {
            let vorher = self.prev[i];
            // Fehlender Eingang: Zustand bleibt, also keine Flanke.
            let jetzt = match inputs.get(i) {
                Some(v) => *v,
                None => vorher,
            };
            self.prev[i] = jetzt;

            let konfig = match self.bank.get(i) {
                Some(b) => (b.state, b.trigger),
                None => continue,
            };
            let (state, trigger) = konfig;

            if !trigger.is_edge(vorher, jetzt) {
                continue;
            }

            // Wiederholschutz: zu frueh nach dem letzten Feuern -> Flanke
            // verfaellt, damit ein prellender Eingang nicht mehrfach ausloest.
            if self.repeat_guard_ms > 0 {
                if let Some(t) = self.last_fire[i] {
                    if now_ms.saturating_sub(t) < self.repeat_guard_ms {
                        continue;
                    }
                }
            }

            match state {
                ButtonState::Off => continue,
                ButtonState::Trigger => self.pulse[i] = true,
                ButtonState::On | ButtonState::Color => self.latched[i] = !self.latched[i],
            }

            self.last_fire[i] = Some(now_ms);
            self.fires = self.fires.saturating_add(1);
            self.fired.push(i);

            if let Some(button) = self.bank.get(i) {
                for a in &button.actions {
                    roh.push((i, self.toggle_aufloesen(a)));
                }
            }
        }

        // Konfliktloesung: gleicher Zielparameter -> der spaetere Slot gewinnt.
        let mut behalten = vec![true; roh.len()];
        for a in 0..roh.len() {
            if !behalten[a] {
                continue;
            }
            let key_a = match roh[a].1.param_key() {
                Some(k) => k,
                None => continue,
            };
            for b in (a + 1)..roh.len() {
                if !behalten[b] {
                    continue;
                }
                // Innerhalb einer Taste ist die Reihenfolge Absicht (setzen,
                // warten, wieder setzen). Konflikte entstehen nur zwischen
                // verschiedenen Tasten.
                if roh[b].0 == roh[a].0 {
                    continue;
                }
                if roh[b].1.param_key().as_ref() == Some(&key_a) {
                    behalten[a] = false;
                    self.conflicts.push(Conflict {
                        key: key_a.clone(),
                        kept: roh[b].0,
                        dropped: roh[a].0,
                    });
                    break;
                }
            }
        }

        let mut aus = Vec::with_capacity(roh.len());
        for (i, (_, action)) in roh.into_iter().enumerate() {
            if behalten[i] {
                aus.push(action);
            }
        }
        aus
    }

    /// Kompletter Reset: Flanken, Rastzustand, Wiederholsperre, Parameterspiegel
    /// und Zaehler. Die Belegung bleibt.
    pub fn reset(&mut self) {
        for p in self.prev.iter_mut() {
            *p = Input::idle();
        }
        for l in self.latched.iter_mut() {
            *l = false;
        }
        for p in self.pulse.iter_mut() {
            *p = false;
        }
        for t in self.last_fire.iter_mut() {
            *t = None;
        }
        self.shadow.clear();
        self.conflicts.clear();
        self.fired.clear();
        self.now_ms = 0;
        self.frames = 0;
        self.fires = 0;
    }

    /// Nur eine Taste zuruecksetzen (z. B. nachdem sie aus der GUI geloest wurde).
    pub fn reset_button(&mut self, index: usize) {
        if index >= self.prev.len() {
            return;
        }
        self.prev[index] = Input::idle();
        self.latched[index] = false;
        self.pulse[index] = false;
        self.last_fire[index] = None;
        self.fired.retain(|i| *i != index);
        self.conflicts.retain(|c| c.kept != index && c.dropped != index);
    }

    /// Alle 80 Eingaenge auf einmal druecken/lassen - bequem fuer Tests und
    /// fuer eine "alle Tasten aus"-Taste.
    pub fn alle_eingaenge(down: bool) -> Vec<Input> {
        vec![Input { down, value: if down { 1.0 } else { 0.0 } }; MAX_BUTTONS]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, Button, ButtonState, Trigger};

    fn bank_mit(index: usize, button: Button) -> Bank {
        let mut bank = Bank::new();
        bank.set(index, button).unwrap();
        bank
    }

    /// Eingangsvektor voller Laenge mit einem gesetzten Slot. Kuerzere Slices
    /// gelten als "Slot unveraendert", deshalb hier immer alle 80 Slots.
    fn ein(index: usize, input: Input) -> Vec<Input> {
        let mut v = vec![Input::idle(); MAX_BUTTONS];
        v[index] = input;
        v
    }

    /// Trigger feuert genau einmal pro Flanke, nicht pro Frame.
    #[test]
    fn trigger_feuert_genau_einmal_pro_flanke() {
        let mut bank = Bank::new();
        bank.set(
            0,
            Button::neu(
                "Mute",
                ButtonState::Trigger,
                Trigger::Press,
                vec![Action::set("strip", 0, "mute", 1.0)],
            ),
        )
        .unwrap();
        let mut e = Engine::new(bank);

        // Frame 1: Ruhe -> nichts.
        assert!(e.tick(0, &[]).is_empty());

        // Frame 2: Druck -> genau eine Aktion, genau eine Ausloesung.
        let a = e.tick(10, &[Input::gedrueckt()]);
        assert_eq!(a.len(), 1);
        assert_eq!(a[0], Action::set("strip", 0, "mute", 1.0));
        assert_eq!(e.fires(), 1);

        // Frame 3 und 4: weiter gedrueckt -> keine neue Flanke, nichts.
        assert!(e.tick(20, &[Input::gedrueckt()]).is_empty());
        assert!(e.tick(30, &[Input::gedrueckt()]).is_empty());
        assert_eq!(e.fires(), 1);

        // Loslassen loest bei Trigger::Press nicht aus.
        assert!(e.tick(40, &[Input::idle()]).is_empty());

        // Naechster Druck -> wieder genau eine Aktion.
        let a = e.tick(50, &[Input::gedrueckt()]);
        assert_eq!(a.len(), 1);
        assert_eq!(e.fires(), 2);
    }

    /// On/Release-Taste: feuert beim Loslassen, nicht beim Druecken.
    #[test]
    fn release_taste_feuert_beim_loslassen() {
        let bank = bank_mit(
            3,
            Button::neu(
                "Los",
                ButtonState::On,
                Trigger::Release,
                vec![Action::midi(1, 60, 0)],
            ),
        );
        let mut e = Engine::new(bank);
        assert!(e.tick(0, &ein(3, Input::gedrueckt())).is_empty());
        let a = e.tick(5, &ein(3, Input::idle()));
        assert_eq!(a, vec![Action::midi(1, 60, 0)]);
    }

    /// Werttrigger: feuert bei Aenderung, nicht bei gleichem Wert.
    #[test]
    fn werttrigger_feuert_nur_bei_aenderung() {
        let bank = bank_mit(
            1,
            Button::neu(
                "Fader",
                ButtonState::Trigger,
                Trigger::ValueChange,
                vec![Action::set("strip", 1, "gain", -6.0)],
            ),
        );
        let mut e = Engine::new(bank);
        assert!(e.tick(0, &ein(1, Input::wert(0.0))).is_empty());
        assert_eq!(e.tick(1, &ein(1, Input::wert(0.25))).len(), 1);
        assert!(e.tick(2, &ein(1, Input::wert(0.25))).is_empty());
        assert_eq!(e.tick(3, &ein(1, Input::wert(0.25 + 1e-12))).len(), 0); // unter der Toleranz
        assert_eq!(e.tick(4, &ein(1, Input::wert(0.9))).len(), 1);
    }

    /// Toggle: schaltet den Parameter um, auch ueber mehrere Frames.
    #[test]
    fn toggle_wechselt() {
        let bank = bank_mit(
            0,
            Button::neu(
                "Mute",
                ButtonState::On,
                Trigger::Press,
                vec![Action::toggle("strip", 0, "mute")],
            ),
        );
        let mut e = Engine::new(bank);
        e.set_param("strip", 0, "mute", 0.0);

        // Erster Druck: 0 -> 1, Taste rastet ein.
        let a = e.tick(0, &[Input::gedrueckt()]);
        assert_eq!(a.len(), 1);
        match &a[0] {
            Action::Toggle { to, .. } => assert_eq!(*to, Some(1.0)),
            other => panic!("Toggle erwartet, war {other:?}"),
        }
        assert!(e.is_latched(0));
        e.apply(&a[0]);

        // Zweiter Druck: 1 -> 0, Taste faellt heraus.
        assert!(e.tick(10, &[Input::idle()]).is_empty());
        let a = e.tick(20, &[Input::gedrueckt()]);
        match &a[0] {
            Action::Toggle { to, .. } => assert_eq!(*to, Some(0.0)),
            other => panic!("Toggle erwartet, war {other:?}"),
        }
        assert!(!e.is_latched(0));
        e.apply(&a[0]);
        assert_eq!(e.known_param(&ParamKey::new("strip", 0, "mute")), Some(0.0));

        // Dritter Druck: wieder 1. Der Wechsel ist zyklisch.
        assert!(e.tick(30, &[Input::idle()]).is_empty());
        let a = e.tick(40, &[Input::gedrueckt()]);
        match &a[0] {
            Action::Toggle { to, .. } => assert_eq!(*to, Some(1.0)),
            other => panic!("Toggle erwartet, war {other:?}"),
        }
    }

    /// Wartezeit wird zur Aktion, blockiert aber nicht. Die Messung belegt,
    /// dass `tick` sofort zurueckkehrt, obwohl 5000 ms angefordert sind.
    #[test]
    fn wartezeit_blockiert_nicht() {
        let bank = bank_mit(
            0,
            Button::neu(
                "Ansage",
                ButtonState::Trigger,
                Trigger::Press,
                vec![
                    Action::set("bus", 0, "mute", 0.0),
                    Action::wait(5000),
                    Action::set("bus", 0, "mute", 1.0),
                ],
            ),
        );
        let mut e = Engine::new(bank);
        assert!(e.tick(0, &[Input::idle()]).is_empty());

        let start = std::time::Instant::now();
        let a = e.tick(1, &[Input::gedrueckt()]);
        let gedauert = start.elapsed();

        assert_eq!(a.len(), 3);
        assert_eq!(a[1], Action::wait(5000));
        assert_eq!(a[0], Action::set("bus", 0, "mute", 0.0));
        assert_eq!(a[2], Action::set("bus", 0, "mute", 1.0));
        // Die Engine hat keine Uhr: sie zaehlt die Zeit nicht hoch.
        assert_eq!(e.now_ms(), 1);
        assert!(
            gedauert < std::time::Duration::from_millis(100),
            "tick hat {gedauert:?} gebraucht, darf nicht warten"
        );
        println!("tick mit Wait 5000 ms dauerte {gedauert:?}");
    }

    /// Ausschalten (State off) ignoriert Trigger voellig.
    #[test]
    fn ausschalten_ignoriert_trigger() {
        let bank = bank_mit(
            0,
            Button::neu(
                "Aus",
                ButtonState::Off,
                Trigger::Press,
                vec![Action::set("strip", 0, "mute", 1.0)],
            ),
        );
        let mut e = Engine::new(bank);
        assert!(e.tick(0, &[Input::gedrueckt()]).is_empty());
        assert!(e.tick(10, &[Input::gedrueckt()]).is_empty());
        assert_eq!(e.fires(), 0);
        assert_eq!(e.display_state(0), Some(ButtonState::Off));
    }

    /// Konflikt: zwei Tasten setzen im selben Frame denselben Parameter.
    #[test]
    fn konflikt_letzter_slot_gewinnt() {
        let mut bank = Bank::new();
        bank.set(
            5,
            Button::neu(
                "Leise",
                ButtonState::Trigger,
                Trigger::Press,
                vec![Action::set("strip", 0, "gain", -20.0)],
            ),
        )
        .unwrap();
        bank.set(
            6,
            Button::neu(
                "Laut",
                ButtonState::Trigger,
                Trigger::Press,
                vec![
                    Action::set("strip", 0, "gain", 0.0),
                    Action::midi(1, 64, 100),
                ],
            ),
        )
        .unwrap();
        let mut e = Engine::new(bank);
        let mut ein = vec![Input::idle(); MAX_BUTTONS];
        ein[5] = Input::gedrueckt();
        ein[6] = Input::gedrueckt();

        let a = e.tick(0, &ein);
        // Zwei Aktionen bleiben: das MIDI von Slot 6 und der Gewinner-Set.
        assert_eq!(a.len(), 2);
        assert!(a.contains(&Action::set("strip", 0, "gain", 0.0)));
        assert!(!a.contains(&Action::set("strip", 0, "gain", -20.0)));
        assert!(a.contains(&Action::midi(1, 64, 100)));

        assert_eq!(e.conflicts().len(), 1);
        let c = &e.conflicts()[0];
        assert_eq!(c.kept, 6);
        assert_eq!(c.dropped, 5);
        assert_eq!(c.key, ParamKey::new("strip", 0, "gain"));
    }

    /// Wiederholschutz: prellender Eingang loest innerhalb der Sperre nicht
    /// erneut aus, danach wieder.
    #[test]
    fn wiederholschutz_sperrt_prellen() {
        let bank = bank_mit(
            0,
            Button::neu(
                "Tipp",
                ButtonState::Trigger,
                Trigger::Press,
                vec![Action::midi(1, 36, 100)],
            ),
        );
        let mut e = Engine::new(bank);
        e.set_repeat_guard_ms(50);

        assert_eq!(e.tick(0, &[Input::gedrueckt()]).len(), 1);
        // Prellen: los und nach 20 ms wieder druck -> innerhalb der Sperre.
        assert!(e.tick(5, &[Input::idle()]).is_empty());
        assert!(e.tick(20, &[Input::gedrueckt()]).is_empty());
        assert_eq!(e.fires(), 1);
        // Nach Ablauf der Sperre feuert die naechste Flanke normal.
        assert!(e.tick(60, &[Input::idle()]).is_empty());
        assert_eq!(e.tick(70, &[Input::gedrueckt()]).len(), 1);
        assert_eq!(e.fires(), 2);
    }

    /// Reset: Flanken, Rastzustand, Spiegel und Zaehler sind danach leer,
    /// die Belegung bleibt.
    #[test]
    fn reset_raeumt_laufzeitzustand() {
        let mut bank = Bank::new();
        bank.set(
            0,
            Button::neu(
                "Mute",
                ButtonState::On,
                Trigger::Press,
                vec![Action::toggle("strip", 0, "mute")],
            ),
        )
        .unwrap();
        let mut e = Engine::new(bank);
        e.set_param("strip", 0, "mute", 1.0);
        assert!(!e.tick(0, &[Input::gedrueckt()]).is_empty());
        assert!(e.is_latched(0));

        e.reset();
        assert!(!e.is_latched(0));
        assert_eq!(e.fires(), 0);
        assert_eq!(e.frames(), 0);
        assert_eq!(e.known_param(&ParamKey::new("strip", 0, "mute")), None);
        assert_eq!(e.bank().get(0).unwrap().name, "Mute");

        // Nach dem Reset ist der naechste Druck wieder eine steigende Flanke,
        // und weil der Spiegel leer ist, bleibt der Toggle-Zielwert offen.
        let a = e.tick(0, &[Input::gedrueckt()]);
        assert_eq!(a.len(), 1);
        match &a[0] {
            Action::Toggle { to, .. } => assert_eq!(*to, None),
            other => panic!("Toggle erwartet, war {other:?}"),
        }
        assert!(e.is_latched(0));
    }

    /// 80 Slots: Index 79 feuert, fehlende Eingaben stoeren nicht.
    #[test]
    fn grenze_80_slots() {
        let mut bank = Bank::new();
        bank.set(
            79,
            Button::neu(
                "Letzte",
                ButtonState::Trigger,
                Trigger::Press,
                vec![Action::midi(16, 127, 127)],
            ),
        )
        .unwrap();
        let mut e = Engine::new(bank);
        assert_eq!(e.tick(0, &[]).len(), 0);
        let ein = Engine::alle_eingaenge(false);
        assert_eq!(ein.len(), MAX_BUTTONS);
        assert_eq!(e.tick(1, &ein).len(), 0);

        let mut ein = vec![Input::idle(); MAX_BUTTONS];
        ein[79] = Input::gedrueckt();
        let a = e.tick(2, &ein);
        assert_eq!(a, vec![Action::midi(16, 127, 127)]);
        assert_eq!(e.fired().len(), 1);
        assert_eq!(e.fired()[0], 79);
    }

    /// Leere Belegung (keine Aktion) ist kein Fehler, sondern ein leerer Frame.
    #[test]
    fn taste_ohne_aktionen_ist_still() {
        let bank = bank_mit(0, Button::neu("Leer", ButtonState::On, Trigger::Press, vec![]));
        let mut e = Engine::new(bank);
        assert!(e.tick(0, &[Input::gedrueckt()]).is_empty());
        // Rasten ja, aber ohne Aktion.
        assert!(e.is_latched(0));
        assert_eq!(e.display_state(0), Some(ButtonState::On));
    }
}
