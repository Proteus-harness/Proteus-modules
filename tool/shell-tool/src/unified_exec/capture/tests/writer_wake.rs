use super::*;

struct Worker {
    record: Arc<Record>,
    waiting: mpsc::Receiver<(u64, u64)>,
    done: mpsc::Receiver<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    fn start(record: Arc<Record>, receiver: mpsc::Receiver<Box<[u8]>>) -> Self {
        let file = tempfile::tempfile().unwrap();
        let (wait_tx, waiting) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let owned = record.clone();
        let thread = std::thread::spawn(move || {
            writer::run_with_wait_hook(owned, receiver, file, |confirmed, queued| {
                wait_tx.send((confirmed, queued)).unwrap();
            });
            done_tx.send(()).unwrap();
        });
        Self {
            record,
            waiting,
            done,
            thread: Some(thread),
        }
    }

    fn waiting(&self, expected: (u64, u64)) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let observed = self
                .waiting
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if observed == expected {
                return;
            }
        }
    }

    fn finished(&mut self) {
        self.done.recv_timeout(Duration::from_secs(5)).unwrap();
        self.thread.take().unwrap().join().unwrap();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            // Release an idle worker even when an assertion unwinds. No fixture
            // writer deliberately outlives this test or retains its tempfile.
            self.record.abort(Failure::BindingDropped);
            self.record.changed.notify_all();
            let _ = thread.join();
        }
    }
}

fn record(
    store: &Arc<CaptureStore>,
    context: &ToolModuleInvocationContext,
) -> (Arc<Record>, mpsc::Receiver<Box<[u8]>>) {
    let (sender, receiver) = mpsc::sync_channel(QUEUE_CHUNKS);
    (
        Arc::new(Record {
            generation: store.generation,
            id: 1,
            owner: owner(context),
            cap: store.config.cap,
            state: Mutex::new(State {
                readers: ReaderCompletions::new(true),
                expected: 1,
                observed: 0,
                saturated: false,
                queued: 0,
                confirmed: 0,
                unconfirmed: 0,
                storage: StorageState::Capturing,
                failure: None,
                terminal_at: None,
                process_started: true,
                attached: true,
            }),
            changed: Condvar::new(),
            sender,
            file: Mutex::new(None),
            _reservation: store.quota.reserve().unwrap(),
        }),
        receiver,
    )
}

#[test]
fn enqueue_at_wait_then_eof_after_final_dequeue_seals_exact_bytes() {
    let (_dir, store, context) = fixture(16, 1);
    let (record, receiver) = record(&store, &context);
    let mut worker = Worker::start(record.clone(), receiver);
    worker.waiting((0, 0));
    record.observe(b"last");
    worker.waiting((4, 4)); // discard stale/spurious initial-empty notifications
    assert_eq!(lock(&record.state).confirmed, 4);
    record.ended(ReaderEnd::Eof);
    worker.finished();
    assert_eq!(record.receipt()["complete_stream"], true);
    assert_eq!(record.receipt()["readable_bytes"], 4);
    use std::io::{Read, Seek, SeekFrom};
    let mut file = lock(&record.file);
    let file = file.as_mut().unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"last");
}

#[test]
fn empty_eof_at_wait_seals_and_spurious_notification_does_not() {
    let (_dir, store, context) = fixture(16, 1);
    let (record, receiver) = record(&store, &context);
    let mut worker = Worker::start(record.clone(), receiver);
    worker.waiting((0, 0));
    {
        let state = lock(&record.state);
        // The hook also holds state. Drain stale spurious-wake markers while
        // no new marker can be published, then request a fresh recheck.
        while worker.waiting.try_recv().is_ok() {}
        record.changed.notify_one();
        assert_eq!(state.storage, StorageState::Capturing);
    }
    worker.waiting((0, 0)); // fresh recheck still cannot seal with readers open
    assert_eq!(lock(&record.state).storage, StorageState::Capturing);
    record.ended(ReaderEnd::Eof);
    worker.finished();
    assert_eq!(record.receipt()["complete_stream"], true);
    assert_eq!(record.receipt()["readable_bytes"], 0);
}

#[test]
fn queued_final_chunk_and_eof_before_writer_starts_are_not_lost() {
    let (_dir, store, context) = fixture(16, 1);
    let (record, receiver) = record(&store, &context);
    record.observe(b"last");
    record.ended(ReaderEnd::Eof);
    let mut worker = Worker::start(record.clone(), receiver);
    worker.finished();
    assert_eq!(record.receipt()["complete_stream"], true);
    assert_eq!(record.receipt()["confirmed_bytes"], 4);
}

#[test]
fn abort_at_wait_closes_file_without_reader_eof() {
    let (_dir, store, context) = fixture(16, 1);
    let (record, receiver) = record(&store, &context);
    let mut worker = Worker::start(record.clone(), receiver);
    worker.waiting((0, 0));
    record.abort(Failure::BindingDropped);
    worker.finished();
    assert_eq!(record.receipt()["storage_failure"], "binding_dropped");
    assert!(lock(&record.file).is_none());
    assert!(lock(&record.state).terminal_at.is_some());
}

#[test]
fn protocol_failure_at_wait_wakes_writer_without_eof() {
    let (_dir, store, context) = fixture(16, 1);
    let (record, receiver) = record(&store, &context);
    let mut worker = Worker::start(record.clone(), receiver);
    worker.waiting((0, 0));
    record.observe(&[0; CHUNK_BYTES + 1]);
    worker.finished();
    assert_eq!(record.receipt()["storage_failure"], "capture_protocol");
    assert!(lock(&record.state).terminal_at.is_some());
}

#[test]
fn full_data_queue_cannot_hide_failure_or_publish_a_seal() {
    let (_dir, store, context) = fixture(1024 * 1024, 1);
    let (record, receiver) = record(&store, &context);
    for _ in 0..=QUEUE_CHUNKS {
        record.observe(&[0; CHUNK_BYTES]);
    }
    record.ended(ReaderEnd::Eof);
    let mut worker = Worker::start(record.clone(), receiver);
    worker.finished();
    assert_eq!(record.receipt()["storage_failure"], "queue_full");
    assert_eq!(record.receipt()["confirmed_bytes"], 0);
    assert_eq!(record.receipt()["complete_stream"], false);
    assert!(record.receipt()["readable_bytes"].is_null());
}
