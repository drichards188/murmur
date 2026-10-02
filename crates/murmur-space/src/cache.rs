//! Element-set cache. CelesTrak updates GP data every 2 hours and blocks clients that
//! download the same data repeatedly or rack up HTTP errors (usage policy:
//! https://celestrak.org/usage-policy.php). So: one file per query, never refetched inside
//! the 2-hour update cycle (even with `force`), refetched by default only after `max_age`,
//! fetched sequentially, and the run stops at the first HTTP error.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CELESTRAK_GP: &str = "https://celestrak.org/NORAD/elements/gp.php";
/// CelesTrak's GP update cycle: refetching sooner can only return the same data.
pub const MIN_REFETCH: Duration = Duration::hours(2);
const USER_AGENT: &str = concat!(
    "murmur-space/",
    env!("CARGO_PKG_VERSION"),
    " (open-source C2 testbed; caches GP data)"
);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMeta {
    pub query: String,
    pub url: String,
    pub fetched_at: DateTime<Utc>,
    pub objects: usize,
    pub bytes: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("fetching {0}: {1}")]
    Http(String, String),
    #[error("{0}: response is not a GP JSON array ({1})")]
    BadData(String, String),
    #[error("no cached element sets for {0}; run `murmur-space fetch`")]
    Missing(String),
}

pub struct Cache {
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub enum FetchOutcome {
    Fetched(CacheMeta),
    Fresh(CacheMeta),
    Failed { query: String, error: String },
    Skipped { query: String, reason: String },
}

/// Turn "GROUP=iridium-NEXT" into a file stem "group-iridium-next".
pub fn cache_key(query: &str) -> String {
    query
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

impl Cache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Cache { dir: dir.into() }
    }

    fn data_path(&self, query: &str) -> PathBuf {
        self.dir.join(format!("{}.json", cache_key(query)))
    }

    fn meta_path(&self, query: &str) -> PathBuf {
        self.dir.join(format!("{}.meta.json", cache_key(query)))
    }

    pub fn meta(&self, query: &str) -> Option<CacheMeta> {
        let text = std::fs::read_to_string(self.meta_path(query)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn read(&self, query: &str) -> Result<(String, CacheMeta), CacheError> {
        let meta = self
            .meta(query)
            .ok_or_else(|| CacheError::Missing(query.to_string()))?;
        let path = self.data_path(query);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| CacheError::Io(path.display().to_string(), e))?;
        Ok((text, meta))
    }

    /// Fetch every query whose cache is missing or older than `max_age`. `force` lowers the
    /// age threshold to CelesTrak's 2-hour cycle, never below it.
    pub fn fetch_all(
        &self,
        queries: &[String],
        max_age: Duration,
        force: bool,
        now: DateTime<Utc>,
    ) -> Vec<FetchOutcome> {
        let threshold = if force {
            MIN_REFETCH
        } else {
            max_age.max(MIN_REFETCH)
        };
        let mut out = Vec::new();
        let mut stop: Option<String> = None;
        for q in queries {
            if let Some(reason) = &stop {
                out.push(FetchOutcome::Skipped {
                    query: q.clone(),
                    reason: reason.clone(),
                });
                continue;
            }
            if let Some(meta) = self.meta(q)
                && now - meta.fetched_at < threshold
                && self.data_path(q).exists()
            {
                out.push(FetchOutcome::Fresh(meta));
                continue;
            }
            match self.fetch_one(q, now) {
                Ok(meta) => out.push(FetchOutcome::Fetched(meta)),
                Err(e) => {
                    stop = Some(format!(
                        "stopped after an error on {q} (CelesTrak counts errors per IP)"
                    ));
                    out.push(FetchOutcome::Failed {
                        query: q.clone(),
                        error: e.to_string(),
                    });
                }
            }
        }
        out
    }

    fn fetch_one(&self, query: &str, now: DateTime<Utc>) -> Result<CacheMeta, CacheError> {
        let url = format!("{CELESTRAK_GP}?{query}&FORMAT=json");
        let mut resp = ureq::get(&url)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| CacheError::Http(url.clone(), e.to_string()))?;
        let body = resp
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| CacheError::Http(url.clone(), e.to_string()))?;
        // CelesTrak answers an empty query with a plain-text message and status 200.
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&body)
            .map_err(|_| CacheError::BadData(query.to_string(), body.chars().take(80).collect()))?;
        if parsed.is_empty() {
            return Err(CacheError::BadData(query.to_string(), "empty".into()));
        }
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| CacheError::Io(self.dir.display().to_string(), e))?;
        let meta = CacheMeta {
            query: query.to_string(),
            url,
            fetched_at: now,
            objects: parsed.len(),
            bytes: body.len(),
        };
        write_atomic(&self.data_path(query), body.as_bytes())?;
        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| CacheError::BadData(query.into(), e.to_string()))?;
        write_atomic(&self.meta_path(query), meta_json.as_bytes())?;
        Ok(meta)
    }

    /// Store element sets obtained elsewhere (e.g. historical GP data from a Space-Track
    /// account) under a query key, so scenarios can run without a time shift.
    pub fn import(
        &self,
        query: &str,
        json: &str,
        now: DateTime<Utc>,
        source: &str,
    ) -> Result<CacheMeta, CacheError> {
        let parsed: Vec<serde_json::Value> = serde_json::from_str(json)
            .map_err(|e| CacheError::BadData(query.to_string(), e.to_string()))?;
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| CacheError::Io(self.dir.display().to_string(), e))?;
        let meta = CacheMeta {
            query: query.to_string(),
            url: source.to_string(),
            fetched_at: now,
            objects: parsed.len(),
            bytes: json.len(),
        };
        write_atomic(&self.data_path(query), json.as_bytes())?;
        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| CacheError::BadData(query.into(), e.to_string()))?;
        write_atomic(&self.meta_path(query), meta_json.as_bytes())?;
        Ok(meta)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CacheError> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| CacheError::Io(tmp.display().to_string(), e))?;
    std::fs::rename(&tmp, path).map_err(|e| CacheError::Io(path.display().to_string(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(cache_key("GROUP=iridium-NEXT"), "group-iridium-next");
        assert_eq!(cache_key("CATNR=25544"), "catnr-25544");
    }

    #[test]
    fn fresh_cache_is_not_refetched_even_when_forced() {
        let dir =
            std::env::temp_dir().join(format!("murmur-space-cache-test-{}", std::process::id()));
        let cache = Cache::new(&dir);
        let now = Utc::now();
        cache
            .import(
                "GROUP=test",
                "[{\"OBJECT_NAME\":\"X\"}]",
                now - Duration::minutes(30),
                "test",
            )
            .expect("import");
        let out = cache.fetch_all(&["GROUP=test".to_string()], Duration::hours(12), true, now);
        assert!(matches!(out[0], FetchOutcome::Fresh(_)), "{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
