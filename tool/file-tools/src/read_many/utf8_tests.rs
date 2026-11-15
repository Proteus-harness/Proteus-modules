use super::*;

#[test]
fn observed_eof_is_strict_even_when_old_metadata_claims_more_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tail.txt");
    std::fs::write(&path, b"valid\xc3").unwrap();
    for original_bytes in [6, 100] {
        for limit in [6, 10] {
            let error = read_text_prefix(&path, limit, original_bytes).unwrap_err();
            assert!(error.contains("failed to decode"));
        }
    }
}

#[test]
fn byte_cut_allows_only_incomplete_tail_and_preserves_conservative_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("text.txt");
    std::fs::write(&path, "valid😀tail").unwrap();
    for limit in 6..9 {
        let read = read_text_prefix(&path, limit, 13).unwrap();
        assert_eq!(read.text, "valid");
        assert!(read.truncated);
        assert_eq!(read.original_bytes, 13);
    }
    std::fs::write(&path, b"valid\xfftail").unwrap();
    assert!(read_text_prefix(&path, 7, 10).is_err());
    std::fs::write(&path, "short").unwrap();
    let read = read_text_prefix(&path, 10, 100).unwrap();
    assert_eq!(read.text, "short");
    assert_eq!(read.original_bytes, 100);
    assert!(
        read.truncated,
        "retain the existing conservative metadata claim"
    );
}
