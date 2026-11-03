//! Keep pipe-process signalling authority until its exit drain has finished.

use std::{
    io,
    process::{Child, ExitStatus},
    sync::Mutex,
};

use super::session::lock;

struct State {
    child: Child,
    can_signal: bool,
    finished: bool,
}

pub(super) struct PipeLifetime(Mutex<State>);

impl PipeLifetime {
    pub(super) fn new(child: Child) -> Self {
        Self(Mutex::new(State {
            child,
            can_signal: true,
            finished: false,
        }))
    }

    pub(super) fn observe_exit(&self) -> io::Result<Option<ExitStatus>> {
        let mut state = lock(&self.0);
        #[cfg(unix)]
        let result = crate::child_status::observe_exit(state.child.id());
        #[cfg(not(unix))]
        let result = state.child.try_wait();
        if result.is_err() {
            // Lost ownership (for example ECHILD) revokes numeric signal targets.
            state.can_signal = false;
        }
        #[cfg(not(unix))]
        if !matches!(result, Ok(None)) {
            state.can_signal = false;
            state.finished = true;
        }
        result
    }

    #[cfg(unix)]
    pub(super) fn interrupt(&self) -> io::Result<()> {
        let state = lock(&self.0);
        if state.can_signal {
            Self::signal(&state.child, libc::SIGINT)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn signal(child: &Child, signal: i32) -> io::Result<()> {
        let pgid = i32::try_from(child.id()).map_err(io::Error::other)?;
        // SAFETY: spawn created a dedicated process group. The lifetime lock
        // excludes reap, and WNOWAIT keeps even an exited leader's PID reserved.
        if unsafe { libc::kill(-pgid, signal) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    fn kill_locked(state: &mut State) {
        if !state.can_signal {
            return;
        }
        state.can_signal = false;
        #[cfg(unix)]
        let _ = Self::signal(&state.child, libc::SIGKILL);
        #[cfg(not(unix))]
        let _ = state.child.kill();
    }

    pub(super) fn kill(&self) {
        Self::kill_locked(&mut lock(&self.0));
    }

    pub(super) fn finish(&self) {
        let mut state = lock(&self.0);
        if state.finished {
            return;
        }
        // Drain is over. Stop remaining group members before releasing its
        // identity, then make later cancellation/eviction a no-op before reap.
        Self::kill_locked(&mut state);
        state.finished = true;
        let _ = state.child.wait();
    }
}
