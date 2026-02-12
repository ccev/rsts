use sqlx::{SqlitePool, sqlite::SqliteConnectOptions};
use std::path::PathBuf;
use std::fs;
use anyhow::Result;
use chrono::Utc;
use tracing::{info, debug};

pub struct TileCache {
    pool: SqlitePool,
    cache_dir: PathBuf,
    max_size_bytes: u64,
}

impl TileCache {
    pub async fn new(cache_dir: PathBuf, max_size_gb: u64) -> Result<Self> {
        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)?;
        }

        let db_path = cache_dir.join("cache_metadata.db");
        let opts = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);

        let pool = SqlitePool::connect_with(opts).await?;

        // Initialize table
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS tiles (
                key TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                size_bytes INTEGER NOT NULL,
                hit_count INTEGER DEFAULT 1,
                last_access_at INTEGER NOT NULL
            )"
        ).execute(&pool).await?;

        Ok(Self {
            pool,
            cache_dir,
            max_size_bytes: max_size_gb * 1024 * 1024 * 1024,
        })
    }

    pub async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let row: Option<(String, i64)> = sqlx::query_as(
            "SELECT file_path, hit_count FROM tiles WHERE key = ?"
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((rel_path, hit_count)) = row {
            let full_path = self.cache_dir.join(rel_path);
            if full_path.exists() {
                let bytes = fs::read(full_path)?;
                
                // Update hit count and last access
                sqlx::query(
                    "UPDATE tiles SET hit_count = ?, last_access_at = ? WHERE key = ?"
                )
                .bind(hit_count + 1)
                .bind(Utc::now().timestamp())
                .bind(key)
                .execute(&self.pool)
                .await?;

                debug!("cache hit: {}", key);
                return Ok(Some(bytes));
            } else {
                // File missing but in DB, cleanup
                sqlx::query("DELETE FROM tiles WHERE key = ?").bind(key).execute(&self.pool).await?;
            }
        }

        debug!("cache miss: {}", key);
        Ok(None)
    }

    pub async fn insert(&self, key: &str, data: Vec<u8>) -> Result<()> {
        let size = data.len() as u64;
        let file_name = format!("{}.bin", key.replace('/', "_").replace(':', "_"));
        let full_path = self.cache_dir.join(&file_name);

        fs::write(&full_path, &data)?;

        sqlx::query(
            "INSERT OR REPLACE INTO tiles (key, file_path, size_bytes, last_access_at) 
             VALUES (?, ?, ?, ?)"
        )
        .bind(key)
        .bind(&file_name)
        .bind(size as i64)
        .bind(Utc::now().timestamp())
        .execute(&self.pool)
        .await?;

        self.evict_if_needed().await?;
        Ok(())
    }

    async fn evict_if_needed(&self) -> Result<()> {
        let current_size: i64 = sqlx::query_scalar::<_, i64>("SELECT COALESCE(SUM(size_bytes), 0) FROM tiles")
            .fetch_one(&self.pool)
            .await?;

        if current_size as u64 > self.max_size_bytes {
            info!("cache limit reached, evicting...");
            
            // Evict items with lowest hit count, then oldest access
            let to_evict: Vec<(String, String, i64)> = sqlx::query_as(
                "SELECT key, file_path, size_bytes FROM tiles 
                 ORDER BY hit_count ASC, last_access_at ASC 
                 LIMIT 100"
            )
            .fetch_all(&self.pool)
            .await?;

            for (key, rel_path, _size) in to_evict {
                let full_path = self.cache_dir.join(rel_path);
                if full_path.exists() {
                    let _ = fs::remove_file(full_path);
                }
                sqlx::query("DELETE FROM tiles WHERE key = ?").bind(&key).execute(&self.pool).await?;
                debug!("evicted: {}", key);
                
                // Check if we are below limit now (simple iterative check or just continue the loop)
                // For simplicity we evict in batches of 100
            }
        }
        Ok(())
    }
}
