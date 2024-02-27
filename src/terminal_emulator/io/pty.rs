use nix::{errno::Errno, ioctl_write_ptr_bad, unistd::ForkResult};

use tempfile::TempDir;
use thiserror::Error;

use std::{
    ffi::CStr,
    os::fd::{AsRawFd, OwnedFd},
    path::Path,
};

use super::{ReadResponse, TermIo, TermIoErr};

ioctl_write_ptr_bad!(
    set_window_size_ioctl,
    nix::libc::TIOCSWINSZ,
    nix::pty::Winsize
);

#[derive(Debug, Error)]
enum CreatePtyIoErrorKind {
    #[error("failed to extract terminfo")]
    ExtractTerminfo(#[from] ExtractTerminfoError),
    #[error("failed to spawn shell")]
    SpawnShell(#[from] SpawnShellError),
    #[error("failed to set fd as non-blocking")]
    SetNonblock(#[from] SetNonblockError),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct CreatePtyIoError(#[from] CreatePtyIoErrorKind);

#[derive(Error, Debug)]
enum ExtractTerminfoError {
    #[error("failed to extract")]
    Extraction(#[source] std::io::Error),
    #[error("failed to create temp dir")]
    CreateTempDir(#[source] std::io::Error),
}

const TERMINFO: &[u8] = include_bytes!(std::concat!(std::env!("OUT_DIR"), "/terminfo.tar"));

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
enum PtyIoErrKind {
    #[error("failed to set win size")]
    SetWinSize(#[from] SetWindowSizeErrorKind),
    #[error("failed to read from file descriptor")]
    Read(#[source] Errno),
    #[error("failed to write to file descriptor")]
    Write(#[source] Errno),
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct PtyIoErr(#[from] PtyIoErrKind);

pub struct PtyIo {
    fd: OwnedFd,
    _terminfo_dir: TempDir,
}

impl PtyIo {
    pub fn new() -> Result<PtyIo, CreatePtyIoError> {
        let terminfo_dir = extract_terminfo().map_err(CreatePtyIoErrorKind::ExtractTerminfo)?;
        let fd = spawn_shell(terminfo_dir.path()).map_err(CreatePtyIoErrorKind::SpawnShell)?;
        set_nonblock(&fd).map_err(CreatePtyIoErrorKind::SetNonblock)?;
        Ok(PtyIo {
            fd,
            _terminfo_dir: terminfo_dir,
        })
    }
}

impl TermIo for PtyIo {
    fn read(&mut self, buf: &mut [u8]) -> Result<ReadResponse, TermIoErr> {
        let res = nix::unistd::read(self.fd.as_raw_fd(), buf);
        match res {
            Ok(v) => Ok(ReadResponse::Success(v)),
            Err(Errno::EAGAIN) => Ok(ReadResponse::Empty),
            Err(e) => Err(Box::new(PtyIoErrKind::Read(e))),
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, TermIoErr> {
        Ok(nix::unistd::write(self.fd.as_raw_fd(), buf).map_err(PtyIoErrKind::Write)?)
    }

    fn set_win_size(&mut self, width: usize, height: usize) -> Result<(), TermIoErr> {
        let win_size = nix::pty::Winsize {
            ws_row: height
                .try_into()
                .map_err(SetWindowSizeErrorKind::HeightTooLarge)
                .map_err(PtyIoErrKind::SetWinSize)?,
            ws_col: width
                .try_into()
                .map_err(SetWindowSizeErrorKind::WidthTooLarge)
                .map_err(PtyIoErrKind::SetWinSize)?,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        unsafe {
            set_window_size_ioctl(self.fd.as_raw_fd(), &win_size)
                .map_err(SetWindowSizeErrorKind::IoctlFailed)
                .map_err(PtyIoErrKind::SetWinSize)?;
        }

        Ok(())
    }
}
