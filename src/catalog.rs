use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use uuid::Uuid;

pub const CATALOG_SCHEMA_VERSION: &str = "2.0.0";
pub const RESOLVER_VERSION: &str = "1.2.0";
pub const MAX_RESOLVE_BATCH: usize = 64;
pub const MAX_CATALOG_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const INSPECTION_AUTHORITY: &str = "inspection_only";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalGameIdentifier(String);

impl fmt::Display for CanonicalGameIdentifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for CanonicalGameIdentifier {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let mut characters = value.chars();
        let valid_first = characters
            .next()
            .is_some_and(|character| character.is_ascii_alphanumeric());
        let valid_rest = characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        });
        if !(1..=128).contains(&value.len()) || !valid_first || !valid_rest {
            bail!(
                "enchantmentId must be a 1-128 character canonical game identifier using ASCII letters, digits, dot, hyphen, or underscore"
            );
        }
        Ok(Self(value.to_owned()))
    }
}

impl Serialize for CanonicalGameIdentifier {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CanonicalGameIdentifier {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalUuid(Uuid);

impl CanonicalUuid {
    #[must_use]
    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl fmt::Display for CanonicalUuid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.hyphenated().fmt(formatter)
    }
}

impl FromStr for CanonicalUuid {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let uuid = Uuid::parse_str(value).map_err(|_| {
            anyhow::anyhow!("templateId must be a canonical lowercase hyphenated UUID")
        })?;
        if uuid.hyphenated().to_string() != value {
            bail!("templateId must be a canonical lowercase hyphenated UUID");
        }
        Ok(Self(uuid))
    }
}

impl Serialize for CanonicalUuid {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for CanonicalUuid {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CardTier {
    Bronze,
    Silver,
    Gold,
    Diamond,
    Legendary,
}

impl CardTier {
    #[must_use]
    pub const fn ordered() -> &'static [Self] {
        &[
            Self::Bronze,
            Self::Silver,
            Self::Gold,
            Self::Diamond,
            Self::Legendary,
        ]
    }

    #[must_use]
    pub const fn rank(self) -> usize {
        match self {
            Self::Bronze => 0,
            Self::Silver => 1,
            Self::Gold => 2,
            Self::Diamond => 3,
            Self::Legendary => 4,
        }
    }
}

impl fmt::Display for CardTier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Bronze => "Bronze",
            Self::Silver => "Silver",
            Self::Gold => "Gold",
            Self::Diamond => "Diamond",
            Self::Legendary => "Legendary",
        })
    }
}

