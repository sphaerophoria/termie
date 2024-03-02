use crate::{
    error::backtraced_err,
    terminal_emulator::{
        ControlAction, LoadRecordingError, LoadSnapshotError, PtyIo, Recording, RecordingAction,
        RecordingHandle, ReplayControl, ReplayIo, TerminalEmulator,
    },
};
use eframe::{
    egui::{self, CentralPanel, Response, Ui},
    epaint::Color32,
};
use terminal::TerminalWidget;
use thiserror::Error;

use std::path::{Path, PathBuf};

mod terminal;

fn set_egui_options(ctx: &egui::Context) {
    ctx.options_mut(|options| {
        options.zoom_with_keyboard = false;
    });
}

fn calc_row_offset_px(current: usize, desired: usize, row_height: f32) -> f32 {
    let offset = desired as i64 - current as i64;
    offset as f32 * row_height
}

fn render_actions(ui: &mut Ui, replay_control: &mut ReplayControl, position_changed: bool) {
    let text_style = egui::TextStyle::Body;
    let text_height = ui.text_style_height(&text_style);
    let spacing = ui.spacing().item_spacing;
    let row_height = text_height + spacing.y;
    let replay_pos = replay_control.current_pos();
    egui::ScrollArea::vertical().show_rows(
        ui,
        text_height,
        replay_control.len(),
        |ui, row_range| {
            if position_changed {
                let offset_px = calc_row_offset_px(row_range.start, replay_pos, row_height);
                let mut tl = ui.cursor().left_top();
                tl.y += offset_px;
                let br = egui::pos2(tl.x, tl.y + row_height);
                let rect = egui::Rect::from_min_max(tl, br);
                ui.scroll_to_rect(rect, Some(egui::Align::Center));
            }
            for (i, item) in replay_control
                .iter()
                .enumerate()
                .skip(row_range.start)
                .take(row_range.len())
            {
                ui.set_width(ui.available_width());
                let text = match item {
                    RecordingAction::Write(b) => {
                        if (0x21..=0x7e).contains(&b) {
                            format!("{}", b as char)
                        } else {
                            format!("0x{:02x}", b)
                        }
                    }
                    RecordingAction::SetWinSize { width, height } => {
                        format!("resize {width}x{height}")
                    }
                    RecordingAction::None => {
                        panic!("recording action never be none");
                    }
                };

                let text = egui::RichText::new(text);
                let text = if i == replay_pos {
                    text.color(Color32::GREEN)
                } else {
                    text
                };

                ui.label(text);
            }
        },
    );
}

struct LoadReplayResponse {
    terminal_emulator: TerminalEmulator<ReplayIo>,
    replay_control: ReplayControl,
}

#[derive(Debug, Error)]
enum LoadReplayError {
    #[error("failed to load recording")]
    Recording(LoadRecordingError),
    #[error("failed to construct terminal emulator")]
    CreateTerminalEmulator(LoadSnapshotError),
}

fn load_replay(path: &Path) -> Result<LoadReplayResponse, LoadReplayError> {
    let recording = Recording::load(path).map_err(LoadReplayError::Recording)?;
    let mut replay_control = ReplayControl::new(recording);
    let io_handle = replay_control.io_handle();
    let snapshot = replay_control.initial_state();
    let terminal_emulator = TerminalEmulator::from_snapshot(snapshot, io_handle)
        .map_err(LoadReplayError::CreateTerminalEmulator)?;
    Ok(LoadReplayResponse {
        terminal_emulator,
        replay_control,
    })
}

struct ReplayTermieGui {
    terminal_emulator: TerminalEmulator<ReplayIo>,
    terminal_widget: TerminalWidget,
    replay_path: PathBuf,
    replay_control: ReplayControl,
    slider_pos: usize,
}

impl ReplayTermieGui {
    fn new(
        cc: &eframe::CreationContext<'_>,
        replay_path: PathBuf,
        terminal_emulator: TerminalEmulator<ReplayIo>,
        replay_control: ReplayControl,
    ) -> Self {
        set_egui_options(&cc.egui_ctx);

        ReplayTermieGui {
            terminal_emulator,
            terminal_widget: TerminalWidget::new(&cc.egui_ctx),
            replay_path,
            replay_control,
            slider_pos: 0,
        }
    }

