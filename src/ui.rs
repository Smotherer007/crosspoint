//! Native Bedienoberfläche (egui) — **kein** HTTP, **kein** Port.
//!
//! Das Fenster läuft im selben Prozess wie die Audio-Engine und greift direkt
//! auf `Params` zu. Damit ist die App autark: ein Prozess, ein Fenster.

use eframe::egui;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use crate::engine::Params;
use crate::tape_io::TapeControl;

const STRIP_LABELS: [&str; 8] = [
    "HW IN 1", "HW IN 2", "HW IN 3", "HW IN 4", "HW IN 5",
    "VIRT IN 1", "VIRT IN 2", "VIRT IN 3",
];
const BUS_LABELS: [&str; 8] = ["A1", "A2", "A3", "A4", "A5", "B1", "B2", "B3"];

pub struct Ui {
    params: Arc<Params>,
    tape: Arc<TapeControl>,
}

impl Ui {
    pub fn new(params: Arc<Params>, tape: Arc<TapeControl>) -> Ui {
        Ui { params, tape }
    }

    /// Startet das Fenster (blockiert bis zum Schließen).
    pub fn start(params: Arc<Params>, tape: Arc<TapeControl>) -> eframe::Result<()> {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1500.0, 900.0])
                .with_title("vm8 · Virtual Mixing Console"),
            ..Default::default()
        };
        eframe::run_native(
            "vm8",
            options,
            Box::new(|_cc| Ok(Box::new(Ui::new(params, tape)))),
        )
    }
}

fn fget(a: &std::sync::atomic::AtomicU32) -> f32 {
    f32::from_bits(a.load(Relaxed))
}
fn fset(a: &std::sync::atomic::AtomicU32, v: f32) {
    a.store(v.to_bits(), Relaxed);
}

impl eframe::App for Ui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let p = self.params.clone();
        let tape = self.tape.clone();
        egui::TopBottomPanel::top("kopf").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("vm8");
                ui.label("VIRTUELLER MIXER · nativ, ohne Netzwerk");
                let sr = fget(&p.sample_rate) as u32;
                ui.separator();
                ui.label(format!("{sr} Hz"));
                ui.label(format!("Block {}", p.block.load(Relaxed)));
            });
        });

        egui::TopBottomPanel::bottom("tape").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("TAPE");
                let rec = tape.recording.load(Relaxed);
                ui.label(
                    egui::RichText::new("● REC")
                        .color(if rec { egui::Color32::RED } else { egui::Color32::DARK_GRAY }),
                );
                if ui.button(if rec { "STOP" } else { "REC" }).clicked() {
                    tape.recording.store(!rec, Relaxed);
                }
                ui.label(tape.status());
                ui.separator();
                ui.label("Standardpfad: ~/.config/vm8-linux/tape/");
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                for s in 0..8 {
                    ui.vertical(|ui| {
                        ui.set_width(150.0);
                        ui.label(egui::RichText::new(STRIP_LABELS[s]).strong());
                        let strip = &p.strips[s];

                        // Pegel
                        let lvl = crate::dsp::lin_to_db(fget(&strip.level));
                        let bar = egui::ProgressBar::new(((lvl + 60.0) / 72.0).clamp(0.0, 1.0))
                            .show_percentage();
                        ui.add(bar);

                        // Gain als vertikaler Schieber
                        let mut gain = fget(&strip.gain);
                        if ui
                            .add(
                                egui::Slider::new(&mut gain, -60.0..=12.0)
                                    .vertical()
                                    .text("dB"),
                            )
                            .changed()
                        {
                            fset(&strip.gain, gain);
                        }

                        ui.horizontal(|ui| {
                            let mut mute = strip.mute.load(Relaxed);
                            if ui.toggle_value(&mut mute, "M").changed() {
                                strip.mute.store(mute, Relaxed);
                            }
                            let mut solo = strip.solo.load(Relaxed);
                            if ui.toggle_value(&mut solo, "S").changed() {
                                strip.solo.store(solo, Relaxed);
                            }
                            let mut mono = strip.mono.load(Relaxed);
                            if ui.toggle_value(&mut mono, "Ø").changed() {
                                strip.mono.store(mono, Relaxed);
                            }
                        });

                        // Routing-Matrix
                        for row in 0..2 {
                            ui.horizontal(|ui| {
                                for k in 0..4 {
                                    let b = row * 4 + k;
                                    if b >= 8 {
                                        continue;
                                    }
                                    let mut on = fget(&strip.sends[b]) > 0.0;
                                    if ui
                                        .selectable_label(on, BUS_LABELS[b])
                                        .clicked()
                                    {
                                        on = !on;
                                        fset(&strip.sends[b], if on { 1.0 } else { 0.0 });
                                    }
                                }
                            });
                        }

                        ui.horizontal(|ui| {
                            let mut eq = strip.eq_on.load(Relaxed);
                            if ui.toggle_value(&mut eq, "EQ").changed() {
                                strip.eq_on.store(eq, Relaxed);
                            }
                            let mut c = strip.comp.on.load(Relaxed);
                            if ui.toggle_value(&mut c, "C").changed() {
                                strip.comp.on.store(c, Relaxed);
                            }
                            let mut g = strip.gate.on.load(Relaxed);
                            if ui.toggle_value(&mut g, "G").changed() {
                                strip.gate.on.store(g, Relaxed);
                            }
                        });
                    });
                    ui.separator();
                }

                // Busse rechts
                for b in 0..8 {
                    ui.vertical(|ui| {
                        ui.set_width(96.0);
                        ui.label(egui::RichText::new(format!("BUS {}", BUS_LABELS[b])).strong());
                        let bus = &p.buses[b];
                        let lvl = crate::dsp::lin_to_db(fget(&bus.level));
                        ui.add(
                            egui::ProgressBar::new(((lvl + 60.0) / 72.0).clamp(0.0, 1.0))
                                .show_percentage(),
                        );
                        let mut gain = fget(&bus.gain);
                        if ui
                            .add(egui::Slider::new(&mut gain, -60.0..=12.0).vertical())
                            .changed()
                        {
                            fset(&bus.gain, gain);
                        }
                        ui.horizontal(|ui| {
                            let mut m = bus.mute.load(Relaxed);
                            if ui.toggle_value(&mut m, "M").changed() {
                                bus.mute.store(m, Relaxed);
                            }
                            let mut l = bus.limiter_on.load(Relaxed);
                            if ui.toggle_value(&mut l, "LIM").changed() {
                                bus.limiter_on.store(l, Relaxed);
                            }
                        });
                    });
                }
            });
        });

        // 20 Hz Neuzeichnen für die Pegel
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}
