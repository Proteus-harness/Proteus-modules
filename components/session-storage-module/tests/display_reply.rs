//! Exact SDK Value bytes and ownership for the installed reply adapter.
#[path = "../src/display_reply.rs"]
mod display_reply;

use proteus_contracts::{
    contracts::{
        SessionLocator, SessionStorageResponse, StorageConversation, StorageDisplaySnapshot,
        StorageRecordPage, StorageResumeState, StorageRevision, StorageSnapshot,
    },
    domain::{SessionUsageSnapshot, new_record_id, new_session_id},
    model_standard::{CanonicalMessage, ContentPart, MessageRole},
    storage::journal::{
        HistoryMutated, HistoryMutationKind, JOURNAL_SCHEMA_VERSION, JournalEntry, JournalRecord,
    },
};
use serde_json::{Map, Value};

fn compact(bytes: usize) -> String {
    let mut text = "é🦀\"\n\t\0".to_owned();
    text.extend(std::iter::repeat_n('x', bytes - text.len()));
    let text = text.into_boxed_str().into_string();
    assert_eq!(text.capacity(), bytes);
    text
}

#[derive(Clone, Copy)]
enum Reply {
    Display,
    Records,
    Conversation,
}

impl Reply {
    fn wrap(self, messages: Vec<CanonicalMessage>) -> SessionStorageResponse {
        let session_id = new_session_id();
        let revision = StorageRevision {
            session_seq: 1,
            history_revision: 1,
        };
        let snapshot = StorageSnapshot {
            session: SessionLocator {
                session_id,
                key: "fixture-only".into(),
            },
            snapshot_id: "fixture-snapshot".into(),
            revision,
            content_sha256: "0".repeat(64),
        };
        let conversation = || StorageConversation {
            messages,
            revision,
            model_context: vec![],
        };
        match self {
            Self::Display => SessionStorageResponse::DisplaySnapshot {
                projection: Some(StorageDisplaySnapshot {
                    snapshot,
                    state: StorageResumeState {
                        conversation: conversation(),
                        usage: SessionUsageSnapshot {
                            session_id,
                            revision: 0,
                            latest_turn_id: None,
                            requests: vec![],
                        },
                        last_history_turn: None,
                        interrupted_turns: vec![],
                    },
                }),
            },
            Self::Conversation => SessionStorageResponse::Conversation {
                conversation: conversation(),
            },
            Self::Records => SessionStorageResponse::Records {
                page: StorageRecordPage {
                    snapshot,
                    records: vec![JournalRecord {
                        schema_version: JOURNAL_SCHEMA_VERSION,
                        record_id: new_record_id(),
                        session_seq: 1,
                        timestamp_ms: 42,
                        session_id,
                        execution_id: None,
                        thread_id: None,
                        turn_id: None,
                        entry: JournalEntry::HistoryMutated(HistoryMutated {
                            previous_revision: 0,
                            new_revision: 1,
                            mutation: HistoryMutationKind::Append,
                            messages: conversation().messages,
                            compaction: None,
                            tool_results: vec![],
                        }),
                    }],
                    next_after_seq: None,
                },
            },
        }
    }

    fn messages(self, wire: &Value) -> &Value {
        match self {
            Self::Display => &wire["projection"]["state"]["conversation"]["messages"],
            Self::Records => &wire["page"]["records"][0]["entry"]["payload"]["messages"],
            Self::Conversation => &wire["conversation"]["messages"],
        }
    }
}

#[test]
fn reasoning_reply_moves_only_compact_strings_and_preserves_exact_wire() {
    for reply in [Reply::Display, Reply::Records, Reply::Conversation] {
        let text = compact(4096);
        let signature = compact(8192);
        let signature_only = compact(4096);
        let summary = compact(4096);
        let short = compact(4095);
        let mut spare = compact(8192);
        spare.reserve_exact(4096);
        assert!(spare.capacity() > spare.len());
        let plain = compact(48);
        let opaque = compact(4096);
        let leaves = [
            (0, "Reasoning", "text", text.as_ptr(), true),
            (0, "Reasoning", "signature", signature.as_ptr(), true),
            (1, "Reasoning", "signature", signature_only.as_ptr(), true),
            (2, "ReasoningSummary", "text", summary.as_ptr(), true),
            (3, "Reasoning", "text", short.as_ptr(), false),
            (4, "Reasoning", "signature", spare.as_ptr(), false),
            (5, "Text", "text", plain.as_ptr(), true),
        ];
        let mut opaque_data = Map::new();
        opaque_data.insert("type".into(), Value::String("compaction".into()));
        let opaque_pointer = opaque.as_ptr();
        opaque_data.insert("encrypted_content".into(), Value::String(opaque));
        let response = reply.wrap(vec![CanonicalMessage::new(
            MessageRole::Assistant,
            vec![
                ContentPart::Reasoning {
                    text,
                    signature: Some(signature),
                },
                ContentPart::Reasoning {
                    text: String::new(),
                    signature: Some(signature_only),
                },
                ContentPart::ReasoningSummary { text: summary },
                ContentPart::Reasoning {
                    text: short,
                    signature: None,
                },
                ContentPart::Reasoning {
                    text: String::new(),
                    signature: Some(spare),
                },
                ContentPart::Text { text: plain },
                ContentPart::Opaque {
                    format: "openai.responses.compaction.v1".into(),
                    data: Value::Object(opaque_data),
                },
                ContentPart::Reasoning {
                    text: String::new(),
                    signature: Some(String::new()),
                },
            ],
        )]);
        let expected = serde_json::to_value(&response).unwrap();
        let actual = display_reply::into_value(response).unwrap();
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );

        let parts = &reply.messages(&actual)[0]["parts"];
        for (index, variant, field, original, moved) in leaves {
            let string = parts[index]["payload"][variant][field].as_str().unwrap();
            assert_eq!(string.as_ptr() == original, moved, "{variant}.{field}");
        }
        let data = &parts[6]["payload"]["Opaque"]["data"];
        assert_eq!(
            data["encrypted_content"].as_str().unwrap().as_ptr(),
            opaque_pointer
        );
        assert!(parts[3]["payload"]["Reasoning"]["signature"].is_null());
        assert_eq!(parts[7]["payload"]["Reasoning"]["signature"], "");
    }
}
