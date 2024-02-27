use crate::{
    error::backtraced_err,
    terminal_emulator::{PtyIo, RecordingHandle, TerminalEmulator},
};
use eframe::egui::{self, CentralPanel, TextStyle};

use terminal::TerminalWidget;

mod terminal;

struct TermieGui {
    terminal_emulator: TerminalEmulator<PtyIo>,
    terminal_widget: TerminalWidget,
    recording_handle: Option<RecordingHandle>,
}

impl TermieGui {
    fn new(cc: &eframe::CreationContext<'_>, terminal_emulator: TerminalEmulator<PtyIo>) -> Self {
        cc.egui_ctx.style_mut(|style| {
            style.override_text_style = Some(TextStyle::Monospace);
        });

        cc.egui_ctx.options_mut(|options| {
            options.zoom_with_keyboard = false;
        });

        TermieGui {
            terminal_emulator,
            terminal_widget: TerminalWidget::new(&cc.egui_ctx),
            recording_handle: None,
        }
    }
}

impl eframe::App for TermieGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let panel_response = CentralPanel::default().show(ctx, |ui| {
            self.terminal_widget.show(ui, &mut self.terminal_emulator);
        });

        panel_response.response.context_menu(|ui| {
            self.terminal_widget.show_options(ui);

            if self.recording_handle.is_some() {
                if ui.button("Stop recording").clicked() {
                    self.recording_handle = None;
                }
            } else if ui.button("Start recording").clicked() {
                match self.terminal_emulator.start_recording() {
                    Ok(v) => {
                        self.recording_handle = Some(v);
                    }
                    Err(e) => {
                        error!("failed to start recording: {}", backtraced_err(&e));
                    }
                }
            }
        });
    }
}

pub fn run(terminal_emulator: TerminalEmulator<PtyIo>) -> Result<(), eframe::Error> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Termie",
        native_options,
        Box::new(move |cc| Box::new(TermieGui::new(cc, terminal_emulator))),
    )
}
