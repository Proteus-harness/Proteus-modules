use super::*;
use std::io::Cursor;

#[test]
fn unnumbered_ranges_preserve_selected_inter_line_endings() {
    for (source, start_line, limit, expected) in [
        ("one\r\ntwo\r\nthree\r\n", 1, Some(2), "one\r\ntwo"),
        ("one\r\ntwo\r\nthree\r\n", 2, None, "two\r\nthree"),
        (
            "skip\none\r\ntwo\nthree\r\nlast",
            2,
            Some(3),
            "one\r\ntwo\nthree",
        ),
        ("skip\r\n\r\n\r\nlast\r", 2, None, "\r\n\r\nlast\r"),
        ("one\r\r\ntwo\r\n", 1, None, "one\r\r\ntwo"),
        ("one\rtwo\r", 1, None, "one\rtwo\r"),
        ("one\ntwo\n", 1, None, "one\ntwo"),
        ("one\r\n", 1, Some(1), "one"),
        ("\r\n\r\n", 1, None, "\r\n"),
        ("", 1, None, ""),
        ("one\r\n", 2, None, ""),
    ] {
        let (output, _) = render_range(
            Cursor::new(source.as_bytes()),
            Path::new("eol-fixture"),
            ReadOptions {
                start_line: Some(start_line),
                limit,
                line_numbers: false,
            },
        )
        .unwrap();
        assert_eq!(output, expected, "source={source:?}, start={start_line}");
        assert!(source.contains(output.as_str()), "{source:?}: {output:?}");
    }
}

#[test]
fn numbered_ranges_keep_the_existing_lf_display_projection() {
    for source in [
        "skip\none\ntwo\nthree\n",
        "skip\r\none\r\ntwo\r\nthree\r\n",
        "skip\none\r\ntwo\nthree\r\n",
    ] {
        let (output, metadata) = render_range(
            Cursor::new(source.as_bytes()),
            Path::new("numbered-eol-fixture"),
            ReadOptions {
                start_line: Some(2),
                limit: Some(2),
                line_numbers: true,
            },
        )
        .unwrap();
        assert_eq!(output, "2\tone\n3\ttwo");
        assert_eq!(metadata["returned_lines"], 2);
        assert_eq!(metadata["truncation_reason"], "line_limit");
        assert!(metadata["total_lines"].is_null());
    }
}

#[test]
fn unnumbered_range_counts_whole_crlf_separator_against_output_budget() {
    let limit = MAX_READ_FILE_BYTES as usize;
    let first = "x".repeat(limit - 4);
    let source = format!("{first}\r\néZ\n");
    let (output, metadata) = render_range(
        Cursor::new(source.as_bytes()),
        Path::new("budget-eol-fixture"),
        ReadOptions {
            start_line: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(output, format!("{first}\r\né"));
    assert_eq!(output.len(), limit);
    assert!(source.starts_with(output.as_str()));
    assert_eq!(metadata["returned_lines"], 2);
    assert_eq!(metadata["truncation_reason"], "output_budget");
    assert!(metadata["total_lines"].is_null());

    for spare in [1, 2] {
        let first = "x".repeat(limit - spare);
        let source = format!("{first}\r\ny\n");
        let (output, metadata) = render_range(
            Cursor::new(source.as_bytes()),
            Path::new("separator-does-not-fit"),
            ReadOptions {
                start_line: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(output, first, "spare={spare}");
        assert_eq!(metadata["returned_lines"], 1);
        assert_eq!(metadata["truncation_reason"], "output_budget");
    }
}

#[test]
fn range_metadata_keeps_raw_scan_counts_and_existing_final_eol_policy() {
    let source = "skip\r\nold\r\nkeep\r\n";
    let (output, metadata) = render_range(
        Cursor::new(source.as_bytes()),
        Path::new("metadata-eol-fixture"),
        ReadOptions {
            start_line: Some(2),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(output, "old\r\nkeep");
    assert_eq!(metadata["scanned_bytes"], source.len());
    assert_eq!(metadata["total_lines"], 3);
    assert_eq!(metadata["start_line"], 2);
    assert_eq!(metadata["end_line"], 3);
    assert_eq!(metadata["returned_lines"], 2);
    assert_eq!(metadata["truncated"], false);
}
