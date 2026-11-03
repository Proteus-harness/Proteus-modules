//! Identity-aware cleanup for real Linux process regressions.

pub(crate) fn state(pid: u32) -> Option<(String, bool)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    recorded_state(&stat)
}

pub(crate) fn recorded_state(stat: &str) -> Option<(String, bool)> {
    let fields = stat
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    Some((
        fields.get(19)?.to_string(),
        !fields[0].starts_with(['Z', 'X']),
    ))
}

pub(crate) struct Cleanup(pub(crate) Vec<(u32, String)>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        // Never signal a PID recycled since this fixture recorded its identity.
        for (pid, start_ticks) in &self.0 {
            if state(*pid).is_some_and(|(current, running)| running && &current == start_ticks) {
                unsafe {
                    let _ = libc::kill(*pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
    }
}
