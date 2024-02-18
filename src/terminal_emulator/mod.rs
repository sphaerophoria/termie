use nix::{errno::Errno, ioctl_write_ptr_bad, unistd::ForkResult};
use tempfile::TempDir;
use thiserror::Error;

use std::{
    ffi::CStr,
    fmt,
    os::fd::{AsRawFd, OwnedFd},
    path::Path,
};

use ansi::{AnsiParser, SelectGraphicRendition, TerminalOutput};
use buffer::TerminalBuffer;
pub use format_tracker::FormatTag;
use format_tracker::FormatTracker;

use crate::error::backtraced_err;

mod ansi;
mod buffer;
mod format_tracker;
const TERMINFO: &[u8] = include_bytes!(std::concat!(std::env!("OUT_DIR"), "/terminfo.tar"));

#[derive(Eq, PartialEq)]
enum Mode {
    // Cursor keys mode
    // https://vt100.net/docs/vt100-ug/chapter3.html
    Decckm,
    Unknown(Vec<u8>),
}

impl fmt::Debug for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Decckm => f.write_str("Decckm"),
            Mode::Unknown(params) => {
                let params_s = std::str::from_utf8(params)
                    .expect("parameter parsing should not allow non-utf8 characters here");
                f.write_fmt(format_args!("Unknown({})", params_s))
            }
        }
    }
}

fn char_to_ctrl_code(c: u8) -> u8 {
    // https://catern.com/posts/terminal_quirks.html
    // man ascii
    c & 0b0001_1111
}

