//! Integrationstest: benutzt nur die oeffentliche API des Crates, so wie es
//! der Hauptprozess tun wird - Belegung bauen, als JSON ablegen und wieder
//! laden, dann Frames auswerten.

use macros::{
    bank_from_json, bank_to_json, Action, Bank, Button, ButtonState, Engine, Input, Trigger,
    MAX_BUTTONS,
};

fn belegung() -> Bank {
    let mut bank = Bank::new();
    bank.set(
        0,
        Button::neu(
            "Szene: Aufnahme",
            ButtonState::On,
            Trigger::Press,
            vec![
                Action::set("strip", 0, "mute", 0.0),
                Action::wait(250),
                Action::midi(1, 60, 127),
                Action::toggle("bus", 2, "mute"),
            ],
        )
        .mit_gui(0),
    )
    .unwrap();
    bank.set(
        79,
        Button::neu(
            "Alles aus",
            ButtonState::Trigger,
            Trigger::Release,
            vec![
                Action::set("bus", 0, "mute", 1.0),
                Action::set("bus", 7, "mute", 1.0),
            ],
        ),
    )
    .unwrap();
    bank
}

#[test]
fn belegung_ueber_die_oeffentliche_api() {
    let bank = belegung();
    let text = bank_to_json(&bank);

    // Eine Taste je Zeile: die Datei bleibt lesbar.
    assert!(
        text.contains("},\n{\"name\":"),
        "Formatierung: {}",
        &text[..80]
    );

    let zurueck = bank_from_json(&text).unwrap();
    assert_eq!(zurueck, bank);
    assert_eq!(zurueck.len(), MAX_BUTTONS);

    let mut engine = Engine::new(zurueck);
    engine.set_param("bus", 2, "mute", 0.0);

    let mut ein = vec![Input::idle(); MAX_BUTTONS];
    ein[0] = Input::gedrueckt();
    let aktionen = engine.tick(0, &ein);
    assert_eq!(aktionen.len(), 4);
    assert_eq!(aktionen[0], Action::set("strip", 0, "mute", 0.0));
    assert_eq!(aktionen[1], Action::wait(250));
    assert_eq!(aktionen[2], Action::midi(1, 60, 127));
    match &aktionen[3] {
        Action::Toggle { to, .. } => assert_eq!(*to, Some(1.0)),
        other => panic!("Toggle erwartet, war {other:?}"),
    }
    assert_eq!(engine.display_state(0), Some(ButtonState::On));

    // Loslassen von Slot 79 (Trigger::Release) setzt beide Busse stumm.
    ein[0] = Input::idle();
    ein[79] = Input::gedrueckt();
    assert!(engine.tick(10, &ein).is_empty());
    ein[79] = Input::idle();
    let aus = engine.tick(20, &ein);
    assert_eq!(
        aus,
        vec![
            Action::set("bus", 0, "mute", 1.0),
            Action::set("bus", 7, "mute", 1.0)
        ]
    );

    println!("--- Beispiel, erste zwei Tasten ---");
    let kurz = Bank::from_buttons(vec![
        belegung().get(0).unwrap().clone(),
        belegung().get(79).unwrap().clone(),
    ])
    .unwrap();
    let kurz_text = bank_to_json(&kurz);
    let ende = kurz_text.find("\"Makro 3\"").unwrap_or(kurz_text.len());
    println!("{}", &kurz_text[..ende]);
}
