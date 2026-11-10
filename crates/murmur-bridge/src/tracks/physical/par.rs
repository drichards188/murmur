//! The physical sensing step's threads (bd murmur-9730). Each vehicle's sensing reads what every
//! observer reads (the truth, the grids, the weather, the cues) and writes only its own state, so
//! the step runs its vehicles side by side and takes their detections back in observer order:
//! the same detections, in the same order, whatever the thread count ([`super::Engine::step`]).
//!
//! The threads are a pool of their own, never the async runtime's: a step hands the pool its
//! vehicles and waits for them (the live bridge calls the sensing step on a blocking thread,
//! `tracks::sim_contacts`). How many: `MURMUR_SENSE_THREADS`, else [`default_threads`], said
//! once at start. One thread is the serial step, on the calling thread, with no pool.

use std::sync::{Arc, OnceLock};

/// The live bridge's default: a quarter of the host's cores, at most 8. The bridge is one process
/// of a stack (the link emulator, the stand-ins, SITL and the console share the host), its other
/// loops and its links' packets take cores of their own, and past 8 the step gains little
/// (docs/load-test.md, "The sensing step on several threads").
#[cfg(not(murmur_headless))]
pub fn default_threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    (cores / 4).clamp(1, 8)
}

/// The headless simulator's default: one. A batch runs one simulator per worker, as many as
/// it is told (docs/HEADLESS.md): threads here on top would oversubscribe the host. One episode
/// on a free host may ask for more.
#[cfg(murmur_headless)]
pub fn default_threads() -> usize {
    1
}

/// Fewer vehicles than this are sensed on the calling thread whatever the pool: handing them
/// over costs more than it saves (the result is the same either way).
const MIN_PARALLEL: usize = 8;

/// Where a step runs its vehicles: on the calling thread, or on a pool of its own.
#[derive(Clone)]
pub struct Pool(Option<Arc<rayon::ThreadPool>>);

impl Pool {
    /// A pool of `threads` (one: none, the calling thread).
    pub fn with_threads(threads: usize) -> Pool {
        if threads <= 1 {
            return Pool(None);
        }
        match rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|i| format!("sense-{i}")).build() {
            Ok(p) => Pool(Some(Arc::new(p))),
            Err(e) => {
                eprintln!("sensing: no pool of {threads} threads ({e}): the step runs on one");
                Pool(None)
            }
        }
    }

    /// The process's pool: `MURMUR_SENSE_THREADS`, else the default, built and said once.
    pub fn shared() -> Pool {
        static SHARED: OnceLock<Pool> = OnceLock::new();
        SHARED
            .get_or_init(|| {
                let (threads, why) = threads_from(std::env::var("MURMUR_SENSE_THREADS").ok().as_deref());
                let pool = Pool::with_threads(threads);
                println!("sensing: the physical step on {} thread{} ({why})", pool.threads(), if pool.threads() == 1 { "" } else { "s" });
                pool
            })
            .clone()
    }

    pub fn threads(&self) -> usize {
        self.0.as_ref().map_or(1, |p| p.current_num_threads())
    }

    /// `items.iter_mut().map(f).collect()`: on the pool when there is one and enough items, its
    /// results in `items`' order either way.
    pub fn map_mut<T: Send, R: Send>(&self, items: &mut [T], f: impl Fn(&mut T) -> R + Sync + Send) -> Vec<R> {
        match &self.0 {
            Some(p) if items.len() >= MIN_PARALLEL => {
                use rayon::prelude::*;
                p.install(|| items.par_iter_mut().map(f).collect())
            }
            _ => items.iter_mut().map(f).collect(),
        }
    }
}

/// The thread count `MURMUR_SENSE_THREADS` asks for (a positive whole number), and why: a value
/// that is not one is said and the default taken.
fn threads_from(var: Option<&str>) -> (usize, String) {
    match var.map(str::trim) {
        None | Some("") => (default_threads(), "the default; MURMUR_SENSE_THREADS sets it".into()),
        Some(v) => match v.parse::<usize>() {
            Ok(n) if n >= 1 => (n, format!("MURMUR_SENSE_THREADS={n}")),
            _ => (default_threads(), format!("MURMUR_SENSE_THREADS={v:?} is not a positive whole number: the default")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thread_count_comes_from_the_variable_or_the_default() {
        assert_eq!(threads_from(Some("3")).0, 3);
        assert_eq!(threads_from(Some(" 12 ")).0, 12);
        for bad in [None, Some(""), Some("0"), Some("-2"), Some("many")] {
            assert_eq!(threads_from(bad).0, default_threads(), "{bad:?}");
        }
        assert!((1..=8).contains(&default_threads()));
    }

    #[test]
    fn a_map_keeps_the_items_order_on_any_pool() {
        let want: Vec<usize> = (0..1_000).map(|i| i * i).collect();
        for threads in [1, 2, 3, 8] {
            let pool = Pool::with_threads(threads);
            assert_eq!(pool.threads(), threads);
            let mut items: Vec<usize> = (0..1_000).collect();
            assert_eq!(pool.map_mut(&mut items, |i| *i * *i), want, "{threads} threads");
        }
    }
}
