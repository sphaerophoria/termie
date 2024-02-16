use nix::{errno::Errno, ioctl_write_ptr_bad, unistd::ForkResult};
use tempfile::TempDir;
use thiserror::Error;

use std::{
    ffi::CStr,
    fmt,
    ops::Range,
    os::fd::{AsRawFd, OwnedFd},
    path::Path,
};

use ansi::{AnsiParser, SelectGraphicRendition, TerminalOutput};
use buffer::TerminalBuffer;

mod ansi;
mod buffer;
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

/// Spawn a shell in a child process and return the file descriptor used for I/O
fn spawn_shell(terminfo_dir: &Path) -> OwnedFd {
    unsafe {
        let res = nix::pty::forkpty(None, None).unwrap();
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
                nix::unistd::execvp(shell_name, &args).unwrap();
                // Should never run
                std::process::exit(1);
            }
        }
        res.master
    }
}

fn set_nonblock(fd: &OwnedFd) {
    let flags = nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL).unwrap();
    let mut flags =
        nix::fcntl::OFlag::from_bits(flags & nix::fcntl::OFlag::O_ACCMODE.bits()).unwrap();
    flags.set(nix::fcntl::OFlag::O_NONBLOCK, true);

    nix::fcntl::fcntl(fd.as_raw_fd(), nix::fcntl::FcntlArg::F_SETFL(flags)).unwrap();
}

fn delete_items_from_vec<T>(mut to_delete: Vec<usize>, vec: &mut Vec<T>) {
    to_delete.sort();
    for idx in to_delete.iter().rev() {
        vec.remove(*idx);
    }
}

struct ColorRangeAdjustment {
    // If a range adjustment results in a 0 width element we need to delete it
    should_delete: bool,
    // If a range was split we need to insert a new one
    to_insert: Option<FormatTag>,
}

/// if a and b overlap like
/// a:  [         ]
/// b:      [  ]
fn range_fully_conatins(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start <= b.start && a.end >= b.end
}

/// if a and b overlap like
/// a:     [      ]
/// b:  [     ]
fn range_starts_overlapping(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start > b.start && a.end > b.end
}

/// if a and b overlap like
/// a: [      ]
/// b:    [      ]
fn range_ends_overlapping(a: &Range<usize>, b: &Range<usize>) -> bool {
    range_starts_overlapping(b, a)
}

fn adjust_existing_format_range(
    existing_elem: &mut FormatTag,
    range: &Range<usize>,
) -> ColorRangeAdjustment {
    let mut ret = ColorRangeAdjustment {
        should_delete: false,
        to_insert: None,
    };

    let existing_range = existing_elem.start..existing_elem.end;
    if range_fully_conatins(range, &existing_range) {
        ret.should_delete = true;
    } else if range_fully_conatins(&existing_range, range) {
        if existing_elem.start == range.start {
            ret.should_delete = true;
        }

        if range.end != existing_elem.end {
            ret.to_insert = Some(FormatTag {
                start: range.end,
                end: existing_elem.end,
                color: existing_elem.color,
                bold: existing_elem.bold,
            });
        }

        existing_elem.end = range.start;
    } else if range_starts_overlapping(range, &existing_range) {
        existing_elem.end = range.start;
        if existing_elem.start == existing_elem.end {
            ret.should_delete = true;
        }
    } else if range_ends_overlapping(range, &existing_range) {
        existing_elem.start = range.end;
        if existing_elem.start == existing_elem.end {
            ret.should_delete = true;
        }
    } else {
        panic!(
            "Unhandled case {}-{}, {}-{}",
            existing_elem.start, existing_elem.end, range.start, range.end
        );
    }

    ret
}

