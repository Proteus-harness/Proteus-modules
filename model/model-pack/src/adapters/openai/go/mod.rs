//! Native OpenCode Go model discovery, account metadata and Responses transport.
mod catalog;
mod credentials;
mod quota;
#[cfg(test)]
mod tests;

use super::OpenAiResponsesClient;
use anyhow::{Context, Result, ensure};
use catalog::Models;
use futures_util::StreamExt;
use proteus_contracts::{
    contracts::{ModelCatalog, ModelQuotaSnapshot},
    model_standard::ModelCapabilities,
};
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};
use std::{
    sync::{Arc, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const MODELS: &str = "https://opencode.ai/inference/go/v1/models";
const USAGE: &str = "https://opencode.ai/inference/go/v1/usage";
const BUDGETS: &str = "https://opencode.ai/console/api/policies/current-budgets";
const BASE: &str = "https://opencode.ai/inference/go/openai/v1";

#[derive(Debug)]
pub(crate) struct Go {
    models: RwLock<Models>,
    refresh_after: Mutex<Option<Instant>>,
    quota: Mutex<Option<(Instant, [u8; 32], ModelQuotaSnapshot)>>,
    http: Client,
}

impl Go {
    pub(crate) fn new() -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            models: RwLock::new(Models::parse(include_bytes!("catalog-seed.json"))?),
            refresh_after: Mutex::new(None),
            quota: Mutex::new(None),
            http: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        }))
    }
    pub(crate) fn transport(
        self: &Arc<Self>,
        mut config: Value,
        stream: bool,
    ) -> Result<OpenAiResponsesClient> {
        ensure!(
            config
                .as_object()
                .is_some_and(|m| m.keys().all(|k| matches!(
                    k.as_str(),
                    "implementation" | "provider" | "api_key" | "database_path"
                ))),
            "unknown native OpenCode Go configuration field"
        );
        ensure!(
            config
                .get("provider")
                .is_none_or(|provider| provider == "opencode-go"),
            "native OpenCode Go requires the opencode-go provider"
        );
        config["base_url"] = json!(BASE);
        config["stream"] = json!(stream);
        config["supports_compaction"] = json!(false);
        config["session_header"] = json!("x-opencode-session");
        config["user_agent"] = json!(concat!("Proteus/", env!("CARGO_PKG_VERSION")));
        config["capabilities"] = json!({"supports_image_input":true,"supports_parallel_tool_calls":true,"supports_reasoning_config":true});
        let mut client = OpenAiResponsesClient::from_provider_config_with_http(
            config,
            Client::builder().redirect(reqwest::redirect::Policy::none()),
        )?;
        client.go = Some(self.clone());
        Ok(client)
    }
    pub(super) fn key(&self, config: &Value) -> Result<String> {
        credentials::key(config)
    }
    pub(crate) fn contains(&self, id: &str) -> bool {
        self.models.read().unwrap().get(id).is_some()
    }
    pub(super) fn capabilities(&self, id: &str) -> Result<ModelCapabilities> {
        Ok(self
            .models
            .read()
            .unwrap()
            .get(id)
            .context("unknown OpenCode Go chat model")?
            .descriptor())
    }
    pub(super) fn filter_sampling(&self, id: &str, body: &mut Value) -> Result<()> {
        let models = self.models.read().unwrap();
        let entry = models.get(id).context("unknown OpenCode Go chat model")?;
        let (temperature, top_p) = entry.sampling();
        let object = body.as_object_mut().unwrap();
        if !temperature {
            object.remove("temperature");
        }
        if !top_p {
            object.remove("top_p");
        }
        Ok(())
    }
    pub(crate) async fn catalog(&self) -> Result<ModelCatalog> {
        self.catalog_from(MODELS).await
    }

    async fn catalog_from(&self, url: &str) -> Result<ModelCatalog> {
        let mut refresh_after = self.refresh_after.lock().await;
        if refresh_after.is_none_or(|deadline| Instant::now() >= deadline) {
            let fresh = bytes(
                self.http.get(url).timeout(Duration::from_secs(3)),
                2 * 1024 * 1024,
            )
            .await
            .and_then(|raw| Models::parse(&raw));
            if let Ok(models) = fresh {
                *self.models.write().unwrap() = models;
                *refresh_after = Some(Instant::now() + Duration::from_secs(300));
            } else {
                // Keep the last valid list or the packaged offline snapshot. Retry in 15 seconds.
                *refresh_after = Some(Instant::now() + Duration::from_secs(15));
            }
        }
        Ok(self.models.read().unwrap().catalog())
    }
    pub(super) async fn quota(&self, config: &Value) -> Result<ModelQuotaSnapshot> {
        self.quota_from(config, USAGE, BUDGETS).await
    }

    async fn quota_from(
        &self,
        config: &Value,
        usage_url: &str,
        budgets_url: &str,
    ) -> Result<ModelQuotaSnapshot> {
        let key = self.key(config)?;
        let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
        let fingerprint: [u8; 32] = digest.as_ref().try_into().unwrap();
        let mut cache = self.quota.lock().await;
        if let Some((at, hash, value)) = &*cache
            && *hash == fingerprint
            && at.elapsed() < Duration::from_secs(30)
        {
            return Ok(value.clone());
        }
        let get = |url, seconds| {
            bytes(
                self.http
                    .get(url)
                    .bearer_auth(&key)
                    .timeout(Duration::from_secs(seconds)),
                1_048_576,
            )
        };
        let (usage, budgets) = tokio::join!(get(usage_url, 15), get(budgets_url, 5));
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let mut snapshot = quota::usage(&usage?, now)?;
        match budgets.and_then(|b| quota::budgets(&b)) {
            Ok(values) => snapshot.buckets.extend(values),
            Err(error) => snapshot.notices.push(format!(
                "Не удалось получить денежные бюджеты OpenCode. {error}"
            )),
        }
        snapshot.validate().map_err(anyhow::Error::msg)?;
        *cache = Some((Instant::now(), fingerprint, snapshot.clone()));
        Ok(snapshot)
    }
}
async fn bytes(request: RequestBuilder, limit: usize) -> Result<Vec<u8>> {
    let response = request.send().await.map_err(|e| e.without_url())?;
    ensure!(
        response.status().is_success(),
        "OpenCode metadata returned HTTP {}",
        response.status().as_u16()
    );
    let mut raw = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|e| e.without_url())?;
        ensure!(
            chunk.len() <= limit - raw.len(),
            "OpenCode metadata exceeds size limit"
        );
        raw.extend_from_slice(&chunk);
    }
    Ok(raw)
}
