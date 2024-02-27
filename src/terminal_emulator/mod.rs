use std::{fmt, num::TryFromIntError, path::PathBuf};

use ansi::{AnsiParser, SelectGraphicRendition, TerminalOutput};
use buffer::TerminalBuffer;
use format_tracker::FormatTracker;
use recording::{NotIntOfType, Recorder};

pub use format_tracker::FormatTag;
pub use io::{PtyIo, TermIo};
pub use recording::{LoadRecordingError, Recording, RecordingHandle, SnapshotItem};
pub use replay::{ControlAction, ReplayControl, ReplayIo};

use crate::{error::backtraced_err, terminal_emulator::io::ReadResponse};
use thiserror::Error;

use self::{
    io::CreatePtyIoError,
    recording::{RecordingItem, StartRecordingResponse},
};

mod ansi;
mod buffer;
mod format_tracker;
mod io;
mod recording;
mod replay;

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
    Delete,
    Insert,
    PageUp,
    PageDown,
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
            // Why \e[3~? It seems like we are emulating the vt510. Other terminals do it, so we
            // can too
            // https://web.archive.org/web/20160304024035/http://www.vt100.net/docs/vt510-rm/chapter8
            // https://en.wikipedia.org/wiki/Delete_character
            TerminalInput::Delete => TerminalInputPayload::Many(b"\x1b[3~"),
            TerminalInput::Insert => TerminalInputPayload::Many(b"\x1b[2~"),
            TerminalInput::PageUp => TerminalInputPayload::Many(b"\x1b[5~"),
            TerminalInput::PageDown => TerminalInputPayload::Many(b"\x1b[6~"),
        }
    }
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