fn adjust_existing_format_ranges(existing: &mut Vec<FormatTag>, range: &Range<usize>) {
    let mut effected_infos = existing
        .iter_mut()
        .enumerate()
        .filter(|(_i, item)| ranges_overlap(item.start..item.end, range.clone()))
        .collect::<Vec<_>>();

    let mut to_delete = Vec::new();
    let mut to_push = Vec::new();
    for info in &mut effected_infos {
        let adjustment = adjust_existing_format_range(info.1, range);
        if adjustment.should_delete {
            to_delete.push(info.0);
        }
        if let Some(item) = adjustment.to_insert {
            to_push.push(item);
        }
    }

    delete_items_from_vec(to_delete, existing);
    existing.extend(to_push);
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

#[derive(Debug, Clone)]
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

fn ranges_overlap(a: Range<usize>, b: Range<usize>) -> bool {
    if a.end <= b.start {
        return false;
    }

    if a.start >= b.end {
        return false;
    }

    true
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatTag {
    pub start: usize,
    pub end: usize,
    pub color: TerminalColor,
    pub bold: bool,
}

struct FormatTracker {
    color_info: Vec<FormatTag>,
}

impl FormatTracker {
    fn new() -> FormatTracker {
        FormatTracker {
            color_info: vec![FormatTag {
                start: 0,
                end: usize::MAX,
                color: TerminalColor::Default,
                bold: false,
            }],
        }
    }

    fn push_range(&mut self, cursor: &CursorState, range: Range<usize>) {
        adjust_existing_format_ranges(&mut self.color_info, &range);

        self.color_info.push(FormatTag {
            start: range.start,
            end: range.end,
            color: cursor.color,
            bold: cursor.bold,
        });

        // FIXME: Insertion sort
        // FIXME: Merge adjacent
        self.color_info.sort_by(|a, b| a.start.cmp(&b.start));
    }

    fn tags(&self) -> Vec<FormatTag> {
        self.color_info.clone()
    }

    fn delete_range(&mut self, range: Range<usize>) {
        let mut to_delete = Vec::new();
        let del_size = range.end - range.start;

        for (i, info) in &mut self.color_info.iter_mut().enumerate() {
            let info_range = info.start..info.end;
            if info.end <= range.start {
                continue;
            }

            if ranges_overlap(range.clone(), info_range.clone()) {
                if range_fully_conatins(&range, &info_range) {
                    to_delete.push(i);
                } else if range_starts_overlapping(&range, &info_range) {
                    if info.end != usize::MAX {
                        info.end = range.start;
                    }
                } else if range_ends_overlapping(&range, &info_range) {
                    info.start = range.start;
                    if info.end != usize::MAX {
                        info.end -= del_size;
                    }
                } else if range_fully_conatins(&info_range, &range) {
                    if info.end != usize::MAX {
                        info.end -= del_size;
                    }
                } else {
                    panic!("Unhandled overlap");
                }
            } else {
                assert!(!ranges_overlap(range.clone(), info_range.clone()));
                info.start -= del_size;
                if info.end != usize::MAX {
                    info.end -= del_size;
                }
            }
        }

        for i in to_delete.into_iter().rev() {
            self.color_info.remove(i);
        }
    }
}

pub struct TerminalData<T> {
    pub scrollback: T,
    pub visible: T,
}

ioctl_write_ptr_bad!(set_window_size, nix::libc::TIOCSWINSZ, nix::pty::Winsize);

pub struct TerminalEmulator {
    parser: AnsiParser,
    terminal_buffer: TerminalBuffer,
    format_tracker: FormatTracker,
    cursor_state: CursorState,
    decckm_mode: bool,
    fd: OwnedFd,
    _terminfo_dir: TempDir,
}

pub const TERMINAL_WIDTH: u16 = 50;
pub const TERMINAL_HEIGHT: u16 = 16;

impl TerminalEmulator {
    pub fn new() -> TerminalEmulator {
        let terminfo_dir = extract_terminfo().unwrap();
        let fd = spawn_shell(terminfo_dir.path());
        set_nonblock(&fd);

        let win_size = nix::pty::Winsize {
            ws_row: TERMINAL_HEIGHT,
            ws_col: TERMINAL_WIDTH,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        unsafe {
            set_window_size(fd.as_raw_fd(), &win_size).unwrap();
        }

        TerminalEmulator {
            parser: AnsiParser::new(),
            // FIXME: Should be provided by GUI (or updated)
            // Initial size matches bash default
            terminal_buffer: TerminalBuffer::new(TERMINAL_WIDTH as usize, TERMINAL_HEIGHT as usize),
            format_tracker: FormatTracker::new(),
            decckm_mode: false,
            cursor_state: CursorState {
                pos: CursorPos { x: 0, y: 0 },
                bold: false,
                color: TerminalColor::Default,
            },
            fd,
            _terminfo_dir: terminfo_dir,
        }
    }

    pub fn write(&mut self, to_write: TerminalInput) {
        match to_write.to_payload(self.decckm_mode) {
            TerminalInputPayload::Single(c) => {
                let mut written = 0;
                while written == 0 {
                    written = nix::unistd::write(self.fd.as_raw_fd(), &[c]).unwrap();
                }
            }
            TerminalInputPayload::Many(mut to_write) => {
                while !to_write.is_empty() {
                    let written = nix::unistd::write(self.fd.as_raw_fd(), to_write).unwrap();
                    to_write = &to_write[written..];
                }
            }
        };
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
    fn basic_color_tracker_test() {
        let mut format_tracker = FormatTracker::new();
        let mut cursor_state = CursorState {
            pos: CursorPos { x: 0, y: 0 },
            color: TerminalColor::Default,
            bold: false,
        };

        cursor_state.color = TerminalColor::Yellow;
        format_tracker.push_range(&cursor_state, 3..10);
        let tags = format_tracker.tags();
        assert_eq!(
            tags,
            &[
                FormatTag {
                    start: 0,
                    end: 3,
                    color: TerminalColor::Default,
                    bold: false
                },
                FormatTag {
                    start: 3,
                    end: 10,
                    color: TerminalColor::Yellow,
                    bold: false
                },
                FormatTag {
                    start: 10,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false
                },
            ]
        );

        cursor_state.color = TerminalColor::Blue;
        format_tracker.push_range(&cursor_state, 5..7);
        let tags = format_tracker.tags();
        assert_eq!(
            tags,
            &[
                FormatTag {
                    start: 0,
                    end: 3,
                    color: TerminalColor::Default,
                    bold: false
                },
                FormatTag {
                    start: 3,
                    end: 5,
                    color: TerminalColor::Yellow,
                    bold: false
                },
                FormatTag {
                    start: 5,
                    end: 7,
                    color: TerminalColor::Blue,
                    bold: false
                },
                FormatTag {
                    start: 7,
                    end: 10,
                    color: TerminalColor::Yellow,
                    bold: false
                },
                FormatTag {
                    start: 10,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false
                },
            ]
        );

        cursor_state.color = TerminalColor::Green;
        format_tracker.push_range(&cursor_state, 7..9);
        let tags = format_tracker.tags();
        assert_eq!(
            tags,
            &[
                FormatTag {
                    start: 0,
                    end: 3,
                    color: TerminalColor::Default,
                    bold: false
                },
                FormatTag {
                    start: 3,
                    end: 5,
                    color: TerminalColor::Yellow,
                    bold: false
                },
                FormatTag {
                    start: 5,
                    end: 7,
                    color: TerminalColor::Blue,
                    bold: false
                },
                FormatTag {
                    start: 7,
                    end: 9,
                    color: TerminalColor::Green,
                    bold: false
                },
                FormatTag {
                    start: 9,
                    end: 10,
                    color: TerminalColor::Yellow,
                    bold: false
                },
                FormatTag {
                    start: 10,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false
                },
            ]
        );

        cursor_state.color = TerminalColor::Red;
        cursor_state.bold = true;
        format_tracker.push_range(&cursor_state, 6..11);
        let tags = format_tracker.tags();
        assert_eq!(
            tags,
            &[
                FormatTag {
                    start: 0,
                    end: 3,
                    color: TerminalColor::Default,
                    bold: false
                },
                FormatTag {
                    start: 3,
                    end: 5,
                    color: TerminalColor::Yellow,
                    bold: false
                },
                FormatTag {
                    start: 5,
                    end: 6,
                    color: TerminalColor::Blue,
                    bold: false
                },
                FormatTag {
                    start: 6,
                    end: 11,
                    color: TerminalColor::Red,
                    bold: true
                },
                FormatTag {
                    start: 11,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false
                },
            ]
        );
    }

    #[test]
    fn test_range_overlap() {
        assert!(ranges_overlap(5..10, 7..9));
        assert!(ranges_overlap(5..10, 8..12));
        assert!(ranges_overlap(5..10, 3..6));
        assert!(ranges_overlap(5..10, 2..12));
        assert!(!ranges_overlap(5..10, 10..12));
        assert!(!ranges_overlap(5..10, 0..5));
    }

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

    #[test]
    fn test_format_tracker_del_range() {
        let mut format_tracker = FormatTracker::new();
        let mut cursor = CursorState {
            pos: CursorPos { x: 0, y: 0 },
            color: TerminalColor::Blue,
            bold: false,
        };
        format_tracker.push_range(&cursor, 0..10);
        cursor.color = TerminalColor::Red;
        format_tracker.push_range(&cursor, 10..20);

        format_tracker.delete_range(0..2);
        assert_eq!(
            format_tracker.tags(),
            [
                FormatTag {
                    start: 0,
                    end: 8,
                    color: TerminalColor::Blue,
                    bold: false
                },
                FormatTag {
                    start: 8,
                    end: 18,
                    color: TerminalColor::Red,
                    bold: false
                },
                FormatTag {
                    start: 18,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false
                }
            ]
        );

        format_tracker.delete_range(2..4);
        assert_eq!(
            format_tracker.tags(),
            [
                FormatTag {
                    start: 0,
                    end: 6,
                    color: TerminalColor::Blue,
                    bold: false
                },
                FormatTag {
                    start: 6,
                    end: 16,
                    color: TerminalColor::Red,
                    bold: false
                },
                FormatTag {
                    start: 16,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false
                }
            ]
        );

        format_tracker.delete_range(4..6);
        assert_eq!(
            format_tracker.tags(),
            [
                FormatTag {
                    start: 0,
                    end: 4,
                    color: TerminalColor::Blue,
                    bold: false
                },
                FormatTag {
                    start: 4,
                    end: 14,
                    color: TerminalColor::Red,
                    bold: false
                },
                FormatTag {
                    start: 14,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false
                }
            ]
        );

        format_tracker.delete_range(2..7);
        assert_eq!(
            format_tracker.tags(),
            [
                FormatTag {
                    start: 0,
                    end: 2,
                    color: TerminalColor::Blue,
                    bold: false
                },
                FormatTag {
                    start: 2,
                    end: 9,
                    color: TerminalColor::Red,
                    bold: false
                },
                FormatTag {
                    start: 9,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false
                }
            ]
        );
    }
}
