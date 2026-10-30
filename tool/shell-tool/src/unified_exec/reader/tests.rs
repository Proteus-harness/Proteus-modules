use std::{cell::Cell, collections::VecDeque, io::Cursor};

use super::*;

enum Step {
    Bytes(Vec<u8>),
    Error(io::ErrorKind),
    Eof,
}

struct ScriptedReader {
    steps: VecDeque<Step>,
    calls: usize,
}

impl ScriptedReader {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
            calls: 0,
        }
    }
}

impl Read for ScriptedReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        assert_eq!(output.len(), 8192, "keep the existing bounded read chunk");
        match self.steps.pop_front().expect("unexpected extra read") {
            Step::Bytes(bytes) => {
                assert!(bytes.len() <= output.len());
                output[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            Step::Error(kind) => Err(io::Error::from(kind)),
            Step::Eof => Ok(0),
        }
    }
}

#[test]
fn eof_and_raw_chunks_preserve_every_byte_without_utf8_conversion() {
    let expected = vec![0, 255, b'a', b'\n', 128, 13];
    let mut reader = ScriptedReader::new([
        Step::Bytes(expected[..3].to_vec()),
        Step::Bytes(expected[3..].to_vec()),
        Step::Eof,
    ]);
    let mut actual = Vec::new();
    let end = drain_output(
        &mut reader,
        || false,
        |chunk| actual.extend_from_slice(chunk),
        || panic!("not pending"),
    );
    assert_eq!(end, ReaderEnd::Eof);
    assert_eq!(actual, expected);
    assert_eq!(reader.calls, 3);
}

#[test]
fn empty_eof_has_no_preview_callback() {
    let end = drain_output(
        &mut Cursor::new([]),
        || false,
        |_| panic!("empty stream"),
        || panic!("not pending"),
    );
    assert_eq!(end, ReaderEnd::Eof);
}

#[test]
fn permanent_read_failure_does_not_claim_eof_or_retry() {
    let mut reader = ScriptedReader::new([
        Step::Bytes(b"prefix".to_vec()),
        Step::Error(io::ErrorKind::BrokenPipe),
        Step::Eof,
    ]);
    let mut actual = Vec::new();
    let end = drain_output(
        &mut reader,
        || false,
        |chunk| actual.extend_from_slice(chunk),
        || panic!("not pending"),
    );
    assert_eq!(end, ReaderEnd::ReadFailed);
    assert_eq!(actual, b"prefix");
    assert_eq!(reader.calls, 2);
}

#[test]
fn interrupted_and_would_block_keep_existing_retry_and_pause_semantics() {
    let mut reader = ScriptedReader::new([
        Step::Error(io::ErrorKind::Interrupted),
        Step::Error(io::ErrorKind::WouldBlock),
        Step::Bytes(b"later".to_vec()),
        Step::Eof,
    ]);
    let mut pauses = 0;
    let mut actual = Vec::new();
    let end = drain_output(
        &mut reader,
        || false,
        |chunk| actual.extend_from_slice(chunk),
        || pauses += 1,
    );
    assert_eq!(end, ReaderEnd::Eof);
    assert_eq!(pauses, 1);
    assert_eq!(reader.calls, 4);
    assert_eq!(actual, b"later");
}

#[test]
fn expired_drain_is_checked_before_read_even_when_eof_is_available() {
    let mut reader = ScriptedReader::new([Step::Eof]);
    let end = drain_output(
        &mut reader,
        || true,
        |_| panic!("expired"),
        || panic!("expired"),
    );
    assert_eq!(end, ReaderEnd::DrainExpired);
    assert_eq!(reader.calls, 0);
}

#[test]
fn drain_expiring_while_pending_never_becomes_eof() {
    let expired = Cell::new(false);
    let mut reader = ScriptedReader::new([Step::Error(io::ErrorKind::WouldBlock), Step::Eof]);
    let end = drain_output(
        &mut reader,
        || expired.get(),
        |_| panic!("no data"),
        || expired.set(true),
    );
    assert_eq!(end, ReaderEnd::DrainExpired);
    assert_eq!(reader.calls, 1);
}

#[test]
fn tty_requires_one_reader_but_closure_is_not_eof() {
    for end in [
        ReaderEnd::Eof,
        ReaderEnd::ReadFailed,
        ReaderEnd::DrainExpired,
    ] {
        let mut completions = ReaderCompletions::new(true);
        assert!(!completions.is_closed());
        completions.record(end);
        assert!(completions.is_closed());
        assert_eq!(completions.eof, u8::from(end == ReaderEnd::Eof));
        assert_eq!(
            completions.read_failed,
            u8::from(end == ReaderEnd::ReadFailed)
        );
        assert_eq!(
            completions.drain_expired,
            u8::from(end == ReaderEnd::DrainExpired)
        );
    }
}

#[test]
fn pipes_require_both_readers_and_preserve_mixed_incomplete_reasons() {
    let mut completions = ReaderCompletions::new(false);
    completions.record(ReaderEnd::ReadFailed);
    assert!(!completions.is_closed());
    completions.record(ReaderEnd::DrainExpired);
    assert!(completions.is_closed());
    assert_eq!(completions.eof, 0);
    assert_eq!(completions.read_failed, 1);
    assert_eq!(completions.drain_expired, 1);
}

#[test]
fn pipes_with_two_eof_facts_are_distinct_from_just_closed() {
    let mut completions = ReaderCompletions::new(false);
    completions.record(ReaderEnd::Eof);
    assert!(!completions.is_closed());
    completions.record(ReaderEnd::Eof);
    assert!(completions.is_closed());
    assert_eq!(completions.eof, 2);
    assert_eq!(completions.read_failed + completions.drain_expired, 0);
}
