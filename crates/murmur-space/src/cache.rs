//! Element-set cache. CelesTrak updates GP data every 2 hours and enforces "one download per
//! update" per IP: a second request for the same data inside the cycle gets HTTP 403 ("GP data
//! has not updated since your last successful download"), and an IP with 50 HTTP errors
//! (301/403/404) in 2 hours, or more than 100 MB a day, goes into the firewall (usage policy
//! https://celestrak.org/usage-policy.php; GP guide
//! https://celestrak.org/NORAD/documentation/gp-data-formats.php). So: one file per query,
//! never refetched inside the 2-hour cycle (even with `force`), refetched by default only
//! after `max_age`, fetched sequentially with a pause between requests, the run stops at the
//! first HTTP error, and a 403 or 429 starts a persistent backoff (`backoff.json` in the cache
//! directory) so that neither a rerun nor another process retries before it ends.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

pub const CELESTRAK_GP: &str = "https://celestrak.org/NORAD/elements/gp.php";
/// CelesTrak's GP update cycle: refetching sooner can only return the same data.
pub const MIN_REFETCH: Duration = Duration::hours(2);
/// Pause between two requests in one run (courtesy; the queries are few).
pub const REQUEST_SPACING: StdDuration = StdDuration::from_secs(2);
/// How long to stay away after a 403 (one update cycle, the only thing that changes the answer)
/// when the server gives no Retry-After.
pub const BACKOFF_403: Duration = MIN_REFETCH;
/// How long after a 429 without a Retry-After header.
pub const BACKOFF_429: Duration = Duration::minutes(30);
/// Environment variable holding the contact URL or address sent in the User-Agent.
pub const CONTACT_ENV: &str = "MURMUR_SPACE_CONTACT";
/// Placeholder contact until the project has a public page; set `MURMUR_SPACE_CONTACT`.
const CONTACT_PLACEHOLDER: &str = "https://example.org/murmur-contact";

/// A descriptive, honest User-Agent: tool, version, purpose and a contact so CelesTrak can
/// reach whoever runs it instead of blocking the address.
pub fn user_agent() -> String {
    let contact = std::env::var(CONTACT_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| CONTACT_PLACEHOLDER.to_string());
    format!(
        "murmur-space/{} (open-source C2 testbed; caches GP data, at most one fetch per 2 h; contact: {contact})",
        env!("CARGO_PKG_VERSION")
    )
}

/// The GP request for a catalog query. `FORMAT` is uppercase like the query keys; the query
/// itself (`GROUP=...`, `NAME=...`, `CATNR=...`) comes from the catalog and must be uppercase.
pub fn gp_url(endpoint: &str, query: &str) -> String {
    format!("{endpoint}?{query}&FORMAT=JSON")
}

/// What an HTTP error status means for the run.
#[derive(Debug, PartialEq)]
pub struct Refusal {
    pub message: String,
    /// Stay away until this long after now (None: not a rate limit, no backoff).
    pub backoff: Option<Duration>,
}

/// Turn a non-2xx answer into a clear message and, for 403 and 429, a backoff. `body` is
/// CelesTrak's plain-text explanation (shown, trimmed).
pub fn refusal(status: u16, retry_after: Option<&str>, body: &str) -> Refusal {
    let said: String = body.trim().chars().take(160).collect();
    let said = if said.is_empty() {
        String::new()
    } else {
        format!(" CelesTrak said: {said:?}.")
    };
    match status {
        403 => Refusal {
            message: format!(
                "HTTP 403 from CelesTrak: it allows one download per data update (every 2 hours) and blocks an IP address after repeated errors, so this means the data was already downloaded in this cycle, from this address or one sharing it, or the address is blocked.{said} Not retrying; backing off {} h. Use the cache (`murmur-space status`), or `murmur-space import` data from elsewhere.",
                BACKOFF_403.num_hours()
            ),
            backoff: Some(
                retry_after_secs(retry_after)
                    .unwrap_or(BACKOFF_403)
                    .max(BACKOFF_403),
            ),
        },
        429 => {
            let wait = retry_after_secs(retry_after).unwrap_or(BACKOFF_429);
            Refusal {
                message: format!(
                    "HTTP 429 from CelesTrak: too many requests.{said} Not retrying; backing off {} min.",
                    wait.num_minutes().max(1)
                ),
                backoff: Some(wait),
            }
        }
        s => Refusal {
            message: format!(
                "HTTP {s} from CelesTrak.{said} Stopping: repeated HTTP errors get an address blocked."
            ),
            backoff: None,
        },
    }
}

/// A Retry-After header in seconds (the HTTP-date form is not used by CelesTrak; ignored).
fn retry_after_secs(h: Option<&str>) -> Option<Duration> {
    let s: i64 = h?.trim().parse().ok()?;
    (0..=7 * 86_400).contains(&s).then(|| Duration::seconds(s))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backoff {
    pub until: DateTime<Utc>,
    pub reason: String,
}

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
    #[error("{}", .0.message)]
    Refused(Refusal),
    #[error("{0}: response is not a GP JSON array ({1})")]
    BadData(String, String),
    #[error("no cached element sets for {0}; run `murmur-space fetch`")]
    Missing(String),
}

