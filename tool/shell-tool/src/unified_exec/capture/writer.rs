use std::{
    fs::File,
    io::{self, Write},
    sync::{
        Arc,
        mpsc::{Receiver, TryRecvError},
    },
    time::Instant,
};

use super::super::session::lock;
use super::{Failure, Record, StorageState};

// Field drop order is important even on panic/thread spawn failure: File closes
// before the final Arc<Record> can return its reservation.
struct Writer {
    file: Option<File>,
    record: Arc<Record>,
}

pub(super) fn start(
    record: Arc<Record>,
    receiver: Receiver<Box<[u8]>>,
    file: File,
) -> io::Result<()> {
    let writer = Writer {
        file: Some(file),
        record,
    };
    std::thread::Builder::new()
        .name("exec-source-capture".into())
        .spawn(move || writer.run(receiver))
        .map(|_| ())
}

pub(super) struct AppendFailure {
    pub(super) confirmed: usize,
    pub(super) uncertain: usize,
}

/// No retry after an error (including Interrupted), no seal after uncertainty.
pub(super) fn append(writer: &mut impl Write, bytes: &[u8]) -> Result<(), AppendFailure> {
    let mut confirmed = 0;
    while confirmed < bytes.len() {
        match writer.write(&bytes[confirmed..]) {
            Ok(0) => {
                return Err(AppendFailure {
                    confirmed,
                    uncertain: 0,
                });
            }
            Ok(n) if n <= bytes.len() - confirmed => confirmed += n,
            Ok(_) => {
                return Err(AppendFailure {
                    confirmed,
                    uncertain: bytes.len() - confirmed,
                });
            }
            Err(_) => {
                return Err(AppendFailure {
                    confirmed,
                    uncertain: bytes.len() - confirmed,
                });
            }
        }
    }
    Ok(())
}

impl Writer {
    fn chunk(&mut self, bytes: &[u8]) {
        if lock(&self.record.state).failure.is_some() {
            return;
        }
        let result = append(self.file.as_mut().expect("writer file"), bytes);
        self.record.append_result(bytes.len(), result);
    }

    fn fail(&mut self) {
        drop(self.file.take());
        let mut state = lock(&self.record.state);
        state.storage = StorageState::Failed;
        state.terminal_at = Some(Instant::now());
    }

    fn run(self, receiver: Receiver<Box<[u8]>>) {
        self.run_with_wait_hook(receiver, |_, _| {});
    }

    fn run_with_wait_hook(
        mut self,
        receiver: Receiver<Box<[u8]>>,
        mut before_wait: impl FnMut(u64, u64),
    ) {
        loop {
            let mut state = lock(&self.record.state);
            if state.failure.is_some() {
                drop(state);
                self.fail();
                return;
            }
            // Queue publication and every terminal predicate use this mutex.
            // An empty check followed by atomic wait-release cannot lose a send,
            // EOF or abort notification. Control facts consume no queue capacity.
            match receiver.try_recv() {
                Ok(chunk) => {
                    drop(state);
                    self.chunk(&chunk);
                }
                Err(TryRecvError::Disconnected) => {
                    drop(state);
                    self.record.abort(Failure::WriterLost);
                }
                Err(TryRecvError::Empty) if state.readers.is_closed() => {
                    if state.confirmed != state.queued {
                        state.failure = Some(Failure::Protocol);
                        drop(state);
                        self.fail();
                        return;
                    }
                    // No I/O under locks: move sole File into immutable sealed
                    // state. This promises readable bytes, NOT fsync durability.
                    *lock(&self.record.file) = self.file.take();
                    state.storage = StorageState::Sealed;
                    state.terminal_at = Some(Instant::now());
                    return;
                }
                Err(TryRecvError::Empty) => {
                    before_wait(state.confirmed, state.queued);
                    drop(
                        self.record
                            .changed
                            .wait(state)
                            .unwrap_or_else(|error| error.into_inner()),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
pub(super) fn run_with_wait_hook(
    record: Arc<Record>,
    receiver: Receiver<Box<[u8]>>,
    file: File,
    before_wait: impl FnMut(u64, u64),
) {
    Writer {
        file: Some(file),
        record,
    }
    .run_with_wait_hook(receiver, before_wait);
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Faulty {
        calls: usize,
    }
    impl Write for Faulty {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.calls == 1 {
                Ok(bytes.len().min(2))
            } else {
                Err(io::Error::other("disk failure"))
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("no durability promise")
        }
    }
    #[test]
    fn partial_write_failure_has_no_retry_or_false_confirmation() {
        let mut sink = Faulty { calls: 0 };
        let failure = append(&mut sink, b"abcdef").err().unwrap();
        assert_eq!(
            (failure.confirmed, failure.uncertain, sink.calls),
            (2, 4, 2)
        );
    }
    #[test]
    fn raw_bytes_are_written_without_text_conversion() {
        let mut sink = Vec::new();
        assert!(append(&mut sink, &[0, 255, 128, 13, 10]).is_ok());
        assert_eq!(sink, [0, 255, 128, 13, 10]);
    }
}
