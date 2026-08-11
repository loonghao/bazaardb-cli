use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::RwLock;

use crate::domain::{CacheMode, OUTPUT_SCHEMA_VERSION, SearchCardsRequest};
use crate::{BazaarService, CatalogContractError, CatalogService, ResolveBatchRequest};

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
    catalog: CatalogService,
}

pub async fn serve(
    service: BazaarService,
    catalog: CatalogService,
    config: ServeConfig,
) -> Result<()> {
    if !config.listen.ip().is_loopback() {
        bail!("serve only accepts a loopback listen address");
    }
    let state = Arc::new(RwLock::new(StateEnvelope {
        schema_version: OUTPUT_SCHEMA_VERSION,
        tick: 0,
        updated_at: timestamp(),
        source: "the-bazaar/GameData.db",
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
        .route("/v1/catalog/status", get(catalog_status_handler))
        .route("/v1/catalog/search", get(catalog_search_handler))
        .route("/v1/catalog/resolve", post(catalog_resolve_handler))
        .route("/healthz", get(health_handler))
        .with_state(AppState { state, catalog });
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(listen = %config.listen, "BazaarDB read-only HTTP server started");
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
        return no_store(StatusCode::NOT_MODIFIED.into_response());
    }
    let mut response = Json(state).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).expect("numeric ETag is valid"),
    );
    no_store(response)
}

async fn catalog_status_handler(State(app): State<AppState>) -> Response {
    match app.catalog.status().await {
        Ok(status) => no_store(Json(status).into_response()),
        Err(error) => {
            tracing::error!(%error, "catalog status failed");
            catalog_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "catalog_unavailable",
                "catalog status is unavailable",
                Value::Null,
            )
        }
    }
}

async fn catalog_search_handler(
    State(app): State<AppState>,
    query: std::result::Result<Query<SearchCardsRequest>, QueryRejection>,
) -> Response {
    let request = match query {
        Ok(Query(request)) => request,
        Err(error) => {
            return catalog_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &error.body_text(),
                Value::Null,
            );
        }
    };
    match app.catalog.search(&request).await {
        Ok(result) => no_store(Json(result).into_response()),
        Err(error) => catalog_request_or_internal_error(error, "catalog search failed"),
    }
}

async fn catalog_resolve_handler(
    State(app): State<AppState>,
    payload: std::result::Result<Json<ResolveBatchRequest>, JsonRejection>,
) -> Response {
    let request = match payload {
        Ok(Json(request)) => request,
        Err(error) => {
            return catalog_error(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &error.body_text(),
                Value::Null,
            );
        }
    };
    match app.catalog.resolve(&request).await {
        Ok(result) => no_store(Json(result).into_response()),
        Err(error) => catalog_request_or_internal_error(error, "catalog resolve failed"),
    }
}

async fn health_handler(State(app): State<AppState>) -> Response {
    let state = app.state.read().await;
    no_store(
        Json(serde_json::json!({
            "ok": state.error.is_none(),
            "schema_version": state.schema_version,
            "tick": state.tick,
        }))
        .into_response(),
    )
}

fn catalog_request_or_internal_error(error: anyhow::Error, log_message: &'static str) -> Response {
    if let Some(contract) = error.downcast_ref::<CatalogContractError>() {
        return catalog_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            contract.code,
            &contract.message,
            contract.details.clone(),
        );
    }
    let message = error.to_string();
    let is_request_error = message.starts_with("category must be")
        || message.starts_with("limit must be")
        || message.starts_with("order must be")
        || message.starts_with("sortBy must be")
        || message.starts_with("resolve requests must contain");
    if is_request_error {
        catalog_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &message,
            Value::Null,
        )
    } else if message.starts_with("catalog response exceeds") {
        catalog_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "response_too_large",
            &message,
            Value::Null,
        )
    } else {
        tracing::error!(%error, "{log_message}");
        catalog_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "catalog_unavailable",
            "catalog operation failed",
            Value::Null,
        )
    }
}

fn catalog_error(
    status: StatusCode,
    code: &'static str,
    message: &str,
    details: Value,
) -> Response {
    no_store(
        (
            status,
            Json(serde_json::json!({
                "catalogSchemaVersion": crate::CATALOG_SCHEMA_VERSION,
                "resolverVersion": crate::RESOLVER_VERSION,
                "authority": crate::catalog::INSPECTION_AUTHORITY,
                "authorizesAction": false,
                "error": {"code": code, "message": message, "details": details},
            })),
        )
            .into_response(),
    )
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
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
