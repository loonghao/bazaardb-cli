use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::{CatalogIdentity, RunRecord, TenWinQuery, analyze_ten_wins};

pub const PROFILE_SCHEMA_VERSION: u32 = 1;
const MAX_SUPPLEMENT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SUPPLEMENT_SOURCES: usize = 32;

#[derive(Debug, Clone)]
pub struct LocalProfileSnapshot {
    pub catalog_identity: CatalogIdentity,
    pub content_versions: Vec<String>,
    pub game_modes: Vec<Value>,
    pub seasons: Vec<Value>,
    pub level_ups: Vec<Value>,
    pub cards: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ProfileRequest {
    pub hero: String,
    pub season_label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameplayProfile {
    pub schema_version: u32,
    pub hero: String,
    pub catalog_identity: ProfileCatalogIdentity,
    pub season: SeasonEvidence,
    pub rules: Vec<GameRules>,
    pub hero_pool: HeroPool,
    pub archetypes: Archetypes,
    pub level_up_choices: Vec<LevelUpChoice>,
    pub ten_win_evidence: TenWinEvidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supplement: Option<SupplementContent>,
    pub sources: Vec<ProfileSource>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCatalogIdentity {
    pub database_sha256: String,
    pub content_id: String,
    pub content_versions: Vec<String>,
    pub explicit_season_label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonEvidence {
    pub label: Option<String>,
    pub evidence: Vec<SeasonRecord>,
    pub verified: bool,
    pub mapping_source: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonRecord {
    pub id: Value,
    pub internal_name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameRules {
    pub internal_name: Option<String>,
    pub version: Option<String>,
    pub victories_to_win: Option<u64>,
    pub number_of_days: Option<u64>,
    pub hours_in_a_day: Option<u64>,
    pub experience_per_level: Option<u64>,
    pub experience_per_hour: Option<u64>,
    pub max_level: Option<u64>,
    pub prestige: Option<Value>,
    pub standard_prices: Option<Value>,
    pub item_skill_spawn_tiers_by_day: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HeroPool {
    pub always: Vec<ProfileCard>,
    pub guid_only: Vec<ProfileCard>,
    pub never: Vec<ProfileCard>,
    pub other: Vec<ProfileCard>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Archetypes {
    pub piggles: PigglesArchetype,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PigglesArchetype {
    pub available_for_hero: bool,
    pub core: Vec<ProfileCard>,
    pub support: Vec<ProfileCard>,
    pub adjacency_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCard {
    pub id: String,
    pub name: String,
    pub card_type: Option<String>,
    pub starting_tier: Option<String>,
    pub spawning_eligibility: Option<String>,
    pub tooltips: Vec<String>,
    pub tier_attributes: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelUpChoice {
    pub level: u64,
    pub health_increase: Option<Value>,
    pub version: Option<String>,
    pub eligible_groups: Vec<LevelUpGroup>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LevelUpGroup {
    pub card_ids: Vec<String>,
    pub cards: Vec<ChoiceCard>,
    pub selection_method: Option<String>,
    pub random_weight: Option<i64>,
    pub limit: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceCard {
    pub id: String,
    pub name: String,
    pub card_type: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenWinEvidence {
    pub available: bool,
    pub input_runs: usize,
    pub ten_win_runs: usize,
    pub matched_runs: usize,
    pub combinations: Vec<crate::TenWinCombination>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSource {
    pub kind: &'static str,
    pub scope: String,
    pub authority: &'static str,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Supplement {
    pub schema_version: u32,
    pub season_label: String,
    pub sources: Vec<SupplementSource>,
    #[serde(default)]
    pub ui_layout: Option<Value>,
    #[serde(default)]
    pub strategy: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SupplementSource {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub published_at: Option<String>,
    pub retrieved_at: String,
    pub sha256: String,
    pub scope: String,
    #[serde(default)]
    pub applies_to: Option<String>,
    #[serde(default)]
    pub confidence: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplementContent {
    pub season_label: String,
    pub sources: Vec<SupplementSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_layout: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<Value>,
}

pub fn load_supplement(path: &Path) -> Result<Supplement> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect supplement {}", path.display()))?;
    if metadata.len() > MAX_SUPPLEMENT_BYTES {
        bail!("supplement exceeds the 2 MiB limit");
    }
    let body =
        fs::read(path).with_context(|| format!("failed to read supplement {}", path.display()))?;
    let supplement: Supplement =
        serde_json::from_slice(&body).context("supplement is not valid schema-versioned JSON")?;
    supplement.validate()?;
    Ok(supplement)
}

impl Supplement {
    fn validate(&self) -> Result<()> {
        if self.schema_version != PROFILE_SCHEMA_VERSION {
            bail!("supplement schemaVersion must be {PROFILE_SCHEMA_VERSION}");
        }
        if self.season_label.trim().is_empty() {
            bail!("supplement seasonLabel is required");
        }
        if self.sources.is_empty() || self.sources.len() > MAX_SUPPLEMENT_SOURCES {
            bail!("supplement sources must contain between 1 and {MAX_SUPPLEMENT_SOURCES} entries");
        }
        for source in &self.sources {
            let url = reqwest::Url::parse(&source.url)
                .context("supplement source URL must be an absolute URL")?;
            if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
                bail!("supplement source URL must be an absolute HTTP(S) URL");
            }
            if source.title.trim().is_empty()
                || source.scope.trim().is_empty()
                || source.title.chars().any(char::is_control)
                || source.scope.chars().any(char::is_control)
            {
                bail!("supplement source title and scope are required");
            }
            OffsetDateTime::parse(
                &source.retrieved_at,
                &time::format_description::well_known::Rfc3339,
            )
            .context("supplement source retrievedAt must be RFC 3339")?;
            if let Some(published_at) = &source.published_at {
                OffsetDateTime::parse(published_at, &time::format_description::well_known::Rfc3339)
                    .context("supplement source publishedAt must be RFC 3339")?;
            }
            if source.sha256.len() != 64
                || !source
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                bail!("supplement source sha256 must be 64 lowercase hexadecimal characters");
            }
        }
        Ok(())
    }
}

pub fn generate_profile(
    snapshot: LocalProfileSnapshot,
    request: &ProfileRequest,
    supplement: Option<Supplement>,
    runs: Option<&[RunRecord]>,
) -> Result<GameplayProfile> {
    let hero = request.hero.trim();
    if hero.is_empty() {
        bail!("hero must not be empty");
    }
    if request
        .season_label
        .as_deref()
        .is_some_and(|label| label.trim().is_empty())
    {
        bail!("season-label must not be empty");
    }
    if let Some(supplement) = supplement.as_ref()
        && request.season_label.as_deref() != Some(supplement.season_label.as_str())
    {
        bail!("supplement seasonLabel must exactly match --season-label");
    }

    let season_label = request.season_label.as_deref();
    let evidence = season_label.map_or_else(Vec::new, |label| {
        snapshot
            .seasons
            .iter()
            .filter(|season| season["InternalName"].as_str() == Some(label))
            .map(|season| SeasonRecord {
                id: season.get("Id").cloned().unwrap_or(Value::Null),
                internal_name: label.to_owned(),
                version: text(season, "Version"),
            })
            .collect()
    });
    let season = SeasonEvidence {
        label: season_label.map(str::to_owned),
        verified: season_label.is_some() && evidence.len() == 1,
        mapping_source: if season_label.is_some() {
            "explicit_exact_local_match"
        } else {
            "unmapped_installed_snapshot"
        },
        evidence,
    };

    let mut pool = HeroPool::default();
    let mut card_index = BTreeMap::new();
    for card in &snapshot.cards {
        let id = text(card, "Id").unwrap_or_default();
        card_index.insert(id, choice_card(card));
        if !card["Heroes"].as_array().is_some_and(|heroes| {
            heroes
                .iter()
                .any(|candidate| candidate.as_str() == Some(hero))
        }) {
            continue;
        }
        let projected = profile_card(card);
        match projected.spawning_eligibility.as_deref() {
            Some("Always") => pool.always.push(projected),
            Some("GuidOnly") => pool.guid_only.push(projected),
            Some("Never") => pool.never.push(projected),
            _ => pool.other.push(projected),
        }
    }
    sort_pool(&mut pool);

    let mut piggles = pool
        .always
        .iter()
        .chain(&pool.guid_only)
        .chain(&pool.never)
        .chain(&pool.other)
        .filter(|card| card.name.to_ascii_lowercase().contains("piggle"))
        .cloned()
        .collect::<Vec<_>>();
    piggles.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.id.cmp(&right.id))
    });
    let core_names = ["Piggles", "Piggles Board", "Piggles Launcher"];
    let (core, support): (Vec<_>, Vec<_>) = piggles
        .into_iter()
        .partition(|card| core_names.contains(&card.name.as_str()));
    let adjacency_notes = core
        .iter()
        .chain(&support)
        .flat_map(|card| card.tooltips.iter())
        .filter(|tip| {
            let lower = tip.to_ascii_lowercase();
            lower.contains("adjacent")
                || lower.contains("to the left")
                || lower.contains("to the right")
        })
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let rules = snapshot
        .game_modes
        .iter()
        .map(game_rules)
        .collect::<Vec<_>>();
    let level_up_choices = snapshot
        .level_ups
        .iter()
        .map(|level| level_up_choice(level, hero, &card_index))
        .collect::<Vec<_>>();

    let ten_win_evidence = if let Some(runs) = runs {
        let analyzed = analyze_ten_wins(
            runs,
            &TenWinQuery {
                hero: Some(hero.to_owned()),
                card: None,
                combination_size: 2,
                min_runs: 1,
                limit: 20,
            },
        )?;
        TenWinEvidence {
            available: analyzed.matched_runs > 0,
            input_runs: analyzed.input_runs,
            ten_win_runs: analyzed.ten_win_runs,
            matched_runs: analyzed.matched_runs,
            combinations: analyzed.combinations,
        }
    } else {
        TenWinEvidence {
            available: false,
            input_runs: 0,
            ten_win_runs: 0,
            matched_runs: 0,
            combinations: Vec::new(),
        }
    };

    let mut warnings = Vec::new();
    if request.season_label.is_none() {
        warnings.push("No explicit season label was supplied; the installed snapshot is not mapped to a season.".to_owned());
    } else if !season.verified {
        warnings.push(
            "The explicit season label has no unique exact match in the local seasons table."
                .to_owned(),
        );
    } else {
        warnings.push(
            "The local seasons table verifies the explicit label exists; it does not prove this installed snapshot is a historical snapshot of that season."
                .to_owned(),
        );
    }
    if pool.always.is_empty()
        && pool.guid_only.is_empty()
        && pool.never.is_empty()
        && pool.other.is_empty()
    {
        warnings.push(format!(
            "No cards exactly list hero {hero:?}; check the canonical hero identifier."
        ));
    }
    if !ten_win_evidence.available {
        warnings.push("No matching local ten-win run evidence is available; strategy is not claimed to be ten-win validated.".to_owned());
    }
    if supplement.is_some() {
        warnings.push("Supplement content is locally supplied evidence and is not independently fetched or verified by this command.".to_owned());
    }

    let profile_content_id = profile_content_id(
        &snapshot.catalog_identity.database_sha256,
        &snapshot.content_versions,
        request.season_label.as_deref(),
    );
    Ok(GameplayProfile {
        schema_version: PROFILE_SCHEMA_VERSION,
        hero: hero.to_owned(),
        catalog_identity: ProfileCatalogIdentity {
            database_sha256: snapshot.catalog_identity.database_sha256,
            content_id: profile_content_id,
            content_versions: snapshot.content_versions,
            explicit_season_label: request.season_label.clone(),
        },
        season,
        rules,
        hero_pool: pool,
        archetypes: Archetypes {
            piggles: PigglesArchetype {
                available_for_hero: !core.is_empty(),
                core,
                support,
                adjacency_notes,
            },
        },
        level_up_choices,
        ten_win_evidence,
        supplement: supplement.map(|value| SupplementContent {
            season_label: value.season_label,
            sources: value.sources,
            ui_layout: value.ui_layout,
            strategy: value.strategy,
        }),
        sources: vec![ProfileSource {
            kind: "local_game_data",
            scope: "cards, game_modes, level_ups, seasons".to_owned(),
            authority: "inspection_only",
        }],
        warnings,
    })
}

fn profile_content_id(
    database_sha256: &str,
    content_versions: &[String],
    season_label: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"bazaardb-gameplay-profile/v1\0");
    digest.update(database_sha256.as_bytes());
    for version in content_versions {
        digest.update(b"\0version\0");
        digest.update(version.as_bytes());
    }
    digest.update(b"\0season\0");
    digest.update(season_label.unwrap_or_default().as_bytes());
    let hash = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256:{hash}")
}

fn game_rules(value: &Value) -> GameRules {
    GameRules {
        internal_name: text(value, "InternalName"),
        version: text(value, "Version"),
        victories_to_win: value["VictoriesToWin"].as_u64(),
        number_of_days: value["NumberOfDays"].as_u64(),
        hours_in_a_day: value["HoursInADay"].as_u64(),
        experience_per_level: value["ExperiencePerLevel"].as_u64(),
        experience_per_hour: value["ExperiencePerHour"].as_u64(),
        max_level: value["MaxLevel"].as_u64(),
        prestige: value.get("Prestige").cloned(),
        standard_prices: value.get("StandardPrices").cloned(),
        item_skill_spawn_tiers_by_day: value.get("ItemSkillSpawnTierPercantagesByDay").cloned(),
    }
}

fn profile_card(card: &Value) -> ProfileCard {
    let name = title(card);
    let tier_attributes = card["Tiers"]
        .as_object()
        .map(|tiers| {
            tiers
                .iter()
                .filter_map(|(tier, value)| {
                    value
                        .get("Attributes")
                        .cloned()
                        .map(|attributes| (tier.clone(), attributes))
                })
                .collect()
        })
        .unwrap_or_default();
    ProfileCard {
        id: text(card, "Id").unwrap_or_default(),
        name,
        card_type: text(card, "Type").or_else(|| text(card, "$type")),
        starting_tier: text(card, "StartingTier"),
        spawning_eligibility: text(card, "SpawningEligibility"),
        tooltips: card["Localization"]["Tooltips"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tooltip| tooltip["Content"]["Text"].as_str().map(str::to_owned))
            .collect(),
        tier_attributes,
    }
}

fn choice_card(card: &Value) -> ChoiceCard {
    ChoiceCard {
        id: text(card, "Id").unwrap_or_default(),
        name: title(card),
        card_type: text(card, "Type").or_else(|| text(card, "$type")),
    }
}

fn level_up_choice(
    level: &Value,
    hero: &str,
    index: &BTreeMap<String, ChoiceCard>,
) -> LevelUpChoice {
    let groups = level["Rewards"]["Groups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|group| hero_allowed(group.get("Prerequisites"), hero))
        .map(|group| {
            let ids = group["Filters"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|filter| filter["Ids"].as_array().into_iter().flatten())
                .filter_map(Value::as_str)
                .take(64)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let cards = ids.iter().filter_map(|id| index.get(id).cloned()).collect();
            LevelUpGroup {
                card_ids: ids,
                cards,
                selection_method: text(group, "SelectionMethod"),
                random_weight: group["RandomWeight"].as_i64(),
                limit: group.get("Limit").cloned(),
            }
        })
        .collect();
    LevelUpChoice {
        level: level["Level"].as_u64().unwrap_or_default(),
        health_increase: level.get("HealthIncrease").cloned(),
        version: text(level, "Version"),
        eligible_groups: groups,
    }
}

fn hero_allowed(value: Option<&Value>, hero: &str) -> bool {
    value.is_none_or(|value| hero_constraint(value, hero) != HeroConstraint::Disallowed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeroConstraint {
    Unknown,
    Allowed,
    Disallowed,
}

fn hero_constraint(value: &Value, hero: &str) -> HeroConstraint {
    match value {
        Value::Array(values) => {
            combine_and(values.iter().map(|value| hero_constraint(value, hero)))
        }
        Value::Object(object) => {
            let object_type = object.get("$type").and_then(Value::as_str);
            if object_type == Some("TRunConditionalPlayerHero") {
                let contains = object["Heroes"].as_array().is_some_and(|heroes| {
                    heroes
                        .iter()
                        .any(|candidate| candidate.as_str() == Some(hero))
                });
                return match object.get("Operator").and_then(Value::as_str) {
                    Some("None") if !contains => HeroConstraint::Allowed,
                    Some("None") => HeroConstraint::Disallowed,
                    _ if contains => HeroConstraint::Allowed,
                    _ => HeroConstraint::Disallowed,
                };
            }
            if object_type.is_some_and(|kind| kind.ends_with("Or")) {
                let constraints = object
                    .get("Conditions")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|value| hero_constraint(value, hero))
                    .collect::<Vec<_>>();
                if constraints.contains(&HeroConstraint::Allowed) {
                    HeroConstraint::Allowed
                } else if !constraints.is_empty()
                    && constraints
                        .iter()
                        .all(|value| *value == HeroConstraint::Disallowed)
                {
                    HeroConstraint::Disallowed
                } else {
                    HeroConstraint::Unknown
                }
            } else {
                combine_and(object.values().map(|value| hero_constraint(value, hero)))
            }
        }
        _ => HeroConstraint::Unknown,
    }
}

fn combine_and(values: impl IntoIterator<Item = HeroConstraint>) -> HeroConstraint {
    let mut saw_allowed = false;
    for value in values {
        match value {
            HeroConstraint::Disallowed => return HeroConstraint::Disallowed,
            HeroConstraint::Allowed => saw_allowed = true,
            HeroConstraint::Unknown => {}
        }
    }
    if saw_allowed {
        HeroConstraint::Allowed
    } else {
        HeroConstraint::Unknown
    }
}

fn title(card: &Value) -> String {
    card["Localization"]["Title"]["Text"]
        .as_str()
        .or_else(|| card["InternalName"].as_str())
        .unwrap_or("<unnamed>")
        .to_owned()
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn sort_pool(pool: &mut HeroPool) {
    for cards in [
        &mut pool.always,
        &mut pool.guid_only,
        &mut pool.never,
        &mut pool.other,
    ] {
        cards.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
    }
}

pub fn render_markdown(profile: &GameplayProfile) -> String {
    let mut output = format!(
        "# The Bazaar season handbook: {}\n\n- Hero: `{}`\n- Season verified: `{}`\n- Catalog: `{}`\n- Database SHA-256: `{}`\n\n",
        profile
            .season
            .label
            .as_deref()
            .unwrap_or("unmapped installed snapshot"),
        profile.hero,
        profile.season.verified,
        profile.catalog_identity.content_id,
        profile.catalog_identity.database_sha256,
    );
    output.push_str("## Rules\n\n");
    for rules in &profile.rules {
        output.push_str(&format!(
            "- {}: {} victories to win; {} days; {} hours per day.\n",
            rules.internal_name.as_deref().unwrap_or("Game mode"),
            rules
                .victories_to_win
                .map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
            rules
                .number_of_days
                .map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
            rules
                .hours_in_a_day
                .map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
        ));
    }
    output.push_str("\n## Piggles archetype\n\n");
    for card in &profile.archetypes.piggles.core {
        output.push_str(&format!(
            "- **{}** ({})\n",
            card.name,
            card.starting_tier.as_deref().unwrap_or("unknown tier")
        ));
        for tooltip in &card.tooltips {
            output.push_str(&format!("  - {}\n", tooltip.replace('\n', "\n    ")));
        }
    }
    output.push_str(&format!(
        "\nHero pool: {} always, {} guided-only, {} never, {} other. Piggles support cards: {}.\n",
        profile.hero_pool.always.len(),
        profile.hero_pool.guid_only.len(),
        profile.hero_pool.never.len(),
        profile.hero_pool.other.len(),
        profile.archetypes.piggles.support.len(),
    ));
    output.push_str("\n## Level-up choices\n\n");
    for level in &profile.level_up_choices {
        let named = level
            .eligible_groups
            .iter()
            .flat_map(|group| group.cards.iter())
            .map(|card| card.name.as_str())
            .take(8)
            .collect::<Vec<_>>();
        output.push_str(&format!(
            "- Level {}: {} eligible reward groups{}\n",
            level.level,
            level.eligible_groups.len(),
            if named.is_empty() {
                String::new()
            } else {
                format!("; referenced cards include {}", named.join(", "))
            }
        ));
    }
    output.push_str("\n## Ten-win evidence\n\n");
    output.push_str(&format!(
        "- Available: `{}`; input runs: {}; matching ten-win runs: {}.\n",
        profile.ten_win_evidence.available,
        profile.ten_win_evidence.input_runs,
        profile.ten_win_evidence.matched_runs,
    ));
    if let Some(supplement) = &profile.supplement {
        output.push_str("\n## Sourced supplement\n\n");
        if let Some(layout) = &supplement.ui_layout {
            output.push_str(&format!(
                "### UI layout\n\n```json\n{}\n```\n\n",
                serde_json::to_string_pretty(layout).unwrap_or_else(|_| "null".to_owned())
            ));
        }
        if let Some(strategy) = &supplement.strategy {
            output.push_str(&format!(
                "### Strategy\n\n```json\n{}\n```\n\n",
                serde_json::to_string_pretty(strategy).unwrap_or_else(|_| "null".to_owned())
            ));
        }
        output.push_str("### Sources\n\n");
        for source in &supplement.sources {
            output.push_str(&format!(
                "- {} — URL: `{}`; scope: {}; retrieved: {}; SHA-256: `{}`\n",
                source.title, source.url, source.scope, source.retrieved_at, source.sha256
            ));
        }
    }
    output.push_str("\n## Evidence warnings\n\n");
    for warning in &profile.warnings {
        output.push_str(&format!("- {warning}\n"));
    }
    output
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DccPlaybook {
    pub schema_version: u32,
    pub profile_id: &'static str,
    pub season_id: String,
    pub hero: String,
    pub generated_at: String,
    pub catalog_fence: DccCatalogFence,
    pub source_refs: Vec<DccSourceRef>,
    pub chapters: DccChapters,
    pub ten_win_evidence: DccTenWinEvidence,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DccCatalogFence {
    pub content_id: String,
    pub database_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DccSourceRef {
    pub kind: String,
    pub reference: String,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DccChapters {
    pub rules: Value,
    pub hero_pool: Value,
    pub archetypes: Value,
    pub level_up_choices: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_layout: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<Value>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DccTenWinEvidence {
    pub status: &'static str,
    pub input_runs: usize,
    pub matched_runs: usize,
    pub combinations: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DccKnowledgeIndex {
    schema_version: u32,
    profile_id: String,
    entries: Vec<DccKnowledgeEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DccKnowledgeEntry {
    season_id: String,
    hero: String,
    path: String,
    catalog_content_ids: Vec<String>,
    generated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_at: Option<String>,
}

pub fn write_dcc_knowledge(profile: &GameplayProfile, directory: &Path) -> Result<PathBuf> {
    let now = OffsetDateTime::now_utc();
    let generated_at = now
        .format(&time::format_description::well_known::Rfc3339)
        .context("failed to format generatedAt")?;
    let season_id = profile.season.label.as_deref().map_or_else(
        || {
            let date = now.date();
            format!("installed-snapshot-{date}")
        },
        slug,
    );
    let hero_id = slug(&profile.hero);
    let relative_path = format!("playbooks/{season_id}/{hero_id}.json");
    let output_path = directory.join(relative_path.replace('/', std::path::MAIN_SEPARATOR_STR));
    let source_refs = profile
        .sources
        .iter()
        .map(|source| DccSourceRef {
            kind: source.kind.to_owned(),
            reference: "installed/GameData.db".to_owned(),
            scope: source.scope.clone(),
        })
        .chain(profile.supplement.iter().flat_map(|supplement| {
            supplement.sources.iter().map(|source| DccSourceRef {
                kind: "supplement".to_owned(),
                reference: source.url.clone(),
                scope: source.scope.clone(),
            })
        }))
        .collect();
    let playbook = DccPlaybook {
        schema_version: 1,
        profile_id: "the-bazaar",
        season_id: season_id.clone(),
        hero: profile.hero.clone(),
        generated_at: generated_at.clone(),
        catalog_fence: DccCatalogFence {
            content_id: format!("sha256:{}", profile.catalog_identity.database_sha256),
            database_sha256: profile.catalog_identity.database_sha256.clone(),
        },
        source_refs,
        chapters: DccChapters {
            rules: serde_json::to_value(&profile.rules)?,
            hero_pool: serde_json::to_value(&profile.hero_pool)?,
            archetypes: serde_json::to_value(&profile.archetypes)?,
            level_up_choices: serde_json::to_value(&profile.level_up_choices)?,
            ui_layout: profile
                .supplement
                .as_ref()
                .and_then(|value| value.ui_layout.clone()),
            strategy: profile
                .supplement
                .as_ref()
                .and_then(|value| value.strategy.clone()),
            warnings: profile.warnings.clone(),
        },
        ten_win_evidence: DccTenWinEvidence {
            status: if profile.ten_win_evidence.available {
                "available"
            } else {
                "unavailable"
            },
            input_runs: profile.ten_win_evidence.input_runs,
            matched_runs: profile.ten_win_evidence.matched_runs,
            combinations: serde_json::to_value(&profile.ten_win_evidence.combinations)?,
        },
    };
    atomic_json_write(&output_path, &playbook)?;

    let index_path = directory.join("index.json");
    let mut index = if index_path.exists() {
        let metadata = fs::metadata(&index_path)?;
        if metadata.len() > MAX_SUPPLEMENT_BYTES {
            bail!("dcc knowledge index exceeds the 2 MiB limit");
        }
        serde_json::from_slice::<DccKnowledgeIndex>(&fs::read(&index_path)?)
            .context("existing dcc knowledge index is invalid")?
    } else {
        DccKnowledgeIndex {
            schema_version: 1,
            profile_id: "the-bazaar".to_owned(),
            entries: Vec::new(),
        }
    };
    if index.schema_version != 1 || index.profile_id != "the-bazaar" {
        bail!("existing dcc knowledge index has an incompatible contract");
    }
    let entry = DccKnowledgeEntry {
        season_id: season_id.clone(),
        hero: profile.hero.clone(),
        path: relative_path,
        catalog_content_ids: vec![playbook.catalog_fence.content_id.clone()],
        generated_at: generated_at.clone(),
        verified_at: profile.season.verified.then_some(generated_at),
    };
    if let Some(existing) = index
        .entries
        .iter_mut()
        .find(|candidate| candidate.season_id == season_id && candidate.hero == profile.hero)
    {
        *existing = entry;
    } else {
        index.entries.push(entry);
    }
    index.entries.sort_by(|left, right| {
        left.season_id
            .cmp(&right.season_id)
            .then_with(|| left.hero.cmp(&right.hero))
    });
    atomic_json_write(&index_path, &index)?;
    Ok(output_path)
}

fn atomic_json_write(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("output path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

fn slug(value: &str) -> String {
    let slug = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "unknown".to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot() -> LocalProfileSnapshot {
        LocalProfileSnapshot {
            catalog_identity: CatalogIdentity::from_hashes(
                "dbhash".to_owned(),
                "cardhash".to_owned(),
            ),
            content_versions: vec!["5.0.0".to_owned()],
            game_modes: vec![json!({
                "InternalName": "Base GameMode",
                "Version": "5.0.0",
                "VictoriesToWin": 10,
                "NumberOfDays": 10,
                "HoursInADay": 6
            })],
            seasons: vec![json!({"Id": 2, "InternalName": "Season 1", "Version": "5.0.0"})],
            level_ups: vec![json!({
                "Level": 2,
                "Version": "5.0.0",
                "Rewards": {"Groups": [{
                    "Filters": [{"Ids": ["launcher"]}],
                    "SelectionMethod": "Random",
                    "Prerequisites": [{
                        "$type": "TPrerequisiteRun",
                        "Conditions": {"$type": "TRunConditionalPlayerHero", "Heroes": ["Pygmalien"], "Operator": "Any"}
                    }]
                }]}
            })],
            cards: vec![
                json!({
                    "Id": "launcher", "Type": "Item", "InternalName": "Piggles Launcher",
                    "Heroes": ["Pygmalien"], "StartingTier": "Bronze", "SpawningEligibility": "Always",
                    "Version": "5.0.0",
                    "Localization": {"Title": {"Text": "Piggles Launcher"}, "Tooltips": [{"Content": {"Text": "Charge an adjacent Small item"}}]},
                    "Tiers": {"Bronze": {"Attributes": {"Damage": 10}}}
                }),
                json!({
                    "Id": "wrong-case", "Type": "Item", "InternalName": "Wrong",
                    "Heroes": ["pygmalien"], "SpawningEligibility": "Always", "Version": "5.0.0"
                }),
            ],
        }
    }

    #[test]
    fn profile_uses_exact_hero_and_explicit_season_evidence() {
        let profile = generate_profile(
            snapshot(),
            &ProfileRequest {
                hero: "Pygmalien".to_owned(),
                season_label: Some("Season 1".to_owned()),
            },
            None,
            None,
        )
        .unwrap();
        assert!(profile.season.verified);
        assert_eq!(profile.hero_pool.always.len(), 1);
        assert_eq!(profile.archetypes.piggles.core[0].name, "Piggles Launcher");
        assert_eq!(profile.level_up_choices[0].eligible_groups.len(), 1);
        assert!(!profile.ten_win_evidence.available);
        assert!(profile.catalog_identity.content_id.starts_with("sha256:"));
    }

    #[test]
    fn profile_does_not_guess_a_season_or_ten_win_evidence() {
        let profile = generate_profile(
            snapshot(),
            &ProfileRequest {
                hero: "Pygmalien".to_owned(),
                season_label: None,
            },
            None,
            None,
        )
        .unwrap();
        assert_eq!(profile.season.label, None);
        assert!(!profile.season.verified);
        assert_eq!(profile.season.mapping_source, "unmapped_installed_snapshot");
        assert_eq!(profile.ten_win_evidence.input_runs, 0);
        assert!(profile.ten_win_evidence.combinations.is_empty());
    }

    #[test]
    fn supplement_is_strict_bounded_and_season_fenced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("supplement.json");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "seasonLabel": "Season 1",
                "sources": [{
                    "url": "https://example.com/rules",
                    "title": "Rules",
                    "retrievedAt": "2026-08-13T00:00:00Z",
                    "sha256": "a".repeat(64),
                    "scope": "rules"
                }],
                "strategy": {"note": "take a skill rather than waste the choice"}
            }))
            .unwrap(),
        )
        .unwrap();
        let supplement = load_supplement(&path).unwrap();
        let error = generate_profile(
            snapshot(),
            &ProfileRequest {
                hero: "Pygmalien".to_owned(),
                season_label: Some("Season 2".to_owned()),
            },
            Some(supplement),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("exactly match"));

        fs::write(
            &path,
            br#"{"schemaVersion":1,"seasonLabel":"Season 1","sources":[],"unknown":true}"#,
        )
        .unwrap();
        assert!(load_supplement(&path).is_err());
    }

    #[test]
    fn dcc_knowledge_writer_preserves_unrelated_index_entries() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("index.json"),
            serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "profileId": "the-bazaar",
                "entries": [{
                    "seasonId": "season-0",
                    "hero": "Dooley",
                    "path": "playbooks/season-0/dooley.json",
                    "catalogContentIds": ["sha256:old"],
                    "generatedAt": "2026-01-01T00:00:00Z"
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let profile = generate_profile(
            snapshot(),
            &ProfileRequest {
                hero: "Pygmalien".to_owned(),
                season_label: Some("Season 1".to_owned()),
            },
            None,
            None,
        )
        .unwrap();
        let playbook_path = write_dcc_knowledge(&profile, directory.path()).unwrap();
        let playbook: Value = serde_json::from_slice(&fs::read(playbook_path).unwrap()).unwrap();
        assert_eq!(playbook["profileId"], "the-bazaar");
        assert_eq!(playbook["seasonId"], "season-1");
        assert_eq!(playbook["tenWinEvidence"]["status"], "unavailable");
        assert_eq!(playbook["catalogFence"]["contentId"], "sha256:dbhash");
        let index: Value =
            serde_json::from_slice(&fs::read(directory.path().join("index.json")).unwrap())
                .unwrap();
        assert_eq!(index["entries"].as_array().unwrap().len(), 2);
        assert!(
            index["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["hero"] == "Dooley")
        );
    }
}
