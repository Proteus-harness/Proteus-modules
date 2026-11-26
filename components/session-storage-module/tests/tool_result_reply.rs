//! Inert DTO parity: never invokes a tool, provider, or subprocess.
#[path = "../src/display_reply.rs"]
mod display_reply;

use proteus_contracts::{
    contracts::{
        SessionLocator, SessionStorageResponse, StorageRecordPage, StorageRevision, StorageSnapshot,
    },
    domain::{ToolContent, ToolResult, new_record_id, new_session_id},
    model_standard::{CanonicalMessage, ContentPart, MessageRole},
    storage::journal::{
        HistoryMutated, HistoryMutationKind, JOURNAL_SCHEMA_VERSION, JournalEntry, JournalRecord,
        ToolResultRecorded,
    },
};
use serde_json::json;

fn string(bytes: usize) -> String {
    let mut text = "é🦀\"\n\t\0".to_owned();
    text.extend(std::iter::repeat_n('x', bytes - text.len()));
    let text = text.into_boxed_str().into_string();
    assert_eq!(text.capacity(), bytes);
    text
}

fn tool(id: &str, output: String, error: Option<String>) -> ToolResult {
    ToolResult::new(
        id.to_owned(),
        error.is_none(),
        output,
        vec![ToolContent::Text {
            text: "inert structured content".into(),
        }],
        error,
        json!({ "z": ["unchanged", 1], "a": {"fixture": true} }),
    )
}

#[test]
fn transfers_only_owned_compact_tool_leaves_on_existing_reply_routes() {
    let compact_output = string(4096);
    let compact_error = string(8192);
    let mut spare_output = string(8192);
    spare_output.reserve_exact(4096);
    assert!(spare_output.capacity() > spare_output.len());
    let short = string(4095);
    let history_output = string(8192);
    let history_error = string(4096);
    let addresses = [
        compact_output.as_ptr(),
        compact_error.as_ptr(),
        spare_output.as_ptr(),
        short.as_ptr(),
        history_output.as_ptr(),
        history_error.as_ptr(),
    ];
    let entries = [
        JournalEntry::ToolResultRecorded(ToolResultRecorded {
            result: tool("sample-one", compact_output, None),
        }),
        JournalEntry::ToolEffectRecorded(ToolResultRecorded {
            result: tool("sample-two", spare_output, Some(compact_error)),
        }),
        JournalEntry::HistoryMutated(HistoryMutated {
            previous_revision: 0,
            new_revision: 1,
            mutation: HistoryMutationKind::Append,
            messages: vec![
                CanonicalMessage::new(
                    MessageRole::Tool,
                    vec![ContentPart::ToolResult {
                        result: tool("sample-three", short, Some(String::new())),
                    }],
                ),
                CanonicalMessage::new(
                    MessageRole::Tool,
                    vec![ContentPart::ToolResult {
                        result: tool("sample-four", history_output, Some(history_error)),
                    }],
                ),
            ],
            compaction: None,
            tool_results: vec![],
        }),
    ];
    let session_id = new_session_id();
    let response = SessionStorageResponse::Records {
        page: StorageRecordPage {
            snapshot: StorageSnapshot {
                session: SessionLocator {
                    session_id,
                    key: "fixture".into(),
                },
                snapshot_id: "fixture-handle".into(),
                revision: StorageRevision {
                    session_seq: 3,
                    history_revision: 1,
                },
                content_sha256: "0".repeat(64),
            },
            records: entries
                .into_iter()
                .enumerate()
                .map(|(i, entry)| JournalRecord {
                    schema_version: JOURNAL_SCHEMA_VERSION,
                    record_id: new_record_id(),
                    session_seq: i as u64 + 1,
                    timestamp_ms: 42,
                    session_id,
                    execution_id: None,
                    thread_id: None,
                    turn_id: None,
                    entry,
                })
                .collect(),
            next_after_seq: None,
        },
    };
    let expected = serde_json::to_value(&response).unwrap();
    let actual = display_reply::into_value(response).unwrap();
    assert_eq!(
        serde_json::to_vec(&actual).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );

    let records = &actual["page"]["records"];
    let first = &records[0]["entry"]["payload"]["result"];
    let second = &records[1]["entry"]["payload"]["result"];
    let messages = &records[2]["entry"]["payload"]["messages"];
    let short_result = &messages[0]["parts"][0]["payload"]["ToolResult"]["result"];
    let long_result = &messages[1]["parts"][0]["payload"]["ToolResult"]["result"];
    assert!(first["error"].is_null());
    assert_eq!(short_result["error"], "");
    let leaves = [
        (&first["output"], true),
        (&second["error"], true),
        (&second["output"], false),
        (&short_result["output"], false),
        (&long_result["output"], true),
        (&long_result["error"], true),
    ];
    for ((leaf, moved), original) in leaves.into_iter().zip(addresses) {
        assert_eq!(leaf.as_str().unwrap().as_ptr() == original, moved);
    }
}
