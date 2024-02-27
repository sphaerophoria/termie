mod pty;
pub use pty::{CreatePtyIoError, PtyIo};

pub type TermIoErr = Box<dyn std::error::Error>;

pub enum ReadResponse {
    Success(usize),
    Empty,
}

pub trait TermIo {
    fn read(&mut self, buf: &mut [u8]) -> Result<ReadResponse, TermIoErr>;
    fn write(&mut self, buf: &[u8]) -> Result<usize, TermIoErr>;
    fn set_win_size(&mut self, width: usize, height: usize) -> Result<(), TermIoErr>;
}
