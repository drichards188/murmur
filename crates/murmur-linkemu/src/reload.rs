//! Reloading the scripted events while running: on SIGHUP, or when the config or scenario
//! file changes on disk (polled once a second). Only events are reloaded; profiles are not.
//! A file that fails to parse leaves the running events in place and logs why.

use crate::config::Config;
use crate::link::Env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Re-reads the events and swaps them in. Returns how many are now scheduled.
pub fn reload_now(env: &Env, config: &Path, scenario: Option<&Path>) -> Result<usize, String> {
    let events = Config::load_events(config, scenario)?;
    env.reload_events(&events)
}

/// Starts the watcher task. Call from inside the runtime.
pub fn spawn(env: Arc<Env>, config: PathBuf, scenario: Option<PathBuf>) {
    tokio::spawn(async move {
        #[cfg(unix)]
        let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok();
        let stamp = |c: &Path, s: Option<&Path>| (mtime(c), s.and_then(mtime));
        let mut seen = stamp(&config, scenario.as_deref());
        let mut poll = tokio::time::interval(Duration::from_secs(1));
        loop {
            let why;
            #[cfg(unix)]
            {
                tokio::select! {
                    _ = poll.tick() => {
                        if stamp(&config, scenario.as_deref()) == seen {
                            continue;
                        }
                        why = "file changed";
                    }
                    got = async {
                        match hup.as_mut() {
                            Some(h) => h.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        if got.is_none() {
                            hup = None;
                            continue;
                        }
                        why = "SIGHUP";
                    }
                }
            }
            #[cfg(not(unix))]
            {
                poll.tick().await;
                if stamp(&config, scenario.as_deref()) == seen {
                    continue;
                }
                why = "file changed";
            }
            seen = stamp(&config, scenario.as_deref());
            match reload_now(&env, &config, scenario.as_deref()) {
                Ok(n) => eprintln!("linkemu: reloaded events ({why}): {n} scheduled"),
                Err(e) => {
                    eprintln!("linkemu: reload failed ({why}), keeping the running events: {e}");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::NoTerrain;

    #[test]
    fn reload_now_swaps_events_and_keeps_old_ones_on_a_parse_error() {
        let dir = std::env::temp_dir().join(format!("linkemu-reload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let cfg_path = dir.join("links.toml");
        let scn_path = dir.join("scenario.toml");
        std::fs::write(&cfg_path, "[class.los]\n[[event]]\nat_s = 5\ncut = true\n").expect("w");
        std::fs::write(&scn_path, "[[event]]\nat_s = 9\nloss = 0.5\n").expect("w");
        let cfg = Config::load(&cfg_path).expect("cfg");
        let env = Env::new(&cfg, Arc::new(NoTerrain), 1).expect("env");
        assert_eq!(env.schedule().names().len(), 1);
        assert_eq!(reload_now(&env, &cfg_path, Some(&scn_path)), Ok(2));
        assert_eq!(env.schedule().names().len(), 2);
        std::fs::write(&scn_path, "[[event]]\nat_s = nope\n").expect("w");
        assert!(reload_now(&env, &cfg_path, Some(&scn_path)).is_err());
        assert_eq!(env.schedule().names().len(), 2, "old events kept");
        std::fs::remove_dir_all(&dir).ok();
    }
}
