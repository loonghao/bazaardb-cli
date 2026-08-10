use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use url::Url;

use crate::application::ApiGateway;
use crate::domain::{ApiResponse, CacheDisposition, CacheMode};

use super::{CacheEntry, CacheStore};

pub const DEFAULT_API_BASE: &str =
    "https://api.parse.bot/scraper/49e1e7c5-ab05-423d-9afa-ac05d9f04241/";
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct ParseGatewayConfig {
    pub api_base: String,
    pub api_key: Option<String>,
    pub cache: CacheStore,
    pub stale_for: Duration,
    pub max_retries: u32,
}

pub struct ParseGateway {
    client: reqwest::Client,
    api_base: Url,
    api_key: Option<String>,
    cache: CacheStore,
    stale_for: Duration,
    max_retries: u32,
}

impl ParseGateway {
    pub fn new(config: ParseGatewayConfig) -> Result<Self> {
        let mut api_base = Url::parse(&config.api_base).context("invalid API base URL")?;
        if !api_base.path().ends_with('/') {
            api_base.set_path(&format!("{}/", api_base.path()));
        }
        let loopback = api_base
            .host_str()
            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1"));
        if api_base.scheme() != "https" && !(api_base.scheme() == "http" && loopback) {
            bail!("API base must use HTTPS, except for a loopback test server");
        }
        if !api_base.username().is_empty()
            || api_base.password().is_some()
            || api_base.query().is_some()
            || api_base.fragment().is_some()
        {
            bail!("API base must not contain credentials, query, or fragment");
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("bazaardb-cli/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            api_base,
            api_key: config.api_key,
            cache: config.cache,
            stale_for: config.stale_for,
            max_retries: config.max_retries,
        })
    }

    fn endpoint_url(&self, endpoint: &'static str) -> Result<Url> {
        if !matches!(endpoint, "search_cards" | "get_card") {
            bail!("unsupported endpoint {endpoint:?}");
        }
        self.api_base.join(endpoint).context("invalid endpoint URL")
    }

    fn cache_key(&self, endpoint: &str, query: &[(String, String)]) -> String {
        let mut query = query.to_vec();
        query.sort();
        let mut digest = Sha256::new();
        digest.update(self.api_base.as_str().as_bytes());
        digest.update(b"\0");
        digest.update(endpoint.as_bytes());
        for (key, value) in query {
            digest.update(b"\0");
            digest.update(key.as_bytes());
            digest.update(b"=");
            digest.update(value.as_bytes());
        }
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    async fn fetch(&self, endpoint: &'static str, query: &[(String, String)]) -> Result<Vec<u8>> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "BazaarDB API key is required; set BAZAARDB_API_KEY or PARSE_API_KEY"
                )
            })?;
        let url = self.endpoint_url(endpoint)?;
        let mut attempt = 0_u32;
        loop {
            let response = self
                .client
                .get(url.clone())
                .query(query)
                .header("X-API-Key", api_key)
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    let bytes = read_limited(response, MAX_RESPONSE_BYTES).await?;
                    serde_json::from_slice::<serde_json::Value>(&bytes)
                        .context("API returned non-JSON content")?;
                    return Ok(bytes);
                }
                Ok(response)
                    if attempt < self.max_retries
                        && (response.status() == StatusCode::TOO_MANY_REQUESTS
                            || response.status().is_server_error()) =>
                {
                    let retry_after = response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(Duration::from_secs)
                        .unwrap_or_else(|| Duration::from_millis(250 * 2_u64.pow(attempt)));
                    tokio::time::sleep(retry_after.min(Duration::from_secs(10))).await;
                    attempt += 1;
                }
                Ok(response) => {
                    let status = response.status();
                    let body = read_limited(response, MAX_ERROR_BYTES)
                        .await
                        .unwrap_or_default();
                    let body = String::from_utf8_lossy(&body)
                        .chars()
                        .take(300)
                        .collect::<String>();
                    bail!("API request failed with HTTP {status}: {body}");
                }
                Err(error) if attempt < self.max_retries => {
                    tokio::time::sleep(Duration::from_millis(250 * 2_u64.pow(attempt))).await;
                    attempt += 1;
                    tracing::debug!(%error, attempt, "retrying BazaarDB request");
                }
                Err(error) => return Err(error).context("API request failed"),
            }
        }
    }
}

async fn read_limited(response: reqwest::Response, maximum: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        bail!("API response exceeds {maximum} bytes");
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > maximum {
            bail!("API response exceeds {maximum} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[async_trait]
impl ApiGateway for ParseGateway {
    async fn get_json(
        &self,
        endpoint: &'static str,
        query: Vec<(String, String)>,
        ttl: Duration,
        cache_mode: CacheMode,
    ) -> Result<ApiResponse> {
        let key = self.cache_key(endpoint, &query);
        let now = now_epoch_seconds()?;
        let cached = self.cache.get(key.clone()).await?;
        if let Some(entry) = cached.as_ref() {
            if cache_mode == CacheMode::Offline {
                return Ok(ApiResponse {
                    body: entry.body.clone(),
                    cache: CacheDisposition::Offline,
                });
            }
            if cache_mode == CacheMode::Use && entry.expires_at >= now {
                return Ok(ApiResponse {
                    body: entry.body.clone(),
                    cache: CacheDisposition::Hit,
                });
            }
        } else if cache_mode == CacheMode::Offline {
            bail!("offline cache miss for {endpoint}");
        }

        match self.fetch(endpoint, &query).await {
            Ok(body) => {
                let expires_at = now.saturating_add(ttl.as_secs());
                self.cache
                    .put(
                        key,
                        CacheEntry {
                            stored_at: now,
                            expires_at,
                            stale_until: expires_at.saturating_add(self.stale_for.as_secs()),
                            body: body.clone(),
                        },
                    )
                    .await?;
                Ok(ApiResponse {
                    body,
                    cache: if cache_mode == CacheMode::Refresh {
                        CacheDisposition::Refresh
                    } else {
                        CacheDisposition::Miss
                    },
                })
            }
            Err(error) => {
                if let Some(entry) = cached.filter(|entry| entry.stale_until >= now) {
                    tracing::warn!(%error, endpoint, "using stale cached response");
                    return Ok(ApiResponse {
                        body: entry.body,
                        cache: CacheDisposition::StaleFallback,
                    });
                }
                Err(error)
            }
        }
    }
}

fn now_epoch_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}
