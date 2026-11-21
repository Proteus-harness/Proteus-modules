use super::*;

#[test]
fn edit_file_counts_original_nonoverlapping_matches_and_utf8_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edit.txt");
    for (input, needle, replacement, expected, n) in [
        ("🙂яяяяя\r\n🙂", "яя", "яя⭐", "🙂яя⭐яя⭐я\r\n🙂", 2),
        ("aaaaa", "aa", "", "a", 2),
        ("終a終", "終", "end", "endaend", 2),
        ("aaa", "aa", "b", "ba", 1),
    ] {
        std::fs::write(&path, input).unwrap();
        let result = invoke(
            &EditFileTool,
            dir.path(),
            json!({
                "path": "edit.txt", "old_string": needle, "new_string": replacement,
                "replace_all": n != 1
            }),
        );
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["metadata"]["replacements"], n);
        assert_eq!(result["metadata"]["bytes_written"], expected.len());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
    }
}

#[test]
fn edit_file_rejects_ambiguous_edits_without_replacing_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("edit.txt");
    let original = "aaaaaaa";
    std::fs::write(&path, original).unwrap();
    let result = invoke(
        &EditFileTool,
        dir.path(),
        json!({
            "path":"edit.txt", "old_string":"aa", "new_string":"z"
        }),
    );
    assert_eq!(result["ok"], false);
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("matches 3 locations")
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
