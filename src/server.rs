use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::RwLock;

use crate::BazaarService;
use crate::domain::{CacheMode, OUTPUT_SCHEMA_VERSION, SearchCardsRequest};

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub listen: SocketAddr,
    pub request: SearchCardsRequest,
    pub refresh_interval: Duration,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateEnvelope {
    pub schema_version: &'static str,
    pub tick: u64,
    pub updated_at: String,
    pub source: &'static str,
    pub query: SearchCardsRequest,
    pub data: Option<Value>,
    pub error: Option<String>,
}

#[derive(Clone)]
struct AppState {
    state: Arc<RwLock<StateEnvelope>>,
}

pub async fn serve(service: BazaarService, config: ServeConfig) -> Result<()> {
    if !config.listen.ip().is_loopback() {
        bail!("serve only accepts a loopback listen address");
    }
    let state = Arc::new(RwLock::new(StateEnvelope {
        schema_version: OUTPUT_SCHEMA_VERSION,
        tick: 0,
        updated_at: timestamp(),
        source: "parse.bot/bazaardb-gg-api",
        query: config.request.clone(),
        data: None,
        error: Some("initial query has not completed".to_owned()),
    }));
    refresh(&service, &state, &config.request).await;

    let refresh_state = Arc::clone(&state);
    let refresh_service = service.clone();
    let refresh_request = config.request.clone();
    let refresh_interval = config.refresh_interval;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(refresh_interval);
        interval.tick().await;
        loop {
            interval.tick().await;
            refresh(&refresh_service, &refresh_state, &refresh_request).await;
        }
    });

    let app = Router::new()
        .route("/v1/state", get(state_handler))
        .route("/healthz", get(health_handler))
        .with_state(AppState { state });
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(listen = %config.listen, "BazaarDB CUA state server started");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn refresh(
    service: &BazaarService,
    state: &Arc<RwLock<StateEnvelope>>,
    request: &SearchCardsRequest,
) {
    let result = service
        .search(request.clone(), false, 1, 1, CacheMode::Use)
        .await;
    let mut current = state.write().await;
    current.tick = current.tick.saturating_add(1);
    current.updated_at = timestamp();
    match result {
        Ok((result, _)) => {
            current.data = serde_json::to_value(result).ok();
            current.error = None;
        }
        Err(error) => {
            current.error = Some(error.to_string());
        }
    }
}

async fn state_handler(State(app): State<AppState>, headers: HeaderMap) -> Response {
    let state = app.state.read().await.clone();
    let etag = format!("\"{}\"", state.tick);
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response = Json(state).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).expect("numeric ETag is valid"),
    );
    response
}

async fn health_handler(State(app): State<AppState>) -> impl IntoResponse {
    let state = app.state.read().await;
    Json(serde_json::json!({
        "ok": state.error.is_none(),
        "schema_version": state.schema_version,
        "tick": state.tick,
    }))
}

fn timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_owned())
}

#[must_use]
pub fn loopback_socket(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::from([127, 0, 0, 1]), port)
}
