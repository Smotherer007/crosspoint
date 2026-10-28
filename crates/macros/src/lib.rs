//! Macro-Button-Engine: 80 Tasten, Trigger, Aktionen.
//!
//! Das Crate ist eigenstaendig und ohne Fremd-Crates. Es beschreibt nur, *was*
//! eine Taste ausloest - die Aktionen werden im Hauptprozess ausgefuehrt
//! (Parameter setzen, MIDI senden, warten). Deshalb gibt es hier keine
//! Seiteneffekte, keine Uhr und keine Threads.
//!
//! ```text
//! Eingang je Slot -> Flankenerkennung -> Betriebsart (off/on/trigger/color)
//!                 -> Aktionen der Taste -> Konfliktloesung -> Vec<Action>
//! ```
//!
//! # Beispiel
//!
//! ```
//! use macros::{Action, Bank, Button, ButtonState, Engine, Input, Trigger};
//!
//! let mut bank = Bank::new();
//! bank.set(
//!     0,
//!     Button::neu(
//!         "Mute Streifen 1",
//!         ButtonState::On,
//!         Trigger::Press,
//!         vec![Action::set("strip", 0, "mute", 1.0)],
//!     ),
//! )
//! .unwrap();
//!
//! let mut engine = Engine::new(bank);
//! assert!(engine.tick(0, &[Input::idle()]).is_empty());      // Ruhe
//! let aktionen = engine.tick(16, &[Input::gedrueckt()]);     // Druck
//! assert_eq!(aktionen, vec![Action::set("strip", 0, "mute", 1.0)]);
//! assert!(engine.tick(32, &[Input::gedrueckt()]).is_empty()); // kein Dauerfeuer
//! ```
//!
//! # Grenzen (ehrlich)
//!
//! Die Existenz von 80 Tasten, der State-Spalte 0..3 und der Trigger-Arten ist
//! aus dem MacroButtons-Fenster uebernommen; das **genaue** Verhalten des
//! Originals (insbesondere ob `on`/`color` beim Umschalten erneut Aktionen
//! ausfuehren) ist hier nicht belegt, sondern festgelegt:
//!
//! * `off`   - keine Aktion, Flanken werden verbraucht.
//! * `on`    - rastend, jede Flanke schaltet um **und** fuehrt die Aktionen aus,
//!             damit der Zielparameter dem Tastenzustand folgt.
//! * `color` - wie `on`, nur mit anderer Anzeige.
//! * `trigger` - tastenartig, jede Flanke fuehrt die Aktionen einmal aus.
//!
//! Wartezeiten sind reine Daten (`Action::Wait`); die Engine misst keine Zeit.

#![forbid(unsafe_code)]

pub mod engine;
pub mod json;
pub mod model;
pub mod persist;

pub use engine::{Conflict, Engine};
pub use model::{
    Action, Bank, BankError, Button, ButtonState, Input, ParamKey, Trigger, MAX_BUTTONS, VALUE_EPS,
};
pub use persist::{bank_from_json, bank_to_json, FORMAT_VERSION};

/// Kurzcheck fuer Aufrufer: ist die Belegung vollstaendig und plausibel?
pub fn belegung_pruefen(bank: &Bank) -> Result<(), String> {
    if bank.len() != MAX_BUTTONS {
        return Err(format!("{} Slots statt {MAX_BUTTONS}", bank.len()));
    }
    bank.validate()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gesamtablauf: Tastenbelegung aus JSON, zwei Tasten im selben Frame,
    /// Konflikt, Wartezeit als Aktion, Rueckweg in JSON.
    #[test]
    fn gesamtlauf_ueber_json_und_engine() {
        let mut bank = Bank::new();
        bank.set(
            0,
            Button::neu(
                "Szene A",
                ButtonState::Trigger,
                Trigger::Press,
                vec![
                    Action::set("bus", 0, "gain", -6.0),
                    Action::wait(300),
                    Action::midi(1, 60, 100),
                ],
            )
            .mit_gui(1),
        )
        .unwrap();
        bank.set(
            1,
            Button::neu(
                "Szene B",
                ButtonState::Trigger,
                Trigger::Press,
                vec![Action::set("bus", 0, "gain", 0.0)],
            ),
        )
        .unwrap();

        let text = bank_to_json(&bank);
        assert!(belegung_pruefen(&bank).is_ok());

        let mut engine = Engine::load(&text).expect("Belegung laden");
        assert_eq!(engine.bank(), &bank);

        let mut ein = vec![Input::idle(); MAX_BUTTONS];
        ein[0] = Input::gedrueckt();
        ein[1] = Input::gedrueckt();
        let aktionen = engine.tick(100, &ein);

        // Konflikt: Taste 1 gewinnt gegen Taste 0, die Reihenfolge der
        // verbleibenden Aktionen bleibt: Wait, MIDI, dann der Gewinner-Set.
        assert_eq!(aktionen.len(), 3);
        assert_eq!(aktionen[0], Action::wait(300));
        assert_eq!(aktionen[1], Action::midi(1, 60, 100));
        assert_eq!(aktionen[2], Action::set("bus", 0, "gain", 0.0));
        assert_eq!(engine.conflicts().len(), 1);
        assert_eq!(engine.display_state(0), Some(ButtonState::Trigger));
    }

    #[test]
    fn belegung_pruefen_meldet_kaputte_slots() {
        let mut bank = Bank::new();
        bank.set(
            4,
            Button::neu("", ButtonState::On, Trigger::Press, vec![]),
        )
        .unwrap();
        let fehler = belegung_pruefen(&bank).unwrap_err();
        assert!(fehler.contains("Taste 4"), "Meldung war: {fehler}");
    }
}
