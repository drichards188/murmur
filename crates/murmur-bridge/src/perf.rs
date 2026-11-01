//! How long the bridge's periodic work takes and how regularly it runs (murmur-z07i): the
//! sensing step and the tracker's fold on each truth message, the tracker's and the decision
//! loop's once-a-second ticks, the push frame four times a second, `/api/swarms`. Each loop
//! counts its runs, its busy time (mean and worst) and the longest gap between two starts, so a
//! loop that falls behind its period shows as a gap past it. Read-only diagnostics: nothing
//! decides on them and nothing here is logged to the audit log. `/api/stats` carries them
//! (`loops`: the last closed minute and the open one); [`report`] prints a closed minute.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[derive(Clone, Copy, Default)]
struct Window {
    runs: u64,
    busy: Duration,
    worst: Duration,
    worst_gap: Duration,
}

impl Window {
    fn json(&self) -> Value {
        let ms = |d: Duration| (d.as_secs_f64() * 1e4).round() / 10.0;
        json!({
            "runs": self.runs,
            "mean_ms": if self.runs == 0 { 0.0 } else { ms(self.busy / self.runs as u32) },
            "worst_ms": ms(self.worst),
            "worst_gap_ms": ms(self.worst_gap),
        })
    }
}

#[derive(Default)]
struct Loop {
    open: Window,
    closed: Option<Window>,
    last_start: Option<Instant>,
}

static LOOPS: Mutex<BTreeMap<&'static str, Loop>> = Mutex::new(BTreeMap::new());

fn loops() -> std::sync::MutexGuard<'static, BTreeMap<&'static str, Loop>> {
    LOOPS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Records one run of `name` that started at `start` and took `busy`.
pub fn record(name: &'static str, start: Instant, busy: Duration) {
    let mut all = loops();
    let l = all.entry(name).or_default();
    if let Some(prev) = l.last_start {
        l.open.worst_gap = l.open.worst_gap.max(start.saturating_duration_since(prev));
    }
    l.last_start = Some(start);
    l.open.runs += 1;
    l.open.busy += busy;
    l.open.worst = l.open.worst.max(busy);
}

/// Runs `f` as one run of `name`.
pub fn time<R>(name: &'static str, f: impl FnOnce() -> R) -> R {
    let start = Instant::now();
    let r = f();
    record(name, start, start.elapsed());
    r
}

/// `/api/stats`'s `loops`: per loop the last closed minute and the open one.
pub fn json() -> Value {
    let all = loops();
    Value::Object(
        all.iter()
            .map(|(k, l)| {
                (
                    (*k).to_string(),
                    json!({ "minute": l.closed.map(|w| w.json()), "open": l.open.json() }),
                )
            })
            .collect(),
    )
}

/// Closes the open minute of every loop and returns it as one line (none before any run).
fn close() -> Option<String> {
    let mut all = loops();
    let parts: Vec<String> = all
        .iter_mut()
        .map(|(k, l)| {
            let w = std::mem::take(&mut l.open);
            l.closed = Some(w);
            let j = w.json();
            format!(
                "{k} {}x mean {} worst {} gap {} ms",
                w.runs, j["mean_ms"], j["worst_ms"], j["worst_gap_ms"]
            )
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// A line a minute in the bridge's log: each loop's runs, mean and worst busy time and
/// longest gap between starts in that minute.
pub async fn report() {
    let mut every = tokio::time::interval(Duration::from_secs(60));
    every.tick().await;
    loop {
        every.tick().await;
        if let Some(line) = close() {
            println!("loops (last minute): {line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_loop_counts_its_runs_busy_time_and_gaps() {
        let t0 = Instant::now();
        record("test-loop", t0, Duration::from_millis(10));
        record("test-loop", t0 + Duration::from_millis(1000), Duration::from_millis(30));
        record("test-loop", t0 + Duration::from_millis(3500), Duration::from_millis(20));
        let j = json();
        let open = &j["test-loop"]["open"];
        assert_eq!(open["runs"], 3);
        assert_eq!(open["mean_ms"], 20.0);
        assert_eq!(open["worst_ms"], 30.0);
        assert_eq!(open["worst_gap_ms"], 2500.0);
        assert!(close().is_some_and(|l| l.contains("test-loop 3x")));
        let j = json();
        assert_eq!(j["test-loop"]["minute"]["runs"], 3);
        assert_eq!(j["test-loop"]["open"]["runs"], 0);
    }
}