pub struct Cache {
    pub dir: PathBuf,
    endpoint: String,
    spacing: StdDuration,
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
        Cache {
            dir: dir.into(),
            endpoint: CELESTRAK_GP.to_string(),
            spacing: REQUEST_SPACING,
        }
    }

    /// For tests: another endpoint, no pause between requests.
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.to_string();
        self.spacing = StdDuration::ZERO;
        self
    }

    fn backoff_path(&self) -> PathBuf {
        self.dir.join("backoff.json")
    }

    /// The backoff in force at `now`, if any (an expired one is removed).
    pub fn backoff(&self, now: DateTime<Utc>) -> Option<Backoff> {
        let b: Backoff =
            serde_json::from_str(&std::fs::read_to_string(self.backoff_path()).ok()?).ok()?;
        if b.until > now {
            Some(b)
        } else {
            let _ = std::fs::remove_file(self.backoff_path());
            None
        }
    }

    fn set_backoff(&self, until: DateTime<Utc>, reason: &str) {
        let b = Backoff {
            until,
            reason: reason.to_string(),
        };
        if std::fs::create_dir_all(&self.dir).is_ok()
            && let Ok(json) = serde_json::to_string_pretty(&b)
        {
            let _ = write_atomic(&self.backoff_path(), json.as_bytes());
        }
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
        let mut stop: Option<String> = self.backoff(now).map(|b| {
            format!(
                "CelesTrak backoff until {} ({}); nothing was requested",
                b.until.format("%Y-%m-%d %H:%MZ"),
                b.reason
            )
        });
        let mut fetched_any = false;
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
            if fetched_any {
                std::thread::sleep(self.spacing);
            }
            fetched_any = true;
            match self.fetch_one(q, now) {
                Ok(meta) => out.push(FetchOutcome::Fetched(meta)),
                Err(e) => {
                    if let CacheError::Refused(r) = &e
                        && let Some(wait) = r.backoff
                    {
                        self.set_backoff(now + wait, &r.message);
                    }
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
        let url = gp_url(&self.endpoint, query);
        // Read error statuses as answers: CelesTrak explains a refusal in the body.
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut resp = agent
            .get(&url)
            .header("User-Agent", user_agent())
            .call()
            .map_err(|e| CacheError::Http(url.clone(), e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let retry_after = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(CacheError::Refused(refusal(
                status,
                retry_after.as_deref(),
                &body,
            )));
        }
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
    fn user_agent_names_the_tool_and_a_contact() {
        let ua = user_agent();
        assert!(ua.starts_with("murmur-space/"), "{ua}");
        assert!(ua.contains("contact: "), "{ua}");
    }

    #[test]
    fn urls_use_uppercase_format() {
        assert_eq!(
            gp_url("https://x/gp.php", "GROUP=starlink"),
            "https://x/gp.php?GROUP=starlink&FORMAT=JSON"
        );
    }

    #[test]
    fn refusals_403_429_and_others() {
        let r = refusal(
            403,
            None,
            "GP data has not updated since your last successful download",
        );
        assert_eq!(r.backoff, Some(BACKOFF_403));
        assert!(r.message.contains("has not updated"), "{}", r.message);
        let r = refusal(429, Some("90"), "");
        assert_eq!(r.backoff, Some(Duration::seconds(90)));
        let r = refusal(403, Some("60"), "");
        assert_eq!(
            r.backoff,
            Some(BACKOFF_403),
            "never shorter than one update cycle"
        );
        let r = refusal(500, None, "oops");
        assert_eq!(r.backoff, None);
    }

    /// One-shot HTTP server answering every request with `status` and `body`; returns its
    /// address and a counter of requests served.
    fn serve(
        status: u16,
        body: &'static str,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = format!("http://{}/gp.php", listener.local_addr().expect("addr"));
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        std::thread::spawn(move || {
            for mut s in listener.incoming().flatten() {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                h.fetch_add(1, SeqCst);
                let _ = write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (addr, hits)
    }

    #[test]
    fn a_403_stops_the_run_and_backs_off_across_runs() {
        use std::sync::atomic::Ordering::SeqCst;
        let dir = std::env::temp_dir().join(format!("murmur-space-403-{}", std::process::id()));
        let (addr, hits) = serve(
            403,
            "GP data has not updated since your last successful download",
        );
        let cache = Cache::new(&dir).with_endpoint(&addr);
        let now = Utc::now();
        let qs = vec!["GROUP=a".to_string(), "GROUP=b".to_string()];
        let out = cache.fetch_all(&qs, Duration::hours(12), false, now);
        assert!(
            matches!(&out[0], FetchOutcome::Failed { error, .. } if error.contains("403")),
            "{out:?}"
        );
        assert!(matches!(out[1], FetchOutcome::Skipped { .. }), "{out:?}");
        assert_eq!(
            hits.load(SeqCst),
            1,
            "the second query must not be requested"
        );
        assert!(cache.backoff(now).is_some());
        // a rerun inside the backoff requests nothing
        let out = cache.fetch_all(&qs, Duration::hours(12), false, now + Duration::minutes(10));
        assert!(
            matches!(&out[0], FetchOutcome::Skipped { reason, .. } if reason.contains("backoff")),
            "{out:?}"
        );
        assert_eq!(hits.load(SeqCst), 1);
        assert!(
            cache
                .backoff(now + BACKOFF_403 + Duration::minutes(1))
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
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
