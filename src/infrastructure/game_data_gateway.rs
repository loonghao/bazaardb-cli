use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::application::{ApiGateway, CatalogGateway};
use crate::catalog::{
    AttributeResolution, CATALOG_SCHEMA_VERSION, CanonicalGameIdentifier, CanonicalUuid, CardTier,
    CatalogCardProjection, CatalogIdentity, CatalogSearchResponse, CatalogStatus,
    ComponentResolution, ComponentShape, ComponentStatus, PayloadIdConsistency, RESOLVER_VERSION,
    ResolveBatchRequest, ResolveBatchResponse, ResolvedCard, TooltipResolution, TooltipShape,
    selector_key,
};
use crate::domain::{
    ApiResponse, CacheDisposition, CacheMode, SearchCardsPage, SearchCardsRequest,
};
use crate::external_identity::ExternalIdentityCatalog;

use super::catalog_snapshot::{LoadedCatalog, NormalizedCard, database_stamp, load_or_rebuild};
use super::{CacheEntry, CacheStore};

const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const MAX_DATABASE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
pub struct GameDataGatewayConfig {
    pub database_path: PathBuf,
    pub catalog_cache_dir: PathBuf,
    pub cache: CacheStore,
}

pub struct GameDataGateway {
    database_path: PathBuf,
    catalog_cache_dir: PathBuf,
    cache: CacheStore,
    external_identities: Arc<ExternalIdentityCatalog>,
    generation: Mutex<Option<LoadedCatalog>>,
}

