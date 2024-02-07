use nix::{errno::Errno, unistd::ForkResult};
use std::{
    ffi::CStr,
    ops::Range,
    os::fd::{AsRawFd, OwnedFd},
};

use ansi::{AnsiParser, SelectGraphicRendition, TerminalOutput};

mod ansi;

/// Spawn a shell in a child process and return the file descriptor used for I/O
fn spawn_shell() -> OwnedFd {
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

#[derive(Clone)]
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
}

/// Calculate the indexes of the start and end of each line in the buffer given an input width.
/// Ranges do not include newlines. If a newline appears past the width, it does not result in an
/// extra line
///
/// Example
/// ```
/// let ranges = calc_line_ranges(b"12\n1234\n12345", 4);
/// assert_eq!(ranges, [0..2, 3..7, 8..11, 12..13]);
/// ```
fn calc_line_ranges(buf: &[u8], width: usize) -> Vec<Range<usize>> {
    let mut ret = vec![];
    let mut bytes_since_newline = 0;

    let mut current_start = 0;

    for (i, c) in buf.iter().enumerate() {
        if *c == b'\n' {
            ret.push(current_start..i);
            current_start = i + 1;
            bytes_since_newline = 0;
            continue;
        }

        assert!(bytes_since_newline <= width);
        if bytes_since_newline == width {
            ret.push(current_start..i);
            current_start = i;
            bytes_since_newline = 0;
            continue;
        }

        bytes_since_newline += 1;
    }

    if buf.len() > current_start {
        ret.push(current_start..buf.len());
    }
    ret
}

fn buf_to_cursor_pos(buf: &[u8], width: usize, buf_pos: usize) -> CursorPos {
    let new_line_ranges = calc_line_ranges(buf, width);
    let (new_cursor_y, new_cursor_line) = new_line_ranges
        .iter()
        .enumerate()
        .find(|(_i, r)| r.end >= buf_pos)
        .unwrap();
    let new_cursor_x = buf_pos - new_cursor_line.start;
    CursorPos {
        x: new_cursor_x,
        y: new_cursor_y,
    }
}

fn unwrapped_line_end_pos(buf: &[u8], start_pos: usize) -> usize {
    buf.iter()
        .enumerate()
        .skip(start_pos)
        .find_map(|(i, c)| match *c {
            b'\n' => Some(i),
            _ => None,
        })
        .unwrap_or(buf.len())
}

fn pad_buffer_for_write(
    buf: &mut Vec<u8>,
    width: usize,
    cursor_pos: &CursorPos,
    write_len: usize,
) -> usize {
    let mut line_ranges = calc_line_ranges(buf, width);

    for _ in line_ranges.len()..cursor_pos.y + 1 {
        buf.push(b'\n');
        let newline_pos = buf.len() - 1;
        line_ranges.push(newline_pos..newline_pos);
    }

    let line_range = &line_ranges[cursor_pos.y];

    let desired_start = line_range.start + cursor_pos.x;
    let desired_end = desired_start + write_len;

    // NOTE: We only want to pad if we hit an early newline. If we wrapped because we hit the edge
    // of the screen we can just keep writing and the wrapping will stay as is. This is an
    // important distinction because in the no-newline case we want to make sure we overwrite
    // whatever was in the buffer before
    let actual_end = unwrapped_line_end_pos(buf, line_range.start);

    let number_of_spaces = if desired_end > actual_end {
        desired_end - actual_end
    } else {
        0
    };

    for i in 0..number_of_spaces {
        buf.insert(actual_end + i, b' ');
    }

    desired_start
}

struct TerminalBufferInsertResponse {
    written_range: Range<usize>,
    new_cursor_pos: CursorPos,
}

struct TerminalBuffer {
    buf: Vec<u8>,
    width: usize,
}

impl TerminalBuffer {
    fn new(width: usize) -> TerminalBuffer {
        TerminalBuffer { buf: vec![], width }
    }

    fn insert_data(&mut self, cursor_pos: &CursorPos, data: &[u8]) -> TerminalBufferInsertResponse {
        let write_idx = pad_buffer_for_write(&mut self.buf, self.width, cursor_pos, data.len());
        let write_range = write_idx..write_idx + data.len();
        self.buf[write_range.clone()].copy_from_slice(data);
        let new_cursor_pos = buf_to_cursor_pos(&self.buf, self.width, write_range.end);
        TerminalBufferInsertResponse {
            written_range: write_range,
            new_cursor_pos,
        }
    }

