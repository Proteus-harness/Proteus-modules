use super::*;

#[test]
fn actual_model_text_exposes_failed_pending_and_truncated_receipts() {
    let (_dir, store, context) = fixture(3, 2);
    let (session, record) = attached(&store, &context, true);
    let pending = read(&store, &context, json!({"handle":record.handle()})).unwrap();
    assert_eq!(pending["envelope"]["retry_read"], true);
    assert_eq!(pending["envelope"]["readable"], false);
    session.push_output(b"abcdef");
    session.mark_reader_ended(ReaderEnd::Eof);
    sealed(&record);
    let view = read(&store, &context, json!({"handle":record.handle()})).unwrap();
    assert_eq!(view["envelope"]["text"], "abc");
    assert_eq!(view["envelope"]["source_capture"]["complete_stream"], false);
    assert_eq!(view["envelope"]["source_capture"]["prefix_truncated"], true);
    assert_eq!(
        view["envelope"]["source_capture"],
        view["metadata"]["source_capture"]
    );
    let mut prepared = store.prepare(owner(&context), true).unwrap();
    let failed = store.find(2).unwrap();
    prepared.process_started();
    let _tap = prepared.attach();
    failed.abort(Failure::Io);
    wait_for(|| lock(&failed.state).terminal_at.is_some());
    let view = read(&store, &context, json!({"handle":failed.handle()})).unwrap();
    assert_eq!(
        view["envelope"]["source_capture"]["storage_state"],
        "failed"
    );
    assert_eq!(
        view["envelope"]["source_capture"]["storage_failure"],
        "storage_io"
    );
    assert_eq!(view["envelope"]["retry_read"], false);
    assert_eq!(view["envelope"]["retry_command"], false);
    assert_eq!(view["envelope"]["readable"], false);
}

#[test]
fn raw_binary_and_mid_codepoint_ranges_have_explicit_utf8_or_hex_behavior() {
    let (_dir, store, context) = fixture(100, 1);
    let (session, record) = attached(&store, &context, true);
    session.push_output(&[0xc3, 0xa9, 0xff]);
    session.mark_reader_ended(ReaderEnd::Eof);
    sealed(&record);
    let handle = record.handle();
    assert_eq!(
        read(&store, &context, json!({"handle":handle,"max_bytes":2})).unwrap()["envelope"]["text"],
        "é"
    );
    assert!(
        read(
            &store,
            &context,
            json!({"handle":handle,"offset":1,"max_bytes":1})
        )
        .unwrap_err()
        .message
        .contains("UTF-8")
    );
    let result = read(&store, &context, json!({"handle":handle,"format":"hex"})).unwrap();
    assert_eq!(result["envelope"]["bytes_hex"], "c3a9ff");
    assert_eq!(result["metadata"]["bytes_returned"], 3);
    assert_eq!(result["metadata"]["range_eof"], true);
}

#[test]
fn owner_workspace_binding_and_restart_handle_checks_are_fresh() {
    let (_dir, store, context) = fixture(100, 1);
    let (session, record) = attached(&store, &context, true);
    session.push_output(b"private");
    session.mark_reader_ended(ReaderEnd::Eof);
    sealed(&record);
    let handle = record.handle();
    let other_owner = super::context(&context.cwd);
    assert!(
        read(&store, &other_owner, json!({"handle":handle}))
            .unwrap_err()
            .message
            .contains("unknown or unavailable")
    );
    let elsewhere = tempfile::tempdir().unwrap();
    let mut other_workspace = context.clone();
    other_workspace.cwd = elsewhere.path().to_path_buf();
    assert!(read(&store, &other_workspace, json!({"handle":handle})).is_err());
    let restarted = CaptureStore::new(CaptureConfig {
        directory: context.cwd.clone(),
        cap: 100,
        slots: 1,
        retention: Duration::from_secs(300),
    });
    assert!(read(&restarted, &context, json!({"handle":handle})).is_err());
    assert!(restarted.metadata(session.capture.as_ref()).is_none());
    assert_eq!(
        read(&store, &context, json!({"handle":handle})).unwrap()["envelope"]["text"],
        "private"
    );
}

#[test]
fn byte_ranges_are_bounded_and_sequential_reads_have_independent_positions() {
    let (_dir, store, context) = fixture(100, 1);
    let (session, record) = attached(&store, &context, true);
    session.push_output(b"0123456789");
    session.mark_reader_ended(ReaderEnd::Eof);
    sealed(&record);
    let handle = record.handle();
    for (offset, expected) in [(6, "67"), (0, "01"), (8, "89")] {
        assert_eq!(
            read(
                &store,
                &context,
                json!({"handle":handle,"offset":offset,"max_bytes":2})
            )
            .unwrap()["envelope"]["text"],
            expected
        );
    }
    assert!(read(&store, &context, json!({"handle":handle,"offset":11})).is_err());
    assert!(read(&store, &context, json!({"handle":handle,"offset":u64::MAX})).is_err());
    assert!(read(&store, &context, json!({"handle":handle,"max_bytes":65537})).is_err());
    let at_end = read(&store, &context, json!({"handle":handle,"offset":10})).unwrap();
    assert_eq!(at_end["envelope"]["text"], "");
    assert_eq!(at_end["metadata"]["range_eof"], true);
}

#[test]
fn busy_file_cursor_and_read_slot_cap_do_not_wait_or_allocate_a_range() {
    let (_dir, store, context) = fixture(100, 1);
    let (session, record) = attached(&store, &context, true);
    session.mark_reader_ended(ReaderEnd::Eof);
    sealed(&record);
    let file_guard = record.file.lock().unwrap();
    assert!(
        read(&store, &context, json!({"handle":record.handle()}))
            .unwrap_err()
            .message
            .contains("busy")
    );
    assert_eq!(store.quota.readers.load(Ordering::Acquire), 0);
    drop(file_guard);
    store.quota.readers.store(MAX_READERS, Ordering::Release);
    assert!(
        read(&store, &context, json!({"handle":record.handle()}))
            .unwrap_err()
            .message
            .contains("busy")
    );
    store.quota.readers.store(0, Ordering::Release);
    assert!(read(&store, &context, json!({"handle":record.handle()})).is_ok());
}

#[test]
fn cancelled_range_read_is_not_a_command_retry_or_storage_mutation() {
    let (_dir, store, context) = fixture(100, 1);
    let (session, record) = attached(&store, &context, true);
    session.push_output(b"bytes");
    session.mark_reader_ended(ReaderEnd::Eof);
    sealed(&record);
    let tool = ReadCaptureTool(store.clone());
    let call =
        json!({"id":"cancel_read","name":"read_exec_capture","args":{"handle":record.handle()}});
    let error = tool
        .invoke_json(
            call.to_string(),
            serde_json::to_string(&context).unwrap(),
            &mut Host { cancelled: true },
        )
        .unwrap_err();
    assert!(error.message.contains("canceled"));
    assert_eq!(
        read(&store, &context, json!({"handle":record.handle()})).unwrap()["envelope"]["text"],
        "bytes"
    );
    assert_eq!(store.quota.readers.load(Ordering::Acquire), 0);
}
