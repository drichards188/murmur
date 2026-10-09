//! Where an episode's computer time goes, per task (the `perf` request): each spawned task's
//! polls timed on the real clock. Diagnostics only: nothing here feeds the simulation.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

static SPENT: Mutex<BTreeMap<&'static str, (Duration, u64)>> = Mutex::new(BTreeMap::new());

pub fn add(name: &'static str, d: Duration) {
    let mut m = SPENT.lock().unwrap_or_else(|e| e.into_inner());
    let e = m.entry(name).or_insert((Duration::ZERO, 0));
    e.0 += d;
    e.1 += 1;
}

/// Seconds and polls per task so far.
pub fn report() -> serde_json::Value {
    let m = SPENT.lock().unwrap_or_else(|e| e.into_inner());
    m.iter()
        .map(|(k, (d, n))| ((*k).to_string(), serde_json::json!({ "s": d.as_secs_f64(), "polls": n })))
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// A future whose polls are timed under `name`.
pub struct Timed<F> {
    f: Pin<Box<F>>,
    name: &'static str,
}

impl<F: Future> Future for Timed<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        let t0 = Instant::now();
        let name = self.name;
        let out = self.f.as_mut().poll(cx);
        add(name, t0.elapsed());
        out
    }
}

pub fn timed<F: Future>(name: &'static str, f: F) -> Timed<F> {
    Timed { f: Box::pin(f), name }
}
