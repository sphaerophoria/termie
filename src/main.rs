use std::path::PathBuf;
use terminal_emulator::TerminalEmulator;

#[macro_use]
mod log;
mod error;
mod gui;
mod terminal_emulator;

struct Args {
    recording_path: PathBuf,
}

impl Args {
    fn parse<It: Iterator<Item = String>>(mut it: It) -> Args {
        let program_name = it.next();

        // Default value
        let mut recording_path = "recordings".into();

        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--recording-path" => {
                    recording_path = match it.next() {
                        Some(p) => p.into(),
                        None => {
                            println!("Missing argument for --recording-path");
                            Self::help(program_name.as_deref());
                        }
                    };
                }
                _ => {
                    println!("Invalid argument {arg}");
                    Self::help(program_name.as_deref())
                }
            }
        }

        Args { recording_path }
    }

    fn help(program_name: Option<&str>) -> ! {
        let program_name = program_name.unwrap_or("termie");
        println!(
            "\
                 Usage:\n\
                 {program_name} [ARGS]\n\
                 \n\
                 Args:\n\
                 --recording-path: Optional, where to output recordings to
                 "
        );
        std::process::exit(1);
    }
}

fn main() {
    log::init();
    let args = Args::parse(std::env::args());
    let res = match TerminalEmulator::new(args.recording_path) {
        Ok(v) => gui::run(v),
        Err(e) => {
            error!(
                "Failed to create terminal emulator: {}",
                error::backtraced_err(&e)
            );
            return;
        }
    };

    if let Err(e) = res {
        error!("Failed to run gui: {}", error::backtraced_err(&e));
    }
}
