//! Explicit identity headers for API-key gateways; values belong to each request.
use anyhow::{Context, Result, bail};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use serde_json::Value;

use crate::model_standard::CanonicalModelRequest;

#[derive(Debug, Clone, Default)]
pub(super) struct ApiIdentity {
    user_agent: Option<HeaderValue>,
    session_header: Option<HeaderName>,
}

impl ApiIdentity {
    pub(super) fn from_config(config: &Value) -> Result<Self> {
        let user_agent = optional_string(config, "user_agent")?
            .map(|value| HeaderValue::from_str(value).context("invalid openai user_agent header"))
            .transpose()?;
        let session_header = optional_string(config, "session_header")?
            .map(|value| {
                let name = HeaderName::from_bytes(value.as_bytes())
                    .context("invalid openai session_header name")?;
                if matches!(
                    name.as_str(),
                    "authorization" | "content-type" | "content-length" | "host" | "user-agent"
                ) {
                    bail!("openai session_header cannot replace authentication or request headers");
                }
                Ok(name)
            })
            .transpose()?;
        Ok(Self {
            user_agent,
            session_header,
        })
    }

    pub(super) fn headers(&self, request: &CanonicalModelRequest) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        if let Some(value) = &self.user_agent {
            headers.insert(USER_AGENT, value.clone());
        }
        if let Some(name) = &self.session_header {
            let session = request
                .client_metadata
                .get("session_id")
                .filter(|value| !value.is_empty())
                .context("openai session_header requires request client_metadata.session_id")?;
            let mut value = HeaderValue::from_str(session)
                .map_err(|_| anyhow::anyhow!("invalid openai session identity header"))?;
            value.set_sensitive(true);
            headers.insert(name.clone(), value);
        }
        Ok(headers)
    }
}

fn optional_string<'a>(config: &'a Value, name: &str) -> Result<Option<&'a str>> {
    match config.get(name) {
        None => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value)),
        Some(_) => bail!("openai {name} must be a non-empty string"),
    }
}
