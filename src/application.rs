use std::cmp::min;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{StreamExt, stream};
use serde_json::Value;

use crate::domain::{
    ApiResponse, CacheDisposition, CacheMode, GetCardRequest, SearchCardsPage, SearchCardsRequest,
    SearchResult, unwrap_data,
};
use crate::{
    CatalogSearchResponse, CatalogStatus, MAX_CATALOG_RESPONSE_BYTES, ResolveBatchRequest,
    ResolveBatchResponse,
};

#[async_trait]
pub trait ApiGateway: Send + Sync {
    async fn get_json(
        &self,
        endpoint: &'static str,
        query: Vec<(String, String)>,
        ttl: std::time::Duration,
        cache_mode: CacheMode,
    ) -> Result<ApiResponse>;
}

#[async_trait]
pub trait CatalogGateway: Send + Sync {
    async fn status(&self) -> Result<CatalogStatus>;
    async fn search_catalog(&self, request: &SearchCardsRequest) -> Result<CatalogSearchResponse>;
    async fn resolve_catalog(&self, request: &ResolveBatchRequest) -> Result<ResolveBatchResponse>;
}

#[derive(Clone)]
pub struct CatalogService {
    gateway: Arc<dyn CatalogGateway>,
}

impl CatalogService {
    #[must_use]
    pub fn new(gateway: Arc<dyn CatalogGateway>) -> Self {
        Self { gateway }
    }

    pub async fn status(&self) -> Result<CatalogStatus> {
        self.gateway.status().await
    }

    pub async fn search(&self, request: &SearchCardsRequest) -> Result<CatalogSearchResponse> {
        request.validate()?;
        let response = self.gateway.search_catalog(request).await?;
        ensure_catalog_response_size(&response)?;
        Ok(response)
    }

    pub async fn resolve(&self, request: &ResolveBatchRequest) -> Result<ResolveBatchResponse> {
        request.validate()?;
        let response = self.gateway.resolve_catalog(request).await?;
        ensure_catalog_response_size(&response)?;
        Ok(response)
    }
}

fn ensure_catalog_response_size(value: &impl serde::Serialize) -> Result<()> {
    let length = serde_json::to_vec(value)?.len();
    if length > MAX_CATALOG_RESPONSE_BYTES {
        anyhow::bail!(
            "catalog response exceeds the {} byte limit",
            MAX_CATALOG_RESPONSE_BYTES
        );
    }
    Ok(())
}

#[derive(Clone)]
pub struct BazaarService {
    gateway: Arc<dyn ApiGateway>,
}

impl BazaarService {
    #[must_use]
    pub fn new(gateway: Arc<dyn ApiGateway>) -> Self {
        Self { gateway }
    }

    pub async fn search_page(
        &self,
        request: &SearchCardsRequest,
        cache_mode: CacheMode,
    ) -> Result<(SearchCardsPage, CacheDisposition)> {
        request.validate()?;
        let response = self
            .gateway
            .get_json(
                "search_cards",
                request.query_pairs(),
                SearchCardsRequest::cache_ttl(),
                cache_mode,
            )
            .await?;
        let value = serde_json::from_slice::<Value>(&response.body)
            .context("search_cards returned invalid JSON")?;
        let page = serde_json::from_value::<SearchCardsPage>(unwrap_data(value))
            .context("search_cards response does not match the documented contract")?;
        Ok((page, response.cache))
    }

    pub async fn search(
        &self,
        request: SearchCardsRequest,
        all: bool,
        concurrency: usize,
        max_pages: usize,
        cache_mode: CacheMode,
    ) -> Result<(SearchResult, Vec<CacheDisposition>)> {
        let (first, first_cache) = self.search_page(&request, cache_mode).await?;
        if !all || first.total <= first.count || first.limit == 0 {
            return Ok((
                SearchResult {
                    page: first.page,
                    limit: first.limit,
                    count: first.cards.len(),
                    total: first.total,
                    pages_fetched: 1,
                    cards: first.cards,
                },
                vec![first_cache],
            ));
        }

        let total_pages = first.total.div_ceil(first.limit) as usize;
        let first_page = first.page as usize;
        let remaining_pages = total_pages.saturating_sub(first_page.saturating_add(1));
        let remaining_budget = max_pages.max(1).saturating_sub(1);
        let end_page = first_page
            .saturating_add(1)
            .saturating_add(min(remaining_pages, remaining_budget));
        let service = self.clone();
        let template = request.clone();
        let remaining = stream::iter((first_page + 1)..end_page)
            .map(move |page| {
                let service = service.clone();
                let mut request = template.clone();
                request.page = page as u32;
                async move {
                    let (response, cache) = service.search_page(&request, cache_mode).await?;
                    Ok::<_, anyhow::Error>((response.page, response.cards, cache))
                }
            })
            .buffer_unordered(concurrency.clamp(1, 32))
            .collect::<Vec<_>>()
            .await;

        let mut pages = vec![(first.page, first.cards, first_cache)];
        for page in remaining {
            pages.push(page?);
        }
        pages.sort_by_key(|(page, _, _)| *page);

        let mut cards = Vec::new();
        let mut cache = Vec::with_capacity(pages.len());
        for (_, mut page_cards, disposition) in pages {
            cards.append(&mut page_cards);
            cache.push(disposition);
        }
        Ok((
            SearchResult {
                page: request.page,
                limit: request.limit,
                count: cards.len(),
                total: first.total,
                pages_fetched: cache.len(),
                cards,
            },
            cache,
        ))
    }

    pub async fn get_card(
        &self,
        request: &GetCardRequest,
        cache_mode: CacheMode,
    ) -> Result<(Value, CacheDisposition)> {
        request.validate()?;
        let response = self
            .gateway
            .get_json(
                "get_card",
                request.query_pairs(),
                GetCardRequest::cache_ttl(),
                cache_mode,
            )
            .await?;
        let value = serde_json::from_slice::<Value>(&response.body)
            .context("get_card returned invalid JSON")?;
        Ok((unwrap_data(value), response.cache))
    }
}