#[derive(Eq, PartialEq, Debug)]
enum TerminalInputPayload {
    Single(u8),
    Many(&'static [u8]),
}

#[derive(Clone)]
pub enum TerminalInput {
    // Normal keypress
    Ascii(u8),
    // Normal keypress with ctrl
    Ctrl(u8),
    Enter,
    Backspace,
    ArrowRight,
    ArrowLeft,
    ArrowUp,
    ArrowDown,
    Home,
    End,
}

impl TerminalInput {
    fn to_payload(&self, decckm_mode: bool) -> TerminalInputPayload {
        match self {
            TerminalInput::Ascii(c) => TerminalInputPayload::Single(*c),
            TerminalInput::Ctrl(c) => TerminalInputPayload::Single(char_to_ctrl_code(*c)),
            TerminalInput::Enter => TerminalInputPayload::Single(b'\n'),
            // Hard to tie back, but check default VERASE in terminfo definition
            TerminalInput::Backspace => TerminalInputPayload::Single(0x7f),
            // https://vt100.net/docs/vt100-ug/chapter3.html
            // Table 3-6
            TerminalInput::ArrowRight => match decckm_mode {
                true => TerminalInputPayload::Many(b"\x1bOC"),
                false => TerminalInputPayload::Many(b"\x1b[C"),
            },
            TerminalInput::ArrowLeft => match decckm_mode {
                true => TerminalInputPayload::Many(b"\x1bOD"),
                false => TerminalInputPayload::Many(b"\x1b[D"),
            },
            TerminalInput::ArrowUp => match decckm_mode {
                true => TerminalInputPayload::Many(b"\x1bOA"),
                false => TerminalInputPayload::Many(b"\x1b[A"),
            },
            TerminalInput::ArrowDown => match decckm_mode {
                true => TerminalInputPayload::Many(b"\x1bOB"),
                false => TerminalInputPayload::Many(b"\x1b[B"),
            },
            TerminalInput::Home => match decckm_mode {
                true => TerminalInputPayload::Many(b"\x1bOH"),
                false => TerminalInputPayload::Many(b"\x1b[H"),
            },
            TerminalInput::End => match decckm_mode {
                true => TerminalInputPayload::Many(b"\x1bOF"),
                false => TerminalInputPayload::Many(b"\x1b[F"),
            },
        }
    }
}

#[derive(Error, Debug)]
enum ExtractTerminfoError {
    #[error("failed to extract")]
    Extraction(#[source] std::io::Error),
    #[error("failed to create temp dir")]
    CreateTempDir(#[source] std::io::Error),
}

fn extract_terminfo() -> Result<TempDir, ExtractTerminfoError> {
    let mut terminfo_tarball = tar::Archive::new(TERMINFO);
    let temp_dir = TempDir::new().map_err(ExtractTerminfoError::CreateTempDir)?;
    terminfo_tarball
        .unpack(temp_dir.path())
        .map_err(ExtractTerminfoError::Extraction)?;

    Ok(temp_dir)
}

#[derive(Error, Debug)]
enum SpawnShellErrorKind {
    #[error("failed to fork")]
    Fork(#[source] Errno),
    #[error("failed to exec")]
    Exec(#[source] Errno),
}

#[derive(Error, Debug)]
#[error(transparent)]
struct SpawnShellError(#[from] SpawnShellErrorKind);

/// Spawn a shell in a child process and return the file descriptor used for I/O
fn spawn_shell(terminfo_dir: &Path) -> Result<OwnedFd, SpawnShellError> {
    unsafe {
        let res = nix::pty::forkpty(None, None).map_err(SpawnShellErrorKind::Fork)?;
        match res.fork_result {
            ForkResult::Parent { .. } => (),
            ForkResult::Child => {
                let shell_name = CStr::from_bytes_with_nul(b"bash\0")
                    .expect("Should always have null terminator");
                let args: &[&[u8]] = &[b"bash\0", b"--noprofile\0", b"--norc\0"];

                let args: Vec<&'static CStr> = args
                    .iter()
                    .map(|v| {
                        CStr::from_bytes_with_nul(v).expect("Should always have null terminator")
                    })
                    .collect::<Vec<_>>();

                // Temporary workaround to avoid rendering issues
                std::env::remove_var("PROMPT_COMMAND");
                std::env::set_var("TERMINFO", terminfo_dir);
                std::env::set_var("TERM", "termie");
                std::env::set_var("PS1", "$ ");
                nix::unistd::execvp(shell_name, &args).map_err(SpawnShellErrorKind::Exec)?;
                // Should never run
                std::process::exit(1);
            }
        }
        Ok(res.master)
    }
}

#[derive(Error, Debug)]
enum SetNonblockError {
    #[error("failed to get current fcntl args")]
    GetCurrent(#[source] Errno),
    #[error("failed to parse retrieved oflags")]
    ParseFlags,
    #[error("failed to set new fcntl args")]
    SetNew(#[source] Errno),
}

fn set_nonblock(fd: &OwnedFd) -> Result<(), SetNonblockError> {
    let flags = nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL)
        .map_err(SetNonblockError::GetCurrent)?;
    let mut flags = nix::fcntl::OFlag::from_bits(flags & nix::fcntl::OFlag::O_ACCMODE.bits())
        .ok_or(SetNonblockError::ParseFlags)?;
    flags.set(nix::fcntl::OFlag::O_NONBLOCK, true);

    nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(flags))
        .map_err(SetNonblockError::SetNew)?;
    Ok(())
}

fn split_format_data_for_scrollback(
    tags: Vec<FormatTag>,
    scrollback_split: usize,
) -> TerminalData<Vec<FormatTag>> {
    let scrollback_tags = tags
        .iter()
        .filter(|tag| tag.start < scrollback_split)
        .cloned()
        .map(|mut tag| {
            tag.end = tag.end.min(scrollback_split);
            tag
        })
        .collect();

    let canvas_tags = tags
        .into_iter()
        .filter(|tag| tag.end > scrollback_split)
        .map(|mut tag| {
            tag.start = tag.start.saturating_sub(scrollback_split);
            if tag.end != usize::MAX {
                tag.end -= scrollback_split;
            }
            tag
        })
        .collect();

    TerminalData {
        scrollback: scrollback_tags,
        visible: canvas_tags,
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CursorPos {
    pub x: usize,
    pub y: usize,
}

#[derive(Clone)]
struct CursorState {
    pos: CursorPos,
    bold: bool,
    color: TerminalColor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalColor {
    Default,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
}

impl TerminalColor {
    fn from_sgr(sgr: SelectGraphicRendition) -> Option<TerminalColor> {
        let ret = match sgr {
            SelectGraphicRendition::ForegroundBlack => TerminalColor::Black,
            SelectGraphicRendition::ForegroundRed => TerminalColor::Red,
            SelectGraphicRendition::ForegroundGreen => TerminalColor::Green,
            SelectGraphicRendition::ForegroundYellow => TerminalColor::Yellow,
            SelectGraphicRendition::ForegroundBlue => TerminalColor::Blue,
            SelectGraphicRendition::ForegroundMagenta => TerminalColor::Magenta,
            SelectGraphicRendition::ForegroundCyan => TerminalColor::Cyan,
            SelectGraphicRendition::ForegroundWhite => TerminalColor::White,
            _ => return None,
        };

        Some(ret)
    }
}

pub struct TerminalData<T> {
    pub scrollback: T,
    pub visible: T,
}

ioctl_write_ptr_bad!(
    set_window_size_ioctl,
    nix::libc::TIOCSWINSZ,
    nix::pty::Winsize
);

#[derive(Debug, Error)]
enum SetWindowSizeErrorKind {
    #[error("height too large")]
    HeightTooLarge(#[source] std::num::TryFromIntError),
    #[error("width too large")]
    WidthTooLarge(#[source] std::num::TryFromIntError),
    #[error("failed to execute ioctl")]
    IoctlFailed(#[source] Errno),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct SetWindowSizeError(#[from] SetWindowSizeErrorKind);

fn set_window_size(fd: &OwnedFd, width: usize, height: usize) -> Result<(), SetWindowSizeError> {
    let win_size = nix::pty::Winsize {
        ws_row: height
            .try_into()
            .map_err(SetWindowSizeErrorKind::HeightTooLarge)?,
        ws_col: width
            .try_into()
            .map_err(SetWindowSizeErrorKind::WidthTooLarge)?,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    unsafe {
        set_window_size_ioctl(fd.as_raw_fd(), &win_size)
            .map_err(SetWindowSizeErrorKind::IoctlFailed)?;
    }

    Ok(())
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct WriteError(#[from] Errno);

pub struct TerminalEmulator {
    parser: AnsiParser,
    terminal_buffer: TerminalBuffer,
    format_tracker: FormatTracker,
    cursor_state: CursorState,
    decckm_mode: bool,
    fd: OwnedFd,
    _terminfo_dir: TempDir,
}

pub const TERMINAL_WIDTH: usize = 50;
pub const TERMINAL_HEIGHT: usize = 16;

#[derive(Debug, Error)]
enum CreateTerminalEmulatorErrorKind {
    #[error("failed to extract terminfo")]
    ExtractTerminfo(#[from] ExtractTerminfoError),
    #[error("failed to spawn shell")]
    SpawnShell(#[from] SpawnShellError),
    #[error("failed to set fd as non-blocking")]
    SetNonblock(#[from] SetNonblockError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct CreateTerminalEmulatorError(#[from] CreateTerminalEmulatorErrorKind);

impl TerminalEmulator {
    pub fn new() -> Result<TerminalEmulator, CreateTerminalEmulatorError> {
        let terminfo_dir =
            extract_terminfo().map_err(CreateTerminalEmulatorErrorKind::ExtractTerminfo)?;
        let fd = spawn_shell(terminfo_dir.path())
            .map_err(CreateTerminalEmulatorErrorKind::SpawnShell)?;
        set_nonblock(&fd).map_err(CreateTerminalEmulatorErrorKind::SetNonblock)?;

        if let Err(e) = set_window_size(&fd, TERMINAL_WIDTH, TERMINAL_HEIGHT) {
            error!("Failed to set initial window size: {}", backtraced_err(&e));
        }

        let ret = TerminalEmulator {
            parser: AnsiParser::new(),
            terminal_buffer: TerminalBuffer::new(TERMINAL_WIDTH, TERMINAL_HEIGHT),
            format_tracker: FormatTracker::new(),
            decckm_mode: false,
            cursor_state: CursorState {
                pos: CursorPos { x: 0, y: 0 },
                bold: false,
                color: TerminalColor::Default,
            },
            fd,
            _terminfo_dir: terminfo_dir,
        };
        Ok(ret)
    }

    pub fn set_win_size(
        &mut self,
        width_chars: usize,
        height_chars: usize,
    ) -> Result<(), SetWindowSizeError> {
        let response =
            self.terminal_buffer
                .set_win_size(width_chars, height_chars, &self.cursor_state.pos);
        self.cursor_state.pos = response.new_cursor_pos;

        if response.changed {
            set_window_size(&self.fd, width_chars, height_chars)?;
        }

        Ok(())
    }

    pub fn write(&mut self, to_write: TerminalInput) -> Result<(), WriteError> {
        match to_write.to_payload(self.decckm_mode) {
            TerminalInputPayload::Single(c) => {
                let mut written = 0;
                while written == 0 {
                    written = nix::unistd::write(self.fd.as_raw_fd(), &[c])?;
                }
            }
            TerminalInputPayload::Many(mut to_write) => {
                while !to_write.is_empty() {
                    let written = nix::unistd::write(self.fd.as_raw_fd(), to_write)?;
                    to_write = &to_write[written..];
                }
            }
        };
        Ok(())
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
            debug!("Incoming data: {:?}", std::str::from_utf8(incoming));
            let parsed = self.parser.push(incoming);
            for segment in parsed {
                match segment {
                    TerminalOutput::Data(data) => {
                        let response = self
                            .terminal_buffer
                            .insert_data(&self.cursor_state.pos, &data);
                        self.format_tracker
                            .push_range_adjustment(response.insertion_range);
                        self.format_tracker
                            .push_range(&self.cursor_state, response.written_range);
                        self.cursor_state.pos = response.new_cursor_pos;
                    }
                    TerminalOutput::SetCursorPos { x, y } => {
                        if let Some(x) = x {
                            self.cursor_state.pos.x = x - 1;
                        }
                        if let Some(y) = y {
                            self.cursor_state.pos.y = y - 1;
                        }
                    }
                    TerminalOutput::ClearForwards => {
                        if let Some(buf_pos) =
                            self.terminal_buffer.clear_forwards(&self.cursor_state.pos)
                        {
                            self.format_tracker
                                .push_range(&self.cursor_state, buf_pos..usize::MAX);
                        }
                    }
                    TerminalOutput::ClearAll => {
                        self.format_tracker
                            .push_range(&self.cursor_state, 0..usize::MAX);
                        self.terminal_buffer.clear_all();
                    }
                    TerminalOutput::ClearLineForwards => {
                        if let Some(range) = self
                            .terminal_buffer
                            .clear_line_forwards(&self.cursor_state.pos)
                        {
                            self.format_tracker.delete_range(range);
                        }
                    }
                    TerminalOutput::CarriageReturn => {
                        self.cursor_state.pos.x = 0;
                    }
                    TerminalOutput::Newline => {
                        self.terminal_buffer
                            .append_newline_at_line_end(&self.cursor_state.pos);
                        self.cursor_state.pos.y += 1;
                    }
                    TerminalOutput::Backspace => {
                        if self.cursor_state.pos.x >= 1 {
                            self.cursor_state.pos.x -= 1;
                        }
                    }
                    TerminalOutput::Delete(num_chars) => {
                        let deleted_buf_range = self
                            .terminal_buffer
                            .delete_forwards(&self.cursor_state.pos, num_chars);
                        if let Some(range) = deleted_buf_range {
                            self.format_tracker.delete_range(range);
                        }
                    }
                    TerminalOutput::Sgr(sgr) => {
                        // Should this be one big match ???????
                        if let Some(color) = TerminalColor::from_sgr(sgr) {
                            self.cursor_state.color = color;
                        } else if sgr == SelectGraphicRendition::Reset {
                            self.cursor_state.color = TerminalColor::Default;
                            self.cursor_state.bold = false;
                        } else if sgr == SelectGraphicRendition::Bold {
                            self.cursor_state.bold = true;
                        } else {
                            warn!("Unhandled sgr: {:?}", sgr);
                        }
                    }
                    TerminalOutput::SetMode(mode) => match mode {
                        Mode::Decckm => {
                            self.decckm_mode = true;
                        }
                        _ => {
                            warn!("unhandled set mode: {mode:?}");
                        }
                    },
                    TerminalOutput::InsertSpaces(num_spaces) => {
                        let response = self
                            .terminal_buffer
                            .insert_spaces(&self.cursor_state.pos, num_spaces);
                        self.format_tracker
                            .push_range_adjustment(response.insertion_range);
                    }
                    TerminalOutput::ResetMode(mode) => match mode {
                        Mode::Decckm => {
                            self.decckm_mode = false;
                        }
                        _ => {
                            warn!("unhandled set mode: {mode:?}");
                        }
                    },
                    TerminalOutput::Invalid => {}
                }
            }
        }

        if let Err(e) = ret {
            if e != Errno::EAGAIN {
                error!("Failed to read from child process: {e}");
            }
        }
    }

    pub fn data(&self) -> TerminalData<&[u8]> {
        self.terminal_buffer.data()
    }

    pub fn format_data(&self) -> TerminalData<Vec<FormatTag>> {
        let offset = self.terminal_buffer.data().scrollback.len();
        split_format_data_for_scrollback(self.format_tracker.tags(), offset)
    }

    pub fn cursor_pos(&self) -> CursorPos {
        self.cursor_state.pos.clone()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_format_tracker_scrollback_split() {
        let tags = vec![
            FormatTag {
                start: 0,
                end: 5,
                color: TerminalColor::Blue,
                bold: true,
            },
            FormatTag {
                start: 5,
                end: 7,
                color: TerminalColor::Red,
                bold: false,
            },
            FormatTag {
                start: 7,
                end: 10,
                color: TerminalColor::Blue,
                bold: true,
            },
            FormatTag {
                start: 10,
                end: usize::MAX,
                color: TerminalColor::Red,
                bold: true,
            },
        ];

        // Case 1: no split
        let res = split_format_data_for_scrollback(tags.clone(), 0);
        assert_eq!(res.scrollback, &[]);
        assert_eq!(res.visible, &tags[..]);

        // Case 2: Split on a boundary
        let res = split_format_data_for_scrollback(tags.clone(), 10);
        assert_eq!(res.scrollback, &tags[0..3]);
        assert_eq!(
            res.visible,
            &[FormatTag {
                start: 0,
                end: usize::MAX,
                color: TerminalColor::Red,
                bold: true,
            },]
        );

        // Case 3: Split a segment
        let res = split_format_data_for_scrollback(tags.clone(), 9);
        assert_eq!(
            res.scrollback,
            &[
                FormatTag {
                    start: 0,
                    end: 5,
                    color: TerminalColor::Blue,
                    bold: true,
                },
                FormatTag {
                    start: 5,
                    end: 7,
                    color: TerminalColor::Red,
                    bold: false,
                },
                FormatTag {
                    start: 7,
                    end: 9,
                    color: TerminalColor::Blue,
                    bold: true,
                },
            ]
        );
        assert_eq!(
            res.visible,
            &[
                FormatTag {
                    start: 0,
                    end: 1,
                    color: TerminalColor::Blue,
                    bold: true,
                },
                FormatTag {
                    start: 1,
                    end: usize::MAX,
                    color: TerminalColor::Red,
                    bold: true,
                },
            ]
        );
    }
}