    fn step_replay(&mut self) {
        let action = self.replay_control.next();
        match action {
            ControlAction::Resize { width, height } => {
                if let Err(e) = self.terminal_emulator.set_win_size(width, height) {
                    error!("failed to set window size: {}", backtraced_err(&*e));
                }
            }
            ControlAction::None => (),
        }
    }

    fn reload_replay(&mut self) {
        match load_replay(&self.replay_path) {
            Ok(response) => {
                self.terminal_emulator = response.terminal_emulator;
                self.replay_control = response.replay_control;
            }
            Err(e) => {
                error!("failed to reload replay: {}", backtraced_err(&e));
            }
        }
    }

    fn update_replay_pos(&mut self, slider_response: &Response, next_response: &Response) -> bool {
        if !next_response.clicked() && !slider_response.changed() {
            return false;
        }

        // Slider has requested that we move backwards, this requires a reload as we can only move
        // forwards
        if self.replay_control.current_pos() > self.slider_pos {
            self.reload_replay();
        }

        // Now we can move to where the slider wants us to be
        let current_pos = self.replay_control.current_pos();
        for _ in current_pos..self.slider_pos {
            self.step_replay();
        }

        if next_response.clicked() {
            self.step_replay();
        }

        // At this point we know that we've satisfied the slider's request, and the button's
        // request, so the replay control is in the right state. The slider position however may
        // not be right since there are other sources of movement
        self.slider_pos = self.replay_control.current_pos();
        true
    }
}

impl eframe::App for ReplayTermieGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let next_response = egui::TopBottomPanel::top("header").show(ctx, |ui| ui.button("next"));

        let slider_response = egui::TopBottomPanel::bottom("seek").show(ctx, |ui| {
            // A little bit of an odd API, but this is how we set the slider width, should reset at
            // end of closure so no reason to reset
            ui.style_mut().spacing.slider_width = ui.available_width();
            let slider = egui::Slider::new(&mut self.slider_pos, 0..=self.replay_control.len() - 1)
                .show_value(false)
                .clamp_to_range(true);
            ui.add(slider)
        });

        let position_changed = self.update_replay_pos(&slider_response.inner, &next_response.inner);

        egui::SidePanel::left("actions").show(ctx, |ui| {
            render_actions(ui, &mut self.replay_control, position_changed);
        });
        let panel_response = CentralPanel::default().show(ctx, |ui| {
            self.terminal_widget.show(ui, &mut self.terminal_emulator);
        });

        panel_response.response.context_menu(|ui| {
            self.terminal_widget.show_options(ui);
        });
    }
}

struct TermieGui {
    terminal_emulator: TerminalEmulator<PtyIo>,
    terminal_widget: TerminalWidget,
    recording_handle: Option<RecordingHandle>,
}

impl TermieGui {
    fn new(cc: &eframe::CreationContext<'_>, terminal_emulator: TerminalEmulator<PtyIo>) -> Self {
        set_egui_options(&cc.egui_ctx);

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
            let (width_chars, height_chars) = self.terminal_widget.calculate_available_size(ui);

            if let Err(e) = self
                .terminal_emulator
                .set_win_size(width_chars, height_chars)
            {
                error!("failed to set window size {}", backtraced_err(&*e));
            }

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

pub fn run_replay(replay_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let native_options = eframe::NativeOptions::default();

    let LoadReplayResponse {
        terminal_emulator,
        replay_control,
    } = load_replay(&replay_path)?;

    eframe::run_native(
        "Termie",
        native_options,
        Box::new(move |cc| {
            Box::new(ReplayTermieGui::new(
                cc,
                replay_path,
                terminal_emulator,
                replay_control,
            ))
        }),
    )?;

    Ok(())
}

pub fn run(terminal_emulator: TerminalEmulator<PtyIo>) -> Result<(), Box<dyn std::error::Error>> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Termie",
        native_options,
        Box::new(move |cc| Box::new(TermieGui::new(cc, terminal_emulator))),
    )?;
    Ok(())
}
