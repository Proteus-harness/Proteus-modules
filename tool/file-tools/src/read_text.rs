//! UTF-8 policy for bounded raw-byte reads. A real EOF is not a budget cut.

/// Only a read stopped by a byte budget may omit an unfinished trailing scalar.
/// Invalid interior bytes are errors at either boundary. File metadata is not
/// evidence that the actual read stopped before EOF.
pub(crate) fn decode_read_bytes(
    bytes: &[u8],
    at_budget_boundary: bool,
) -> Result<&str, std::str::Utf8Error> {
    match std::str::from_utf8(bytes) {
        Err(error) if at_budget_boundary && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()])
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_budget_boundary_can_omit_an_incomplete_scalar() {
        for text in ["я", "€", "😀"] {
            let bytes = text.as_bytes();
            for end in 1..bytes.len() {
                assert!(decode_read_bytes(&bytes[..end], false).is_err());
                assert_eq!(decode_read_bytes(&bytes[..end], true).unwrap(), "");
                let mut prefixed = b"valid".to_vec();
                prefixed.extend_from_slice(&bytes[..end]);
                assert!(decode_read_bytes(&prefixed, false).is_err());
                assert_eq!(decode_read_bytes(&prefixed, true).unwrap(), "valid");
            }
        }
    }

    #[test]
    fn invalid_bytes_are_never_repaired_and_valid_text_is_borrowed() {
        for bytes in [
            &b"valid\xfftail"[..],
            &b"\xc0\xaf"[..],
            &b"\xed\xa0\x80"[..],
        ] {
            for boundary in [false, true] {
                assert!(decode_read_bytes(bytes, boundary).is_err());
            }
        }
        for boundary in [false, true] {
            assert_eq!(decode_read_bytes(b"", boundary).unwrap(), "");
            let bytes = "a😀z".as_bytes();
            let text = decode_read_bytes(bytes, boundary).unwrap();
            assert_eq!(text, "a😀z");
            assert_eq!(text.as_ptr(), bytes.as_ptr());
        }
    }
}
