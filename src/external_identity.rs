use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::CanonicalUuid;

pub const EXTERNAL_IDENTITY_SCHEMA_VERSION: &str = "1.0.0";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ExternalIdentityCatalog {
    schema_version: u8,
    provider: String,
    records: BTreeMap<String, ExternalIdentityRecord>,
    #[serde(skip)]
    content_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExternalIdentityRecord {
    external_card_id: String,
    canonical_name: String,
    card_type: String,
    url: String,
    source_patch: String,
    verified_at: String,
    match_basis: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CardExternalReference {
    pub provider: String,
    pub external_card_id: String,
    pub canonical_name: String,
    pub card_type: String,
    pub url: String,
    pub source_patch: String,
    pub verified_at: String,
    pub match_basis: Vec<String>,
}

impl ExternalIdentityCatalog {
    pub(crate) fn bundled() -> Result<Self> {
        let mut catalog =
            serde_json::from_str::<Self>(include_str!("../data/card-identities.json"))
                .context("bundled external identity catalog is invalid JSON")?;
        catalog.validate()?;
        let canonical = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": catalog.schema_version,
            "provider": catalog.provider,
            "records": catalog.records,
        }))?;
        let mut digest = Sha256::new();
        digest.update(b"bazaardb-cli/external-identities\0");
        digest.update(EXTERNAL_IDENTITY_SCHEMA_VERSION.as_bytes());
        digest.update(b"\0");
        digest.update(canonical);
        catalog.content_id = format!("sha256:{}", hex_digest(&digest.finalize()));
        Ok(catalog)
    }

    pub(crate) fn content_id(&self) -> &str {
        &self.content_id
    }

    pub(crate) fn reference_for(
        &self,
        template_id: &str,
        canonical_name: Option<&str>,
        card_type: Option<&str>,
    ) -> std::result::Result<Vec<CardExternalReference>, String> {
        let Some(record) = self.records.get(template_id) else {
            return Ok(Vec::new());
        };
        if canonical_name != Some(record.canonical_name.as_str()) {
            return Err("externalReferences:canonical_name_mismatch".to_owned());
        }
        if !card_type.is_some_and(|value| value.eq_ignore_ascii_case(&record.card_type)) {
            return Err("externalReferences:card_type_mismatch".to_owned());
        }
        Ok(vec![CardExternalReference {
            provider: self.provider.clone(),
            external_card_id: record.external_card_id.clone(),
            canonical_name: record.canonical_name.clone(),
            card_type: record.card_type.clone(),
            url: record.url.clone(),
            source_patch: record.source_patch.clone(),
            verified_at: record.verified_at.clone(),
            match_basis: record.match_basis.clone(),
        }])
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            bail!("unsupported external identity schemaVersion");
        }
        if self.provider != "bazaardb" {
            bail!("external identity provider must be bazaardb");
        }
        for (template_id, record) in &self.records {
            template_id.parse::<CanonicalUuid>().with_context(|| {
                format!("external identity has invalid template ID {template_id}")
            })?;
            if record.external_card_id.is_empty()
                || !record
                    .external_card_id
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            {
                bail!("external identity has invalid BazaarDB card ID for {template_id}");
            }
            let prefix = format!("https://bazaardb.gg/card/{}/", record.external_card_id);
            if !record.url.starts_with(&prefix) {
                bail!("external identity has non-canonical URL for {template_id}");
            }
            if record.canonical_name.is_empty()
                || record.card_type.is_empty()
                || record.source_patch.is_empty()
                || record.verified_at.is_empty()
                || record.match_basis.is_empty()
            {
                bail!("external identity is missing provenance for {template_id}");
            }
        }
        Ok(())
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_is_canonical_and_content_addressed() {
        let first = ExternalIdentityCatalog::bundled().unwrap();
        let second = ExternalIdentityCatalog::bundled().unwrap();
        assert_eq!(first.content_id(), second.content_id());
        assert!(first.content_id().starts_with("sha256:"));
        let reference = first
            .reference_for(
                "7317d6a2-adea-442c-9e97-7f7bbf64ae99",
                Some("Unibou"),
                Some("Item"),
            )
            .unwrap();
        assert_eq!(reference[0].external_card_id, "l1n7dqkk5gpl0n6h52880y0jq5");
    }
}
