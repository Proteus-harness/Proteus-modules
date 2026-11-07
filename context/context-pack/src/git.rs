//! Read-only Git collection with bounded retained output and a local deadline.
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) fn output(cwd: &Path, args: &[&str], limit: usize) -> anyhow::Result<Option<String>> {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    proteus_contracts::git_support::protect_readonly_git(&mut command)?;
    command
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.external=",
            "-c",
            "diff.trustExitCode=false",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .args(args);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut stdout = child.stdout.take().unwrap();
    let exceeded = Arc::new(AtomicBool::new(false));
    let signal = exceeded.clone();
    let reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let count = stdout.read(&mut chunk)?;
            if count == 0 {
                break;
            }
            let take = count.min(limit.saturating_sub(bytes.len()));
            bytes.extend_from_slice(&chunk[..take]);
            if take < count {
                signal.store(true, Ordering::Relaxed);
                break;
            }
        }
        Ok(bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut interrupted = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if exceeded.load(Ordering::Relaxed) || Instant::now() >= deadline {
            interrupted = true;
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let bytes = reader
        .join()
        .map_err(|_| anyhow::anyhow!("Git output reader panicked"))??;
    if !status.success() && !interrupted && !exceeded.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let mut text = String::from_utf8_lossy(&bytes).trim().to_owned();
    if interrupted || exceeded.load(Ordering::Relaxed) {
        text.push_str("\n[Git context truncated: output budget or deadline reached]");
    }
    Ok((!text.is_empty()).then_some(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn git_diff_is_bounded_and_filters_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(dir.path())
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&["init", "--quiet"]);
        std::fs::write(dir.path().join("file"), "old\n").unwrap();
        run(&["add", "file"]);
        std::fs::write(dir.path().join("file"), "x".repeat(1000000)).unwrap();
        let text = output(
            dir.path(),
            &["diff", "--no-ext-diff", "--no-textconv"],
            1024,
        )
        .unwrap()
        .unwrap();
        assert!(text.len() < 1200);
        assert!(text.contains("truncated"));
        std::fs::write(dir.path().join(".gitattributes"), "file filter=marker\n").unwrap();
        run(&["config", "filter.marker.clean", "touch marker; cat"]);
        assert!(output(dir.path(), &["status", "--short"], 1024).is_err());
        assert!(!dir.path().join("marker").exists());
    }
}
