//! Nonblocking reads let an owner close inherited pipes at its drain deadline.

use std::{io, os::fd::AsRawFd};

pub(super) fn set_nonblocking(file: &impl AsRawFd) -> io::Result<()> {
    let fd = file.as_raw_fd();
    // SAFETY: the caller owns this live pipe; only its file flags change.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
