use crate::terminal_emulator::{
    CursorPos, FormatTag, TerminalColor, TerminalEmulator, TerminalInput,
};
use eframe::egui::{
    self, text::LayoutJob, CentralPanel, Color32, DragValue, Event, FontData, FontDefinitions,
    FontFamily, FontId, InputState, Key, Modifiers, Rect, TextFormat, TextStyle, Ui,
};

use std::borrow::Cow;

use crate::error::backtraced_err;

const REGULAR_FONT_NAME: &str = "hack";
const BOLD_FONT_NAME: &str = "hack-bold";

fn write_input_to_terminal(input: &InputState, terminal_emulator: &mut TerminalEmulator) {
    for event in &input.raw.events {
        let inputs: Cow<'static, [TerminalInput]> = match event {
            Event::Text(text) => text
                .as_bytes()
                .iter()
                .map(|c| TerminalInput::Ascii(*c))
                .collect::<Vec<_>>()
                .into(),
            Event::Key {
                key: Key::Enter,
                pressed: true,
                ..
            } => [TerminalInput::Enter].as_ref().into(),
            // https://github.com/emilk/egui/issues/3653
            Event::Copy => {
                // NOTE: Technically not correct if we were on a mac, but also we are using linux
                // syscalls so we'd have to solve that before this is a problem
                [TerminalInput::Ctrl(b'c')].as_ref().into()
            }
            Event::Key {
                key,
                pressed: true,
                modifiers: Modifiers { ctrl: true, .. },
                ..
            } => {
                if *key >= Key::A && *key <= Key::Z {
                    let name = key.name();
                    assert!(name.len() == 1);
                    let name_c = name.as_bytes()[0];
                    vec![TerminalInput::Ctrl(name_c)].into()
                } else if *key == Key::OpenBracket {
                    [TerminalInput::Ctrl(b'[')].as_ref().into()
                } else if *key == Key::CloseBracket {
                    [TerminalInput::Ctrl(b']')].as_ref().into()
                } else if *key == Key::Backslash {
                    [TerminalInput::Ctrl(b'\\')].as_ref().into()
                } else {
                    info!("Unexpected ctrl key: {}", key.name());
                    continue;
                }
            }
            Event::Key {
                key: Key::Backspace,
                pressed: true,
                ..
            } => [TerminalInput::Backspace].as_ref().into(),
            Event::Key {
                key: Key::ArrowUp,
                pressed: true,
                ..
            } => [TerminalInput::ArrowUp].as_ref().into(),
            Event::Key {
                key: Key::ArrowDown,
                pressed: true,
                ..
            } => [TerminalInput::ArrowDown].as_ref().into(),
            Event::Key {
                key: Key::ArrowLeft,
                pressed: true,
                ..
            } => [TerminalInput::ArrowLeft].as_ref().into(),
            Event::Key {
                key: Key::ArrowRight,
                pressed: true,
                ..
            } => [TerminalInput::ArrowRight].as_ref().into(),
            Event::Key {
                key: Key::Home,
                pressed: true,
                ..
            } => [TerminalInput::Home].as_ref().into(),
            Event::Key {
                key: Key::End,
                pressed: true,
                ..
            } => [TerminalInput::End].as_ref().into(),
            _ => {
                continue;
            }
        };

        for input in inputs.as_ref() {
            if let Err(e) = terminal_emulator.write(input.clone()) {
                error!(
                    "Failed to write input to terminal emulator: {}",
                    backtraced_err(&e)
                );
            }
        }
    }
}

fn get_char_size(ctx: &egui::Context, font_size: f32) -> (f32, f32) {
    let font_id = FontId {
        size: font_size,
        family: FontFamily::Name(REGULAR_FONT_NAME.into()),
    };

    // NOTE: Using glyph width and row height do not give accurate results. Even using the mesh
    // bounds of a single character is not reasonable. Instead we layout 16 rows and 16 cols and
    // divide by 16. This seems to work better at all font scales
    ctx.fonts(move |fonts| {
        let rect = fonts
            .layout(
                "asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf\n\
                 asdfasdfasdfasdf"
                    .to_string(),
                font_id.clone(),
                Color32::WHITE,
                f32::INFINITY,
            )
            .rect;

        let width = rect.width() / 16.0;
        let height = rect.height() / 16.0;

        (width, height)
    })
}

