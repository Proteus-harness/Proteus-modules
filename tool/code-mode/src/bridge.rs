//! Bounded host calls run outside the JS thread. No worker outlives invocation.
use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use proteus_contracts::{domain::ToolCall, process_module::ToolModuleHost};
use serde::Deserialize;

use crate::config::{Limits, MAX_TRANSFER_BYTES};

pub(crate) const TIMEOUT: u8 = 1;
pub(crate) const CANCELLED: u8 = 2;
pub(crate) const FAILED: u8 = 3;
const FINISHED: u8 = 4;

pub(crate) struct Request {
    pub sequence: usize,
    pub call: ToolCall,
}

pub(crate) struct Completion {
    pub sequence: usize,
    pub result: Result<String, String>,
}

#[derive(Default)]
pub(crate) struct Stats {
    pub attempted: AtomicUsize,
    pub completed: AtomicUsize,
    pub failed: AtomicUsize,
    pub host_errors: AtomicUsize,
    pub result_bytes: AtomicUsize,
}

pub(crate) struct Channels {
    pub requests: SyncSender<Request>,
    pub completions: Receiver<Completion>,
    pub stop: Arc<AtomicU8>,
}

struct Finish<'a> {
    stop: &'a AtomicU8,
    done: &'a (Mutex<bool>, Condvar),
}

impl Drop for Finish<'_> {
    fn drop(&mut self) {
        let _ = self
            .stop
            .compare_exchange(0, FINISHED, Ordering::AcqRel, Ordering::Acquire);
        *self.done.0.lock().expect("watcher state") = true;
        self.done.1.notify_one();
    }
}

/// Scope teardown stops admission, discards queued requests and joins live host
/// callbacks. A script budget cannot revoke a host effect already in progress;
/// such callbacks retain the enclosing Core invocation's cancellation/deadline.
pub(crate) fn with_workers<T>(
    host: &dyn ToolModuleHost,
    limits: &Limits,
    deadline: Instant,
    run: impl FnOnce(Channels) -> T,
) -> (T, Arc<Stats>) {
    let stop = Arc::new(AtomicU8::new(0));
    let stats = Arc::new(Stats::default());
    let done = (Mutex::new(false), Condvar::new());
    let result = thread::scope(|scope| {
        let (requests_tx, requests_rx) = mpsc::sync_channel::<Request>(limits.max_calls);
        let requests_rx = Arc::new(Mutex::new(requests_rx));
        let (completions_tx, completions_rx) = mpsc::sync_channel(limits.max_concurrency);
        for _ in 0..limits.max_concurrency {
            let receiver = requests_rx.clone();
            let sender = completions_tx.clone();
            let stats = stats.clone();
            let stop = stop.clone();
            scope.spawn(move || {
                loop {
                    let request = match receiver.lock().expect("request receiver").recv() {
                        Ok(request) => request,
                        Err(_) => return,
                    };
                    if stop.load(Ordering::Acquire) != 0 {
                        continue;
                    }
                    stats.attempted.fetch_add(1, Ordering::Relaxed);
                    let result = execute(host, &request.call, &stats);
                    stats.completed.fetch_add(1, Ordering::Release);
                    if result.is_err() {
                        stats.host_errors.fetch_add(1, Ordering::Relaxed);
                    }
                    if sender
                        .send(Completion {
                            sequence: request.sequence,
                            result,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        drop(completions_tx);
        let done_ref = &done;
        let watcher_stop = stop.clone();
        scope.spawn(move || {
            let (mutex, changed) = done_ref;
            let mut finished = mutex.lock().expect("watcher state");
            while !*finished {
                if Instant::now() >= deadline {
                    let _ = watcher_stop.compare_exchange(
                        0,
                        TIMEOUT,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                } else if host.is_cancelled().unwrap_or(true) {
                    let _ = watcher_stop.compare_exchange(
                        0,
                        CANCELLED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                }
                finished = changed
                    .wait_timeout(finished, Duration::from_millis(5))
                    .expect("watcher wait")
                    .0;
            }
        });
        let _finish = Finish {
            stop: &stop,
            done: &done,
        };
        run(Channels {
            requests: requests_tx,
            completions: completions_rx,
            stop: stop.clone(),
        })
    });
    (result, stats)
}

fn execute(host: &dyn ToolModuleHost, call: &ToolCall, stats: &Stats) -> Result<String, String> {
    let request = serde_json::to_string(call).map_err(|error| error.to_string())?;
    let response = host
        .execute_tool_json(request)
        .map_err(|error| error.to_string())?;
    let previous = stats
        .result_bytes
        .fetch_add(response.len(), Ordering::Relaxed);
    if response.len() > MAX_TRANSFER_BYTES
        || previous.saturating_add(response.len()) > MAX_TRANSFER_BYTES
    {
        return Err(format!(
            "code_mode nested results exceed {MAX_TRANSFER_BYTES} total bytes"
        ));
    }
    #[derive(Deserialize)]
    struct Status<'a> {
        #[serde(borrow)]
        call_id: &'a str,
        ok: bool,
    }
    let status: Status<'_> = serde_json::from_str(&response)
        .map_err(|error| format!("invalid nested ToolResult: {error}"))?;
    if status.call_id != call.id.as_str() {
        return Err("nested ToolResult call_id does not match request".to_owned());
    }
    if !status.ok {
        stats.failed.fetch_add(1, Ordering::Relaxed);
    }
    Ok(response)
}
