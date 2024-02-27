use crate::terminal_emulator::{
    PtyIo, TerminalEmulator,
};
use eframe::egui::{
    self, CentralPanel, TextStyle,
};

use terminal::TerminalWidget;

mod terminal;

struct TermieGui {
    terminal_emulator: TerminalEmulator<PtyIo>,
    terminal_widget: TerminalWidget,
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
