use crate::terminal_emulator::{CursorState, TerminalColor, TerminalEmulator};
use eframe::egui::{
    self, CentralPanel, Color32, Event, FontData, FontDefinitions, FontFamily, InputState, Key,
    Rect, TextStyle, Ui,
};

const REGULAR_FONT_NAME: &str = "hack";
const BOLD_FONT_NAME: &str = "hack-bold";

fn write_input_to_terminal(input: &InputState, terminal_emulator: &mut TerminalEmulator) {
    for event in &input.events {
        let text = match event {
            Event::Text(text) => text,
            Event::Key {
                key: Key::Enter,
                pressed: true,
                ..
            } => "\n",
            _ => "",
        };

        terminal_emulator.write(text.as_bytes());
    }
}

fn get_char_size(ctx: &egui::Context) -> (f32, f32) {
    let font_id = ctx.style().text_styles[&egui::TextStyle::Monospace].clone();
    ctx.fonts(move |fonts| {
        // NOTE: Glyph width seems to be a little too wide
        let width = fonts
            .layout(
                "@".to_string(),
                font_id.clone(),
                Color32::WHITE,
                f32::INFINITY,
            )
            .mesh_bounds
            .width();

        let height = fonts.row_height(&font_id);

        (width, height)
    })
}

fn character_to_cursor_offset(
    character_pos: &CursorState,
    character_size: &(f32, f32),
    content: &[u8],
) -> (f32, f32) {
    let content_by_lines: Vec<&[u8]> = content.split(|b| *b == b'\n').collect();
    let num_lines = content_by_lines.len();
    let x_offset = character_pos.x as f32 * character_size.0;
    let y_offset = (character_pos.y as i64 - num_lines as i64) as f32 * character_size.1;
    (x_offset, y_offset)
}

fn paint_cursor(
    label_rect: Rect,
    character_size: &(f32, f32),
    cursor_pos: &CursorState,
    terminal_buf: &[u8],
    ui: &mut Ui,
) {
    let painter = ui.painter();

    let bottom = label_rect.bottom();
    let left = label_rect.left();
    let cursor_offset = character_to_cursor_offset(cursor_pos, character_size, terminal_buf);
    painter.rect_filled(
        Rect::from_min_size(
            egui::pos2(left + cursor_offset.0, bottom + cursor_offset.1),
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
        .unwrap()
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

struct TermieGui {
    terminal_emulator: TerminalEmulator,
    character_size: Option<(f32, f32)>,
}

impl TermieGui {
    fn new(cc: &eframe::CreationContext<'_>, terminal_emulator: TerminalEmulator) -> Self {
        cc.egui_ctx.style_mut(|style| {
            style.override_text_style = Some(TextStyle::Monospace);
        });

        cc.egui_ctx.set_pixels_per_point(2.0);
        setup_fonts(&cc.egui_ctx);

        TermieGui {
            terminal_emulator,
            character_size: None,
        }
    }
}

impl eframe::App for TermieGui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.character_size.is_none() {
            self.character_size = Some(get_char_size(ctx));
        }

        self.terminal_emulator.read();

        CentralPanel::default().show(ctx, |ui| {
            ui.input(|input_state| {
                write_input_to_terminal(input_state, &mut self.terminal_emulator);
            });

            let response = unsafe {
                let style = &ctx.style().text_styles[&TextStyle::Monospace];
                let mut job = egui::text::LayoutJob::simple(
                    std::str::from_utf8_unchecked(self.terminal_emulator.data()).to_string(),
                    style.clone(),
                    ctx.style().visuals.text_color(),
                    ui.available_width(),
                );

                let mut textformat = job.sections[0].format.clone();
                job.sections.clear();
                let default_color = textformat.color;
                let bold_font_family = FontFamily::Name(BOLD_FONT_NAME.to_string().into());
                let regular_font_family = FontFamily::Name(REGULAR_FONT_NAME.to_string().into());

                for tag in self.terminal_emulator.format_data() {
                    let mut range = tag.start..tag.end;
                    let color = tag.color;

                    if range.end == usize::MAX {
                        range.end = self.terminal_emulator.data().len()
                    }

                    if tag.bold {
                        textformat.font_id.family = bold_font_family.clone();
                    } else {
                        textformat.font_id.family = regular_font_family.clone();
                    }

                    textformat.color = match color {
                        TerminalColor::Default => default_color,
                        TerminalColor::Black => Color32::BLACK,
                        TerminalColor::Red => Color32::RED,
                        TerminalColor::Green => Color32::GREEN,
                        TerminalColor::Yellow => Color32::YELLOW,
                        TerminalColor::Blue => Color32::BLUE,
                        TerminalColor::Magenta => Color32::from_rgb(255, 0, 255),
                        TerminalColor::Cyan => Color32::from_rgb(0, 255, 255),
                        TerminalColor::White => Color32::WHITE,
                    };

                    job.sections.push(egui::text::LayoutSection {
                        leading_space: 0.0f32,
                        byte_range: range,
                        format: textformat.clone(),
                    });
                }

                // FIXME: Brakes something for sure
                ui.label(job)
            };

            paint_cursor(
                response.rect,
                self.character_size.as_ref().unwrap(),
                &self.terminal_emulator.cursor_pos(),
                self.terminal_emulator.data(),
                ui,
            );
        });
    }
}

pub fn run(terminal_emulator: TerminalEmulator) {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Termie",
        native_options,
        Box::new(move |cc| Box::new(TermieGui::new(cc, terminal_emulator))),
    )
    .unwrap();
}