#[derive(Debug, Error)]
enum SnapshotCursorPosErrorPriv {
    #[error("x pos cannot be cast to i64")]
    XNotI64(#[source] TryFromIntError),
    #[error("y pos cannot be cast to i64")]
    YNotI64(#[source] TryFromIntError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct SnapshotCursorPosError(#[from] SnapshotCursorPosErrorPriv);

#[derive(Debug, Error)]
enum LoadCursorPosError {
    #[error("root element is not a map")]
    RootNotMap,
    #[error("x element not present")]
    MissingX,
    #[error("x cannot be case to usize")]
    XNotUsize(#[source] NotIntOfType),
    #[error("y element not present")]
    MissingY,
    #[error("y cannot be case to usize")]
    YNotUsize(#[source] NotIntOfType),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CursorPos {
    pub x: usize,
    pub y: usize,
}

impl CursorPos {
    fn from_snapshot(snapshot: SnapshotItem) -> Result<CursorPos, LoadCursorPosError> {
        use LoadCursorPosError::*;

        let mut map = snapshot.into_map().map_err(|_| RootNotMap)?;

        let x = map.remove("x").ok_or(MissingX)?;
        let x = x.into_num::<usize>().map_err(XNotUsize)?;

        let y = map.remove("y").ok_or(MissingY)?;
        let y = y.into_num::<usize>().map_err(YNotUsize)?;

        Ok(CursorPos { x, y })
    }

    fn snapshot(&self) -> Result<SnapshotItem, SnapshotCursorPosErrorPriv> {
        use SnapshotCursorPosErrorPriv::*;
        let x_i64: i64 = self.x.try_into().map_err(XNotI64)?;
        let y_i64: i64 = self.y.try_into().map_err(YNotI64)?;
        let res = SnapshotItem::Map(
            [
                ("x".to_string(), x_i64.into()),
                ("y".to_string(), y_i64.into()),
            ]
            .into(),
        );
        Ok(res)
    }
}

mod cursor_state_keys {
    pub const POS: &str = "pos";
    pub const BOLD: &str = "bold";
    pub const COLOR: &str = "color";
}

#[derive(Debug, Error)]
enum LoadCursorStateErrorPriv {
    #[error("root element is not a map")]
    RootNotMap,
    #[error("bold field is not present")]
    BoldNotPresent,
    #[error("bold field is not a bool")]
    BoldNotBool,
    #[error("color field is not present")]
    ColorNotPresent,
    #[error("color field is not a bool")]
    ColorNotString,
    #[error("color failed to parse")]
    ColorInvalid(()),
    #[error("pos field not present")]
    PosNotPresent,
    #[error("failed to parse position")]
    FailParsePos(#[source] LoadCursorPosError),
}

#[derive(Error, Debug)]
#[error(transparent)]
pub struct LoadCursorStateError(#[from] LoadCursorStateErrorPriv);

#[derive(Eq, PartialEq, Debug, Clone)]
struct CursorState {
    pos: CursorPos,
    bold: bool,
    color: TerminalColor,
}

impl CursorState {
    fn from_snapshot(snapshot: SnapshotItem) -> Result<CursorState, LoadCursorStateError> {
        use LoadCursorStateErrorPriv::*;
        let mut map = snapshot.into_map().map_err(|_| RootNotMap)?;

        let bold = map.remove(cursor_state_keys::BOLD).ok_or(BoldNotPresent)?;
        let SnapshotItem::Bool(bold) = bold else {
            Err(BoldNotBool)?
        };

        let color = map
            .remove(cursor_state_keys::COLOR)
            .ok_or(ColorNotPresent)?;
        let SnapshotItem::String(color) = color else {
            Err(ColorNotString)?
        };
        let color = color.parse().map_err(ColorInvalid)?;

        let pos = map.remove(cursor_state_keys::POS).ok_or(PosNotPresent)?;
        let pos = CursorPos::from_snapshot(pos).map_err(FailParsePos)?;

        Ok(CursorState { bold, color, pos })
    }

    fn snapshot(&self) -> Result<SnapshotItem, SnapshotCursorPosError> {
        let res = SnapshotItem::Map(
            [
                (cursor_state_keys::POS.to_string(), self.pos.snapshot()?),
                (cursor_state_keys::BOLD.to_string(), self.bold.into()),
                (
                    cursor_state_keys::COLOR.to_string(),
                    self.color.to_string().into(),
                ),
            ]
            .into(),
        );
        Ok(res)
    }
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

impl fmt::Display for TerminalColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TerminalColor::Default => "default",
            TerminalColor::Black => "black",
            TerminalColor::Red => "red",
            TerminalColor::Green => "green",
            TerminalColor::Yellow => "yellow",
            TerminalColor::Blue => "blue",
            TerminalColor::Magenta => "magenta",
            TerminalColor::Cyan => "cyan",
            TerminalColor::White => "white",
        };

        f.write_str(s)
    }
}

impl std::str::FromStr for TerminalColor {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let ret = match s {
            "default" => TerminalColor::Default,
            "black" => TerminalColor::Black,
            "red" => TerminalColor::Red,
            "green" => TerminalColor::Green,
            "yellow" => TerminalColor::Yellow,
            "blue" => TerminalColor::Blue,
            "magenta" => TerminalColor::Magenta,
            "cyan" => TerminalColor::Cyan,
            "white" => TerminalColor::White,
            _ => return Err(()),
        };
        Ok(ret)
    }
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

#[derive(Debug, Error)]
enum StartRecordingErrorPriv {
    #[error("failed to start recording")]
    Start(#[from] std::io::Error),
    #[error("failed to snapshot terminal buffer")]
    SnapshotBuffer(#[from] buffer::CreateSnapshotError),
    #[error("failed to snapshot format tracker")]
    SnapshotFormatTracker(#[from] format_tracker::SnapshotFormatTagError),
    #[error("failed to snapshot cursor")]
    SnapshotCursor(#[from] SnapshotCursorPosError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct StartRecordingError(#[from] StartRecordingErrorPriv);

#[derive(Debug, Error)]
enum LoadSnapshotErrorPriv {
    #[error("root element is not a map")]
    RootNotMap,
    #[error("parser field is not present")]
    ParserNotPresent,
    #[error("failed to load parser")]
    LoadParser(#[from] ansi::LoadSnapshotError),
    #[error("terminal_buffer field not present")]
    BufferNotPresent,
    #[error("failed to load buffer")]
    LoadBuffer(#[from] buffer::LoadSnapshotError),
    #[error("format tracker not present")]
    FormatTrackerNotPresent,
    #[error("failed to load format tracker")]
    LoadFormatTracker(#[from] format_tracker::LoadFormatTrackerSnapshotError),
    #[error("decckm field not present")]
    DecckmNotPresent,
    #[error("decckm field not bool")]
    DecckmNotBool,
    #[error("cursor_state not present")]
    CursorStateNotPresent,
    #[error("failed to load cursor state")]
    LoadCursorState(#[from] LoadCursorStateError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct LoadSnapshotError(#[from] LoadSnapshotErrorPriv);

pub struct TerminalEmulator<Io: TermIo> {
    parser: AnsiParser,
    terminal_buffer: TerminalBuffer,
    format_tracker: FormatTracker,
    cursor_state: CursorState,
    decckm_mode: bool,
    recorder: Recorder,
    io: Io,
}

pub const TERMINAL_WIDTH: usize = 50;
pub const TERMINAL_HEIGHT: usize = 16;

impl TerminalEmulator<PtyIo> {
    pub fn new(recording_path: PathBuf) -> Result<TerminalEmulator<PtyIo>, CreatePtyIoError> {
        let mut io = PtyIo::new()?;

        if let Err(e) = io.set_win_size(TERMINAL_WIDTH, TERMINAL_HEIGHT) {
            error!("Failed to set initial window size: {}", backtraced_err(&*e));
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
            recorder: Recorder::new(recording_path),
            io,
        };
        Ok(ret)
    }
}

impl TerminalEmulator<ReplayIo> {
    pub fn from_snapshot(
        snapshot: SnapshotItem,
        io_handle: ReplayIo,
    ) -> Result<TerminalEmulator<ReplayIo>, LoadSnapshotError> {
        use LoadSnapshotErrorPriv::*;

        let mut root = snapshot.into_map().map_err(|_| RootNotMap)?;
        let parser = AnsiParser::from_snapshot(root.remove("parser").ok_or(ParserNotPresent)?)
            .map_err(LoadParser)?;
        let terminal_buffer =
            TerminalBuffer::from_snapshot(root.remove("terminal_buffer").ok_or(BufferNotPresent)?)
                .map_err(LoadBuffer)?;
        let format_tracker = FormatTracker::from_snapshot(
            root.remove("format_tracker")
                .ok_or(FormatTrackerNotPresent)?,
        )
        .map_err(LoadFormatTracker)?;
        let SnapshotItem::Bool(decckm_mode) = root.remove("decckm_mode").ok_or(DecckmNotPresent)?
        else {
            Err(DecckmNotBool)?
        };
        let cursor_state =
            CursorState::from_snapshot(root.remove("cursor_state").ok_or(CursorStateNotPresent)?)
                .map_err(LoadCursorState)?;

        Ok(TerminalEmulator {
            parser,
            terminal_buffer,
            format_tracker,
            decckm_mode,
            cursor_state,
            recorder: Recorder::new("recordings".into()),
            io: io_handle,
        })
    }
}

impl<Io: TermIo> TerminalEmulator<Io> {
    pub fn get_win_size(&self) -> (usize, usize) {
        self.terminal_buffer.get_win_size()
    }

    pub fn set_win_size(
        &mut self,
        width_chars: usize,
        height_chars: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response =
            self.terminal_buffer
                .set_win_size(width_chars, height_chars, &self.cursor_state.pos);
        self.cursor_state.pos = response.new_cursor_pos;

        if response.changed {
            self.io.set_win_size(width_chars, height_chars)?;
            self.recorder.set_win_size(width_chars, height_chars);
        }

        Ok(())
    }

    pub fn write(&mut self, to_write: TerminalInput) -> Result<(), Box<dyn std::error::Error>> {
        match to_write.to_payload(self.decckm_mode) {
            TerminalInputPayload::Single(c) => {
                let mut written = 0;
                while written == 0 {
                    written = self.io.write(&[c])?;
                }
            }
            TerminalInputPayload::Many(mut to_write) => {
                while !to_write.is_empty() {
                    let written = self.io.write(to_write)?;
                    to_write = &to_write[written..];
                }
            }
        };
        Ok(())
    }

    fn handle_incoming_data(&mut self, incoming: &[u8]) {
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
                TerminalOutput::SetCursorPosRel { x, y } => {
                    if let Some(x) = x {
                        let x: i64 = x.into();
                        let current_x: i64 = self
                            .cursor_state
                            .pos
                            .x
                            .try_into()
                            .expect("x position larger than i64 can handle");
                        self.cursor_state.pos.x = (current_x + x).max(0) as usize;
                    }
                    if let Some(y) = y {
                        let y: i64 = y.into();
                        let current_y: i64 = self
                            .cursor_state
                            .pos
                            .y
                            .try_into()
                            .expect("y position larger than i64 can handle");
                        self.cursor_state.pos.y = (current_y + y).max(0) as usize;
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
                    self.cursor_state.pos.y += 1;
                }
                TerminalOutput::Backspace => {
                    if self.cursor_state.pos.x >= 1 {
                        self.cursor_state.pos.x -= 1;
                    }
                }
                TerminalOutput::InsertLines(num_lines) => {
                    let response = self
                        .terminal_buffer
                        .insert_lines(&self.cursor_state.pos, num_lines);
                    self.format_tracker.delete_range(response.deleted_range);
                    self.format_tracker
                        .push_range_adjustment(response.inserted_range);
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

    pub fn read(&mut self) {
        let mut buf = vec![0u8; 4096];
        loop {
            let read_size = match self.io.read(&mut buf) {
                Ok(ReadResponse::Empty) => break,
                Ok(ReadResponse::Success(v)) => v,
                Err(e) => {
                    error!("Failed to read from child process: {e}");
                    break;
                }
            };

            let incoming = &buf[0..read_size];
            debug!("Incoming data: {:?}", std::str::from_utf8(incoming));
            self.recorder.write(incoming);
            self.handle_incoming_data(incoming);
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

    pub fn start_recording(&mut self) -> Result<RecordingHandle, StartRecordingError> {
        use StartRecordingErrorPriv::*;

        let recording_handle = self.recorder.start_recording().map_err(Start)?;
        match recording_handle {
            StartRecordingResponse::New(initializer) => {
                initializer.snapshot_item("parser".to_string(), self.parser.snapshot());
                initializer.snapshot_item(
                    "terminal_buffer".to_string(),
                    self.terminal_buffer.snapshot().map_err(SnapshotBuffer)?,
                );
                initializer.snapshot_item(
                    "format_tracker".to_string(),
                    self.format_tracker
                        .snapshot()
                        .map_err(SnapshotFormatTracker)?,
                );
                initializer.snapshot_item("decckm_mode".to_string(), self.decckm_mode.into());
                initializer.snapshot_item(
                    "cursor_state".to_string(),
                    self.cursor_state.snapshot().map_err(SnapshotCursor)?,
                );
                Ok(initializer.into_handle())
            }
            StartRecordingResponse::Existing(handle) => Ok(handle),
        }
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

    #[test]
    fn test_cursor_state_snapshot() {
        let state = CursorState {
            pos: CursorPos { x: 10, y: 50 },
            bold: false,
            color: TerminalColor::Magenta,
        };

        let snapshot = state.snapshot().expect("failed to create snapshot");
        let loaded = CursorState::from_snapshot(snapshot).expect("failed to load snapshot");
        assert_eq!(loaded, state);
    }
}
