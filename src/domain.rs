use std::fmt;
use std::time::Duration;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const OUTPUT_SCHEMA_VERSION: &str = "1.0.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMode {
    Use,
    Refresh,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheDisposition {
    Hit,
    Miss,
    Refresh,
    Offline,
    StaleFallback,
}

impl fmt::Display for CacheDisposition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Refresh => "refresh",
            Self::Offline => "offline",
            Self::StaleFallback => "stale_fallback",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub body: Vec<u8>,
    pub cache: CacheDisposition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchCardsRequest {
    pub query: Option<String>,
    pub category: String,
    pub page: u32,
    pub limit: u32,
    pub sort_by: String,
    pub order: String,
    pub show_unobtainable: bool,
}

impl Default for SearchCardsRequest {
    fn default() -> Self {
        Self {
            query: None,
            category: "all".to_owned(),
            page: 0,
            limit: 25,
            sort_by: "Auto".to_owned(),
            order: "ascending".to_owned(),
            show_unobtainable: false,
        }
    }
}

impl SearchCardsRequest {
    pub fn validate(&self) -> Result<()> {
        const CATEGORIES: &[&str] = &[
            "all",
            "items",
            "skills",
            "merchants",
            "trainers",
            "monsters",
            "events",
        ];
        if !CATEGORIES.contains(&self.category.as_str()) {
            bail!("category must be one of: {}", CATEGORIES.join(", "));
        }
        if !(1..=100).contains(&self.limit) {
            bail!("limit must be between 1 and 100");
        }
        if !matches!(self.order.as_str(), "ascending" | "descending") {
            bail!("order must be ascending or descending");
        }
        Ok(())
    }

    #[must_use]
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        let mut query = vec![
            ("page".to_owned(), self.page.to_string()),
            ("limit".to_owned(), self.limit.to_string()),
            ("order".to_owned(), self.order.clone()),
            ("sort_by".to_owned(), self.sort_by.clone()),
            ("category".to_owned(), self.category.clone()),
            (
                "show_unobtainable".to_owned(),
                self.show_unobtainable.to_string(),
            ),
        ];
        if let Some(value) = self.query.as_deref().filter(|value| !value.is_empty()) {
            query.push(("query".to_owned(), value.to_owned()));
        }
        query
    }

    #[must_use]
    pub const fn cache_ttl() -> Duration {
        Duration::from_secs(15 * 60)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetCardRequest {
    pub name: String,
}

impl GetCardRequest {
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("card name is required");
        }
        Ok(())
    }

    #[must_use]
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        vec![("name".to_owned(), self.name.trim().to_owned())]
    }

    #[must_use]
    pub const fn cache_ttl() -> Duration {
        Duration::from_secs(6 * 60 * 60)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchCardsPage {
    #[serde(default)]
    pub page: u32,
    #[serde(default)]
    pub limit: u32,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub total: u32,
    #[serde(default)]
    pub cards: Vec<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub page: u32,
    pub limit: u32,
    pub count: usize,
    pub total: u32,
    pub pages_fetched: usize,
    pub cards: Vec<Value>,
}

pub fn unwrap_data(value: Value) -> Value {
    match value {
        Value::Object(mut object) if object.contains_key("data") => {
            object.remove("data").unwrap_or(Value::Null)
        }
        value => value,
    }
}
