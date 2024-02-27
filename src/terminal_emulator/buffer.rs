use std::{num::TryFromIntError, ops::Range};
use thiserror::Error;

use super::{
    recording::{NotIntOfType, SnapshotItem},
    CursorPos, TerminalData,
};

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

    let mut current_start = 0;

    for (i, c) in buf.iter().enumerate() {
        if *c == b'\n' {
            ret.push(current_start..i);
            current_start = i + 1;
            continue;
        }

        let bytes_since_start = i - current_start;
        assert!(bytes_since_start <= width);
        if bytes_since_start == width {
            ret.push(current_start..i);
            current_start = i;
            continue;
        }
    }

    if buf.len() > current_start {
        ret.push(current_start..buf.len());
    }
    ret
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("invalid buffer position {buf_pos} for buffer of len {buf_len}")]
struct InvalidBufPos {
    buf_pos: usize,
    buf_len: usize,
}

fn buf_to_cursor_pos(
    buf: &[u8],
    width: usize,
    height: usize,
    buf_pos: usize,
) -> Result<CursorPos, InvalidBufPos> {
    let new_line_ranges = calc_line_ranges(buf, width);
    let new_visible_line_ranges = line_ranges_to_visible_line_ranges(&new_line_ranges, height);
    let (new_cursor_y, new_cursor_line) = new_visible_line_ranges
        .iter()
        .enumerate()
        .find(|(_i, r)| r.end >= buf_pos)
        .ok_or(InvalidBufPos {
            buf_pos,
            buf_len: buf.len(),
        })?;

    if buf_pos < new_cursor_line.start {
        info!("Old cursor position no longer on screen");
        return Ok(CursorPos { x: 0, y: 0 });
    };

    let new_cursor_x = buf_pos - new_cursor_line.start;
    Ok(CursorPos {
        x: new_cursor_x,
        y: new_cursor_y,
    })
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

/// Given terminal height `height`, extract the visible line ranges from all line ranges (which
/// include scrollback) assuming "visible" is the bottom N lines
fn line_ranges_to_visible_line_ranges(
    line_ranges: &[Range<usize>],
    height: usize,
) -> &[Range<usize>] {
    if line_ranges.is_empty() {
        return line_ranges;
    }
    let num_lines = line_ranges.len();
    let first_visible_line = num_lines.saturating_sub(height);
    &line_ranges[first_visible_line..]
}

struct PadBufferForWriteResponse {
    /// Where to copy data into
    write_idx: usize,
    /// Indexes where we added data
    inserted_padding: Range<usize>,
}

fn pad_buffer_for_write(
    buf: &mut Vec<u8>,
    width: usize,
    height: usize,
    cursor_pos: &CursorPos,
    write_len: usize,
) -> PadBufferForWriteResponse {
    let mut visible_line_ranges = {
        // Calculate in block scope to avoid accidental usage of scrollback line ranges later
        let line_ranges = calc_line_ranges(buf, width);
        line_ranges_to_visible_line_ranges(&line_ranges, height).to_vec()
    };

    let mut padding_start_pos = None;
    let mut num_inserted_characters = 0;

    let vertical_padding_needed = if cursor_pos.y + 1 > visible_line_ranges.len() {
        cursor_pos.y + 1 - visible_line_ranges.len()
    } else {
        0
    };

    if vertical_padding_needed != 0 {
        padding_start_pos = Some(buf.len());
        num_inserted_characters += vertical_padding_needed;
    }

    for _ in 0..vertical_padding_needed {
        buf.push(b'\n');
        let newline_pos = buf.len() - 1;
        visible_line_ranges.push(newline_pos..newline_pos);
    }

    let line_range = &visible_line_ranges[cursor_pos.y];

    let desired_start = line_range.start + cursor_pos.x;
    let desired_end = desired_start + write_len;

    // NOTE: We only want to pad if we hit an early newline. If we wrapped because we hit the edge
    // of the screen we can just keep writing and the wrapping will stay as is. This is an
    // important distinction because in the no-newline case we want to make sure we overwrite
    // whatever was in the buffer before
    let actual_end = unwrapped_line_end_pos(buf, line_range.start);

    // If we did not set the padding start position, it means that we are padding not at the end of
    // the buffer, but at the end of a line
    if padding_start_pos.is_none() {
        padding_start_pos = Some(actual_end);
    }

    let number_of_spaces = if desired_end > actual_end {
        desired_end - actual_end
    } else {
        0
    };

    num_inserted_characters += number_of_spaces;

    for i in 0..number_of_spaces {
        buf.insert(actual_end + i, b' ');
    }

    let start_buf_pos =
        padding_start_pos.expect("start buf pos should be guaranteed initialized by this point");

    PadBufferForWriteResponse {
        write_idx: desired_start,
        inserted_padding: start_buf_pos..start_buf_pos + num_inserted_characters,
    }
}

fn cursor_to_buf_pos_from_visible_line_ranges(
    cursor_pos: &CursorPos,
    visible_line_ranges: &[Range<usize>],
) -> Option<(usize, Range<usize>)> {
    visible_line_ranges.get(cursor_pos.y).and_then(|range| {
        let candidate_pos = range.start + cursor_pos.x;
        if candidate_pos > range.end {
            None
        } else {
            Some((candidate_pos, range.clone()))
        }
    })
}

fn cursor_to_buf_pos(
    buf: &[u8],
    cursor_pos: &CursorPos,
    width: usize,
    height: usize,
) -> Option<(usize, Range<usize>)> {
    let line_ranges = calc_line_ranges(buf, width);
    let visible_line_ranges = line_ranges_to_visible_line_ranges(&line_ranges, height);

    cursor_to_buf_pos_from_visible_line_ranges(cursor_pos, visible_line_ranges)
}

pub struct TerminalBufferInsertResponse {
    /// Range of written data after insertion of padding
    pub written_range: Range<usize>,
    /// Range of written data that is new. Note this will shift all data after it
    /// Includes padding that was previously not there, e.g. newlines needed to get to the
    /// requested row for writing
    pub insertion_range: Range<usize>,
    pub new_cursor_pos: CursorPos,
}

#[derive(Debug)]
pub struct TerminalBufferInsertLineResponse {
    /// Range of deleted data **before insertion**
    pub deleted_range: Range<usize>,
    /// Range of inserted data
    pub inserted_range: Range<usize>,
}

pub struct TerminalBufferSetWinSizeResponse {
    pub changed: bool,
    pub insertion_range: Range<usize>,
    pub new_cursor_pos: CursorPos,
}

mod terminal_buffer_keys {
    pub const BUF: &str = "buf";
    pub const WIDTH: &str = "width";
    pub const HEIGHT: &str = "height";
}

#[derive(Debug, Error)]
enum CreateSnapshotErrorKind {
    #[error("failed to convert width to i64")]
    Width(#[source] TryFromIntError),
    #[error("failed to convert height to i64")]
    Height(#[source] TryFromIntError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct CreateSnapshotError(#[from] CreateSnapshotErrorKind);

#[derive(Debug, Error)]
enum LoadSnapshotErrorKind {
    #[error("root elem is not a map")]
    NotMap,
    #[error("buf missing")]
    BufMissing,
    #[error("buf is not a vec")]
    BufNotVec,
    #[error("buf element is not u8")]
    BufElemNotU8(#[source] NotIntOfType),
    #[error("width missing")]
    WidthMissing,
    #[error("failed to get width as usize")]
    WidthNotUsize(#[source] NotIntOfType),
    #[error("height missing")]
    HeightMissing,
    #[error("failed to get height as usize")]
    HeightNotUsize(#[source] NotIntOfType),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct LoadSnapshotError(#[from] LoadSnapshotErrorKind);

#[derive(Eq, PartialEq, Debug)]
pub struct TerminalBuffer {
    buf: Vec<u8>,
    width: usize,
    height: usize,
}

impl TerminalBuffer {
    pub fn new(width: usize, height: usize) -> TerminalBuffer {
        TerminalBuffer {
            buf: vec![],
            width,
            height,
        }
    }

    pub fn from_snapshot(snapshot: SnapshotItem) -> Result<TerminalBuffer, LoadSnapshotError> {
        use LoadSnapshotErrorKind::*;
        let mut root = snapshot.into_map().map_err(|_| NotMap)?;

        let buf = root.remove(terminal_buffer_keys::BUF).ok_or(BufMissing)?;
        let buf = buf.into_vec().map_err(|_| BufNotVec)?;
        let buf: Result<Vec<u8>, _> = buf.into_iter().map(|x| x.into_num::<u8>()).collect();
        let buf = buf.map_err(BufElemNotU8)?;

        let width = root
            .remove(terminal_buffer_keys::WIDTH)
            .ok_or(WidthMissing)?;
        let width = width.into_num().map_err(WidthNotUsize)?;

        let height = root
            .remove(terminal_buffer_keys::HEIGHT)
            .ok_or(HeightMissing)?;
        let height = height.into_num().map_err(HeightNotUsize)?;

        Ok(TerminalBuffer { buf, width, height })
    }

    pub fn snapshot(&self) -> Result<SnapshotItem, CreateSnapshotError> {
        use CreateSnapshotErrorKind::*;
        let width_i64: i64 = self.width.try_into().map_err(Width)?;
        let height_i64: i64 = self.height.try_into().map_err(Height)?;
        let ret = SnapshotItem::Map(
            [
                (
                    terminal_buffer_keys::BUF.to_string(),
                    self.buf.iter().collect(),
                ),
                (terminal_buffer_keys::WIDTH.to_string(), width_i64.into()),
                (terminal_buffer_keys::HEIGHT.to_string(), height_i64.into()),
            ]
            .into(),
        );
        Ok(ret)
    }

    pub fn insert_data(
        &mut self,
        cursor_pos: &CursorPos,
        data: &[u8],
    ) -> TerminalBufferInsertResponse {
        let PadBufferForWriteResponse {
            write_idx,
            inserted_padding,
        } = pad_buffer_for_write(
            &mut self.buf,
            self.width,
            self.height,
            cursor_pos,
            data.len(),
        );
        let write_range = write_idx..write_idx + data.len();
        self.buf[write_range.clone()].copy_from_slice(data);
        let new_cursor_pos = buf_to_cursor_pos(&self.buf, self.width, self.height, write_range.end)
            .expect("write range should be valid in buf");
        TerminalBufferInsertResponse {
            written_range: write_range,
            insertion_range: inserted_padding,
            new_cursor_pos,
        }
    }

    /// Inserts data, but will not wrap. If line end is hit, data stops
    pub fn insert_spaces(
        &mut self,
        cursor_pos: &CursorPos,
        mut num_spaces: usize,
    ) -> TerminalBufferInsertResponse {
        num_spaces = self.width.min(num_spaces);

        let buf_pos = cursor_to_buf_pos(&self.buf, cursor_pos, self.width, self.height);
        match buf_pos {
            Some((buf_pos, line_range)) => {
                // Insert spaces until either we hit num_spaces, or the line width is too long
                let line_len = line_range.end - line_range.start;
                let num_inserted = (num_spaces).min(self.width - line_len);

                // Overwrite existing with spaces until we hit num_spaces or we hit the line end
                let num_overwritten = (num_spaces - num_inserted).min(line_range.end - buf_pos);

                // NOTE: We do the overwrite first so we don't have to worry about adjusting
                // indices for the newly inserted data
                self.buf[buf_pos..buf_pos + num_overwritten].fill(b' ');
                self.buf
                    .splice(buf_pos..buf_pos, std::iter::repeat(b' ').take(num_inserted));

                let used_spaces = num_inserted + num_overwritten;
                TerminalBufferInsertResponse {
                    written_range: buf_pos..buf_pos + used_spaces,
                    insertion_range: buf_pos..buf_pos + num_inserted,
                    new_cursor_pos: cursor_pos.clone(),
                }
            }
            None => {
                let PadBufferForWriteResponse {
                    write_idx,
                    inserted_padding,
                } = pad_buffer_for_write(
                    &mut self.buf,
                    self.width,
                    self.height,
                    cursor_pos,
                    num_spaces,
                );
                TerminalBufferInsertResponse {
                    written_range: write_idx..write_idx + num_spaces,
                    insertion_range: inserted_padding,
                    new_cursor_pos: cursor_pos.clone(),
                }
            }
        }
    }

    pub fn insert_lines(
        &mut self,
        cursor_pos: &CursorPos,
        mut num_lines: usize,
    ) -> TerminalBufferInsertLineResponse {
        let line_ranges = calc_line_ranges(&self.buf, self.width);
        let visible_line_ranges = line_ranges_to_visible_line_ranges(&line_ranges, self.height);

        // NOTE: Cursor x position is not used. If the cursor position was too far to the right,
        // there may be no buffer position associated with it. Use Y only
        let Some(line_range) = visible_line_ranges.get(cursor_pos.y) else {
            return TerminalBufferInsertLineResponse {
                deleted_range: 0..0,
                inserted_range: 0..0,
            };
        };

        let available_space = self.height - visible_line_ranges.len();
        // If height is 10, and y is 5, we can only insert 5 lines. If we inserted more it would
        // adjust the visible line range, and that would be a problem
        num_lines = num_lines.min(self.height - cursor_pos.y);

        let deletion_range = if num_lines > available_space {
            let num_lines_removed = num_lines - available_space;
            let removal_start_idx =
                visible_line_ranges[visible_line_ranges.len() - num_lines_removed].start;
            let deletion_range = removal_start_idx..self.buf.len();
            self.buf.truncate(removal_start_idx);
            deletion_range
        } else {
            0..0
        };

        let insertion_pos = line_range.start;

        // Edge case, if the previous line ended in a line wrap, inserting a new line will not
        // result in an extra line being shown on screen. E.g. with a width of 5, 01234 and 01234\n
        // both look like a line of length 5. In this case we need to add another newline
        if insertion_pos > 0 && self.buf[insertion_pos - 1] != b'\n' {
            num_lines += 1;
        }

        self.buf.splice(
            insertion_pos..insertion_pos,
            std::iter::repeat(b'\n').take(num_lines),
        );

        TerminalBufferInsertLineResponse {
            deleted_range: deletion_range,
            inserted_range: insertion_pos..insertion_pos + num_lines,
        }
    }

    pub fn clear_forwards(&mut self, cursor_pos: &CursorPos) -> Option<usize> {
        let line_ranges = calc_line_ranges(&self.buf, self.width);
        let visible_line_ranges = line_ranges_to_visible_line_ranges(&line_ranges, self.height);

        let Some((buf_pos, _)) =
            cursor_to_buf_pos_from_visible_line_ranges(cursor_pos, visible_line_ranges)
        else {
            return None;
        };

        let previous_last_char = self.buf[buf_pos];
        self.buf.truncate(buf_pos);

        // If we truncate at the start of a line, and the previous line did not end with a newline,
        // the first inserted newline will not have an effect on the number of visible lines. This
        // is because we are allowed to have a trailing newline that is longer than the terminal
        // width. To keep the cursor pos the same as it was before, if the truncate position is the
        // start of a line, and the previous character is _not_ a newline, insert an extra newline
        // to compensate
        //
        // If we truncated a newline it's the same situation
        if cursor_pos.x == 0 && buf_pos > 0 && self.buf[buf_pos - 1] != b'\n'
            || previous_last_char == b'\n'
        {
            self.buf.push(b'\n');
        }

        for line in visible_line_ranges {
            if line.end > buf_pos {
                self.buf.push(b'\n');
            }
        }

        let new_cursor_pos =
            buf_to_cursor_pos(&self.buf, self.width, self.height, buf_pos).map(|mut pos| {
                // NOTE: buf to cursor pos may put the cursor one past the end of the line. In this
                // case it's ok because there are two valid cursor positions and we only care about one
                // of them
                if pos.x == self.width {
                    pos.x = 0;
                    pos.y += 1;
                }
                pos
            });

        assert_eq!(new_cursor_pos, Ok(cursor_pos.clone()));
        Some(buf_pos)
    }

    pub fn clear_line_forwards(&mut self, cursor_pos: &CursorPos) -> Option<Range<usize>> {
        // Can return early if none, we didn't delete anything if there is nothing to delete
        let (buf_pos, line_range) =
            cursor_to_buf_pos(&self.buf, cursor_pos, self.width, self.height)?;

        let del_range = buf_pos..line_range.end;
        self.buf.drain(del_range.clone());
        Some(del_range)
    }

    pub fn clear_all(&mut self) {
        self.buf.clear();
    }

    pub fn delete_forwards(
        &mut self,
        cursor_pos: &CursorPos,
        num_chars: usize,
    ) -> Option<Range<usize>> {
        let Some((buf_pos, line_range)) =
            cursor_to_buf_pos(&self.buf, cursor_pos, self.width, self.height)
        else {
            return None;
        };

        let mut delete_range = buf_pos..buf_pos + num_chars;

        if delete_range.end > line_range.end && self.buf.get(line_range.end) != Some(&b'\n') {
            self.buf.insert(line_range.end, b'\n');
        }

        delete_range.end = line_range.end.min(delete_range.end);

        self.buf.drain(delete_range.clone());
        Some(delete_range)
    }

    pub fn data(&self) -> TerminalData<&[u8]> {
        let line_ranges = calc_line_ranges(&self.buf, self.width);
        let visible_line_ranges = line_ranges_to_visible_line_ranges(&line_ranges, self.height);
        if self.buf.is_empty() {
            return TerminalData {
                scrollback: &[],
                visible: &self.buf,
            };
        }
        let start = visible_line_ranges[0].start;
        TerminalData {
            scrollback: &self.buf[0..start],
            visible: &self.buf[start..],
        }
    }

    pub fn get_win_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn set_win_size(
        &mut self,
        width: usize,
        height: usize,
        cursor_pos: &CursorPos,
    ) -> TerminalBufferSetWinSizeResponse {
        let changed = self.width != width || self.height != height;
        if !changed {
            return TerminalBufferSetWinSizeResponse {
                changed: false,
                insertion_range: 0..0,
                new_cursor_pos: cursor_pos.clone(),
            };
        }

        // Ensure that the cursor position has a valid buffer position. That way when we resize we
        // can just look up where the cursor is supposed to be and map it back to it's new cursor
        // position
        let pad_response =
            pad_buffer_for_write(&mut self.buf, self.width, self.height, cursor_pos, 0);
        let buf_pos = pad_response.write_idx;
        let inserted_padding = pad_response.inserted_padding;
        let new_cursor_pos = buf_to_cursor_pos(&self.buf, width, height, buf_pos)
            .expect("buf pos should exist in buffer");

        self.width = width;
        self.height = height;

        TerminalBufferSetWinSizeResponse {
            changed,
            insertion_range: inserted_padding,
            new_cursor_pos,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_calc_line_ranges() {
        let line_starts = calc_line_ranges(b"asdf\n0123456789\n012345678901", 10);
        assert_eq!(line_starts, &[0..4, 5..15, 16..26, 26..28]);
    }

    #[test]
    fn test_buffer_padding() {
        let mut buf = b"asdf\n1234\nzxyw".to_vec();

        let cursor_pos = CursorPos { x: 8, y: 0 };
        let response = pad_buffer_for_write(&mut buf, 10, 10, &cursor_pos, 10);
        assert_eq!(buf, b"asdf              \n1234\nzxyw");
        assert_eq!(response.write_idx, 8);
        assert_eq!(response.inserted_padding, 4..18);
    }

    #[test]
    fn test_canvas_clear_forwards() {
        let mut buffer = TerminalBuffer::new(5, 5);
        // Push enough data to get some in scrollback
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"012343456789\n0123456789\n1234");

        assert_eq!(
            buffer.data().visible,
            b"\
                   34567\
                   89\n\
                   01234\
                   56789\n\
                   1234\n"
        );
        buffer.clear_forwards(&CursorPos { x: 1, y: 1 });
        // Same amount of lines should be present before and after clear
        assert_eq!(
            buffer.data().visible,
            b"\
                   34567\
                   8\n\
                   \n\
                   \n\
                   \n"
        );

        // A few special cases.
        // 1. Truncating on beginning of line and previous char was not a newline
        let mut buffer = TerminalBuffer::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"012340123401234012340123401234");
        buffer.clear_forwards(&CursorPos { x: 0, y: 1 });
        assert_eq!(buffer.data().visible, b"01234\n\n\n\n\n");

        // 2. Truncating on beginning of line and previous char was a newline
        let mut buffer = TerminalBuffer::new(5, 5);
        buffer.insert_data(
            &CursorPos { x: 0, y: 0 },
            b"01234\n0123401234012340123401234",
        );
        buffer.clear_forwards(&CursorPos { x: 0, y: 1 });
        assert_eq!(buffer.data().visible, b"01234\n\n\n\n\n");

        // 3. Truncating on a newline
        let mut buffer = TerminalBuffer::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"\n\n\n\n\n\n");
        buffer.clear_forwards(&CursorPos { x: 0, y: 1 });
        assert_eq!(buffer.data().visible, b"\n\n\n\n\n");
    }

    #[test]
    fn test_canvas_clear() {
        let mut buffer = TerminalBuffer::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"0123456789");
        buffer.clear_all();
        assert_eq!(buffer.data().visible, &[]);
    }

    #[test]
    fn test_terminal_buffer_overwrite_early_newline() {
        let mut buffer = TerminalBuffer::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"012\n3456789");
        assert_eq!(buffer.data().visible, b"012\n3456789\n");

        // Cursor pos should be calculated based off wrapping at column 5, but should not result in
        // an extra newline
        buffer.insert_data(&CursorPos { x: 2, y: 1 }, b"test");
        assert_eq!(buffer.data().visible, b"012\n34test9\n");
    }

    #[test]
    fn test_terminal_buffer_overwrite_no_newline() {
        let mut buffer = TerminalBuffer::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"0123456789");
        assert_eq!(buffer.data().visible, b"0123456789\n");

        // Cursor pos should be calculated based off wrapping at column 5, but should not result in
        // an extra newline
        buffer.insert_data(&CursorPos { x: 2, y: 1 }, b"test");
        assert_eq!(buffer.data().visible, b"0123456test\n");
    }

    #[test]
    fn test_terminal_buffer_overwrite_late_newline() {
        // This should behave exactly as test_terminal_buffer_overwrite_no_newline(), except with a
        // neline between lines 1 and 2
        let mut buffer = TerminalBuffer::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"01234\n56789");
        assert_eq!(buffer.data().visible, b"01234\n56789\n");

        buffer.insert_data(&CursorPos { x: 2, y: 1 }, b"test");
        assert_eq!(buffer.data().visible, b"01234\n56test\n");
    }

    #[test]
    fn test_terminal_buffer_insert_unallocated_data() {
        let mut buffer = TerminalBuffer::new(10, 10);
        buffer.insert_data(&CursorPos { x: 4, y: 5 }, b"hello world");
        assert_eq!(buffer.data().visible, b"\n\n\n\n\n    hello world\n");

        buffer.insert_data(&CursorPos { x: 3, y: 2 }, b"hello world");
        assert_eq!(
            buffer.data().visible,
            b"\n\n   hello world\n\n\n    hello world\n"
        );
    }

    #[test]
    fn test_canvas_scrolling() {
        let mut canvas = TerminalBuffer::new(10, 3);
        let initial_cursor_pos = CursorPos { x: 0, y: 0 };

        fn crlf(pos: &mut CursorPos) {
            pos.y += 1;
            pos.x = 0;
        }

        // Simulate real terminal usage where newlines are injected with cursor moves
        let mut response = canvas.insert_data(&initial_cursor_pos, b"asdf");
        crlf(&mut response.new_cursor_pos);
        let mut response = canvas.insert_data(&response.new_cursor_pos, b"xyzw");
        crlf(&mut response.new_cursor_pos);
        let mut response = canvas.insert_data(&response.new_cursor_pos, b"1234");
        crlf(&mut response.new_cursor_pos);
        let mut response = canvas.insert_data(&response.new_cursor_pos, b"5678");
        crlf(&mut response.new_cursor_pos);

        assert_eq!(canvas.data().scrollback, b"asdf\n");
        assert_eq!(canvas.data().visible, b"xyzw\n1234\n5678\n");
    }

    #[test]
    fn test_canvas_delete_forwards() {
        let mut canvas = TerminalBuffer::new(10, 5);
        canvas.insert_data(&CursorPos { x: 0, y: 0 }, b"asdf\n123456789012345");

        // Test normal deletion
        let deleted_range = canvas.delete_forwards(&CursorPos { x: 1, y: 0 }, 1);

        assert_eq!(deleted_range, Some(1..2));
        assert_eq!(canvas.data().visible, b"adf\n123456789012345\n");

        // Test deletion clamped on newline
        let deleted_range = canvas.delete_forwards(&CursorPos { x: 1, y: 0 }, 10);
        assert_eq!(deleted_range, Some(1..3));
        assert_eq!(canvas.data().visible, b"a\n123456789012345\n");

        // Test deletion clamped on wrap
        let deleted_range = canvas.delete_forwards(&CursorPos { x: 7, y: 1 }, 10);
        assert_eq!(deleted_range, Some(9..12));
        assert_eq!(canvas.data().visible, b"a\n1234567\n12345\n");

        // Test deletion in case where nothing is deleted
        let deleted_range = canvas.delete_forwards(&CursorPos { x: 5, y: 5 }, 10);
        assert_eq!(deleted_range, None);
        assert_eq!(canvas.data().visible, b"a\n1234567\n12345\n");
    }

    #[test]
    fn test_canvas_insert_spaces() {
        let mut canvas = TerminalBuffer::new(10, 5);
        canvas.insert_data(&CursorPos { x: 0, y: 0 }, b"asdf\n123456789012345");

        // Happy path
        let response = canvas.insert_spaces(&CursorPos { x: 2, y: 0 }, 2);
        assert_eq!(response.written_range, 2..4);
        assert_eq!(response.insertion_range, 2..4);
        assert_eq!(response.new_cursor_pos, CursorPos { x: 2, y: 0 });
        assert_eq!(canvas.data().visible, b"as  df\n123456789012345\n");

        // Truncation at newline
        let response = canvas.insert_spaces(&CursorPos { x: 2, y: 0 }, 1000);
        assert_eq!(response.written_range, 2..10);
        assert_eq!(response.insertion_range, 2..6);
        assert_eq!(response.new_cursor_pos, CursorPos { x: 2, y: 0 });
        assert_eq!(canvas.data().visible, b"as        \n123456789012345\n");

        // Truncation at line wrap
        let response = canvas.insert_spaces(&CursorPos { x: 4, y: 1 }, 1000);
        assert_eq!(response.written_range, 15..21);
        assert_eq!(
            response.insertion_range.start - response.insertion_range.end,
            0
        );
        assert_eq!(response.new_cursor_pos, CursorPos { x: 4, y: 1 });
        assert_eq!(canvas.data().visible, b"as        \n1234      12345\n");

        // Insertion at non-existant buffer pos
        let response = canvas.insert_spaces(&CursorPos { x: 2, y: 4 }, 3);
        assert_eq!(response.written_range, 30..33);
        assert_eq!(response.insertion_range, 27..34);
        assert_eq!(response.new_cursor_pos, CursorPos { x: 2, y: 4 });
        assert_eq!(
            canvas.data().visible,
            b"as        \n1234      12345\n\n     \n"
        );
    }

    #[test]
    fn test_clear_line_forwards() {
        let mut canvas = TerminalBuffer::new(10, 5);
        canvas.insert_data(&CursorPos { x: 0, y: 0 }, b"asdf\n123456789012345");

        // Nothing do delete
        let response = canvas.clear_line_forwards(&CursorPos { x: 5, y: 5 });
        assert_eq!(response, None);
        assert_eq!(canvas.data().visible, b"asdf\n123456789012345\n");

        // Hit a newline
        let response = canvas.clear_line_forwards(&CursorPos { x: 2, y: 0 });
        assert_eq!(response, Some(2..4));
        assert_eq!(canvas.data().visible, b"as\n123456789012345\n");

        // Hit a wrap
        let response = canvas.clear_line_forwards(&CursorPos { x: 2, y: 1 });
        assert_eq!(response, Some(5..13));
        assert_eq!(canvas.data().visible, b"as\n1212345\n");
    }

    #[test]
    fn test_resize_expand() {
        // Ensure that on window size increase, text stays in same spot relative to cursor position
        // This was problematic with our initial implementation. It's less of a problem after some
        // later improvements, but we can keep the test to make sure it still seems sane
        let mut canvas = TerminalBuffer::new(10, 6);

        let cursor_pos = CursorPos { x: 0, y: 0 };

        fn simulate_resize(
            canvas: &mut TerminalBuffer,
            width: usize,
            height: usize,
            cursor_pos: &CursorPos,
        ) -> TerminalBufferInsertResponse {
            let mut response = canvas.set_win_size(width, height, cursor_pos);
            response.new_cursor_pos.x = 0;
            let mut response = canvas.insert_data(&response.new_cursor_pos, &vec![b' '; width]);
            response.new_cursor_pos.x = 0;

            canvas.insert_data(&response.new_cursor_pos, b"$ ")
        }
        let response = simulate_resize(&mut canvas, 10, 5, &cursor_pos);
        let response = simulate_resize(&mut canvas, 10, 4, &response.new_cursor_pos);
        let response = simulate_resize(&mut canvas, 10, 3, &response.new_cursor_pos);
        simulate_resize(&mut canvas, 10, 5, &response.new_cursor_pos);
        assert_eq!(canvas.data().visible, b"$         \n");
    }

    #[test]
    fn test_insert_lines() {
        let mut canvas = TerminalBuffer::new(5, 5);

        // Test empty canvas
        let response = canvas.insert_lines(&CursorPos { x: 0, y: 0 }, 3);
        // Clear doesn't have to do anything as there's nothing in the canvas to push aside
        assert_eq!(response.deleted_range.start - response.deleted_range.end, 0);
        assert_eq!(
            response.inserted_range.start - response.inserted_range.end,
            0
        );
        assert_eq!(canvas.data().visible, b"");

        // Test edge wrapped
        canvas.insert_data(&CursorPos { x: 0, y: 0 }, b"0123456789asdf\nxyzw");
        assert_eq!(canvas.data().visible, b"0123456789asdf\nxyzw\n");
        let response = canvas.insert_lines(&CursorPos { x: 3, y: 2 }, 1);
        assert_eq!(canvas.data().visible, b"0123456789\n\nasdf\nxyzw\n");
        assert_eq!(response.deleted_range.start - response.deleted_range.end, 0);
        assert_eq!(response.inserted_range, 10..12);

        // Test newline wrapped + lines pushed off the edge
        let response = canvas.insert_lines(&CursorPos { x: 3, y: 2 }, 1);
        assert_eq!(canvas.data().visible, b"0123456789\n\n\nasdf\n");
        assert_eq!(response.deleted_range, 17..22);
        assert_eq!(response.inserted_range, 11..12);
    }

    #[test]
    fn test_buffer_snapshot() {
        let buf = TerminalBuffer {
            buf: vec![1, 5, 9, 11],
            width: 342,
            height: 9999,
        };

        let snapshot = buf.snapshot().expect("failed to snapshot");
        let loaded = TerminalBuffer::from_snapshot(snapshot).expect("failed to load snapshot");
        assert_eq!(buf, loaded);
    }
}
