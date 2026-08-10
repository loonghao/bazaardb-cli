use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

const CACHE_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("http_responses_v1");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub stored_at: u64,
    pub expires_at: u64,
    pub stale_until: u64,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheStatus {
    pub path: PathBuf,
    pub entries: u64,
    pub response_bytes: u64,
}

#[derive(Clone)]
pub struct CacheStore {
    database: Arc<Database>,
    path: PathBuf,
}

impl CacheStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create cache directory {}", parent.display())
            })?;
        }
        let database = Database::create(&path)
            .with_context(|| format!("failed to open cache database {}", path.display()))?;
        {
            let write = database.begin_write()?;
            write.open_table(CACHE_TABLE)?;
            write.commit()?;
        }
        Ok(Self {
            database: Arc::new(database),
            path,
        })
    }

    pub async fn get(&self, key: String) -> Result<Option<CacheEntry>> {
        let database = Arc::clone(&self.database);
        tokio::task::spawn_blocking(move || {
            let read = database.begin_read()?;
            let table = read.open_table(CACHE_TABLE)?;
            let entry = table
                .get(key.as_str())?
                .map(|value| serde_json::from_slice::<CacheEntry>(value.value()))
                .transpose()?;
            Ok(entry)
        })
        .await?
    }

    pub async fn put(&self, key: String, entry: CacheEntry) -> Result<()> {
        let database = Arc::clone(&self.database);
        let bytes = serde_json::to_vec(&entry)?;
        tokio::task::spawn_blocking(move || {
            let write = database.begin_write()?;
            {
                let mut table = write.open_table(CACHE_TABLE)?;
                table.insert(key.as_str(), bytes.as_slice())?;
            }
            write.commit()?;
            Ok(())
        })
        .await?
    }

    pub async fn status(&self) -> Result<CacheStatus> {
        let database = Arc::clone(&self.database);
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let read = database.begin_read()?;
            let table = read.open_table(CACHE_TABLE)?;
            let mut entries = 0_u64;
            let mut response_bytes = 0_u64;
            for item in table.iter()? {
                let (_, value) = item?;
                let entry = serde_json::from_slice::<CacheEntry>(value.value())?;
                entries += 1;
                response_bytes += entry.body.len() as u64;
            }
            Ok(CacheStatus {
                path,
                entries,
                response_bytes,
            })
        })
        .await?
    }

    pub async fn clear(&self) -> Result<u64> {
        let database = Arc::clone(&self.database);
        tokio::task::spawn_blocking(move || {
            let write = database.begin_write()?;
            let removed;
            {
                let mut table = write.open_table(CACHE_TABLE)?;
                let keys = table
                    .iter()?
                    .map(|item| item.map(|(key, _)| key.value().to_owned()))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                removed = keys.len() as u64;
                for key in keys {
                    table.remove(key.as_str())?;
                }
            }
            write.commit()?;
            Ok(removed)
        })
        .await?
    }

    pub async fn prune(&self, now: u64) -> Result<u64> {
        let database = Arc::clone(&self.database);
        tokio::task::spawn_blocking(move || {
            let write = database.begin_write()?;
            let removed;
            {
                let mut table = write.open_table(CACHE_TABLE)?;
                let mut keys = Vec::new();
                for item in table.iter()? {
                    let (key, value) = item?;
                    let entry = serde_json::from_slice::<CacheEntry>(value.value())?;
                    if entry.stale_until < now {
                        keys.push(key.value().to_owned());
                    }
                }
                removed = keys.len() as u64;
                for key in keys {
                    table.remove(key.as_str())?;
                }
            }
            write.commit()?;
            Ok(removed)
        })
        .await?
    }
}