fn paint_cursor(
    label_rect: Rect,
    character_size: &(f32, f32),
    cursor_pos: &CursorPos,
    ui: &mut Ui,
) {
    let painter = ui.painter();

    let top = label_rect.top();
    let left = label_rect.left();
    let y_offset = cursor_pos.y as f32 * character_size.1;
    let x_offset = cursor_pos.x as f32 * character_size.0;
    painter.rect_filled(
        Rect::from_min_size(
            egui::pos2(left + x_offset, top + y_offset),
            egui::vec2(character_size.0, character_size.1),
        ),
        0.0,
        Color32::GRAY,
    );
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        REGULAR_FONT_NAME.to_owned(),
        FontData::from_static(include_bytes!("../res/Hack-Regular.ttf")),
    );

    fonts.font_data.insert(
        BOLD_FONT_NAME.to_owned(),
        FontData::from_static(include_bytes!("../res/Hack-Bold.ttf")),
    );

    fonts
        .families
        .get_mut(&FontFamily::Monospace)
        .expect("egui should provide a monospace font")
        .insert(0, REGULAR_FONT_NAME.to_owned());

    fonts.families.insert(
        FontFamily::Name(REGULAR_FONT_NAME.to_string().into()),
        vec![REGULAR_FONT_NAME.to_string()],
    );
    fonts.families.insert(
        FontFamily::Name(BOLD_FONT_NAME.to_string().into()),
        vec![BOLD_FONT_NAME.to_string()],
    );

    ctx.set_fonts(fonts);
}

struct TerminalFonts {
    regular: FontFamily,
    bold: FontFamily,
}

impl TerminalFonts {
    fn new() -> TerminalFonts {
        let bold = FontFamily::Name(BOLD_FONT_NAME.to_string().into());
        let regular = FontFamily::Name(REGULAR_FONT_NAME.to_string().into());

        TerminalFonts { regular, bold }
    }

    fn get_family(&self, is_bold: bool) -> FontFamily {
        if is_bold {
            self.bold.clone()
        } else {
            self.regular.clone()
        }
    }
}

fn terminal_color_to_egui(default_color: &Color32, color: &TerminalColor) -> Color32 {
    match color {
        TerminalColor::Default => *default_color,
        TerminalColor::Black => Color32::BLACK,
        TerminalColor::Red => Color32::RED,
        TerminalColor::Green => Color32::GREEN,
        TerminalColor::Yellow => Color32::YELLOW,
        TerminalColor::Blue => Color32::BLUE,
        TerminalColor::Magenta => Color32::from_rgb(255, 0, 255),
        TerminalColor::Cyan => Color32::from_rgb(0, 255, 255),
        TerminalColor::White => Color32::WHITE,
    }
}

fn create_terminal_output_layout_job(
    style: &egui::Style,
    width: f32,
    data: &[u8],
) -> Result<(LayoutJob, TextFormat), std::str::Utf8Error> {
    let text_style = &style.text_styles[&TextStyle::Monospace];
    let data_utf8 = std::str::from_utf8(data)?;
    let mut job = egui::text::LayoutJob::simple(
        data_utf8.to_string(),
        text_style.clone(),
        style.visuals.text_color(),
        width,
    );

    job.wrap.break_anywhere = true;
    let textformat = job.sections[0].format.clone();
    job.sections.clear();
    Ok((job, textformat))
}

fn add_terminal_data_to_ui(
    ui: &mut Ui,
    data: &[u8],
    format_data: &[FormatTag],
    font_size: f32,
) -> Result<egui::Response, std::str::Utf8Error> {
    let (mut job, mut textformat) =
        create_terminal_output_layout_job(ui.style(), ui.available_width(), data)?;

    let default_color = textformat.color;
    let terminal_fonts = TerminalFonts::new();

    for tag in format_data {
        let mut range = tag.start..tag.end;
        let color = tag.color;

        if range.end == usize::MAX {
            range.end = data.len()
        }

        textformat.font_id.family = terminal_fonts.get_family(tag.bold);
        textformat.font_id.size = font_size;
        textformat.color = terminal_color_to_egui(&default_color, &color);

        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0f32,
            byte_range: range,
            format: textformat.clone(),
        });
    }

    Ok(ui.label(job))
}

struct TerminalOutputRenderResponse {
    scrollback_area: Rect,
    canvas_area: Rect,
}

