use super::io::TermIo;
use crate::terminal_emulator::{ReadResponse, Recording, RecordingItem, SnapshotItem};

use std::sync::mpsc::{self, Receiver, Sender};

pub struct ReplayIo {
    rx: Receiver<u8>,
}

impl TermIo for ReplayIo {
    fn read(&mut self, buf: &mut [u8]) -> Result<super::io::ReadResponse, super::io::TermIoErr> {
        let mut idx = 0;
        while let Ok(b) = self.rx.try_recv() {
            if idx >= buf.len() {
                break;
            }

            buf[idx] = b;
            idx += 1;
        }
        if idx == 0 {
            Ok(ReadResponse::Empty)
        } else {
            Ok(ReadResponse::Success(idx))
        }
    }

    fn write(&mut self, _buf: &[u8]) -> Result<usize, super::io::TermIoErr> {
        Ok(_buf.len())
    }

    fn set_win_size(&mut self, _width: usize, _height: usize) -> Result<(), super::io::TermIoErr> {
        Ok(())
    }
}

fn item_len(item: &RecordingItem) -> usize {
    match item {
        RecordingItem::Write { data } => data.len(),
        RecordingItem::SetWinSize { .. } => 1,
    }
}

fn calc_segment_lengths(recording: &Recording) -> Vec<usize> {
    recording.items().iter().map(item_len).collect()
}

enum RecordingAction {
    Write(u8),
    SetWinSize { width: usize, height: usize },
    None,
}

struct RecordingTracker {
    /// Which item are we iterating
    item_idx: usize,
    /// How deep in the item are we
    item_pos: usize,
}

impl RecordingTracker {
    fn next(&mut self, recording: &Recording) -> RecordingAction {
        loop {
            let items = recording.items();
            if self.item_idx >= items.len() {
                return RecordingAction::None;
            }

            let item = &items[self.item_idx];

            if self.item_pos >= item_len(item) {
                self.item_idx += 1;
                self.item_pos = 0;
                continue;
            }

            let ret = match item {
                RecordingItem::Write { data } => RecordingAction::Write(data[self.item_pos]),
                RecordingItem::SetWinSize { width, height } => RecordingAction::SetWinSize {
                    width: *width,
                    height: *height,
                },
            };

            self.item_pos += 1;

            return ret;
        }
    }
}

pub enum ControlAction {
    Resize { width: usize, height: usize },
    None,
}

pub struct ReplayControl {
    recording: Recording,
    tracker: RecordingTracker,
    segment_lengths: Vec<usize>,
    total_len: usize,
    tx: Sender<u8>,
    rx: Option<Receiver<u8>>,
}

impl ReplayControl {
    pub fn new(recording: Recording) -> ReplayControl {
        let tracker = RecordingTracker {
            item_pos: 0,
            item_idx: 0,
        };

        let (tx, rx) = mpsc::channel();
        let segment_lengths = calc_segment_lengths(&recording);
        let total_len = segment_lengths.iter().sum();
        ReplayControl {
            recording,
            tracker,
            segment_lengths,
            total_len,
            tx,
            rx: Some(rx),
        }
    }

    pub fn initial_state(&self) -> SnapshotItem {
        self.recording.initial_state()
    }

    pub fn io_handle(&mut self) -> ReplayIo {
        if let Some(rx) = std::mem::take(&mut self.rx) {
            ReplayIo { rx }
        } else {
            panic!("io_handle should only be called once");
        }
    }

    pub fn current_pos(&self) -> usize {
        let mut ret = self
            .segment_lengths
            .iter()
            .take(self.tracker.item_idx)
            .sum();
        ret += self.tracker.item_pos;
        ret
    }

    pub fn len(&self) -> usize {
        self.total_len
    }

    pub fn next(&mut self) -> ControlAction {
        let action = self.tracker.next(&self.recording);
        match action {
            RecordingAction::Write(b) => {
                self.tx.send(b).expect("failed to send write action");
                ControlAction::None
            }
            RecordingAction::SetWinSize { width, height } => {
                ControlAction::Resize { width, height }
            }
            RecordingAction::None => ControlAction::None,
        }
    }
}
