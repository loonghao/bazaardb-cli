use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::catalog::{CATALOG_SCHEMA_VERSION, CatalogIdentity, RESOLVER_VERSION};

const SNAPSHOT_MAGIC: &[u8; 16] = b"BDBCATALOGSNAP1\0";
const SNAPSHOT_FORMAT_VERSION: u32 = 2;
const MEMO_FORMAT_VERSION: u32 = 1;
const MAX_CARD_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_CARD_BYTES: usize = 512 * 1024 * 1024;
const MAX_SNAPSHOT_BYTES: u64 = 768 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileStamp {
    present: bool,
    pub length: u64,
    pub modified_seconds: u64,
    pub modified_nanos: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DatabaseStamp {
    main: FileStamp,
    wal: FileStamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct NormalizedCard {
    pub row_id: String,
    pub payload: Value,
}

#[derive(Clone)]
pub(super) struct LoadedCatalog {
    pub stamp: DatabaseStamp,
    pub identity: CatalogIdentity,
    pub cards: Arc<Vec<NormalizedCard>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StampMemo {
    format_version: u32,
    path_hash: String,
    stamp: DatabaseStamp,
    database_sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotHeader {
    format_version: u32,
    catalog_schema_version: String,
    resolver_version: String,
    database_sha256: String,
    payload_sha256: String,
    content_id: String,
    card_count: u32,
    source_stamp: DatabaseStamp,
}

struct IoTrace {
    database_bytes_read: u64,
    sqlite_rows_read: u64,
    snapshot_bytes_read: u64,
    snapshot_bytes_written: u64,
}

impl IoTrace {
    const fn new() -> Self {
        Self {
            database_bytes_read: 0,
            sqlite_rows_read: 0,
            snapshot_bytes_read: 0,
            snapshot_bytes_written: 0,
        }
    }
}

pub(super) fn database_stamp(path: &Path) -> Result<DatabaseStamp> {
    Ok(DatabaseStamp {
        main: file_stamp(path, true)?,
        wal: file_stamp(&sqlite_sidecar_path(path, "-wal"), false)?,
    })
}

fn file_stamp(path: &Path, required: bool) -> Result<FileStamp> {
    let metadata = match path.metadata() {
        Ok(metadata) => metadata,
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FileStamp {
                present: false,
                length: 0,
                modified_seconds: 0,
                modified_nanos: 0,
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", path.display()));
        }
    };
    let modified = metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Ok(FileStamp {
        present: true,
        length: metadata.len(),
        modified_seconds: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
    })
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

pub(super) fn load_or_rebuild(
    database_path: &Path,
    cache_dir: &Path,
    expected_stamp: DatabaseStamp,
) -> Result<LoadedCatalog> {
    let started = Instant::now();
    fs::create_dir_all(cache_dir).with_context(|| {
        format!(
            "failed to create catalog cache directory {}",
            cache_dir.display()
        )
    })?;
    let path_hash = sha256_bytes(database_path.as_os_str().to_string_lossy().as_bytes());
    let memo_path = cache_dir.join(format!("stamp-{path_hash}.json"));
    let mut trace = IoTrace::new();

    let (database_sha256, memo_hit) = match read_memo(&memo_path, &path_hash, expected_stamp) {
        Ok(database_sha256) => (database_sha256, true),
        Err(reason) => {
            tracing::debug!(reason = %reason, "catalog stamp memo miss");
            let database_sha256 = hash_database(database_path, &mut trace)?;
            (database_sha256, false)
        }
    };
    let snapshot_path = snapshot_path(cache_dir, &database_sha256);
    match read_snapshot(&snapshot_path, &database_sha256, expected_stamp, &mut trace) {
        Ok(loaded) => {
            if !memo_hit {
                write_memo(&memo_path, &path_hash, expected_stamp, &database_sha256)?;
            }
            emit_trace("hit", "validated_snapshot", started, &trace);
            Ok(loaded)
        }
        Err(reason) => {
            tracing::debug!(reason = %reason, "catalog snapshot rebuild required");
            let cards = read_cards(database_path, &mut trace)?;
            let payload = serde_json::to_vec(&cards)
                .context("failed to serialize normalized catalog payload")?;
            let payload_sha256 = sha256_bytes(&payload);
            let catalog_sha256 = catalog_content_sha256(&payload);
            let identity =
                CatalogIdentity::from_hashes(database_sha256.clone(), catalog_sha256.clone());
            let header = SnapshotHeader {
                format_version: SNAPSHOT_FORMAT_VERSION,
                catalog_schema_version: CATALOG_SCHEMA_VERSION.to_owned(),
                resolver_version: RESOLVER_VERSION.to_owned(),
                database_sha256: database_sha256.clone(),
                payload_sha256,
                content_id: identity.content_id.clone(),
                card_count: u32::try_from(cards.len()).context("card count exceeds u32")?,
                source_stamp: expected_stamp,
            };
            trace.snapshot_bytes_written = write_snapshot(&snapshot_path, &header, &payload)?;
            write_memo(&memo_path, &path_hash, expected_stamp, &database_sha256)?;
            let final_stamp = database_stamp(database_path)?;
            if final_stamp != expected_stamp {
                bail!("GameData.db changed while rebuilding the catalog snapshot; retry");
            }
            emit_trace("miss_rebuilt", &reason.to_string(), started, &trace);
            Ok(LoadedCatalog {
                stamp: final_stamp,
                identity,
                cards: Arc::new(cards),
            })
        }
    }
}

fn read_memo(path: &Path, path_hash: &str, stamp: DatabaseStamp) -> Result<String> {
    let bytes = fs::read(path).context("memo_missing_or_unreadable")?;
    let memo = serde_json::from_slice::<StampMemo>(&bytes).context("memo_corrupt")?;
    if memo.format_version != MEMO_FORMAT_VERSION {
        bail!("memo_format_mismatch");
    }
    if memo.path_hash != path_hash {
        bail!("memo_path_mismatch");
    }
    if memo.stamp != stamp {
        bail!("memo_stamp_mismatch");
    }
    validate_sha256(&memo.database_sha256).context("memo_database_sha256_invalid")?;
    Ok(memo.database_sha256)
}

fn write_memo(
    path: &Path,
    path_hash: &str,
    stamp: DatabaseStamp,
    database_sha256: &str,
) -> Result<()> {
    let bytes = serde_json::to_vec(&StampMemo {
        format_version: MEMO_FORMAT_VERSION,
        path_hash: path_hash.to_owned(),
        stamp,
        database_sha256: database_sha256.to_owned(),
    })?;
    atomic_write(path, &bytes).map(|_| ())
}

fn snapshot_path(cache_dir: &Path, database_sha256: &str) -> PathBuf {
    let contract = sha256_bytes(format!("{CATALOG_SCHEMA_VERSION}\0{RESOLVER_VERSION}").as_bytes());
    cache_dir.join(format!(
        "catalog-{database_sha256}-{}.snapshot",
        &contract[..16]
    ))
}

fn read_snapshot(
    path: &Path,
    database_sha256: &str,
    stamp: DatabaseStamp,
    trace: &mut IoTrace,
) -> Result<LoadedCatalog> {
    let metadata = path.metadata().context("snapshot_missing")?;
    if metadata.len() > MAX_SNAPSHOT_BYTES {
        bail!("snapshot_too_large");
    }
    trace.snapshot_bytes_read = metadata.len();
    let mut file = File::open(path).context("snapshot_unreadable")?;
    let mut magic = [0_u8; SNAPSHOT_MAGIC.len()];
    file.read_exact(&mut magic)
        .context("snapshot_truncated_magic")?;
    if &magic != SNAPSHOT_MAGIC {
        bail!("snapshot_magic_mismatch");
    }
    let mut header_length = [0_u8; 4];
    file.read_exact(&mut header_length)
        .context("snapshot_truncated_header_length")?;
    let header_length = u32::from_le_bytes(header_length) as usize;
    if !(2..=64 * 1024).contains(&header_length) {
        bail!("snapshot_header_length_invalid");
    }
    let mut header_bytes = vec![0_u8; header_length];
    file.read_exact(&mut header_bytes)
        .context("snapshot_truncated_header")?;
    let header = serde_json::from_slice::<SnapshotHeader>(&header_bytes)
        .context("snapshot_header_corrupt")?;
    validate_header(&header, database_sha256, stamp)?;
    let mut payload = Vec::new();
    file.read_to_end(&mut payload)
        .context("snapshot_payload_unreadable")?;
    if sha256_bytes(&payload) != header.payload_sha256 {
        bail!("snapshot_payload_sha256_mismatch");
    }
    let expected_content_id = format!("sha256:{}", catalog_content_sha256(&payload));
    if header.content_id != expected_content_id {
        bail!("snapshot_content_id_mismatch");
    }
    let cards = serde_json::from_slice::<Vec<NormalizedCard>>(&payload)
        .context("snapshot_payload_corrupt")?;
    if cards.len() != header.card_count as usize
        || cards
            .iter()
            .any(|card| card.row_id.is_empty() || !card.payload.is_object())
    {
        bail!("snapshot_payload_shape_mismatch");
    }
    Ok(LoadedCatalog {
        stamp,
        identity: CatalogIdentity::from_hashes(
            database_sha256.to_owned(),
            header
                .content_id
                .strip_prefix("sha256:")
                .expect("validated content ID prefix")
                .to_owned(),
        ),
        cards: Arc::new(cards),
    })
}

fn validate_header(
    header: &SnapshotHeader,
    database_sha256: &str,
    stamp: DatabaseStamp,
) -> Result<()> {
    if header.format_version != SNAPSHOT_FORMAT_VERSION {
        bail!("snapshot_format_mismatch");
    }
    if header.catalog_schema_version != CATALOG_SCHEMA_VERSION {
        bail!("snapshot_catalog_schema_mismatch");
    }
    if header.resolver_version != RESOLVER_VERSION {
        bail!("snapshot_resolver_mismatch");
    }
    if header.database_sha256 != database_sha256 {
        bail!("snapshot_database_sha256_mismatch");
    }
    if header.source_stamp != stamp {
        bail!("snapshot_source_generation_mismatch");
    }
    validate_sha256(&header.database_sha256)?;
    validate_sha256(&header.payload_sha256)?;
    let content_sha256 = header
        .content_id
        .strip_prefix("sha256:")
        .context("snapshot_content_id_prefix_invalid")?;
    validate_sha256(content_sha256)?;
    Ok(())
}

fn hash_database(path: &Path, trace: &mut IoTrace) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open {} for hashing", path.display()))?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .context("failed to hash GameData.db")?;
        if read == 0 {
            break;
        }
        trace.database_bytes_read = trace.database_bytes_read.saturating_add(read as u64);
        digest.update(&buffer[..read]);
    }
    Ok(hex_digest(&digest.finalize()))
}

fn read_cards(path: &Path, trace: &mut IoTrace) -> Result<Vec<NormalizedCard>> {
    let connection = open_read_only(path)?;
    let data_version_before = connection
        .pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))
        .context("failed to read SQLite data_version")?;
    let cards_table_exists = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'cards')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .context("failed to inspect GameData.db schema")?;
    if !cards_table_exists {
        bail!("GameData.db does not contain the cards table");
    }
    let mut statement = connection
        .prepare("SELECT Id, Data FROM cards ORDER BY Id")
        .context("failed to prepare card query")?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .context("failed to query cards")?;
    let mut cards = Vec::new();
    let mut total_bytes = 0_usize;
    for row in rows {
        let (row_id, body) = row.context("failed to read a card row")?;
        trace.sqlite_rows_read = trace.sqlite_rows_read.saturating_add(1);
        if body.len() > MAX_CARD_BYTES {
            bail!("GameData.db contains a card larger than 8 MiB");
        }
        total_bytes = total_bytes.saturating_add(body.len());
        if total_bytes > MAX_TOTAL_CARD_BYTES {
            bail!("GameData.db card payloads exceed the 512 MiB safety limit");
        }
        let value = serde_json::from_slice::<Value>(&body)
            .context("GameData.db contains invalid card JSON")?;
        if !value.is_object() {
            bail!("GameData.db contains a non-object card payload");
        }
        cards.push(NormalizedCard {
            row_id,
            payload: value,
        });
    }
    let data_version_after = connection
        .pragma_query_value(None, "data_version", |row| row.get::<_, i64>(0))
        .context("failed to re-read SQLite data_version")?;
    if data_version_before != data_version_after {
        bail!("GameData.db changed during the catalog read; retry");
    }
    Ok(cards)
}

