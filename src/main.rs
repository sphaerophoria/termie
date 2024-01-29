use terminal_emulator::TerminalEmulator;

mod gui;
mod terminal_emulator;

fn main() {
    let terminal_emulator = TerminalEmulator::new();
    gui::run(terminal_emulator);
}