fn render_terminal_output(
    ui: &mut egui::Ui,
    terminal_emulator: &TerminalEmulator,
    font_size: f32,
) -> TerminalOutputRenderResponse {
    let terminal_data = terminal_emulator.data();
    let mut scrollback_data = terminal_data.scrollback;
    let mut canvas_data = terminal_data.visible;
    let mut format_data = terminal_emulator.format_data();

    // Arguably incorrect. Scrollback does end with a newline, and that newline causes a blank
    // space between widgets. Should we strip it here, or in the terminal emulator output?
    if scrollback_data.ends_with(b"\n") {
        scrollback_data = &scrollback_data[0..scrollback_data.len() - 1];
        if let Some(last_tag) = format_data.scrollback.last_mut() {
            last_tag.end = last_tag.end.min(scrollback_data.len());
        }
    }

    if canvas_data.ends_with(b"\n") {
        canvas_data = &canvas_data[0..canvas_data.len() - 1];
    }

    let response = egui::ScrollArea::new([false, true])
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            let error_logged_rect =
                |response: Result<egui::Response, std::str::Utf8Error>| match response {
                    Ok(v) => v.rect,
                    Err(e) => {
                        error!("failed to add terminal data to ui: {}", backtraced_err(&e));
                        Rect::NOTHING
                    }
                };
            let scrollback_area = error_logged_rect(add_terminal_data_to_ui(
                ui,
                scrollback_data,
                &format_data.scrollback,
                font_size,
            ));
            let canvas_area = error_logged_rect(add_terminal_data_to_ui(
                ui,
                canvas_data,
                &format_data.visible,
                font_size,
            ));
            TerminalOutputRenderResponse {
                scrollback_area,
                canvas_area,
            }
        });

    response.inner
}

struct DebugRenderer {
    enable: bool,
}

impl DebugRenderer {
    fn new() -> DebugRenderer {
        DebugRenderer { enable: false }
    }

    fn render(&self, ui: &mut Ui, rect: Rect, color: Color32) {
        if !self.enable {
            return;
        }

        let color = color.gamma_multiply(0.25);
        ui.painter().rect_filled(rect, 0.0, color);
    }
}

struct TermieGui {
    terminal_emulator: TerminalEmulator,
    font_size: f32,
    debug_renderer: DebugRenderer,
}

impl TermieGui {
    fn new(cc: &eframe::CreationContext<'_>, terminal_emulator: TerminalEmulator) -> Self {
        cc.egui_ctx.style_mut(|style| {
            style.override_text_style = Some(TextStyle::Monospace);
        });

        cc.egui_ctx.options_mut(|options| {
            options.zoom_with_keyboard = false;
        });

        setup_fonts(&cc.egui_ctx);

        TermieGui {
            terminal_emulator,
            font_size: 12.0,
            debug_renderer: DebugRenderer::new(),
        }
    }
}

impl eframe::App for TermieGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let character_size = get_char_size(ctx, self.font_size);

        self.terminal_emulator.read();

        let panel_response = CentralPanel::default().show(ctx, |ui| {
            let frame_response = egui::Frame::none().show(ui, |ui| {
                let width_chars = (ui.available_width() / character_size.0).floor();
                let height_chars = (ui.available_height() / character_size.1).floor();

                if let Err(e) = self
                    .terminal_emulator
                    .set_win_size(width_chars as usize, height_chars as usize)
                {
                    error!("Failed to update window size: {}", backtraced_err(&e));
                }

                ui.set_width((width_chars + 0.5) * character_size.0);
                ui.set_height((height_chars + 0.5) * character_size.1);

                ui.input(|input_state| {
                    write_input_to_terminal(input_state, &mut self.terminal_emulator);
                });

                let output_response =
                    render_terminal_output(ui, &self.terminal_emulator, self.font_size);
                self.debug_renderer
                    .render(ui, output_response.canvas_area, Color32::BLUE);

                self.debug_renderer
                    .render(ui, output_response.scrollback_area, Color32::YELLOW);

                paint_cursor(
                    output_response.canvas_area,
                    &character_size,
                    &self.terminal_emulator.cursor_pos(),
                    ui,
                );
            });
            self.debug_renderer
                .render(ui, frame_response.response.rect, Color32::RED);
        });

        panel_response.response.context_menu(|ui| {
            ui.horizontal(|ui| {
                ui.label("Font size:");
                ui.add(DragValue::new(&mut self.font_size).clamp_range(1.0..=100.0));
            });
            ui.checkbox(&mut self.debug_renderer.enable, "Debug render");
        });
    }
}

pub fn run(terminal_emulator: TerminalEmulator) -> Result<(), eframe::Error> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Termie",
        native_options,
        Box::new(move |cc| Box::new(TermieGui::new(cc, terminal_emulator))),
    )
}
