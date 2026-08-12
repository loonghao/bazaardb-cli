use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::CatalogIdentity;
use crate::profile::LocalProfileSnapshot;

const MAX_ROWS_PER_TABLE: usize = 10_000;
const MAX_ROW_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 512 * 1024 * 1024;

pub fn load_profile_snapshot(
    path: &Path,
    catalog_identity: CatalogIdentity,
) -> Result<LocalProfileSnapshot> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open {} read-only", path.display()))?;
    connection
        .pragma_update(None, "query_only", true)
        .context("failed to make GameData.db connection query-only")?;
    let data_version_before =
        connection.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))?;
    let mut total_bytes = 0;
    let cards = read_table(&connection, "cards", &mut total_bytes)?;
    let game_modes = read_table(&connection, "game_modes", &mut total_bytes)?;
    let level_ups = read_table(&connection, "level_ups", &mut total_bytes)?;
    let seasons = read_table(&connection, "seasons", &mut total_bytes)?;
    let data_version_after =
        connection.pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))?;
    if data_version_before != data_version_after {
        bail!("GameData.db changed during the profile read; retry");
    }
    let content_versions = cards
        .iter()
        .chain(&game_modes)
        .chain(&level_ups)
        .chain(&seasons)
        .filter_map(|value| value["Version"].as_str().map(str::to_owned))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok(LocalProfileSnapshot {
        catalog_identity,
        content_versions,
        game_modes,
        seasons,
        level_ups,
        cards,
    })
}

fn read_table(
    connection: &Connection,
    table: &'static str,
    total_bytes: &mut usize,
) -> Result<Vec<Value>> {
    let exists = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, bool>(0),
    )?;
    if !exists {
        bail!("GameData.db does not contain required table {table}");
    }
    let sql = format!("SELECT Data FROM {table} ORDER BY Id");
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    let mut values = Vec::new();
    for row in rows {
        if values.len() >= MAX_ROWS_PER_TABLE {
            bail!("GameData.db table {table} exceeds the {MAX_ROWS_PER_TABLE} row limit");
        }
        let body = row.with_context(|| format!("failed to read {table} row"))?;
        if body.len() > MAX_ROW_BYTES {
            bail!("GameData.db table {table} contains a row larger than 8 MiB");
        }
        *total_bytes = total_bytes.saturating_add(body.len());
        if *total_bytes > MAX_TOTAL_BYTES {
            bail!("GameData.db profile payloads exceed the 512 MiB safety limit");
        }
        let value = serde_json::from_slice::<Value>(&body)
            .with_context(|| format!("GameData.db table {table} contains invalid JSON"))?;
        if !value.is_object() {
            bail!("GameData.db table {table} contains a non-object payload");
        }
        values.push(value);
    }
    Ok(values)
}
