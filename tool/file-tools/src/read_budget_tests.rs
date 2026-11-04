use super::*;
use std::{cell::Cell, io::Cursor, rc::Rc};

struct Counted<R> {
    inner: R,
    consumed: Rc<Cell<usize>>,
}
impl<R: Read> Read for Counted<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(bytes)?;
        self.consumed.set(self.consumed.get() + read);
        Ok(read)
    }
}

#[test]
fn line_limit_stops_before_an_enormous_tail_and_does_not_claim_total_lines() {
    let consumed = Rc::new(Cell::new(0));
    let input = Cursor::new(b"one\n").chain(std::io::repeat(b'x').take(1 << 30));
    let reader = BufReader::new(Counted {
        inner: input,
        consumed: consumed.clone(),
    });
    let (output, metadata) = render_range(
        reader,
        Path::new("synthetic"),
        ReadOptions {
            limit: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(output, "one");
    assert!(metadata["total_lines"].is_null());
    assert_eq!(metadata["truncated"], true);
    assert_eq!(
        consumed.get(),
        4,
        "the tail must not be read just to count lines"
    );
}

#[test]
fn every_range_option_preserves_output_and_line_byte_budgets() {
    for options in [
        ReadOptions {
            start_line: Some(1),
            ..Default::default()
        },
        ReadOptions {
            line_numbers: true,
            ..Default::default()
        },
        ReadOptions {
            limit: Some(1),
            ..Default::default()
        },
    ] {
        let consumed = Rc::new(Cell::new(0));
        let reader = BufReader::new(Counted {
            inner: std::io::repeat(b'x'),
            consumed: consumed.clone(),
        });
        let (output, metadata) = render_range(reader, Path::new("infinite-line"), options).unwrap();
        assert!(output.len() as u64 <= MAX_READ_FILE_BYTES);
        assert_eq!(metadata["truncated"], true);
        assert!(metadata["total_lines"].is_null());
        assert!(consumed.get() as u64 <= MAX_READ_FILE_BYTES + 8192);
    }
}

#[test]
fn skipping_a_huge_prefix_is_bounded_and_utf8_truncation_is_valid() {
    let reader = Cursor::new("x\n".repeat(MAX_RANGE_SCAN_BYTES as usize));
    let (output, metadata) = render_range(
        reader,
        Path::new("huge-prefix"),
        ReadOptions {
            start_line: Some(usize::MAX),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(output.is_empty());
    assert_eq!(metadata["scanned_bytes"], MAX_RANGE_SCAN_BYTES);
    assert_eq!(metadata["truncation_reason"], "scan_budget");
    let reader = Cursor::new("я".repeat(MAX_READ_FILE_BYTES as usize));
    let (output, metadata) = render_range(
        reader,
        Path::new("utf8-line"),
        ReadOptions {
            line_numbers: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(output.len() as u64 <= MAX_READ_FILE_BYTES);
    assert!(output.starts_with("1\tя"));
    assert_eq!(metadata["truncated"], true);
}

#[test]
fn full_read_rechecks_the_actual_bytes_instead_of_trusting_old_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("grown");
    std::fs::write(&path, vec![b'x'; MAX_READ_FILE_BYTES as usize + 1]).unwrap();
    assert!(read_full(&path).unwrap_err().contains("budget"));
}
