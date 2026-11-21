use super::*;

#[test]
fn unnumbered_range_can_supply_an_exact_multiline_edit_preimage() {
    for source in [
        "before\nold\nkeep\nafter\n",
        "before\r\nold\r\nkeep\r\nafter\r\n",
        "before\nold\r\nkeep\nafter\r\n",
    ] {
        let directory = tempfile::tempdir().expect("workspace");
        let path = directory.path().join("sample.txt");
        std::fs::write(&path, source).expect("source fixture");

        let full = invoke(
            &ReadFileTool,
            directory.path(),
            json!({"path": "sample.txt"}),
        );
        assert_eq!(full["ok"], true);
        assert_eq!(full["output"], source);
        let batch = invoke(
            &ReadManyFilesTool,
            directory.path(),
            json!({"paths": ["sample.txt"]}),
        );
        assert_eq!(batch["ok"], true);
        assert_eq!(batch["output"], format!("== sample.txt ==\n{source}"));

        let ranged = invoke(
            &ReadFileTool,
            directory.path(),
            json!({"path": "sample.txt", "start_line": 2, "limit": 2}),
        );
        assert_eq!(ranged["ok"], true);
        let old = ranged["output"].as_str().unwrap();
        let expected = if source.contains("old\r\n") {
            "old\r\nkeep"
        } else {
            "old\nkeep"
        };
        assert_eq!(old, expected);

        if source.contains("old\r\n") {
            let normalized = invoke(
                &EditFileTool,
                directory.path(),
                json!({"path": "sample.txt", "old_string": "old\nkeep", "new_string": "new\nkeep"}),
            );
            assert_eq!(normalized["ok"], false);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        }
        let edited = invoke(
            &EditFileTool,
            directory.path(),
            json!({"path": "sample.txt", "old_string": old, "new_string": old.replacen("old", "new", 1)}),
        );
        assert_eq!(edited["ok"], true, "{edited}");
        assert_eq!(edited["metadata"]["replacements"], 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            source.replacen("old", "new", 1)
        );
    }
}
