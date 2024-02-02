use nix::{errno::Errno, unistd::ForkResult};
use std::{
    ffi::CStr,
    os::fd::{AsRawFd, OwnedFd},
};

use ansi::{AnsiParser, TerminalOutput};

mod ansi;

/// Spawn a shell in a child process and return the file descriptor used for I/O
fn spawn_shell() -> OwnedFd {
    unsafe {
        let res = nix::pty::forkpty(None, None).unwrap();
        match res.fork_result {
            ForkResult::Parent { .. } => (),
            ForkResult::Child => {
                let shell_name = CStr::from_bytes_with_nul(b"ash\0")
                    .expect("Should always have null terminator");
                let args: &[&[u8]] = &[b"ash\0", b"--noprofile\0", b"--norc\0"];

                let args: Vec<&'static CStr> = args
                    .iter()
                    .map(|v| {
                        CStr::from_bytes_with_nul(v).expect("Should always have null terminator")
                    })
                    .collect::<Vec<_>>();

                // Temporary workaround to avoid rendering issues
                std::env::remove_var("PROMPT_COMMAND");
                std::env::set_var("PS1", "$ ");
                nix::unistd::execvp(shell_name, &args).unwrap();
                // Should never run
                std::process::exit(1);
            }
        }
        res.master
    }
}

fn update_cursor(incoming: &[u8], cursor: &mut CursorPos) {
    for c in incoming {
        match c {
            b'\n' => {
                cursor.x = 0;
                cursor.y += 1;
            }
            _ => {
                cursor.x += 1;
            }
        }
    }
}

fn set_nonblock(fd: &OwnedFd) {
    let flags = nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL).unwrap();
    let mut flags =
        nix::fcntl::OFlag::from_bits(flags & nix::fcntl::OFlag::O_ACCMODE.bits()).unwrap();
    flags.set(nix::fcntl::OFlag::O_NONBLOCK, true);

    nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(flags)).unwrap();
}

fn cursor_to_buffer_position(cursor_pos: &CursorPos, buf: &[u8]) -> usize {
    let line_start = buf
        .split(|b| *b == b'\n')
        .take(cursor_pos.y)
        .fold(0, |acc, item| acc + item.len() + 1);
    line_start + cursor_pos.x
}

#[derive(Clone)]
pub struct CursorPos {
    pub x: usize,
    pub y: usize,
}

pub struct TerminalEmulator {
    output_buf: AnsiParser,
    buf: Vec<u8>,
    cursor_pos: CursorPos,
    fd: OwnedFd,
}

impl TerminalEmulator {
    pub fn new() -> TerminalEmulator {
        let fd = spawn_shell();
        set_nonblock(&fd);

        TerminalEmulator {
            output_buf: AnsiParser::new(),
            buf: Vec::new(),
            cursor_pos: CursorPos { x: 0, y: 0 },
            fd,
        }
    }

    pub fn write(&mut self, mut to_write: &[u8]) {
        while !to_write.is_empty() {
            let written = nix::unistd::write(self.fd.as_raw_fd(), to_write).unwrap();
            to_write = &to_write[written..];
        }
    }

    pub fn read(&mut self) {
        let mut buf = vec![0u8; 4096];
        let mut ret = Ok(0);
        while ret.is_ok() {
            ret = nix::unistd::read(self.fd.as_raw_fd(), &mut buf);
            let Ok(read_size) = ret else {
                break;
            };

            let incoming = &buf[0..read_size];
            let parsed = self.output_buf.push(incoming);
            for segment in parsed {
                match segment {
                    TerminalOutput::Data(data) => {
                        update_cursor(&data, &mut self.cursor_pos);
                        self.buf.extend_from_slice(&data);
                    }
                    TerminalOutput::SetCursorPos { x, y } => {
                        if let Some(x) = x {
                            self.cursor_pos.x = x - 1;
                        }
                        if let Some(y) = y {
                            self.cursor_pos.y = y - 1;
                        }
                    }
                    TerminalOutput::ClearForwards => {
                        let buf_pos = cursor_to_buffer_position(&self.cursor_pos, &self.buf);
                        self.buf = self.buf[..buf_pos].to_vec();
                    }
                    TerminalOutput::ClearBackwards => {
                        // FIXME: Write a test to check expected behavior here, might expect
                        // existing content to stay in the same position
                        let buf_pos = cursor_to_buffer_position(&self.cursor_pos, &self.buf);
                        self.buf = self.buf[buf_pos..].to_vec();
                    }
                    TerminalOutput::ClearAll => {
                        self.buf.clear();
                    }
                    TerminalOutput::Invalid => {}
                }
            }
        }

        if let Err(e) = ret {
            if e != Errno::EAGAIN {
                println!("Failed to read: {e}");
            }
        }
    }

    pub fn data(&self) -> &[u8] {
        &self.buf
    }

    pub fn cursor_pos(&self) -> CursorPos {
        self.cursor_pos.clone()
    }
}
