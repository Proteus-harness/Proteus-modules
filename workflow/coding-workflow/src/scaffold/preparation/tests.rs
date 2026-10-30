use super::merge_request_context;
use proteus_contracts::{
    domain::ContextChunk,
    model_standard::{CanonicalMessage, ContentPart, MessageRole, PartScope},
};
use serde_json::json;

fn message(text: &str) -> CanonicalMessage {
    CanonicalMessage::text(MessageRole::User, text)
}

fn context(text: &str) -> CanonicalMessage {
    CanonicalMessage::new(
        MessageRole::User,
        vec![ContentPart::Context {
            chunk: ContextChunk::new("fixture", text),
        }],
    )
}

#[test]
fn context_follows_retained_anchors_and_preserves_each_groups_order() {
    let a = message("a");
    let b = message("old b");
    let removed = message("removed");
    let steering = message("new steering");
    let before_a = context("before a");
    let before_removed = context("before removed");
    let before_b = context("before b");
    let before_b_second = context("before b second");
    let tail = context("tail");
    let tail_second = context("tail second");
    let model = vec![
        before_a.clone(),
        a.clone(),
        before_removed.clone(),
        removed,
        before_b.clone(),
        before_b_second.clone(),
        b.clone(),
        tail.clone(),
        tail_second.clone(),
    ];
    let mut accepted_b = b;
    accepted_b.parts[0].payload = ContentPart::Text {
        text: "accepted b".into(),
    };
    accepted_b.metadata = json!({"accepted": true});
    let canonical = vec![steering.clone(), accepted_b.clone(), a.clone()];
    assert_eq!(
        merge_request_context(&model, &canonical),
        vec![
            steering,
            before_removed,
            before_b,
            before_b_second,
            accepted_b,
            before_a,
            a,
            tail,
            tail_second,
        ]
    );
    assert_eq!(model[6].display_text(), "old b");
    assert_eq!(canonical[1].display_text(), "accepted b");
}

#[test]
fn only_nonempty_all_request_messages_are_retained_as_context() {
    let first = context("first");
    let second = context("second");
    let third = context("third");
    let fourth = context("fourth");
    let mut mixed = context("mixed");
    mixed.parts.extend(message("conversation part").parts);
    let empty = CanonicalMessage::new(MessageRole::Assistant, Vec::new());
    let mut trace = message("trace");
    trace.parts[0].scope = PartScope::Trace;
    let named = message("named but not request scope").with_name("context");
    let mut multi = context("multi first");
    multi.parts.extend(context("multi second").parts);
    let model = vec![
        first.clone(),
        mixed.clone(),
        second.clone(),
        empty.clone(),
        third.clone(),
        trace.clone(),
        fourth.clone(),
        named.clone(),
        multi.clone(),
    ];
    let canonical = vec![mixed, empty, trace, named];
    assert_eq!(merge_request_context(&model, &canonical), model);
    assert_eq!(
        merge_request_context(&model, &[]),
        vec![first, second, third, fourth, multi]
    );
}

#[test]
fn no_context_and_empty_inputs_adopt_only_the_canonical_snapshot() {
    let old = vec![message("old"), message("removed")];
    let canonical = vec![message("new"), old[0].clone()];
    assert_eq!(merge_request_context(&old, &canonical), canonical);
    assert_eq!(merge_request_context(&[], &canonical), canonical);
    assert!(merge_request_context(&old, &[]).is_empty());
    assert!(merge_request_context(&[], &[]).is_empty());
}

// Pre-change scan is an independent oracle for anchor placement, not a
// source-layout assertion or a runtime performance measurement.
fn reference_merge(
    model: &[CanonicalMessage],
    canonical: &[CanonicalMessage],
) -> Vec<CanonicalMessage> {
    let ephemeral = |message: &CanonicalMessage| {
        !message.parts.is_empty()
            && message
                .parts
                .iter()
                .all(|part| part.scope == PartScope::Request)
    };
    let mut groups = std::collections::HashMap::new();
    let mut trailing = Vec::new();
    for (index, message) in model
        .iter()
        .enumerate()
        .filter(|(_, message)| ephemeral(message))
    {
        let next = model[index + 1..].iter().find(|next| {
            !ephemeral(next) && canonical.iter().any(|accepted| accepted.id == next.id)
        });
        match next {
            Some(next) => groups
                .entry(next.id)
                .or_insert_with(Vec::new)
                .push(message.clone()),
            None => trailing.push(message.clone()),
        }
    }
    let mut merged = Vec::new();
    for message in canonical {
        merged.extend(groups.remove(&message.id).unwrap_or_default());
        merged.push(message.clone());
    }
    merged.extend(trailing);
    merged
}

#[test]
fn anchor_sweep_matches_scan_for_every_retained_subset_and_reordered_ack() {
    let anchors = (0..6)
        .map(|index| message(&format!("anchor {index}")))
        .collect::<Vec<_>>();
    let mut model = Vec::new();
    for (index, anchor) in anchors.iter().enumerate() {
        model.push(context(&format!("before {index}")));
        model.push(context(&format!("before {index} second")));
        model.push(anchor.clone());
    }
    model.push(context("trailing first"));
    model.push(context("trailing second"));
    let steering = message("new steering");
    for mask in 0usize..(1usize << anchors.len()) {
        let retained = anchors
            .iter()
            .enumerate()
            .filter(|(index, _)| mask & (1usize << *index) != 0)
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        for order in 0..3 {
            let mut canonical = retained.clone();
            if order == 1 {
                canonical.reverse();
            } else if order == 2 && !canonical.is_empty() {
                canonical.rotate_left(1);
            }
            canonical.insert(canonical.len() / 2, steering.clone());
            assert_eq!(
                merge_request_context(&model, &canonical),
                reference_merge(&model, &canonical),
                "mask={mask}, order={order}"
            );
        }
    }
}

#[test]
fn grouped_ids_match_scan_even_for_repeated_ids_and_changed_model_order() {
    // The merge is not a history validator. Preserve its existing behavior
    // on these inputs without treating them as valid canonical checkpoints.
    let a = message("a");
    let b = message("b");
    let repeated_a = a.clone().with_metadata(json!({"occurrence": 2}));
    let first = context("first");
    let second = context("second");
    let third = context("third");
    let tail = context("tail");
    let canonical = vec![b.clone(), repeated_a.clone(), a.clone()];
    let model = vec![first, a, second, b, third, repeated_a, tail];
    let mut reordered = model.clone();
    reordered.rotate_left(3);
    for model in [&model, &reordered] {
        assert_eq!(
            merge_request_context(model, &canonical),
            reference_merge(model, &canonical)
        );
    }
}
