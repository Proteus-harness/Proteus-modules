//! Codex /models metadata stays inside the provider; no remote instructions
//! or capabilities are imported into the configured Proteus assembly.
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use proteus_contracts::contracts::{ModelCatalog, ModelCatalogEntry};
use serde::Deserialize;
use serde_json::Value;

use super::OpenAiResponsesClient;
use crate::adapters::codex_auth::CODEX_CLIENT_VERSION;

pub(super) type CatalogCache = tokio::sync::Mutex<Option<(Instant, ModelCatalog)>>;
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Accepts a plain `MAJOR.MINOR.PATCH` version, as upstream sends it.
pub(super) fn validate_client_version(version: &str) -> Result<()> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        bail!("openai_codex client_version must look like 0.163.0");
    }
    Ok(())
}

#[derive(Deserialize)]
struct ModelsResponse {
    models: Vec<RemoteModel>,
}

#[derive(Deserialize)]
struct RemoteModel {
    slug: String,
    display_name: String,
    description: Option<String>,
    visibility: String,
    priority: i64,
    default_reasoning_level: Option<String>,
    supported_reasoning_levels: Vec<RemoteEffort>,
}

#[derive(Deserialize)]
struct RemoteEffort {
    effort: String,
}

impl OpenAiResponsesClient {
    fn catalog_client_version(&self) -> &str {
        self.secret_config
            .get("client_version")
            .and_then(Value::as_str)
            .unwrap_or(CODEX_CLIENT_VERSION)
    }

    pub(super) async fn codex_catalog(&self) -> Result<Option<ModelCatalog>> {
        let Some(auth) = &self.codex_auth else {
            return Ok(None);
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            // The caller's total budget includes coalescing behind another
            // reader, not only the HTTP request after acquiring the cache.
            let mut cache = self.catalog_cache.lock().await;
            if let Some((fetched, catalog)) = &*cache {
                if fetched.elapsed() < CACHE_TTL {
                    return Ok(Some(catalog.clone()));
                }
            }
            let response = super::transport::send_metadata_request(auth, "model catalog", || {
                self.http
                    .get(format!("{}/models", self.base_url))
                    .query(&[("client_version", self.catalog_client_version())])
            })
            .await?;
            let mut response: ModelsResponse = response
                .json()
                .await
                .context("invalid ChatGPT model catalog response")?;
            response.models.sort_by_key(|model| model.priority);
            let catalog = ModelCatalog {
                models: response
                    .models
                    .into_iter()
                    .map(|model| ModelCatalogEntry {
                        id: model.slug,
                        display_name: model.display_name,
                        description: model.description,
                        hidden: model.visibility != "list",
                        reasoning_efforts: model
                            .supported_reasoning_levels
                            .into_iter()
                            .map(|level| level.effort)
                            .collect(),
                        default_reasoning_effort: model.default_reasoning_level,
                    })
                    .collect(),
            };
            catalog.validate().map_err(anyhow::Error::msg)?;
            *cache = Some((Instant::now(), catalog.clone()));
            Ok(Some(catalog))
        })
        .await
        .context("ChatGPT model catalog timed out")?
    }
}