    fn clear_forwards(&mut self, cursor_pos: &CursorPos) -> Option<usize> {
        let line_ranges = calc_line_ranges(&self.buf, self.width);

        let line_range = line_ranges.get(cursor_pos.y)?;

        if cursor_pos.x > line_range.end {
            return None;
        }

        let clear_pos = line_range.start + cursor_pos.x;
        self.buf.truncate(clear_pos);
        Some(clear_pos)
    }

    fn clear_all(&mut self) {
        self.buf.clear();
    }

    fn data(&self) -> &[u8] {
        &self.buf
    }
}

pub struct TerminalEmulator {
    parser: AnsiParser,
    terminal_buffer: TerminalBuffer,
    format_tracker: FormatTracker,
    cursor_state: CursorState,
    fd: OwnedFd,
}

impl TerminalEmulator {
    pub fn new() -> TerminalEmulator {
        let fd = spawn_shell();
        set_nonblock(&fd);

        TerminalEmulator {
            parser: AnsiParser::new(),
            // FIXME: Should be provided by GUI (or updated)
            // Initial size matches bash default
            terminal_buffer: TerminalBuffer::new(80),
            format_tracker: FormatTracker::new(),
            cursor_state: CursorState {
                pos: CursorPos { x: 0, y: 0 },
                bold: false,
                color: TerminalColor::Default,
            },
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
                            println!("Unhandled sgr: {:?}", sgr);
                        }
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
        self.terminal_buffer.data()
    }

    pub fn format_data(&self) -> Vec<FormatTag> {
        self.format_tracker.tags()
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
    fn test_calc_line_ranges() {
        let line_starts = calc_line_ranges(b"asdf\n0123456789\n012345678901", 10);
        assert_eq!(line_starts, &[0..4, 5..15, 16..26, 26..28]);
    }

    #[test]
    fn test_buffer_padding() {
        let mut buf = b"asdf\n1234\nzxyw".to_vec();

        let cursor_pos = CursorPos { x: 8, y: 0 };
        let copy_idx = pad_buffer_for_write(&mut buf, 10, &cursor_pos, 10);
        assert_eq!(buf, b"asdf              \n1234\nzxyw");
        assert_eq!(copy_idx, 8);
    }

    #[test]
    fn test_canvas_clear_forwards() {
        let mut buffer = TerminalBuffer::new(5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"012\n3456789");
        buffer.clear_forwards(&CursorPos { x: 1, y: 1 });
        assert_eq!(buffer.data(), b"012\n3");
    }

    #[test]
    fn test_canvas_clear() {
        let mut buffer = TerminalBuffer::new(5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"0123456789");
        buffer.clear_all();
        assert_eq!(buffer.data(), &[]);
    }

    #[test]
    fn test_terminal_buffer_overwrite_early_newline() {
        let mut buffer = TerminalBuffer::new(5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"012\n3456789");
        assert_eq!(buffer.data(), b"012\n3456789\n");

        // Cursor pos should be calculated based off wrapping at column 5, but should not result in
        // an extra newline
        buffer.insert_data(&CursorPos { x: 2, y: 1 }, b"test");
        assert_eq!(buffer.data(), b"012\n34test9\n");
    }

    #[test]
    fn test_terminal_buffer_overwrite_no_newline() {
        let mut buffer = TerminalBuffer::new(5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"0123456789");
        assert_eq!(buffer.data(), b"0123456789\n");

        // Cursor pos should be calculated based off wrapping at column 5, but should not result in
        // an extra newline
        buffer.insert_data(&CursorPos { x: 2, y: 1 }, b"test");
        assert_eq!(buffer.data(), b"0123456test\n");
    }

    #[test]
    fn test_terminal_buffer_overwrite_late_newline() {
        // This should behave exactly as test_terminal_buffer_overwrite_no_newline(), except with a
        // neline between lines 1 and 2
        let mut buffer = TerminalBuffer::new(5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"01234\n56789");
        assert_eq!(buffer.data(), b"01234\n56789\n");

        buffer.insert_data(&CursorPos { x: 2, y: 1 }, b"test");
        assert_eq!(buffer.data(), b"01234\n56test\n");
    }

    #[test]
    fn test_terminal_buffer_insert_unallocated_data() {
        let mut buffer = TerminalBuffer::new(10);
        buffer.insert_data(&CursorPos { x: 4, y: 5 }, b"hello world");
        assert_eq!(buffer.data(), b"\n\n\n\n\n    hello world\n");

        buffer.insert_data(&CursorPos { x: 3, y: 2 }, b"hello world");
        assert_eq!(buffer.data(), b"\n\n   hello world\n\n\n    hello world\n");
    }
}
