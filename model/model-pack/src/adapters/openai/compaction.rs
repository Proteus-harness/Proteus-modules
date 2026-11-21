//! Opaque Responses checkpoints and strict V2 compaction output collection.
use anyhow::{Result, anyhow, bail};
use serde_json::Value;

pub(crate) const FORMAT: &str = "openai.responses.compaction.v1";

pub(crate) fn default_support(config: &Value) -> bool {
    let base = config
        .get("base_url")
        .and_then(Value::as_str)
        .unwrap_or("https://api.openai.com/v1");
    reqwest::Url::parse(base).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("api.openai.com")
            && url.path().trim_end_matches('/') == "/v1"
    })
}

pub(super) fn is_checkpoint(item: &Value) -> bool {
    matches!(
        item.get("type").and_then(Value::as_str),
        Some("compaction" | "compaction_summary")
    )
}

/// Keep the exact provider item, rather than reducing the ciphertext to text.
/// Unknown fields remain owned by this provider format; the discriminator and
/// ciphertext are checked again before replay so another opaque item cannot
/// be smuggled through this encoder.
pub(super) fn validate_item(item: &Value) -> Result<()> {
    if item.get("type").and_then(Value::as_str) != Some("compaction") {
        bail!("OpenAI compaction checkpoint has an invalid type");
    }
    if !item
        .get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|content| !content.trim().is_empty())
    {
        bail!("OpenAI compaction checkpoint requires non-empty encrypted_content");
    }
    if item
        .get("id")
        .is_some_and(|id| !id.is_null() && id.as_str().is_none_or(str::is_empty))
    {
        bail!("OpenAI compaction checkpoint id must be a non-empty string or null");
    }
    Ok(())
}

pub(super) fn canonical_item(item: &Value) -> Result<Value> {
    into_canonical_item(item.clone())
}

fn into_canonical_item(mut item: Value) -> Result<Value> {
    // Native Codex accepts this upstream alias while deserializing, then
    // serializes its single canonical Compaction variant back as compaction.
    if item.get("type").and_then(Value::as_str) == Some("compaction_summary") {
        item["type"] = Value::String("compaction".to_owned());
    }
    validate_item(&item)?;
    Ok(item)
}

/// Match Codex's V2 collector: count completed output items, not an optional
/// duplicate output array on response.completed. Nothing is published before
/// that terminal event, and a terminal output array cannot repair a missing
/// output_item.done checkpoint.
pub(super) fn completed_checkpoint(items: &mut [Value]) -> Result<Value> {
    let mut checkpoints = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| is_checkpoint(item).then_some(index));
    let index = checkpoints.next().ok_or_else(|| {
        anyhow!("OpenAI compaction expected exactly one completed checkpoint, got 0")
    })?;
    if checkpoints.next().is_some() {
        bail!("OpenAI compaction expected exactly one completed checkpoint, got multiple");
    }
    into_canonical_item(items[index].take())
}
