use terminal_emulator::TerminalEmulator;

#[macro_use]
mod log;
mod gui;
mod terminal_emulator;

fn main() {
    log::init();
    let terminal_emulator = TerminalEmulator::new();
    gui::run(terminal_emulator);
}