impl GameDataGateway {
    pub fn new(config: GameDataGatewayConfig) -> Result<Self> {
        let database_path = validate_database_path(&config.database_path)?;
        let external_identities = Arc::new(ExternalIdentityCatalog::bundled()?);
        Ok(Self {
            database_path,
            catalog_cache_dir: config.catalog_cache_dir,
            cache: config.cache,
            external_identities,
            generation: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    async fn catalog_generation(&self) -> Result<LoadedCatalog> {
        let stamp = database_stamp(&self.database_path)?;
        let mut generation = self.generation.lock().await;
        if let Some(current) = generation.as_ref().filter(|current| current.stamp == stamp) {
            return Ok(current.clone());
        }

        let database_path = self.database_path.clone();
        let catalog_cache_dir = self.catalog_cache_dir.clone();
        let external_identity_content_id = self.external_identities.content_id().to_owned();
        let computed = tokio::task::spawn_blocking(move || {
            let mut attempt_stamp = stamp;
            for attempt in 0..3 {
                match load_or_rebuild(
                    &database_path,
                    &catalog_cache_dir,
                    attempt_stamp,
                    &external_identity_content_id,
                ) {
                    Ok(catalog) => return Ok(catalog),
                    Err(error)
                        if attempt < 2
                            && error
                                .to_string()
                                .contains("changed while rebuilding the catalog snapshot") =>
                    {
                        attempt_stamp = database_stamp(&database_path)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            unreachable!("bounded catalog rebuild loop returns on every branch")
        })
        .await
        .context("GameData.db catalog generation task failed")??;
        *generation = Some(computed.clone());
        Ok(computed)
    }

    fn response_cache_key(
        &self,
        identity: &CatalogIdentity,
        endpoint: &str,
        query: &[(String, String)],
    ) -> String {
        let mut query = query.to_vec();
        query.sort();
        let mut digest = Sha256::new();
        digest.update(identity.cache_key.as_bytes());
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

    async fn query(
        &self,
        endpoint: &'static str,
        query: &[(String, String)],
        generation: &LoadedCatalog,
    ) -> Result<Vec<u8>> {
        match endpoint {
            "search_cards" => search_cards(
                generation.cards.as_slice(),
                query,
                &generation.identity.content_id,
                &self.external_identities,
            ),
            "get_card" => get_card(generation.cards.as_slice(), query),
            _ => bail!("unsupported endpoint {endpoint:?}"),
        }
    }
}

#[async_trait]
impl CatalogGateway for GameDataGateway {
    async fn status(&self) -> Result<CatalogStatus> {
        let generation = self.catalog_generation().await?;
        Ok(CatalogStatus {
            identity: generation.identity,
            source: "the-bazaar/GameData.db",
            card_count: u32::try_from(generation.cards.len()).context("card count exceeds u32")?,
            offline: true,
            read_only: true,
            action_authority: false,
            authority: crate::catalog::INSPECTION_AUTHORITY,
            authorizes_action: false,
        })
    }

    async fn search_catalog(&self, request: &SearchCardsRequest) -> Result<CatalogSearchResponse> {
        let generation = self.catalog_generation().await?;
        let body = search_cards(
            generation.cards.as_slice(),
            &request.query_pairs(),
            &generation.identity.content_id,
            &self.external_identities,
        )?;
        let page = serde_json::from_slice::<SearchCardsPage>(&body)
            .context("local catalog search produced an invalid page")?;
        Ok(CatalogSearchResponse {
            identity: generation.identity,
            page: page.page,
            limit: page.limit,
            count: page.count,
            total: page.total,
            cards: page
                .cards
                .into_iter()
                .map(serde_json::from_value)
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("local catalog search produced an invalid compact projection")?,
            authority: crate::catalog::INSPECTION_AUTHORITY,
            authorizes_action: false,
        })
    }

    async fn resolve_catalog(&self, request: &ResolveBatchRequest) -> Result<ResolveBatchResponse> {
        let generation = self.catalog_generation().await?;
        let results = request
            .requests
            .iter()
            .map(|requested| {
                let template_id = requested.template_id.to_string();
                let card = generation
                    .cards
                    .iter()
                    .find(|card| card.row_id == template_id);
                resolve_card_with_context(
                    requested.template_id.clone(),
                    requested.tier,
                    card,
                    requested.enchantment_id.as_ref(),
                    resolution_key(
                        &generation.identity,
                        &requested.template_id,
                        requested.tier,
                        requested.enchantment_id.as_ref(),
                        request.include_all_enchantments,
                    ),
                    &ResolveCardContext {
                        include_raw_template: request.include_raw_template,
                        include_all_enchantments: request.include_all_enchantments,
                        catalog_content_id: &generation.identity.content_id,
                        external_identities: &self.external_identities,
                    },
                )
            })
            .collect::<Vec<_>>();
        if request.mode == crate::ResolveMode::Strict {
            let unknown_enchantment =
                results
                    .iter()
                    .zip(&request.requests)
                    .any(|(result, requested)| {
                        requested.enchantment_id.is_some()
                            && !result.enchantments.missing.is_empty()
                    });
            let failures = results
                .iter()
                .enumerate()
                .filter(|(_, result)| !result.complete)
                .map(|(index, result)| {
                    json!({
                        "index": index,
                        "templateId": result.template_id,
                        "tier": result.tier,
                        "found": result.found,
                        "missing": result.missing,
                        "malformed": result.malformed,
                    })
                })
                .collect::<Vec<_>>();
            if !failures.is_empty() {
                return Err(crate::CatalogContractError::new(
                    if unknown_enchantment {
                        "unknown_enchantment"
                    } else {
                        "resolve_incomplete"
                    },
                    if unknown_enchantment {
                        "strict catalog resolve rejected an unknown enchantment".to_owned()
                    } else {
                        "strict catalog resolve failed closed".to_owned()
                    },
                    json!({"failures": failures}),
                )
                .into());
            }
        }
        Ok(ResolveBatchResponse {
            identity: generation.identity,
            results,
            authority: crate::catalog::INSPECTION_AUTHORITY,
            authorizes_action: false,
        })
    }
}

#[async_trait]
impl ApiGateway for GameDataGateway {
    async fn get_json(
        &self,
        endpoint: &'static str,
        query: Vec<(String, String)>,
        ttl: Duration,
        cache_mode: CacheMode,
    ) -> Result<ApiResponse> {
        if !matches!(endpoint, "search_cards" | "get_card") {
            bail!("unsupported endpoint {endpoint:?}");
        }

        let generation = self.catalog_generation().await?;
        let key = self.response_cache_key(&generation.identity, endpoint, &query);
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
        }

        match self.query(endpoint, &query, &generation).await {
            Ok(body) => {
                let expires_at = now.saturating_add(ttl.as_secs());
                self.cache
                    .put(
                        key,
                        CacheEntry {
                            stored_at: now,
                            expires_at,
                            stale_until: expires_at,
                            body: body.clone(),
                        },
                    )
                    .await?;
                let cache = match cache_mode {
                    CacheMode::Use => CacheDisposition::Miss,
                    CacheMode::Refresh => CacheDisposition::Refresh,
                    CacheMode::Offline => CacheDisposition::Offline,
                };
                Ok(ApiResponse { body, cache })
            }
            Err(error) => {
                if let Some(entry) = cached.filter(|entry| entry.stale_until >= now) {
                    tracing::warn!(%error, endpoint, "using cached local GameData response");
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

#[must_use]
pub fn detect_game_data_path() -> Option<PathBuf> {
    let mut roots = Vec::new();

    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let local_app_data = PathBuf::from(local_app_data);
        if let Some(app_data) = local_app_data.parent() {
            roots.push(app_data.join("LocalLow/Tempo Storm/The Bazaar"));
        }
    }
    if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        roots.push(PathBuf::from(user_profile).join("AppData/LocalLow/Tempo Storm/The Bazaar"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join("Library/Application Support/Tempo Storm/The Bazaar"));
        for steam_root in [home.join(".local/share/Steam"), home.join(".steam/steam")] {
            roots.push(
                steam_root
                    .join("steamapps/compatdata/1617400/pfx/drive_c/users/steamuser")
                    .join("AppData/LocalLow/Tempo Storm/The Bazaar"),
            );
        }
    }

    roots.sort();
    roots.dedup();
    let mut candidates = roots
        .into_iter()
        .flat_map(|root| database_candidates(&root))
        .filter_map(|path| {
            let modified = path.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    candidates
        .into_iter()
        .find_map(|(_, path)| validate_database_path(&path).ok())
}

fn database_candidates(root: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![
        root.join("prod/cache/GameData.db"),
        root.join("cache/GameData.db"),
    ];
    if let Ok(entries) = root.read_dir() {
        for entry in entries.flatten().filter(|entry| entry.path().is_dir()) {
            candidates.push(entry.path().join("cache/GameData.db"));
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn validate_database_path(path: &Path) -> Result<PathBuf> {
    let metadata = path
        .metadata()
        .with_context(|| format!("GameData.db does not exist at {}", path.display()))?;
    if !metadata.is_file() {
        bail!("GameData.db path is not a file: {}", path.display());
    }
    if metadata.len() > MAX_DATABASE_BYTES {
        bail!("GameData.db exceeds the 1 GiB safety limit");
    }

    let mut header = [0_u8; SQLITE_HEADER.len()];
    File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?
        .read_exact(&mut header)
        .context("failed to read GameData.db header")?;
    if &header != SQLITE_HEADER {
        bail!("GameData.db is not a SQLite 3 database");
    }
    path.canonicalize()
        .with_context(|| format!("failed to resolve {}", path.display()))
}

fn search_cards(
    cards: &[NormalizedCard],
    query: &[(String, String)],
    catalog_content_id: &str,
    external_identities: &ExternalIdentityCatalog,
) -> Result<Vec<u8>> {
    let query = query_map(query);
    let page = parse_u32(&query, "page")?;
    let limit = parse_u32(&query, "limit")?;
    if limit == 0 {
        bail!("limit must be greater than zero");
    }
    let category = required(&query, "category")?;
    let show_unobtainable = required(&query, "show_unobtainable")?
        .parse::<bool>()
        .context("show_unobtainable must be true or false")?;
    let search = query.get("query").map(|value| value.to_lowercase());
    let sort_by = required(&query, "sort_by")?;
    let descending = match required(&query, "order")? {
        "ascending" => false,
        "descending" => true,
        _ => bail!("order must be ascending or descending"),
    };

    let mut matches = cards
        .iter()
        .filter(|card| category_matches(&card.payload, category))
        .filter(|card| show_unobtainable || is_obtainable(&card.payload))
        .filter(|card| {
            search.as_ref().is_none_or(|query| {
                serde_json::to_string(&card.payload)
                    .is_ok_and(|body| body.to_lowercase().contains(query))
            })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| compare_cards(left, right, sort_by, search.as_deref()));
    if descending {
        matches.reverse();
    }

    let total = u32::try_from(matches.len()).context("card count exceeds u32")?;
    let start = (page as usize).saturating_mul(limit as usize);
    let page_cards = matches
        .into_iter()
        .skip(start)
        .take(limit as usize)
        .map(|card| {
            project_card_with_context(
                &card.row_id,
                &card.payload,
                catalog_content_id,
                external_identities,
            )
        })
        .collect::<Vec<_>>();
    serde_json::to_vec(&json!({
        "page": page,
        "limit": limit,
        "count": page_cards.len(),
        "total": total,
        "cards": page_cards,
    }))
    .context("failed to serialize local card search")
}

fn get_card(cards: &[NormalizedCard], query: &[(String, String)]) -> Result<Vec<u8>> {
    let query = query_map(query);
    let name = required(&query, "name")?.trim();
    if name.is_empty() {
        bail!("card name is required");
    }

    let card = cards
        .iter()
        .filter(|card| {
            [
                Some(card.row_id.as_str()),
                display_name(&card.payload),
                internal_name(&card.payload),
            ]
            .into_iter()
            .flatten()
            .any(|value| value.eq_ignore_ascii_case(name))
        })
        .min_by(|left, right| left.row_id.cmp(&right.row_id))
        .with_context(|| format!("card not found in local GameData.db: {name}"))?;
    serde_json::to_vec(&json!({"data": card.payload})).context("failed to serialize local card")
}

fn project_card_with_context(
    row_id: &str,
    card: &Value,
    catalog_content_id: &str,
    external_identities: &ExternalIdentityCatalog,
) -> CatalogCardProjection {
    let mut missing = Vec::new();
    let mut malformed = Vec::new();
    if row_id.parse::<CanonicalUuid>().is_err() {
        malformed.push("template.rowId:not_canonical_uuid".to_owned());
    }
    let payload_id_consistency = match card.get("Id") {
        None => PayloadIdConsistency::Absent,
        Some(Value::String(payload_id)) if payload_id == row_id => PayloadIdConsistency::Matching,
        Some(Value::String(payload_id)) => {
            malformed.push(format!("template.payloadId:mismatch:{payload_id}"));
            PayloadIdConsistency::Mismatch
        }
        Some(_) => {
            malformed.push("template.payloadId:expected_string".to_owned());
            PayloadIdConsistency::Malformed
        }
    };
    let card_type =
        required_string_field(card, "Type", "template.type", &mut missing, &mut malformed);
    let version = optional_string_field(card, "Version", "template.version", &mut malformed);
    let starting_tier = required_string_field(
        card,
        "StartingTier",
        "template.startingTier",
        &mut missing,
        &mut malformed,
    )
    .and_then(|value| match value.parse() {
        Ok(tier) => Some(tier),
        Err(_) => {
            malformed.push("template.startingTier:invalid".to_owned());
            None
        }
    });
    let size = match card.get("Size") {
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            malformed.push("template.size:expected_string".to_owned());
            None
        }
        None if card_type.as_deref() == Some("Item") || card_type.is_none() => {
            missing.push("template.size".to_owned());
            None
        }
        None => None,
    };
    let tags = required_string_array(card, "Tags", "template.tags", &mut missing, &mut malformed);
    let hidden_tags =
        optional_string_array(card, "HiddenTags", "template.hiddenTags", &mut malformed);
    validate_optional_array(card, "Tooltips", "template.tooltips", &mut malformed);
    let tooltips = resolve_tooltips(card);
    missing.extend(tooltips.missing.iter().cloned());
    malformed.extend(tooltips.malformed.iter().cloned());
    let name = display_name(card)
        .or_else(|| internal_name(card))
        .map(str::to_owned);
    let external_references =
        match external_identities.reference_for(row_id, name.as_deref(), card_type.as_deref()) {
            Ok(references) => references,
            Err(detail) => {
                tracing::debug!(
                    template_id = row_id,
                    reason = detail,
                    "external identity reference omitted"
                );
                Vec::new()
            }
        };
    CatalogCardProjection {
        template_id: row_id.to_owned(),
        template_content_id: template_content_id(row_id, card, catalog_content_id),
        payload_id_consistency,
        complete: missing.is_empty() && malformed.is_empty(),
        missing,
        malformed,
        name,
        card_type,
        version,
        starting_tier,
        size,
        tags,
        hidden_tags,
        tooltips,
        external_references,
    }
}

#[cfg(test)]
fn project_card(row_id: &str, card: &Value) -> CatalogCardProjection {
    let external_identities = ExternalIdentityCatalog::bundled().unwrap();
    project_card_with_context(
        row_id,
        card,
        "sha256:test-catalog-content",
        &external_identities,
    )
}

fn template_content_id(row_id: &str, card: &Value, catalog_content_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"bazaardb-cli/template-definition\0");
    digest.update(CATALOG_SCHEMA_VERSION.as_bytes());
    digest.update(b"\0");
    digest.update(RESOLVER_VERSION.as_bytes());
    digest.update(b"\0");
    digest.update(catalog_content_id.as_bytes());
    digest.update(b"\0");
    digest.update(row_id.as_bytes());
    digest.update(b"\0");
    digest.update(serde_json::to_vec(card).expect("serializing a serde_json::Value cannot fail"));
    format!(
        "sha256:{}",
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn resolve_tooltips(card: &Value) -> TooltipResolution {
    let localization = match card.get("Localization") {
        None => {
            return TooltipResolution {
                shape: TooltipShape::Absent,
                values: Vec::new(),
                missing: Vec::new(),
                malformed: Vec::new(),
                complete: true,
            };
        }
        Some(Value::Object(localization)) => localization,
        Some(_) => {
            return TooltipResolution {
                shape: TooltipShape::Malformed,
                values: Vec::new(),
                missing: Vec::new(),
                malformed: vec!["template.localization:expected_object".to_owned()],
                complete: false,
            };
        }
    };
    let tooltips = match localization.get("Tooltips") {
        None => {
            return TooltipResolution {
                shape: TooltipShape::Absent,
                values: Vec::new(),
                missing: Vec::new(),
                malformed: Vec::new(),
                complete: true,
            };
        }
        Some(Value::Null) => {
            return TooltipResolution {
                shape: TooltipShape::Null,
                values: Vec::new(),
                missing: Vec::new(),
                malformed: Vec::new(),
                complete: true,
            };
        }
        Some(Value::Array(tooltips)) => tooltips,
        Some(_) => {
            return TooltipResolution {
                shape: TooltipShape::Malformed,
                values: Vec::new(),
                missing: Vec::new(),
                malformed: vec!["template.tooltips:expected_array".to_owned()],
                complete: false,
            };
        }
    };
    let mut values = Vec::new();
    let mut missing = Vec::new();
    let mut malformed = Vec::new();
    for (index, tooltip) in tooltips.iter().enumerate() {
        let Some(tooltip) = tooltip.as_object() else {
            malformed.push(format!("template.tooltips[{index}]:expected_object"));
            continue;
        };
        let Some(content) = tooltip.get("Content") else {
            missing.push(format!("template.tooltips[{index}].content"));
            continue;
        };
        let Some(content) = content.as_object() else {
            malformed.push(format!(
                "template.tooltips[{index}].content:expected_object"
            ));
            continue;
        };
        match content.get("Text") {
            Some(Value::String(text)) => values.push(text.clone()),
            Some(_) => malformed.push(format!(
                "template.tooltips[{index}].content.text:expected_string"
            )),
            None => missing.push(format!("template.tooltips[{index}].content.text")),
        }
    }
    TooltipResolution {
        shape: TooltipShape::Array,
        values,
        complete: missing.is_empty() && malformed.is_empty(),
        missing,
        malformed,
    }
}

struct ResolveCardContext<'a> {
    include_raw_template: bool,
    include_all_enchantments: bool,
    catalog_content_id: &'a str,
    external_identities: &'a ExternalIdentityCatalog,
}

fn resolve_card_with_context(
    template_id: CanonicalUuid,
    tier: CardTier,
    card: Option<&NormalizedCard>,
    enchantment_id: Option<&CanonicalGameIdentifier>,
    resolution_key: String,
    context: &ResolveCardContext<'_>,
) -> ResolvedCard {
    let Some(card) = card else {
        return ResolvedCard {
            resolution_key,
            template_content_id: None,
            template_id,
            found: false,
            complete: false,
            tier,
            starting_tier: None,
            size: None,
            tags: Vec::new(),
            hidden_tags: Vec::new(),
            template: None,
            raw_template: None,
            attributes: AttributeResolution::missing_template(),
            abilities: ComponentResolution::missing_template(),
            auras: ComponentResolution::missing_template(),
            enchantments: ComponentResolution::missing_template(),
            missing: vec!["template".to_owned()],
            malformed: Vec::new(),
        };
    };
    let row_id = card.row_id.as_str();
    let card = &card.payload;
    let projection = project_card_with_context(
        row_id,
        card,
        context.catalog_content_id,
        context.external_identities,
    );

    let starting_tier = string_field(card, "StartingTier").and_then(|value| value.parse().ok());
    let (attributes, ability_ids, aura_ids, mut missing, mut malformed) =
        resolve_tier_layers(card, starting_tier, tier);
    let abilities = resolve_components(card, "Abilities", ability_ids);
    let auras = resolve_components(card, "Auras", aura_ids);
    let enchantments = resolve_enchantments(card, enchantment_id, context.include_all_enchantments);
    missing.extend(abilities.missing.iter().map(|id| format!("abilities:{id}")));
    missing.extend(auras.missing.iter().map(|id| format!("auras:{id}")));
    missing.extend(
        enchantments
            .missing
            .iter()
            .map(|id| format!("enchantments:{id}")),
    );
    malformed.extend(
        abilities
            .malformed
            .iter()
            .map(|detail| format!("abilities:{detail}")),
    );
    malformed.extend(
        auras
            .malformed
            .iter()
            .map(|detail| format!("auras:{detail}")),
    );
    malformed.extend(
        enchantments
            .malformed
            .iter()
            .map(|detail| format!("enchantments:{detail}")),
    );
    missing.extend(projection.missing.iter().cloned());
    malformed.extend(projection.malformed.iter().cloned());
    let template_content_id = projection.template_content_id.clone();
    let complete = missing.is_empty()
        && malformed.is_empty()
        && projection.complete
        && attributes.complete
        && abilities.complete
        && auras.complete
        && enchantments.complete;

    ResolvedCard {
        resolution_key,
        template_content_id: Some(template_content_id),
        template_id,
        found: true,
        complete,
        tier,
        starting_tier,
        size: string_field(card, "Size").map(str::to_owned),
        tags: string_array(card, "Tags").map(str::to_owned).collect(),
        hidden_tags: string_array(card, "HiddenTags")
            .map(str::to_owned)
            .collect(),
        template: Some(projection),
        raw_template: context.include_raw_template.then(|| card.clone()),
        attributes,
        abilities,
        auras,
        enchantments,
        missing,
        malformed,
    }
}

#[cfg(test)]
fn resolve_card(
    template_id: CanonicalUuid,
    tier: CardTier,
    card: Option<&NormalizedCard>,
    include_raw_template: bool,
    enchantment_id: Option<&CanonicalGameIdentifier>,
    include_all_enchantments: bool,
    resolution_key: String,
) -> ResolvedCard {
    let external_identities = ExternalIdentityCatalog::bundled().unwrap();
    resolve_card_with_context(
        template_id,
        tier,
        card,
        enchantment_id,
        resolution_key,
        &ResolveCardContext {
            include_raw_template,
            include_all_enchantments,
            catalog_content_id: "sha256:test-catalog-content",
            external_identities: &external_identities,
        },
    )
}

fn resolve_tier_layers(
    card: &Value,
    starting_tier: Option<CardTier>,
    target_tier: CardTier,
) -> (
    AttributeResolution,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
) {
    let mut values = BTreeMap::new();
    let mut tiers_applied = Vec::new();
    let mut ability_ids = Vec::new();
    let mut aura_ids = Vec::new();
    let mut missing = Vec::new();
    let mut attribute_missing = Vec::new();
    let mut malformed = Vec::new();
    let Some(starting_tier) = starting_tier else {
        let starting_tier_present = card.get("StartingTier").is_some();
        let tier_missing = if starting_tier_present {
            Vec::new()
        } else {
            vec!["startingTier".to_owned()]
        };
        let tier_malformed = if starting_tier_present {
            vec!["startingTier:invalid".to_owned()]
        } else {
            Vec::new()
        };
        let resolution = AttributeResolution {
            values,
            tiers_applied,
            missing: tier_missing.clone(),
            malformed: tier_malformed.clone(),
            complete: false,
        };
        return (
            resolution,
            ability_ids,
            aura_ids,
            tier_missing,
            tier_malformed,
        );
    };
    if target_tier.rank() < starting_tier.rank() {
        let detail = format!("tier:{}:belowStartingTier:{starting_tier}", target_tier);
        let resolution = AttributeResolution {
            values,
            tiers_applied,
            missing: vec![detail.clone()],
            malformed: Vec::new(),
            complete: false,
        };
        return (resolution, ability_ids, aura_ids, vec![detail], malformed);
    }

    let tiers = match card.get("Tiers") {
        None => {
            attribute_missing.push("tiers".to_owned());
            missing.push("tiers".to_owned());
            None
        }
        Some(Value::Object(tiers)) => Some(tiers),
        Some(Value::Null) => {
            malformed.push("tiers:null".to_owned());
            None
        }
        Some(_) => {
            malformed.push("tiers:expected_object".to_owned());
            None
        }
    };
    let mut target_available = false;
    for layer in CardTier::ordered()
        .iter()
        .copied()
        .filter(|layer| layer.rank() >= starting_tier.rank() && layer.rank() <= target_tier.rank())
    {
        let layer_name = layer.to_string();
        let Some(layer_value) = tiers.and_then(|tiers| tiers.get(&layer_name)) else {
            continue;
        };
        if layer == target_tier {
            target_available = true;
        }
        let Some(layer_value) = layer_value.as_object() else {
            malformed.push(format!("tiers.{layer_name}:expected_object"));
            continue;
        };
        tiers_applied.push(layer);
        match layer_value.get("Attributes") {
            None => {
                let detail = format!("tiers.{layer_name}.attributes");
                attribute_missing.push(detail.clone());
                missing.push(detail);
            }
            Some(Value::Object(attributes)) => {
                for (key, value) in attributes {
                    values.insert(key.clone(), value.clone());
                }
            }
            Some(Value::Null) => malformed.push(format!("tiers.{layer_name}.attributes:null")),
            Some(_) => malformed.push(format!("tiers.{layer_name}.attributes:expected_object")),
        }
        append_unique_ids(
            &mut ability_ids,
            layer_value.get("AbilityIds"),
            &format!("tiers.{layer_name}.abilityIds"),
            &mut malformed,
        );
        append_unique_ids(
            &mut aura_ids,
            layer_value.get("AuraIds"),
            &format!("tiers.{layer_name}.auraIds"),
            &mut malformed,
        );
    }

    if !target_available {
        let detail = format!("tier:{target_tier}");
        attribute_missing.push(detail.clone());
        missing.push(detail);
    }
    let resolution = AttributeResolution {
        values,
        tiers_applied,
        complete: attribute_missing.is_empty() && malformed.is_empty(),
        missing: attribute_missing,
        malformed: malformed.clone(),
    };
    (resolution, ability_ids, aura_ids, missing, malformed)
}

fn append_unique_ids(
    target: &mut Vec<String>,
    value: Option<&Value>,
    path: &str,
    malformed: &mut Vec<String>,
) {
    let existing = target.iter().cloned().collect::<BTreeSet<_>>();
    let mut seen = existing;
    let Some(value) = value else {
        return;
    };
    let Value::Array(ids) = value else {
        malformed.push(format!(
            "{path}:{}",
            if value.is_null() {
                "null"
            } else {
                "expected_array"
            }
        ));
        return;
    };
    for (index, value) in ids.iter().enumerate() {
        let Some(id) = value.as_str() else {
            malformed.push(format!("{path}[{index}]:expected_string"));
            continue;
        };
        if seen.insert(id.to_owned()) {
            target.push(id.to_owned());
        }
    }
}

fn resolve_components(card: &Value, key: &str, ids: Vec<String>) -> ComponentResolution {
    let (shape, definitions, mut malformed) = match card.get(key) {
        None => (ComponentShape::Absent, None, Vec::new()),
        Some(Value::Null) => (ComponentShape::Null, None, Vec::new()),
        Some(Value::Object(definitions)) => (ComponentShape::Object, Some(definitions), Vec::new()),
        Some(_) => (
            ComponentShape::Malformed,
            None,
            vec!["definition:expected_object".to_owned()],
        ),
    };
    let mut values = BTreeMap::new();
    let mut missing = Vec::new();
    for id in &ids {
        match definitions.and_then(|definitions| definitions.get(id)) {
            Some(value) if value.is_null() => malformed.push(format!("{id}:null")),
            Some(value) if value.is_object() => {
                values.insert(id.clone(), value.clone());
            }
            Some(_) => malformed.push(format!("{id}:expected_object")),
            None => missing.push(id.clone()),
        }
    }
    ComponentResolution {
        status: if missing.is_empty() && malformed.is_empty() {
            ComponentStatus::Complete
        } else {
            ComponentStatus::Incomplete
        },
        shape,
        ids,
        complete: missing.is_empty() && malformed.is_empty(),
        values,
        missing,
        malformed,
    }
}

fn resolve_enchantments(
    card: &Value,
    enchantment_id: Option<&CanonicalGameIdentifier>,
    include_all: bool,
) -> ComponentResolution {
    let (shape, definitions) = match card.get("Enchantments") {
        None => (ComponentShape::Absent, None),
        Some(Value::Null) => (ComponentShape::Null, None),
        Some(Value::Object(definitions)) => (ComponentShape::Object, Some(definitions)),
        Some(_) => (ComponentShape::Malformed, None),
    };
    if enchantment_id.is_none() && !include_all {
        let malformed = (shape == ComponentShape::Malformed)
            .then(|| "definition:expected_object".to_owned())
            .into_iter()
            .collect::<Vec<_>>();
        return ComponentResolution {
            status: if malformed.is_empty() {
                ComponentStatus::NotRequested
            } else {
                ComponentStatus::Incomplete
            },
            shape,
            ids: Vec::new(),
            values: BTreeMap::new(),
            missing: Vec::new(),
            complete: malformed.is_empty(),
            malformed,
        };
    }
    if shape == ComponentShape::Malformed {
        return ComponentResolution {
            status: ComponentStatus::Incomplete,
            shape,
            ids: enchantment_id
                .map(ToString::to_string)
                .into_iter()
                .collect(),
            values: BTreeMap::new(),
            missing: Vec::new(),
            malformed: vec!["definition:expected_object".to_owned()],
            complete: false,
        };
    }
    let ids = if include_all {
        definitions
            .into_iter()
            .flat_map(|definitions| definitions.keys().cloned())
            .collect::<Vec<_>>()
    } else {
        vec![enchantment_id.expect("selection was checked").to_string()]
    };
    let mut values = BTreeMap::new();
    let mut missing = Vec::new();
    let mut malformed = Vec::new();
    for id in &ids {
        match definitions.and_then(|definitions| definitions.get(id)) {
            Some(value) if value.is_null() => malformed.push(format!("{id}:null")),
            Some(value) if value.is_object() => {
                values.insert(id.clone(), value.clone());
            }
            Some(_) => malformed.push(format!("{id}:expected_object")),
            None => missing.push(id.clone()),
        }
    }
    ComponentResolution {
        status: if missing.is_empty() && malformed.is_empty() {
            ComponentStatus::Complete
        } else {
            ComponentStatus::Incomplete
        },
        shape,
        ids,
        complete: missing.is_empty() && malformed.is_empty(),
        values,
        missing,
        malformed,
    }
}

fn resolution_key(
    identity: &CatalogIdentity,
    template_id: &CanonicalUuid,
    tier: CardTier,
    enchantment_id: Option<&CanonicalGameIdentifier>,
    include_all_enchantments: bool,
) -> String {
    format!(
        "resolve/{}/{template_id}/{tier}/{}",
        identity.content_id,
        selector_key(enchantment_id, include_all_enchantments)
    )
}

fn query_map(query: &[(String, String)]) -> BTreeMap<&str, &str> {
    query
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect()
}

fn required<'a>(query: &'a BTreeMap<&str, &str>, key: &str) -> Result<&'a str> {
    query
        .get(key)
        .copied()
        .with_context(|| format!("missing query parameter {key}"))
}

fn parse_u32(query: &BTreeMap<&str, &str>, key: &str) -> Result<u32> {
    required(query, key)?
        .parse::<u32>()
        .with_context(|| format!("{key} must be an unsigned integer"))
}

fn category_matches(card: &Value, requested: &str) -> bool {
    let category = card_category(card);
    requested == "all" && category.is_some() || category == Some(requested)
}

fn card_category(card: &Value) -> Option<&'static str> {
    if string_array(card, "Tags").any(|tag| tag.eq_ignore_ascii_case("Merchant")) {
        return Some("merchants");
    }
    if is_trainer(card) {
        return Some("trainers");
    }
    match string_field(card, "Type")? {
        "Item" => Some("items"),
        "Skill" => Some("skills"),
        "CombatEncounter" => Some("monsters"),
        "EventEncounter" | "EncounterStep" | "PedestalEncounter" => Some("events"),
        _ => None,
    }
}

fn is_trainer(card: &Value) -> bool {
    if string_field(card, "Type") != Some("EventEncounter") {
        return false;
    }
    let internal = internal_name(card).unwrap_or_default();
    let title = display_name(card).unwrap_or_default();
    internal.to_ascii_lowercase().contains("level up") || matches!(title, "Nonna" | "Nufu")
}

fn is_obtainable(card: &Value) -> bool {
    if string_field(card, "SpawningEligibility") == Some("Never") {
        return false;
    }
    let internal = internal_name(card).unwrap_or_default().to_ascii_lowercase();
    !internal.contains("[debug]") && !internal.contains("template")
}

fn compare_cards(
    left: &NormalizedCard,
    right: &NormalizedCard,
    sort_by: &str,
    query: Option<&str>,
) -> Ordering {
    let left_payload = &left.payload;
    let right_payload = &right.payload;
    let field = sort_by.to_ascii_lowercase();
    let primary = match field.as_str() {
        "auto" if query.is_some() => relevance(left_payload, query.unwrap_or_default())
            .cmp(&relevance(right_payload, query.unwrap_or_default())),
        "tier" | "base_tier" | "basetier" | "startingtier" => {
            tier_rank(left_payload).cmp(&tier_rank(right_payload))
        }
        "size" => size_rank(left_payload).cmp(&size_rank(right_payload)),
        "type" => {
            normalized_field(left_payload, "Type").cmp(&normalized_field(right_payload, "Type"))
        }
        _ => normalized_name(left_payload).cmp(&normalized_name(right_payload)),
    };
    primary
        .then_with(|| normalized_name(left_payload).cmp(&normalized_name(right_payload)))
        .then_with(|| left.row_id.cmp(&right.row_id))
}

fn relevance(card: &Value, query: &str) -> u8 {
    let name = normalized_name(card);
    if name == query {
        0
    } else if name.starts_with(query) {
        1
    } else {
        2
    }
}

fn tier_rank(card: &Value) -> u8 {
    match string_field(card, "StartingTier") {
        Some("Bronze") => 0,
        Some("Silver") => 1,
        Some("Gold") => 2,
        Some("Diamond") => 3,
        Some("Legendary") => 4,
        _ => 5,
    }
}

fn size_rank(card: &Value) -> u8 {
    match string_field(card, "Size") {
        Some("Small") => 0,
        Some("Medium") => 1,
        Some("Large") => 2,
        _ => 3,
    }
}

fn normalized_name(card: &Value) -> String {
    display_name(card)
        .or_else(|| internal_name(card))
        .unwrap_or_default()
        .to_lowercase()
}

fn normalized_field(card: &Value, key: &str) -> String {
    string_field(card, key).unwrap_or_default().to_lowercase()
}

fn display_name(card: &Value) -> Option<&str> {
    card.pointer("/Localization/Title/Text")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn internal_name(card: &Value) -> Option<&str> {
    string_field(card, "InternalName")
}

fn string_field<'a>(card: &'a Value, key: &str) -> Option<&'a str> {
    card.get(key).and_then(Value::as_str)
}

fn required_string_field(
    card: &Value,
    key: &str,
    path: &str,
    missing: &mut Vec<String>,
    malformed: &mut Vec<String>,
) -> Option<String> {
    match card.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            malformed.push(format!("{path}:expected_string"));
            None
        }
        None => {
            missing.push(path.to_owned());
            None
        }
    }
}

fn optional_string_field(
    card: &Value,
    key: &str,
    path: &str,
    malformed: &mut Vec<String>,
) -> Option<String> {
    match card.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            malformed.push(format!("{path}:expected_string"));
            None
        }
        None => None,
    }
}

fn required_string_array(
    card: &Value,
    key: &str,
    path: &str,
    missing: &mut Vec<String>,
    malformed: &mut Vec<String>,
) -> Vec<String> {
    match card.get(key) {
        Some(Value::Array(values)) => values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| match value.as_str() {
                Some(value) => Some(value.to_owned()),
                None => {
                    malformed.push(format!("{path}[{index}]:expected_string"));
                    None
                }
            })
            .collect(),
        Some(_) => {
            malformed.push(format!("{path}:expected_array"));
            Vec::new()
        }
        None => {
            missing.push(path.to_owned());
            Vec::new()
        }
    }
}

fn optional_string_array(
    card: &Value,
    key: &str,
    path: &str,
    malformed: &mut Vec<String>,
) -> Vec<String> {
    match card.get(key) {
        None => Vec::new(),
        Some(Value::Array(values)) => values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| match value.as_str() {
                Some(value) => Some(value.to_owned()),
                None => {
                    malformed.push(format!("{path}[{index}]:expected_string"));
                    None
                }
            })
            .collect(),
        Some(_) => {
            malformed.push(format!("{path}:expected_array"));
            Vec::new()
        }
    }
}

fn validate_optional_array(card: &Value, key: &str, path: &str, malformed: &mut Vec<String>) {
    if card.get(key).is_some_and(|value| !value.is_array()) {
        malformed.push(format!("{path}:expected_array"));
    }
}

fn string_array<'a>(card: &'a Value, key: &str) -> impl Iterator<Item = &'a str> {
    card.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn now_epoch_seconds() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str, kind: &str, internal: &str, tags: &[&str]) -> Value {
        json!({
            "Id": format!("id-{name}"),
            "InternalName": internal,
            "Type": kind,
            "StartingTier": "Silver",
            "Size": "Small",
            "Tags": tags,
            "SpawningEligibility": "Always",
            "Localization": {"Title": {"Text": name}},
        })
    }

    fn normalized(payload: Value) -> NormalizedCard {
        NormalizedCard {
            row_id: payload["Id"].as_str().unwrap().to_owned(),
            payload,
        }
    }

    #[test]
    fn classifies_all_documented_categories() {
        assert_eq!(
            card_category(&card("Item", "Item", "Item", &[])),
            Some("items")
        );
        assert_eq!(
            card_category(&card("Skill", "Skill", "Skill", &[])),
            Some("skills")
        );
        assert_eq!(
            card_category(&card("Shop", "EventEncounter", "Shop", &["Merchant"])),
            Some("merchants")
        );
        assert_eq!(
            card_category(&card("Coach", "EventEncounter", "Coach (Level Up)", &[])),
            Some("trainers")
        );
        assert_eq!(
            card_category(&card("Monster", "CombatEncounter", "Monster", &[])),
            Some("monsters")
        );
        assert_eq!(
            card_category(&card("Event", "EventEncounter", "Event", &[])),
            Some("events")
        );
    }

    #[test]
    fn local_search_is_zero_based_and_preserves_complete_objects() {
        let cards = vec![
            normalized(card("Alpha", "Item", "Alpha", &[])),
            normalized(card("Beta", "Item", "Beta", &[])),
            normalized(card("Gamma", "Skill", "Gamma", &[])),
        ];
        let query = vec![
            ("page".to_owned(), "1".to_owned()),
            ("limit".to_owned(), "1".to_owned()),
            ("order".to_owned(), "ascending".to_owned()),
            ("sort_by".to_owned(), "name".to_owned()),
            ("category".to_owned(), "items".to_owned()),
            ("show_unobtainable".to_owned(), "false".to_owned()),
        ];
        let external_identities = ExternalIdentityCatalog::bundled().unwrap();
        let value: Value = serde_json::from_slice(
            &search_cards(
                &cards,
                &query,
                "sha256:test-catalog-content",
                &external_identities,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(value["total"], 2);
        assert_eq!(value["cards"][0]["name"], "Beta");
    }

    #[test]
    fn resolver_accumulates_sparse_tiers_and_overrides_later_values() {
        let template_id = "0022c409-c839-41e8-8022-65a407457dfe";
        let card = normalized(json!({
            "Id": template_id,
            "InternalName": "GoldenFixture",
            "Type": "Item",
            "StartingTier": "Bronze",
            "Size": "Small",
            "Tags": ["Ammo"],
            "HiddenTags": ["Fixture"],
            "Localization": {"Title": {"Text": "Golden Fixture"}},
            "Tiers": {
                "Bronze": {
                    "Attributes": {"AmmoMax": 5, "Damage": 10},
                    "AbilityIds": ["bronze-ability"],
                    "AuraIds": []
                },
                "Silver": {
                    "Attributes": {"Damage": 20},
                    "AbilityIds": ["silver-ability", "bronze-ability"],
                    "AuraIds": ["silver-aura"]
                }
            },
            "Abilities": {
                "bronze-ability": {"kind": "bronze"},
                "silver-ability": {"kind": "silver"}
            },
            "Auras": {"silver-aura": {"kind": "silver"}},
            "Enchantments": null
        }));

        let resolved = resolve_card(
            template_id.parse().unwrap(),
            CardTier::Silver,
            Some(&card),
            false,
            None,
            false,
            "test".to_owned(),
        );

        assert!(resolved.complete);
        assert_eq!(resolved.attributes.values["AmmoMax"], 5);
        assert_eq!(resolved.attributes.values["Damage"], 20);
        assert_eq!(
            resolved.attributes.tiers_applied,
            [CardTier::Bronze, CardTier::Silver]
        );
        assert_eq!(resolved.abilities.ids, ["bronze-ability", "silver-ability"]);
        assert_eq!(resolved.auras.ids, ["silver-aura"]);
        assert_eq!(resolved.enchantments.shape, ComponentShape::Null);
        assert!(resolved.raw_template.is_none());
        assert_eq!(
            resolved.template.unwrap().name.as_deref(),
            Some("Golden Fixture")
        );
    }

    #[test]
    fn resolver_distinguishes_absent_null_and_malformed_component_shapes() {
        let template_id = "0022c409-c839-41e8-8022-65a407457dfe";
        let base = json!({
            "Id": template_id,
            "Type": "Item",
            "StartingTier": "Bronze",
            "Size": "Small",
            "Tags": [],
            "Tiers": {"Bronze": {"Attributes": {}}}
        });
        let absent_card = normalized(base.clone());
        let absent = resolve_card(
            template_id.parse().unwrap(),
            CardTier::Bronze,
            Some(&absent_card),
            false,
            None,
            false,
            "test".to_owned(),
        );
        assert!(absent.complete);
        assert_eq!(absent.abilities.shape, ComponentShape::Absent);
        assert_eq!(absent.auras.shape, ComponentShape::Absent);
        assert_eq!(absent.enchantments.shape, ComponentShape::Absent);

        let mut null = base.clone();
        null["Abilities"] = Value::Null;
        null["Auras"] = Value::Null;
        null["Enchantments"] = Value::Null;
        let null = normalized(null);
        let null = resolve_card(
            template_id.parse().unwrap(),
            CardTier::Bronze,
            Some(&null),
            false,
            None,
            false,
            "test".to_owned(),
        );
        assert!(null.complete);
        assert_eq!(null.abilities.shape, ComponentShape::Null);
        assert_eq!(null.auras.shape, ComponentShape::Null);
        assert_eq!(null.enchantments.shape, ComponentShape::Null);

        let mut malformed = base;
        malformed["Tiers"]["Bronze"]["AbilityIds"] = Value::Null;
        malformed["Tiers"]["Bronze"]["AuraIds"] = json!({});
        malformed["Abilities"] = json!([]);
        malformed["Auras"] = json!("wrong");
        malformed["Enchantments"] = json!([]);
        let malformed = normalized(malformed);
        let malformed = resolve_card(
            template_id.parse().unwrap(),
            CardTier::Bronze,
            Some(&malformed),
            false,
            None,
            false,
            "test".to_owned(),
        );
        assert!(!malformed.complete);
        assert_eq!(malformed.abilities.shape, ComponentShape::Malformed);
        assert_eq!(malformed.auras.shape, ComponentShape::Malformed);
        assert_eq!(malformed.enchantments.shape, ComponentShape::Malformed);
        assert!(
            malformed
                .malformed
                .iter()
                .any(|detail| detail.contains("abilityIds:null"))
        );
        assert!(
            malformed
                .malformed
                .iter()
                .any(|detail| detail.contains("auraIds:expected_array"))
        );
    }

    #[test]
    fn resolver_fails_tiers_before_start() {
        let template_id = "0022c409-c839-41e8-8022-65a407457dfe";
        let card = normalized(json!({
            "Id": template_id,
            "Type": "Item",
            "StartingTier": "Silver",
            "Size": "Small",
            "Tags": [],
            "Tiers": {"Silver": {"Attributes": {}}}
        }));
        let resolved = resolve_card(
            template_id.parse().unwrap(),
            CardTier::Bronze,
            Some(&card),
            false,
            None,
            false,
            "test".to_owned(),
        );
        assert!(!resolved.complete);
        assert!(resolved.missing[0].contains("belowStartingTier"));
    }

    #[test]
    fn resolver_selects_only_the_requested_enchantment() {
        let template_id = "0022c409-c839-41e8-8022-65a407457dfe";
        let card = normalized(json!({
            "Id": template_id,
            "Type": "Item",
            "StartingTier": "Bronze",
            "Size": "Small",
            "Tags": [],
            "Tiers": {"Bronze": {"Attributes": {}}},
            "Enchantments": {
                "Fiery": {"Damage": 10},
                "Broken": null
            }
        }));

        let not_requested = resolve_card(
            template_id.parse().unwrap(),
            CardTier::Bronze,
            Some(&card),
            false,
            None,
            false,
            "not-requested".to_owned(),
        );
        assert!(not_requested.complete);
        assert_eq!(
            not_requested.enchantments.status,
            ComponentStatus::NotRequested
        );
        assert!(not_requested.enchantments.values.is_empty());

        let fiery = "Fiery".parse::<CanonicalGameIdentifier>().unwrap();
        let known = resolve_card(
            template_id.parse().unwrap(),
            CardTier::Bronze,
            Some(&card),
            false,
            Some(&fiery),
            false,
            "known".to_owned(),
        );
        assert!(known.complete);
        assert_eq!(known.enchantments.ids, ["Fiery"]);
        assert_eq!(known.enchantments.values["Fiery"]["Damage"], 10);
        assert!(!known.enchantments.values.contains_key("Broken"));

        let unknown = "Unknown".parse::<CanonicalGameIdentifier>().unwrap();
        let missing = resolve_card(
            template_id.parse().unwrap(),
            CardTier::Bronze,
            Some(&card),
            false,
            Some(&unknown),
            false,
            "missing".to_owned(),
        );
        assert!(!missing.complete);
        assert_eq!(missing.enchantments.missing, ["Unknown"]);

        let broken = "Broken".parse::<CanonicalGameIdentifier>().unwrap();
        let malformed = resolve_card(
            template_id.parse().unwrap(),
            CardTier::Bronze,
            Some(&card),
            false,
            Some(&broken),
            false,
            "malformed".to_owned(),
        );
        assert!(!malformed.complete);
        assert_eq!(malformed.enchantments.malformed, ["Broken:null"]);
    }

    #[test]
    fn resolver_rejects_non_object_component_definitions() {
        let template_id = "0022c409-c839-41e8-8022-65a407457dfe";
        let card = normalized(json!({
            "Id": template_id,
            "Type": "Item",
            "StartingTier": "Bronze",
            "Size": "Small",
            "Tags": [],
            "Tiers": {
                "Bronze": {
                    "Attributes": {},
                    "AbilityIds": ["scalar"],
                    "AuraIds": ["array"]
                }
            },
            "Abilities": {"scalar": 7},
            "Auras": {"array": []},
            "Enchantments": {"Fiery": "wrong"}
        }));
        let fiery = "Fiery".parse::<CanonicalGameIdentifier>().unwrap();
        let resolved = resolve_card(
            template_id.parse().unwrap(),
            CardTier::Bronze,
            Some(&card),
            false,
            Some(&fiery),
            false,
            "test".to_owned(),
        );
        assert!(!resolved.complete);
        assert_eq!(resolved.abilities.malformed, ["scalar:expected_object"]);
        assert_eq!(resolved.auras.malformed, ["array:expected_object"]);
        assert_eq!(resolved.enchantments.malformed, ["Fiery:expected_object"]);
        assert!(resolved.abilities.values.is_empty());
        assert!(resolved.auras.values.is_empty());
        assert!(resolved.enchantments.values.is_empty());
    }

    #[test]
    fn compact_projection_preserves_golden_tooltips_and_hashes_static_definition() {
        let template_id = "0022c409-c839-41e8-8022-65a407457dfe";
        let card = json!({
            "Id": template_id,
            "InternalName": "Tooltip Fixture",
            "Type": "Item",
            "Version": "1.0.0",
            "StartingTier": "Bronze",
            "Size": "Small",
            "Tags": ["Tool"],
            "Localization": {
                "Title": {"Text": "Tooltip Fixture"},
                "Tooltips": [
                    {"Content": {"Text": "When you sell this, gain 5 Gold"}},
                    {"Content": {"Text": "At the start of each day, get a Small item"}}
                ]
            },
            "Tiers": {"Bronze": {"Attributes": {}}}
        });
        let first = project_card(template_id, &card);
        let second = project_card(template_id, &card);
        assert!(first.complete);
        assert_eq!(first.tooltips.shape, TooltipShape::Array);
        assert_eq!(
            first.tooltips.values,
            [
                "When you sell this, gain 5 Gold",
                "At the start of each day, get a Small item"
            ]
        );
        assert_eq!(first.template_content_id, second.template_content_id);

        let mut changed = card.clone();
        changed["Localization"]["Tooltips"][0]["Content"]["Text"] =
            json!("When you sell this, gain 6 Gold");
        let changed = project_card(template_id, &changed);
        assert_ne!(first.template_content_id, changed.template_content_id);
    }

    #[test]
    fn compact_projection_reports_tooltip_missing_and_malformed_entries() {
        let template_id = "0022c409-c839-41e8-8022-65a407457dfe";
        let card = json!({
            "Id": template_id,
            "Type": "Item",
            "StartingTier": "Bronze",
            "Size": "Small",
            "Tags": [],
            "Localization": {
                "Tooltips": [
                    {"Content": {}},
                    {"Content": {"Text": 42}},
                    "wrong"
                ]
            },
            "Tiers": {"Bronze": {"Attributes": {}}}
        });
        let projection = project_card(template_id, &card);
        assert!(!projection.complete);
        assert_eq!(
            projection.tooltips.missing,
            ["template.tooltips[0].content.text"]
        );
        assert_eq!(
            projection.tooltips.malformed,
            [
                "template.tooltips[1].content.text:expected_string",
                "template.tooltips[2]:expected_object"
            ]
        );
    }
}
