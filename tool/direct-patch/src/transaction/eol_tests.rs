use super::*;

fn update(original: &str, patch: &str) -> Result<String, String> {
    let operations = crate::parse_patch(patch)?;
    let [
        PatchOperation::Update {
            hunks,
            no_newline_at_eof,
            ..
        },
    ] = operations.as_slice()
    else {
        panic!("fixture must contain one update");
    };
    apply_hunks(original, hunks, *no_newline_at_eof)
}

const REPLACE_FIRST: &str =
    "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n+new\n keep\n*** End Patch";

#[test]
fn source_eol_not_patch_eol_controls_uniform_updates() {
    for source_eol in ["\n", "\r\n"] {
        for patch_eol in ["\n", "\r\n"] {
            let original = format!("old{source_eol}keep{source_eol}");
            let patch = REPLACE_FIRST.replace('\n', patch_eol);
            assert_eq!(
                update(&original, &patch).unwrap(),
                format!("new{source_eol}keep{source_eol}")
            );
        }
    }
}

#[test]
fn crlf_insert_remove_and_multiple_hunks_keep_the_source_delimiter() {
    let patch = "*** Begin Patch\n*** Update File: file.txt\n@@\n old\n+inserted\n keep\n@@\n-remove\n+replacement\n tail\n*** End Patch";
    assert_eq!(
        update("old\r\nkeep\r\nremove\r\ntail\r\n", patch).unwrap(),
        "old\r\ninserted\r\nkeep\r\nreplacement\r\ntail\r\n"
    );
}

#[test]
fn unterminated_and_explicit_eof_updates_do_not_leave_a_stray_carriage_return() {
    assert_eq!(update("old\r\nkeep", REPLACE_FIRST).unwrap(), "new\r\nkeep");
    let patch = REPLACE_FIRST.replace("*** End Patch", "*** End of File\n*** End Patch");
    assert_eq!(update("old\r\nkeep\r\n", &patch).unwrap(), "new\r\nkeep");

    let append =
        "*** Begin Patch\n*** Update File: file.txt\n@@\n-drop\n last\n+added\n*** End Patch";
    assert_eq!(update("drop\r\nlast", append).unwrap(), "last\r\nadded");
}

#[test]
fn empty_blank_and_no_hunk_roundtrips_keep_existing_newline_semantics() {
    for source in [
        "",
        "plain",
        "bare\r",
        "\r\n",
        "a\r\n\r\n",
        "a\n",
        "a\nother\r\n",
    ] {
        assert_eq!(apply_hunks(source, &[], false).unwrap(), source);
    }
    let remove = "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n*** End Patch";
    assert_eq!(update("old\r\n", remove).unwrap(), "");
    let blank =
        "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n+new\n \n keep\n*** End Patch";
    assert_eq!(
        update("old\r\n\r\nkeep\r\n", blank).unwrap(),
        "new\r\n\r\nkeep\r\n"
    );
}

#[test]
fn bare_carriage_returns_remain_data_and_no_eol_source_uses_lf() {
    let first_only = "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n+new\n*** End Patch";
    assert_eq!(
        update("old\r\nliteral\r", first_only).unwrap(),
        "new\r\nliteral\r"
    );
    assert!(update("old\r", first_only).is_err());
    let internal =
        "*** Begin Patch\n*** Update File: file.txt\n@@\n-left\rmid\n+right\rmid\n*** End Patch";
    assert_eq!(update("left\rmid\r\n", internal).unwrap(), "right\rmid\r\n");
    let insert =
        "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n+new\n+second\n*** End Patch";
    assert_eq!(update("old", insert).unwrap(), "new\nsecond");
}

#[test]
fn mixed_eol_keeps_legacy_matching_and_untouched_bytes() {
    let first_only = "*** Begin Patch\n*** Update File: file.txt\n@@\n-old\n+new\n*** End Patch";
    assert_eq!(
        update("old\nkeep\r\n", first_only).unwrap(),
        "new\nkeep\r\n"
    );
    assert!(update("old\r\nkeep\n", first_only).is_err());
    assert!(update("old\nkeep\r\n", REPLACE_FIRST).is_err());
}

#[test]
fn add_file_content_still_uses_lf_even_when_patch_text_is_crlf() {
    let operations = crate::parse_patch(
        "*** Begin Patch\r\n*** Add File: new.txt\r\n+one\r\n+two\r\n*** End Patch",
    )
    .unwrap();
    let [PatchOperation::Add { lines, .. }] = operations.as_slice() else {
        panic!("fixture must contain one add");
    };
    assert_eq!(render_text(lines, true, "\n"), "one\ntwo\n");
}

#[test]
fn multiple_hunks_do_not_rematch_identical_lines_before_the_cursor() {
    let patch = concat!(
        "*** Begin Patch\n*** Update File: file.txt\n",
        "@@\n-repeat\n+repeat\n+новая строка 🦀\n",
        "@@\n-repeat\n+last\n tail\n*** End Patch",
    );
    assert_eq!(
        update("header\nrepeat\nmiddle\nrepeat\ntail", patch).unwrap(),
        "header\nrepeat\nновая строка 🦀\nmiddle\nlast\ntail"
    );
}
