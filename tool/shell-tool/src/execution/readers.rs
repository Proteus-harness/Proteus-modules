//! Output readiness and an owner-only wake endpoint for bounded teardown.

use std::{
    io::{self, Read},
    process::{ChildStderr, ChildStdout},
    sync::{Arc, OnceLock},
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::{
    io::Write,
    os::{fd::AsRawFd, unix::net::UnixStream},
};

use super::BoundedBuffer;

const STOP_GRACE: Duration = Duration::from_millis(50);

struct DrainState {
    deadline: OnceLock<Instant>,
    #[cfg(unix)]
    wake: UnixStream,
}

struct DrainControl {
    state: Arc<DrainState>,
    // Only the caller owns this endpoint. Reader threads must see HUP if the
    // owner unwinds; cloning it into their Arc would keep them alive forever.
    #[cfg(unix)]
    wake: UnixStream,
}

impl DrainControl {
    fn new() -> io::Result<Self> {
        #[cfg(unix)]
        let (receiver, writer) = UnixStream::pair()?;
        Ok(Self {
            state: Arc::new(DrainState {
                deadline: OnceLock::new(),
                #[cfg(unix)]
                wake: receiver,
            }),
            #[cfg(unix)]
            wake: writer,
        })
    }

    fn stop_after_grace(&self) {
        if self
            .state
            .deadline
            .set(Instant::now() + STOP_GRACE)
            .is_err()
        {
            return;
        }
        #[cfg(unix)]
        {
            // Leave the byte unread: level-triggered readiness wakes both
            // readers, without one reader consuming the other's notification.
            let _ = (&self.wake).write_all(&[1]);
        }
    }
}

pub(super) struct OutputReaders {
    stdout: JoinHandle<io::Result<BoundedBuffer>>,
    stderr: JoinHandle<io::Result<BoundedBuffer>>,
    control: DrainControl,
}

impl OutputReaders {
    pub(super) fn new(
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
    ) -> io::Result<Self> {
        #[cfg(unix)]
        {
            if let Some(pipe) = &stdout {
                crate::pipe_io::set_nonblocking(pipe)?;
            }
            if let Some(pipe) = &stderr {
                crate::pipe_io::set_nonblocking(pipe)?;
            }
        }
        let control = DrainControl::new()?;
        Ok(Self {
            stdout: spawn_reader(stdout, control.state.clone()),
            stderr: spawn_reader(stderr, control.state.clone()),
            control,
        })
    }

    pub(super) fn is_finished(&self) -> bool {
        self.stdout.is_finished() && self.stderr.is_finished()
    }

    pub(super) fn stop_after_grace(&self) {
        self.control.stop_after_grace();
    }

    pub(super) fn join(self) -> (io::Result<BoundedBuffer>, io::Result<BoundedBuffer>) {
        let Self {
            stdout,
            stderr,
            control,
        } = self;
        let stdout = join_reader(stdout);
        let stderr = join_reader(stderr);
        // Keep the owner's wake endpoint alive until both joins complete.
        drop(control);
        (stdout, stderr)
    }
}

#[cfg(unix)]
trait OutputPipe: Read + AsRawFd {}
#[cfg(unix)]
impl<T: Read + AsRawFd> OutputPipe for T {}
#[cfg(not(unix))]
trait OutputPipe: Read {}
#[cfg(not(unix))]
impl<T: Read> OutputPipe for T {}

fn spawn_reader<R: OutputPipe + Send + 'static>(
    reader: Option<R>,
    drain: Arc<DrainState>,
) -> JoinHandle<io::Result<BoundedBuffer>> {
    std::thread::spawn(move || {
        let mut buffer = BoundedBuffer::new();
        let Some(mut reader) = reader else {
            return Ok(buffer);
        };
        let mut bytes = [0; 8192];
        loop {
            if drain.deadline.get().is_some_and(|at| Instant::now() >= *at) {
                break;
            }
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(size) => buffer.push(&bytes[..size]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    #[cfg(unix)]
                    if !wait_readable(&reader, &drain)? {
                        break;
                    }
                    #[cfg(not(unix))]
                    std::thread::yield_now();
                }
                Err(error) => return Err(error),
            }
        }
        Ok(buffer)
    })
}

#[cfg(unix)]
fn wait_readable(pipe: &impl AsRawFd, drain: &DrainState) -> io::Result<bool> {
    loop {
        let deadline = drain.deadline.get();
        let timeout = match deadline {
            Some(at) => {
                let remaining = at.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(false);
                }
                remaining
                    .as_millis()
                    .saturating_add(1)
                    .min(i32::MAX as u128) as i32
            }
            None => -1,
        };
        let mut fds = [
            libc::pollfd {
                fd: pipe.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: drain.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // Once woken, exclude the permanently readable notification and wait
        // for output/deadline only, avoiding a busy loop during the final drain.
        let count = if deadline.is_some() { 1 } else { 2 };
        // SAFETY: both descriptors are owned and alive throughout this call.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), count, timeout) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if fds[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
            return Ok(false);
        }
        if fds[0].revents & libc::POLLNVAL != 0 {
            return Err(io::Error::other("shell output descriptor is invalid"));
        }
        if fds[0].revents != 0 {
            return Ok(true);
        }
        // Wake or timeout: resample the published deadline before waiting again.
    }
}

fn join_reader(handle: JoinHandle<io::Result<BoundedBuffer>>) -> io::Result<BoundedBuffer> {
    handle
        .join()
        .map_err(|_| io::Error::other("shell output reader thread panicked"))?
}

#[cfg(all(test, unix))]
#[path = "readers/tests.rs"]
mod tests;