fn open_read_only(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("failed to open {} read-only", path.display()))?;
    connection
        .pragma_update(None, "query_only", true)
        .context("failed to make GameData.db connection query-only")?;
    Ok(connection)
}

fn write_snapshot(path: &Path, header: &SnapshotHeader, payload: &[u8]) -> Result<u64> {
    let header = serde_json::to_vec(header)?;
    let header_length = u32::try_from(header.len()).context("snapshot header exceeds u32")?;
    let mut bytes = Vec::with_capacity(SNAPSHOT_MAGIC.len() + 4 + header.len() + payload.len());
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&header_length.to_le_bytes());
    bytes.extend_from_slice(&header);
    bytes.extend_from_slice(payload);
    atomic_write(path, &bytes)?;
    Ok(bytes.len() as u64)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<u64> {
    let parent = path.parent().context("cache path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    match temporary.persist(path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let displaced = path.with_extension(format!(
                "invalid-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            fs::rename(path, &displaced)?;
            let temporary = error.file;
            temporary.persist(path)?;
            let _ = fs::remove_file(displaced);
        }
        Err(error) => return Err(error.error.into()),
    }
    sync_directory(parent);
    Ok(bytes.len() as u64)
}

fn sync_directory(path: &Path) {
    if let Ok(directory) = OpenOptions::new().read(true).open(path) {
        let _ = directory.sync_all();
    }
}

fn catalog_content_sha256(payload: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"bazaardb-cli/canonical-catalog\0");
    digest.update(CATALOG_SCHEMA_VERSION.as_bytes());
    digest.update(b"\0");
    digest.update(RESOLVER_VERSION.as_bytes());
    digest.update(b"\0");
    digest.update(payload);
    hex_digest(&digest.finalize())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex_digest(&digest.finalize())
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("invalid_sha256");
    }
    Ok(())
}

fn emit_trace(outcome: &str, reason: &str, started: Instant, trace: &IoTrace) {
    tracing::info!(
        target: "bazaardb_cli::catalog_cache",
        outcome,
        reason,
        duration_ms = started.elapsed().as_millis() as u64,
        database_bytes_read = trace.database_bytes_read,
        sqlite_rows_read = trace.sqlite_rows_read,
        snapshot_bytes_read = trace.snapshot_bytes_read,
        snapshot_bytes_written = trace.snapshot_bytes_written,
        "catalog cache load"
    );
}
