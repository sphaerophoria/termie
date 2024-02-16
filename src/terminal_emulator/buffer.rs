use std::ops::Range;

use super::{CursorPos, TerminalData};

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

fn buf_to_cursor_pos(buf: &[u8], width: usize, height: usize, buf_pos: usize) -> CursorPos {
    let new_line_ranges = calc_line_ranges(buf, width);
    let new_visible_line_ranges = line_ranges_to_visible_line_ranges(&new_line_ranges, height);
    let (new_cursor_y, new_cursor_line) = new_visible_line_ranges
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

/// Given terminal height `height`, extract the visible line ranges from all line ranges (which
/// include scrollback) assuming "visible" is the bottom N lines
fn line_ranges_to_visible_line_ranges(
    line_ranges: &[Range<usize>],
    height: usize,
) -> &[Range<usize>] {
    if line_ranges.is_empty() {
        return line_ranges;
    }
    let last_line_idx = line_ranges.len();
    let first_visible_line = last_line_idx.saturating_sub(height);
    &line_ranges[first_visible_line..]
}

fn pad_buffer_for_write(
    buf: &mut Vec<u8>,
    width: usize,
    height: usize,
    cursor_pos: &CursorPos,
    write_len: usize,
) -> usize {
    let mut visible_line_ranges = {
        // Calculate in block scope to avoid accidental usage of scrollback line ranges later
        let line_ranges = calc_line_ranges(buf, width);
        line_ranges_to_visible_line_ranges(&line_ranges, height).to_vec()
    };

    for _ in visible_line_ranges.len()..cursor_pos.y + 1 {
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

fn cursor_to_buf_pos(
    buf: &[u8],
    cursor_pos: &CursorPos,
    width: usize,
    height: usize,
) -> Option<(usize, Range<usize>)> {
    let line_ranges = calc_line_ranges(buf, width);
    let visible_line_ranges = line_ranges_to_visible_line_ranges(&line_ranges, height);

    visible_line_ranges.get(cursor_pos.y).and_then(|range| {
        let candidate_pos = range.start + cursor_pos.x;
        if candidate_pos > range.end {
            None
        } else {
            Some((candidate_pos, range.clone()))
        }
    })
}

pub struct TerminalBufferInsertResponse {
    pub written_range: Range<usize>,
    pub new_cursor_pos: CursorPos,
}

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

    pub fn insert_data(
        &mut self,
        cursor_pos: &CursorPos,
        data: &[u8],
    ) -> TerminalBufferInsertResponse {
        let write_idx = pad_buffer_for_write(
            &mut self.buf,
            self.width,
            self.height,
            cursor_pos,
            data.len(),
        );
        let write_range = write_idx..write_idx + data.len();
        self.buf[write_range.clone()].copy_from_slice(data);
        let new_cursor_pos = buf_to_cursor_pos(&self.buf, self.width, self.height, write_range.end);
        TerminalBufferInsertResponse {
            written_range: write_range,
            new_cursor_pos,
        }
    }

    pub fn clear_forwards(&mut self, cursor_pos: &CursorPos) -> Option<usize> {
        let line_ranges = calc_line_ranges(&self.buf, self.width);

        let line_range = line_ranges.get(cursor_pos.y)?;

        if cursor_pos.x > line_range.end {
            return None;
        }

        let clear_pos = line_range.start + cursor_pos.x;
        self.buf.truncate(clear_pos);
        Some(clear_pos)
    }

    pub fn clear_all(&mut self) {
        self.buf.clear();
    }

    pub fn append_newline_at_line_end(&mut self, pos: &CursorPos) {
        let line_ranges = calc_line_ranges(&self.buf, self.width);
        let Some(line_range) = line_ranges.get(pos.y) else {
            return;
        };

        let newline_pos = self
            .buf
            .iter()
            .enumerate()
            .skip(line_range.start)
            .find(|(_i, b)| **b == b'\n')
            .map(|(i, _b)| i);

        if newline_pos.is_none() {
            self.buf.push(b'\n');
        }
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
        let copy_idx = pad_buffer_for_write(&mut buf, 10, 10, &cursor_pos, 10);
        assert_eq!(buf, b"asdf              \n1234\nzxyw");
        assert_eq!(copy_idx, 8);
    }

    #[test]
    fn test_canvas_clear_forwards() {
        let mut buffer = TerminalBuffer::new(5, 5);
        buffer.insert_data(&CursorPos { x: 0, y: 0 }, b"012\n3456789");
        buffer.clear_forwards(&CursorPos { x: 1, y: 1 });
        assert_eq!(buffer.data().visible, b"012\n3");
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
    fn test_canvas_newline_append() {
        let mut canvas = TerminalBuffer::new(10, 10);
        let mut cursor_pos = CursorPos { x: 0, y: 0 };
        canvas.insert_data(&cursor_pos, b"asdf\n1234\nzxyw");

        cursor_pos.x = 2;
        cursor_pos.y = 1;
        canvas.append_newline_at_line_end(&cursor_pos);
        assert_eq!(canvas.buf, b"asdf\n1234\nzxyw\n");

        canvas.clear_forwards(&cursor_pos);
        assert_eq!(canvas.buf, b"asdf\n12");

        cursor_pos.x = 0;
        cursor_pos.y = 1;
        canvas.append_newline_at_line_end(&cursor_pos);
        assert_eq!(canvas.buf, b"asdf\n12\n");

        cursor_pos.x = 0;
        cursor_pos.y = 2;
        canvas.insert_data(&cursor_pos, b"01234567890123456");

        cursor_pos.x = 4;
        cursor_pos.y = 3;
        canvas.clear_forwards(&cursor_pos);
        assert_eq!(canvas.buf, b"asdf\n12\n01234567890123");

        cursor_pos.x = 4;
        cursor_pos.y = 2;
        canvas.append_newline_at_line_end(&cursor_pos);
        assert_eq!(canvas.buf, b"asdf\n12\n01234567890123\n");
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
}
