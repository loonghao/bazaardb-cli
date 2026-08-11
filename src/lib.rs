pub mod application;
pub mod catalog;
pub mod domain;
pub mod infrastructure;
pub mod server;

pub use application::{BazaarService, CatalogGateway, CatalogService};
pub use catalog::{
    CATALOG_SCHEMA_VERSION, CanonicalGameIdentifier, CanonicalUuid, CardTier,
    CatalogCardProjection, CatalogContractError, CatalogIdentity, CatalogSearchResponse,
    CatalogStatus, ComponentResolution, ComponentShape, ComponentStatus,
    MAX_CATALOG_RESPONSE_BYTES, PayloadIdConsistency, RESOLVER_VERSION, ResolveBatchRequest,
    ResolveBatchResponse, ResolveCardRequest, ResolveJsonlRecord, ResolveMode, ResolvedCard,
    TooltipResolution, TooltipShape,
};
pub use domain::{
    ApiResponse, CacheDisposition, CacheMode, GetCardRequest, SearchCardsPage, SearchCardsRequest,
    SearchResult,
};
pub use infrastructure::{
    CacheStore, GameDataGateway, GameDataGatewayConfig, ParseGateway, ParseGatewayConfig,
    detect_game_data_path,
};
