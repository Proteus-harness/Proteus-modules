use super::*;
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, FileTimes},
    time::{Duration, UNIX_EPOCH},
};

fn legacy_classify(name: &OsStr) -> bool {
    let label = name.to_string_lossy();
    label.starts_with(".storage-cache-") && label.ends_with(".json")
}

#[test]
fn cache_name_checks_leave_ascii_boundary_behavior_exact() {
    let mut names: Vec<OsString> = [
        ".storage-cache-.json",
        ".storage-cache-normal.json",
        ".storage-cache-résumé-архив.json",
        ".storage-cache-old.JSON",
        ".storage-cache-write.tmp",
        "history.json",
        "other-.storage-cache-page.json",
        ".storage-cache-without-extension",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        // An older filename from a Latin locale, held in its original bytes.
        names.push(OsString::from_vec(b".storage-cache-caf\xe9.json".to_vec()));
        names.push(OsString::from_vec(b"caf\xe9.json".to_vec()));
    }
    for name in names {
        assert_eq!(is_cache_name(&name), legacy_classify(&name), "{name:?}");
    }
}

fn size_and_age(dir: &Path, name: &OsStr, bytes: u64, age: u64) {
    let file = File::create(dir.join(name)).unwrap();
    file.set_len(bytes).unwrap();
    file.set_times(
        FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_600_000_000 + age)),
    )
    .unwrap();
}

#[test]
fn cache_name_filter_preserves_quota_keep_and_normal_missing_directory_error() {
    let mib = 1024 * 1024;
    let keep = OsStr::new(".storage-cache-protected.json");
    let oldest = OsStr::new(".storage-cache-oldest.json");
    let newest = OsStr::new(".storage-cache-newest.json");
    let temp = OsStr::new(".storage-cache-unpublished.tmp");
    for (other, remove_oldest) in [(56 * mib, false), (64 * mib, true)] {
        let dir = tempfile::tempdir().unwrap();
        size_and_age(dir.path(), keep, 16 * mib, 100);
        size_and_age(dir.path(), oldest, other, 200);
        size_and_age(dir.path(), newest, other, 300);
        size_and_age(dir.path(), temp, 24 * mib, 25);
        let ambiguous = dir.path().join(".storage-cache-directory.json");
        fs::create_dir(&ambiguous).unwrap();

        evict(dir.path(), &dir.path().join(keep)).unwrap();
        assert!(dir.path().join(keep).is_file());
        assert!(dir.path().join(newest).is_file());
        assert_eq!(dir.path().join(oldest).is_file(), !remove_oldest);
        assert!(dir.path().join(temp).is_file());
        assert!(ambiguous.is_dir());
    }

    let parent = tempfile::tempdir().unwrap();
    let gone = parent.path().join("normal-empty-derived-dir");
    fs::create_dir(&gone).unwrap();
    fs::remove_dir(&gone).unwrap();
    let err = evict(&gone, &gone.join(keep)).unwrap_err();
    assert_eq!(
        err.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::NotFound
    );
}