impl FromStr for CardTier {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            "bronze" => Ok(Self::Bronze),
            "silver" => Ok(Self::Silver),
            "gold" => Ok(Self::Gold),
            "diamond" => Ok(Self::Diamond),
            "legendary" => Ok(Self::Legendary),
            _ => bail!("tier must be Bronze, Silver, Gold, Diamond, or Legendary"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogIdentity {
    pub catalog_schema_version: &'static str,
    pub resolver_version: &'static str,
    pub database_sha256: String,
    pub content_id: String,
    pub cache_key: String,
}

impl CatalogIdentity {
    #[must_use]
    pub fn from_hashes(database_sha256: String, catalog_sha256: String) -> Self {
        Self {
            content_id: format!("sha256:{catalog_sha256}"),
            cache_key: format!(
                "catalog/{CATALOG_SCHEMA_VERSION}/resolver/{RESOLVER_VERSION}/database/{database_sha256}/content/{catalog_sha256}"
            ),
            catalog_schema_version: CATALOG_SCHEMA_VERSION,
            resolver_version: RESOLVER_VERSION,
            database_sha256,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogStatus {
    #[serde(flatten)]
    pub identity: CatalogIdentity,
    pub source: &'static str,
    pub card_count: u32,
    pub offline: bool,
    pub read_only: bool,
    pub action_authority: bool,
    pub authority: &'static str,
    pub authorizes_action: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveBatchRequest {
    pub requests: Vec<ResolveCardRequest>,
    #[serde(default)]
    pub mode: ResolveMode,
    #[serde(default)]
    pub include_raw_template: bool,
    #[serde(default)]
    pub include_all_enchantments: bool,
}

impl ResolveBatchRequest {
    pub fn validate(&self) -> Result<()> {
        if !(1..=MAX_RESOLVE_BATCH).contains(&self.requests.len()) {
            return Err(CatalogContractError::new(
                "invalid_batch_size",
                format!("resolve requests must contain between 1 and {MAX_RESOLVE_BATCH} items"),
                Value::Null,
            )
            .into());
        }
        let mut resolutions = BTreeSet::new();
        let duplicates = self
            .requests
            .iter()
            .map(|request| {
                format!(
                    "{}/{}/{}",
                    request.template_id,
                    request.tier,
                    selector_key(
                        request.enchantment_id.as_ref(),
                        self.include_all_enchantments
                    )
                )
            })
            .filter(|resolution| !resolutions.insert(resolution.clone()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if !duplicates.is_empty() {
            return Err(CatalogContractError::new(
                "duplicate_resolution",
                "resolve requests must not contain duplicate resolution tuples".to_owned(),
                serde_json::json!({"resolutionTuples": duplicates}),
            )
            .into());
        }
        if self.include_all_enchantments
            && self
                .requests
                .iter()
                .any(|request| request.enchantment_id.is_some())
        {
            return Err(CatalogContractError::new(
                "invalid_enchantment_selection",
                "includeAllEnchantments cannot be combined with per-card enchantmentId".to_owned(),
                Value::Null,
            )
            .into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveMode {
    #[default]
    Strict,
    Partial,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveCardRequest {
    pub template_id: CanonicalUuid,
    pub tier: CardTier,
    #[serde(default)]
    pub enchantment_id: Option<CanonicalGameIdentifier>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveBatchResponse {
    #[serde(flatten)]
    pub identity: CatalogIdentity,
    pub results: Vec<ResolvedCard>,
    pub authority: &'static str,
    pub authorizes_action: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveJsonlRecord<'a> {
    #[serde(flatten)]
    pub identity: &'a CatalogIdentity,
    pub result: &'a ResolvedCard,
    pub authority: &'static str,
    pub authorizes_action: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedCard {
    pub resolution_key: String,
    pub template_content_id: Option<String>,
    pub template_id: CanonicalUuid,
    pub found: bool,
    pub complete: bool,
    pub tier: CardTier,
    pub starting_tier: Option<CardTier>,
    pub size: Option<String>,
    pub tags: Vec<String>,
    pub hidden_tags: Vec<String>,
    pub template: Option<CatalogCardProjection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_template: Option<Value>,
    pub attributes: AttributeResolution,
    pub abilities: ComponentResolution,
    pub auras: ComponentResolution,
    pub enchantments: ComponentResolution,
    pub missing: Vec<String>,
    pub malformed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogCardProjection {
    pub template_id: String,
    pub template_content_id: String,
    pub payload_id_consistency: PayloadIdConsistency,
    pub complete: bool,
    pub missing: Vec<String>,
    pub malformed: Vec<String>,
    pub name: Option<String>,
    pub card_type: Option<String>,
    pub version: Option<String>,
    pub starting_tier: Option<CardTier>,
    pub size: Option<String>,
    pub tags: Vec<String>,
    pub hidden_tags: Vec<String>,
    pub tooltips: TooltipResolution,
}

pub(crate) fn selector_key(
    enchantment_id: Option<&CanonicalGameIdentifier>,
    include_all_enchantments: bool,
) -> String {
    if include_all_enchantments {
        "selector/all".to_owned()
    } else if let Some(enchantment_id) = enchantment_id {
        format!("selector/exact/{enchantment_id}")
    } else {
        "selector/not_requested".to_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadIdConsistency {
    Matching,
    Absent,
    Mismatch,
    Malformed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TooltipResolution {
    pub shape: TooltipShape,
    pub values: Vec<String>,
    pub missing: Vec<String>,
    pub malformed: Vec<String>,
    pub complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TooltipShape {
    Absent,
    Null,
    Array,
    Malformed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeResolution {
    pub values: BTreeMap<String, Value>,
    pub tiers_applied: Vec<CardTier>,
    pub missing: Vec<String>,
    pub malformed: Vec<String>,
    pub complete: bool,
}

impl AttributeResolution {
    #[must_use]
    pub fn missing_template() -> Self {
        Self {
            values: BTreeMap::new(),
            tiers_applied: Vec::new(),
            missing: vec!["template".to_owned()],
            malformed: Vec::new(),
            complete: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentResolution {
    pub status: ComponentStatus,
    pub shape: ComponentShape,
    pub ids: Vec<String>,
    pub values: BTreeMap<String, Value>,
    pub missing: Vec<String>,
    pub malformed: Vec<String>,
    pub complete: bool,
}

impl ComponentResolution {
    #[must_use]
    pub fn missing_template() -> Self {
        Self {
            status: ComponentStatus::Incomplete,
            shape: ComponentShape::Absent,
            ids: Vec::new(),
            values: BTreeMap::new(),
            missing: vec!["template".to_owned()],
            malformed: Vec::new(),
            complete: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    NotRequested,
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentShape {
    Absent,
    Null,
    Object,
    Malformed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSearchResponse {
    #[serde(flatten)]
    pub identity: CatalogIdentity,
    pub page: u32,
    pub limit: u32,
    pub count: u32,
    pub total: u32,
    pub cards: Vec<CatalogCardProjection>,
    pub authority: &'static str,
    pub authorizes_action: bool,
}

#[derive(Debug)]
pub struct CatalogContractError {
    pub code: &'static str,
    pub message: String,
    pub details: Value,
}

impl CatalogContractError {
    #[must_use]
    pub fn new(code: &'static str, message: String, details: Value) -> Self {
        Self {
            code,
            message,
            details,
        }
    }
}

impl fmt::Display for CatalogContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CatalogContractError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_requires_canonical_lowercase_hyphenated_form() {
        let canonical = "0022c409-c839-41e8-8022-65a407457dfe";
        assert!(canonical.parse::<CanonicalUuid>().is_ok());
        assert!(
            canonical
                .to_ascii_uppercase()
                .parse::<CanonicalUuid>()
                .is_err()
        );
        assert!(canonical.replace('-', "").parse::<CanonicalUuid>().is_err());
    }

    #[test]
    fn game_identifier_is_strict_and_case_preserving() {
        assert_eq!(
            "Fiery_2.0"
                .parse::<CanonicalGameIdentifier>()
                .unwrap()
                .to_string(),
            "Fiery_2.0"
        );
        assert!(" fiery".parse::<CanonicalGameIdentifier>().is_err());
        assert!("Fiery/../../x".parse::<CanonicalGameIdentifier>().is_err());
        assert!("".parse::<CanonicalGameIdentifier>().is_err());
    }

    #[test]
    fn batch_bounds_are_stable() {
        assert!(
            ResolveBatchRequest {
                requests: vec![],
                mode: ResolveMode::Strict,
                include_raw_template: false,
                include_all_enchantments: false,
            }
            .validate()
            .is_err()
        );
        let request = ResolveCardRequest {
            template_id: "0022c409-c839-41e8-8022-65a407457dfe".parse().unwrap(),
            tier: CardTier::Silver,
            enchantment_id: None,
        };
        assert!(
            ResolveBatchRequest {
                requests: (0..MAX_RESOLVE_BATCH)
                    .map(|index| ResolveCardRequest {
                        template_id: format!("00000000-0000-0000-0000-{index:012x}")
                            .parse()
                            .unwrap(),
                        tier: request.tier,
                        enchantment_id: None,
                    })
                    .collect(),
                mode: ResolveMode::Strict,
                include_raw_template: false,
                include_all_enchantments: false,
            }
            .validate()
            .is_ok()
        );
        assert!(
            ResolveBatchRequest {
                requests: (0..=MAX_RESOLVE_BATCH)
                    .map(|index| ResolveCardRequest {
                        template_id: format!("00000000-0000-0000-0000-{index:012x}")
                            .parse()
                            .unwrap(),
                        tier: request.tier,
                        enchantment_id: None,
                    })
                    .collect(),
                mode: ResolveMode::Strict,
                include_raw_template: false,
                include_all_enchantments: false,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn batch_allows_same_template_at_distinct_resolution_tuples() {
        let template_id = "0022c409-c839-41e8-8022-65a407457dfe";
        let request = ResolveBatchRequest {
            requests: [CardTier::Bronze, CardTier::Silver]
                .into_iter()
                .map(|tier| ResolveCardRequest {
                    template_id: template_id.parse().unwrap(),
                    tier,
                    enchantment_id: None,
                })
                .collect(),
            mode: ResolveMode::Strict,
            include_raw_template: false,
            include_all_enchantments: false,
        };
        request.validate().unwrap();
    }

    #[test]
    fn batch_rejects_only_duplicate_resolution_tuples() {
        let template_id = "0022c409-c839-41e8-8022-65a407457dfe";
        let request = ResolveBatchRequest {
            requests: ["all", "all"]
                .into_iter()
                .map(|enchantment_id| ResolveCardRequest {
                    template_id: template_id.parse().unwrap(),
                    tier: CardTier::Silver,
                    enchantment_id: Some(enchantment_id.parse().unwrap()),
                })
                .collect(),
            mode: ResolveMode::Strict,
            include_raw_template: false,
            include_all_enchantments: false,
        };
        let error = request.validate().unwrap_err();
        let contract = error.downcast_ref::<CatalogContractError>().unwrap();
        assert_eq!(contract.code, "duplicate_resolution");
    }
}
