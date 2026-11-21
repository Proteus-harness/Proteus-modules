use super::*;

fn turn() -> TurnScaffold {
    let current = CanonicalMessage::text(MessageRole::User, "current request");
    TurnScaffold {
        model_messages: Vec::new(),
        current_user_message_id: current.id,
        persistent_messages: vec![
            CanonicalMessage::text(MessageRole::Assistant, "old answer"),
            current,
        ],
        current_turn_messages_start: 2,
        context_chunks: 0,
        context_token_estimate: None,
        compactions: vec![HistoryCompactionReport::unchanged(2, Some("fits".into()))],
        history_replacement_len: None,
        pending_preparation: None,
        result_bindings: Default::default(),
    }
}

#[test]
fn ordinary_checkpoint_copies_only_messages_after_current_user() {
    let mut turn = turn();
    assert!(turn.history_update().unwrap().is_none());
    turn.persistent_messages.extend([
        CanonicalMessage::text(MessageRole::Assistant, "first\nответ 🦀"),
        CanonicalMessage::text(MessageRole::Assistant, "final"),
    ]);
    let history = turn.history_update().unwrap().unwrap();
    assert!(history.history_replacement.is_none());
    assert_eq!(history.new_messages, turn.persistent_messages[2..]);
    assert_eq!(history.compactions, turn.compactions);
    // The source history remains available for the next checkpoint.
    assert_eq!(turn.history_update().unwrap().unwrap(), history);
}

#[test]
fn replacement_checkpoint_preserves_both_sides_and_validates_bounds() {
    let mut turn = turn();
    turn.persistent_messages
        .push(CanonicalMessage::text(MessageRole::Assistant, "final"));
    for boundary in 0..=turn.persistent_messages.len() {
        turn.history_replacement_len = Some(boundary);
        let history = turn.history_update().unwrap().unwrap();
        let (prefix, suffix) = turn.persistent_messages.split_at(boundary);
        assert_eq!(history.history_replacement.as_deref(), Some(prefix));
        assert_eq!(history.new_messages, suffix);
        assert_eq!(history.compactions, turn.compactions);
    }
    turn.history_replacement_len = Some(turn.persistent_messages.len() + 1);
    assert_eq!(
        turn.history_update().unwrap_err().message,
        "workflow history replacement boundary is beyond persistent history"
    );
    turn.history_replacement_len = None;
    turn.persistent_messages.remove(1);
    assert_eq!(
        turn.history_update().unwrap_err().message,
        "workflow persistent history dropped the current user message"
    );
}
