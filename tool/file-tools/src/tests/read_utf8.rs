use super::*;

#[test]
fn file_read_modes_reject_invalid_utf8_at_observed_eof() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("text.txt");
    for bytes in [
        &b"valid\xc3"[..],
        &b"valid\xf0\x9f\x98"[..],
        &b"valid\xff"[..],
    ] {
        std::fs::write(&path, bytes).unwrap();
        let results = [
            invoke(&ReadFileTool, dir.path(), json!({"path":"text.txt"})),
            invoke(
                &ReadFileTool,
                dir.path(),
                json!({"path":"text.txt", "start_line":1}),
            ),
            invoke(
                &ReadManyFilesTool,
                dir.path(),
                json!({"paths":["text.txt"]}),
            ),
            invoke(
                &ReadManyFilesTool,
                dir.path(),
                json!({"paths":["text.txt"], "line_numbers":true}),
            ),
        ];
        for result in results {
            assert_eq!(result["ok"], false);
            assert!(
                result["error"]
                    .as_str()
                    .unwrap()
                    .to_ascii_lowercase()
                    .contains("utf-8")
            );
        }
    }
}

#[test]
fn batch_read_reports_an_intentional_scalar_cut_as_truncated() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("text.txt"), "valid😀tail").unwrap();
    let result = invoke(
        &ReadManyFilesTool,
        dir.path(),
        json!({
            "paths":["text.txt"], "max_bytes_per_file":7, "max_bytes_total":100
        }),
    );
    assert_eq!(result["ok"], true);
    assert_eq!(result["output"], "== text.txt ==\nvalid");
    assert_eq!(result["metadata"]["files"][0]["original_bytes"], 13);
    assert_eq!(result["metadata"]["files"][0]["returned_bytes"], 5);
    assert_eq!(result["metadata"]["files"][0]["truncated"], true);
    assert_eq!(result["metadata"]["truncated"], true);
}

#[test]
fn numbered_rendering_truncates_valid_string_at_a_scalar_boundary() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("text.txt"), "я").unwrap();
    let budget = "== text.txt ==\n".len() + 3;
    let result = invoke(
        &ReadManyFilesTool,
        dir.path(),
        json!({
            "paths":["text.txt"], "line_numbers":true, "max_bytes_total":budget
        }),
    );
    assert_eq!(result["ok"], true);
    assert_eq!(result["output"], "== text.txt ==\n1\t");
    assert!(result["output"].as_str().unwrap().len() <= budget);
    assert_eq!(result["metadata"]["files"][0]["returned_bytes"], 2);
    assert_eq!(result["metadata"]["files"][0]["truncated"], true);
}
