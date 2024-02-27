use std::{num::TryFromIntError, ops::Range};

use super::{recording::NotIntOfType, CursorState, TerminalColor};
use crate::terminal_emulator::recording::SnapshotItem;
use thiserror::Error;

fn ranges_overlap(a: Range<usize>, b: Range<usize>) -> bool {
    if a.end <= b.start {
        return false;
    }

    if a.start >= b.end {
        return false;
    }

    true
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

fn delete_items_from_vec<T>(mut to_delete: Vec<usize>, vec: &mut Vec<T>) {
    to_delete.sort();
    for idx in to_delete.iter().rev() {
        vec.remove(*idx);
    }
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

struct ColorRangeAdjustment {
    // If a range adjustment results in a 0 width element we need to delete it
    should_delete: bool,
    // If a range was split we need to insert a new one
    to_insert: Option<FormatTag>,
}

#[derive(Debug, Error)]
enum LoadFormatTagSnapshotError {
    #[error("root element is not a map")]
    RootNotMap,
    #[error("start elemnt missing")]
    StartMissing,
    #[error("start is not a usize")]
    StartNotUsize(#[source] NotIntOfType),
    #[error("end element missing")]
    EndMissing,
    #[error("end could not be parsed as i64")]
    EndNotInt,
    #[error("end not usize (or -1)")]
    EndNotUsize(#[source] TryFromIntError),
    #[error("bold element missing")]
    BoldMissing,
    #[error("bold element not bool")]
    BoldNotBool,
    #[error("color element is missing")]
    ColorMissing,
    #[error("color not a string")]
    ColorNotString,
    #[error("failed to parse color from string")]
    ParseColor(()),
}

#[derive(Debug, Error)]
enum SnapshotFormatTagErrorKind {
    #[error("start cannot be serialized as i64")]
    StartNotI64(#[source] TryFromIntError),
    #[error("end cannot be serialized as i64")]
    EndNotI64(#[source] TryFromIntError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct SnapshotFormatTagError(#[from] SnapshotFormatTagErrorKind);

mod format_tag_keys {
    pub const START: &str = "start";
    pub const END: &str = "end";
    pub const COLOR: &str = "color";
    pub const BOLD: &str = "bold";
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatTag {
    pub start: usize,
    pub end: usize,
    pub color: TerminalColor,
    pub bold: bool,
}

impl FormatTag {
    fn from_snapshot(snapshot: SnapshotItem) -> Result<FormatTag, LoadFormatTagSnapshotError> {
        use LoadFormatTagSnapshotError::*;
        let mut root = snapshot.into_map().map_err(|_| RootNotMap)?;

        let start = root.remove(format_tag_keys::START).ok_or(StartMissing)?;
        let start = start.into_num::<usize>().map_err(StartNotUsize)?;

        let end = root.remove(format_tag_keys::END).ok_or(EndMissing)?;
        let end = end.into_i64().map_err(|_| EndNotInt)?;
        let end: usize = if end == -1 {
            usize::MAX
        } else {
            end.try_into().map_err(EndNotUsize)?
        };

        let bold = root.remove(format_tag_keys::BOLD).ok_or(BoldMissing)?;
        let bold = bold.into_bool().map_err(|_| BoldNotBool)?;

        let color = root.remove(format_tag_keys::COLOR).ok_or(ColorMissing)?;
        let color = color.into_string().map_err(|_| ColorNotString)?;
        let color = color.parse().map_err(ParseColor)?;

        Ok(FormatTag {
            start,
            end,
            bold,
            color,
        })
    }

    fn snapshot(&self) -> Result<SnapshotItem, SnapshotFormatTagError> {
        use SnapshotFormatTagErrorKind::*;
        let start_i64: i64 = self.start.try_into().map_err(StartNotI64)?;
        let end_i64: i64 = if self.end == usize::MAX {
            -1
        } else {
            self.end.try_into().map_err(EndNotI64)?
        };
        let arr = [
            (format_tag_keys::START.to_string(), start_i64.into()),
            (format_tag_keys::END.to_string(), end_i64.into()),
            (
                format_tag_keys::COLOR.to_string(),
                self.color.to_string().into(),
            ),
            (format_tag_keys::BOLD.to_string(), self.bold.into()),
        ];
        Ok(SnapshotItem::Map(arr.into()))
    }
}

#[derive(Debug, Error)]
enum LoadFormatTrackerSnapshotErrorKind {
    #[error("root element is not an array")]
    NotArray,
    #[error("failed to load format tag")]
    LoadTag(#[from] LoadFormatTagSnapshotError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct LoadFormatTrackerSnapshotError(#[from] LoadFormatTrackerSnapshotErrorKind);

pub struct FormatTracker {
    color_info: Vec<FormatTag>,
}

impl FormatTracker {
    pub fn new() -> FormatTracker {
        FormatTracker {
            color_info: vec![FormatTag {
                start: 0,
                end: usize::MAX,
                color: TerminalColor::Default,
                bold: false,
            }],
        }
    }

    pub fn from_snapshot(
        snapshot: SnapshotItem,
    ) -> Result<FormatTracker, LoadFormatTrackerSnapshotError> {
        use LoadFormatTrackerSnapshotErrorKind::*;
        let arr = snapshot.into_vec().map_err(|_| NotArray)?;

        let color_info: Result<Vec<FormatTag>, LoadFormatTagSnapshotError> =
            arr.into_iter().map(FormatTag::from_snapshot).collect();
        let color_info = color_info.map_err(LoadTag)?;
        Ok(FormatTracker { color_info })
    }

    pub fn snapshot(&self) -> Result<SnapshotItem, SnapshotFormatTagError> {
        Ok(SnapshotItem::Array(
            self.color_info
                .iter()
                .map(FormatTag::snapshot)
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }

    pub fn push_range(&mut self, cursor: &CursorState, range: Range<usize>) {
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

    /// Move all tags > range.start to range.start + range.len
    /// No gaps in coloring data, so one range must expand instead of just be adjusted
    pub fn push_range_adjustment(&mut self, range: Range<usize>) {
        let range_len = range.end - range.start;
        for info in &mut self.color_info {
            if info.end <= range.start {
                continue;
            }

            if info.start > range.start {
                info.start += range_len;
                if info.end != usize::MAX {
                    info.end += range_len;
                }
            } else if info.end != usize::MAX {
                info.end += range_len;
            }
        }
    }

    pub fn tags(&self) -> Vec<FormatTag> {
        self.color_info.clone()
    }

    pub fn delete_range(&mut self, range: Range<usize>) {
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

#[cfg(test)]
mod test {
    use super::super::{CursorPos, CursorState};
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

    #[test]
    fn test_range_adjustment() {
        let mut format_tracker = FormatTracker::new();
        let mut cursor = CursorState {
            pos: CursorPos { x: 0, y: 0 },
            color: TerminalColor::Blue,
            bold: false,
        };
        format_tracker.push_range(&cursor, 0..5);
        cursor.color = TerminalColor::Red;
        format_tracker.push_range(&cursor, 5..10);

        assert_eq!(
            format_tracker.tags(),
            [
                FormatTag {
                    start: 0,
                    end: 5,
                    color: TerminalColor::Blue,
                    bold: false,
                },
                FormatTag {
                    start: 5,
                    end: 10,
                    color: TerminalColor::Red,
                    bold: false,
                },
                FormatTag {
                    start: 10,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false,
                },
            ]
        );

        // This should extend the first section, and push all the ones after
        format_tracker.push_range_adjustment(0..3);
        assert_eq!(
            format_tracker.tags(),
            [
                FormatTag {
                    start: 0,
                    end: 8,
                    color: TerminalColor::Blue,
                    bold: false,
                },
                FormatTag {
                    start: 8,
                    end: 13,
                    color: TerminalColor::Red,
                    bold: false,
                },
                FormatTag {
                    start: 13,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false,
                },
            ]
        );

        // Should have no effect as we're in the last range
        format_tracker.push_range_adjustment(15..50);
        assert_eq!(
            format_tracker.tags(),
            [
                FormatTag {
                    start: 0,
                    end: 8,
                    color: TerminalColor::Blue,
                    bold: false,
                },
                FormatTag {
                    start: 8,
                    end: 13,
                    color: TerminalColor::Red,
                    bold: false,
                },
                FormatTag {
                    start: 13,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false,
                },
            ]
        );

        // And for good measure, check something in the middle
        // This should not touch the first segment, extend the second, and move the third forward
        format_tracker.push_range_adjustment(10..12);
        assert_eq!(
            format_tracker.tags(),
            [
                FormatTag {
                    start: 0,
                    end: 8,
                    color: TerminalColor::Blue,
                    bold: false,
                },
                FormatTag {
                    start: 8,
                    end: 15,
                    color: TerminalColor::Red,
                    bold: false,
                },
                FormatTag {
                    start: 15,
                    end: usize::MAX,
                    color: TerminalColor::Default,
                    bold: false,
                },
            ]
        );
    }

    #[test]
    fn test_format_tag_snapshot() {
        let tag = FormatTag {
            start: 0,
            // Edge case test, usize max needs to be set to -1
            end: usize::MAX,
            color: TerminalColor::Blue,
            bold: true,
        };

        let loaded = FormatTag::from_snapshot(tag.snapshot().expect("failed to snapshot"))
            .expect("failed to load snapshot");
        assert_eq!(loaded, tag);

        let tag = FormatTag {
            start: 50,
            // Edge case test, usize max needs to be set to -1
            end: 105,
            color: TerminalColor::Red,
            bold: false,
        };
        let loaded = FormatTag::from_snapshot(tag.snapshot().expect("failed to snapshot"))
            .expect("failed to load snapshot");
        assert_eq!(loaded, tag);
    }

    #[test]
    fn test_format_tracker_snapshot() {
        let tracker = FormatTracker {
            color_info: vec![
                FormatTag {
                    start: 0,
                    end: 5,
                    color: TerminalColor::Black,
                    bold: false,
                },
                FormatTag {
                    start: 5,
                    end: usize::MAX,
                    color: TerminalColor::Red,
                    bold: true,
                },
            ],
        };

        let loaded = FormatTracker::from_snapshot(tracker.snapshot().expect("failed to snapshot"))
            .expect("failed to load snapshot");
        assert_eq!(loaded.color_info, tracker.color_info);
    }
}
