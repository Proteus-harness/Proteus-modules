//! Reader termination facts, separate from command exit and preview truncation.
use std::io::{self, Read};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReaderEnd {
    Eof,
    ReadFailed,
    DrainExpired,
}

/// At most two readers (pipes), or one reader (PTY). This records why they
/// stopped; merely closing all readers does not establish complete output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ReaderCompletions {
    remaining: u8,
    pub(super) eof: u8,
    pub(super) read_failed: u8,
    pub(super) drain_expired: u8,
}

impl ReaderCompletions {
    pub(super) fn new(tty: bool) -> Self {
        Self {
            remaining: if tty { 1 } else { 2 },
            eof: 0,
            read_failed: 0,
            drain_expired: 0,
        }
    }

    pub(super) fn record(&mut self, end: ReaderEnd) {
        assert!(self.remaining > 0, "output reader ended more than once");
        self.remaining -= 1;
        match end {
            ReaderEnd::Eof => self.eof += 1,
            ReaderEnd::ReadFailed => self.read_failed += 1,
            ReaderEnd::DrainExpired => self.drain_expired += 1,
        }
    }

    pub(super) fn is_closed(&self) -> bool {
        self.remaining == 0
    }
}

/// Same read/drain order as the existing exec reader. Blocking I/O is not made
/// preemptible by this helper. The owner still decides deadlines and teardown.
pub(super) fn drain_output(
    reader: &mut impl Read,
    mut drain_expired: impl FnMut() -> bool,
    mut on_chunk: impl FnMut(&[u8]),
    mut on_would_block: impl FnMut(),
) -> ReaderEnd {
    let mut buffer = [0; 8192];
    loop {
        if drain_expired() {
            return ReaderEnd::DrainExpired;
        }
        match reader.read(&mut buffer) {
            Ok(0) => return ReaderEnd::Eof,
            Ok(size) => on_chunk(&buffer[..size]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => on_would_block(),
            Err(_) => return ReaderEnd::ReadFailed,
        }
    }
}

#[cfg(test)]
mod tests;
