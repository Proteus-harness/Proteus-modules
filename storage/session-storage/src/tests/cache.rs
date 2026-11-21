use super::*;

#[test]
fn cache_file_bytes_and_failed_replacement_are_stable() {
    for kind in backends() {
        let mut f = Fixture::new(kind);
        f.append(vec![text("cache source")]);
        let mut source = f.snapshot();
        // A long, escaped handle still describes the same prefix.
        source.snapshot_id = "\n".repeat(530_000);
        source.snapshot_id.push('x');
        let value = serde_json::json!({"parts":["\u{0000}","юникод", {"text":"Z".repeat(24_000)}]});
        let key = "display.pages";
        let expected = format!(
            r#"{{"source":{},"value":{}}}"#,
            serde_json::to_string(&source).unwrap(),
            serde_json::to_string(&value).unwrap()
        );
        let request = SessionStorageRequest::WriteCache {
            session: source.session.clone(),
            key: key.into(),
            source,
            value: value.clone(),
        };
        let path = f.descriptor.local_path.as_ref().unwrap().join(format!(
            ".storage-cache-{}.json",
            crate::io::sha256_hex(key.as_bytes())
        ));
        assert_eq!(
            f.service.execute(request.clone()).unwrap(),
            SessionStorageResponse::CacheWritten {
                etag: Some(storage_value_etag(&value).unwrap())
            }
        );
        assert_eq!(std::fs::read(&path).unwrap(), expected.as_bytes());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap(); // force rename failure
        assert_eq!(
            f.service.execute(request).unwrap(),
            SessionStorageResponse::CacheWritten { etag: None }
        );
        assert!(path.is_dir());
        assert!(
            !std::fs::read_dir(path.parent().unwrap()).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
    }
}
