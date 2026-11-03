use super::*;
use std::sync::mpsc;

#[derive(Debug)]
enum Notice {
    Attempt(usize),
    Data(usize),
}

struct ObservedPipe {
    pipe: UnixStream,
    notices: mpsc::Sender<Notice>,
    index: usize,
}

impl Read for ObservedPipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let _ = self.notices.send(Notice::Attempt(self.index));
        let result = self.pipe.read(buffer);
        if matches!(result, Ok(size) if size > 0) {
            let _ = self.notices.send(Notice::Data(self.index));
        }
        result
    }
}

impl AsRawFd for ObservedPipe {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.pipe.as_raw_fd()
    }
}

#[test]
fn idle_readers_wait_for_readiness_and_both_wake_on_stop_or_owner_drop() {
    for drop_owner in [false, true] {
        let control = DrainControl::new().unwrap();
        let (notices, received) = mpsc::channel();
        let (stdout, mut stdout_writer) = UnixStream::pair().unwrap();
        let (stderr, mut stderr_writer) = UnixStream::pair().unwrap();
        crate::pipe_io::set_nonblocking(&stdout).unwrap();
        crate::pipe_io::set_nonblocking(&stderr).unwrap();
        let stdout = spawn_reader(
            Some(ObservedPipe {
                pipe: stdout,
                notices: notices.clone(),
                index: 0,
            }),
            control.state.clone(),
        );
        let stderr = spawn_reader(
            Some(ObservedPipe {
                pipe: stderr,
                notices,
                index: 1,
            }),
            control.state.clone(),
        );
        let mut attempted = [false; 2];
        while attempted.contains(&false) {
            match received.recv_timeout(Duration::from_secs(2)).unwrap() {
                Notice::Attempt(index) => attempted[index] = true,
                Notice::Data(_) => panic!("fixture has not written output yet"),
            }
        }
        assert!(
            received.recv_timeout(Duration::from_millis(100)).is_err(),
            "a quiet reader must not repeatedly retry read on a timer"
        );
        stdout_writer.write_all(b"stdout-tail").unwrap();
        stderr_writer.write_all(b"stderr-tail").unwrap();
        let mut read = [false; 2];
        while read.contains(&false) {
            if let Notice::Data(index) = received.recv_timeout(Duration::from_secs(2)).unwrap() {
                read[index] = true;
            }
        }

        // Keep both output streams open: only the owner's wake can finish the
        // readers promptly. A broken wake must fail within two seconds, not
        // leave the test blocked forever inside join.
        let (release, released) = mpsc::channel();
        let fallback = std::thread::spawn(move || {
            let _ = released.recv_timeout(Duration::from_secs(2));
            drop((stdout_writer, stderr_writer));
        });
        let started = Instant::now();
        if drop_owner {
            drop(control);
        } else {
            control.stop_after_grace();
        }
        let stdout = join_reader(stdout).unwrap();
        let stderr = join_reader(stderr).unwrap();
        let _ = release.send(());
        fallback.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "wake did not stop both readers"
        );
        assert_eq!(stdout.to_text(), "stdout-tail");
        assert_eq!(stderr.to_text(), "stderr-tail");
    }
}
