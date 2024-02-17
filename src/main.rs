use terminal_emulator::TerminalEmulator;

#[macro_use]
mod log;
mod error;
mod gui;
mod terminal_emulator;

fn main() {
    log::init();
    let terminal_emulator = match TerminalEmulator::new() {
        Ok(v) => v,
        Err(e) => {
            error!(
                "Failed to create terminal emulator: {}",
                error::backtraced_err(&e)
            );
            return;
        }
    };
    if let Err(e) = gui::run(terminal_emulator) {
        error!("Failed to run gui: {}", error::backtraced_err(&e));
    }
}
