//! The sockets: one task per link, owning its one socket (the vehicle-facing port, which also
//! sends to and hears from the bridge) and a delay queue per direction.

use crate::config::{Config, VehicleSpec};
use crate::link::{Env, Link, LinkMeta, LosState};
use crate::model::{Cause, Dir, Params, Verdict};
use crate::stats::LinkStats;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::Instant;

pub struct Options {
    pub listen_host: String,
    pub bridge_host: String,
    pub bridge_base: u16,
    /// The base port the vehicles were launched with (swarm.py `--base-port`); a vehicle's
    /// bridge port is `bridge_base + (port - vehicle_base)`.
    pub vehicle_base: u16,
}

pub struct Pending {
    pub at_ns: u64,
    pub seq: u64,
    pub data: Box<[u8]>,
}

impl PartialEq for Pending {
    fn eq(&self, o: &Self) -> bool {
        self.at_ns == o.at_ns && self.seq == o.seq
    }
}
impl Eq for Pending {}
impl PartialOrd for Pending {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Pending {
    // Reversed: BinaryHeap is a max-heap and we want the earliest delivery first.
    fn cmp(&self, o: &Self) -> Ordering {
        (o.at_ns, o.seq).cmp(&(self.at_ns, self.seq))
    }
}

#[derive(Default)]
pub struct Delay {
    pub heap: BinaryHeap<Pending>,
    pub bytes: u64,
    pub seq: u64,
}

/// The bridge-side port of a vehicle: `bridge_base + (port - vehicle_base)`.
pub fn bridge_port(port: u16, vehicle_base: u16, bridge_base: u16) -> Result<u16, String> {
    let p = i32::from(bridge_base) + i32::from(port) - i32::from(vehicle_base);
    u16::try_from(p).map_err(|_| format!("bridge port {p} out of range (vehicle port {port})"))
}

/// Copies the fleet manifest with each vehicle's `port` replaced by its bridge-side port (the
/// original is kept as `vehicle_port`). The bridge opens exactly the ports its manifest lists,
/// so it must read this copy, not the launcher's, or it would fight the emulator for the
/// vehicle-facing ports.
pub fn write_bridge_manifest(
    src: &std::path::Path,
    dst: &std::path::Path,
    vehicle_base: u16,
    bridge_base: u16,
) -> Result<(), String> {
    let text = std::fs::read_to_string(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let mut doc: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", src.display()))?;
    let vehicles = doc
        .get_mut("vehicles")
        .and_then(|v| v.as_array_mut())
        .ok_or("manifest has no vehicles array")?;
    for v in vehicles {
        let port = v
            .get("port")
            .and_then(serde_json::Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .ok_or("vehicle without a port")?;
        v["vehicle_port"] = port.into();
        v["port"] = bridge_port(port, vehicle_base, bridge_base)?.into();
    }
    let tmp = dst.with_extension("tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dst).map_err(|e| format!("{}: {e}", dst.display()))
}

pub fn build_links(
    fleet: &[VehicleSpec],
    cfg: &Config,
    env: &Arc<Env>,
) -> Result<Vec<Link>, String> {
    let mut out = Vec::new();
    // profile -> (params, gated profile name and params)
    type Entry = (Params, Option<(String, Params)>);
    let mut cache: std::collections::HashMap<String, Entry> = Default::default();
    for v in fleet {
        let profile = cfg.profile_for(v);
        if !cache.contains_key(&profile) {
            let raw = cfg.resolve(&profile).map_err(|e| {
                format!(
                    "uid {} ({}): {e}; add [class.{profile}] to the links file",
                    v.uid, v.platform
                )
            })?;
            let gated = match &raw.gated_profile {
                Some(g) => {
                    let graw = cfg.resolve(g).map_err(|e| format!("profile {profile}: {e}"))?;
                    let gp = Params::from_profile(&graw)
                        .map_err(|e| format!("profile {g} (gated_profile of {profile}): {e}"))?;
                    Some((g.clone(), gp))
                }
                None => None,
            };
            cache.insert(
                profile.clone(),
                (
                    Params::from_profile(&raw).map_err(|e| format!("profile {profile}: {e}"))?,
                    gated,
                ),
            );
        }
        let meta = LinkMeta {
            uid: v.uid,
            sysid: v.sysid,
            port: v.port,
            class: v.link.clone(),
            profile: profile.clone(),
            domain: v.domain.clone(),
            platform: v.platform.clone(),
        };
        let (params, gated) = cache[&profile].clone();
        let link = Link::new(meta, params, env.clone());
        out.push(match gated {
            Some((name, gp)) => link.with_gated(&name, gp),
            None => link,
        });
    }
    Ok(out)
}

/// Binds every socket and spawns the link tasks. Returns the stats handles.
///
/// One socket per link, the vehicle-facing one (`A+i`): the emulator sends to the bridge from
/// it too, so the bridge's replies come back to it and are told from the vehicle's packets by
/// their source (the bridge's port for this link, [`from_bridge`]). Before, each link had a
/// second, bridge-facing socket on an ephemeral port, which with the stand-ins' own capped a
/// host near 4,000 vehicles (the ephemeral range) and left the emulator unusable at 3,000.
pub async fn start(
    links: Vec<Link>,
    opts: &Options,
    start: Instant,
) -> Result<Vec<Arc<LinkStats>>, String> {
    use std::net::ToSocketAddrs;
    let mut all = Vec::new();
    for link in links {
        let port = link.meta.port;
        let bport = bridge_port(port, opts.vehicle_base, opts.bridge_base)?;
        let bridge = (opts.bridge_host.as_str(), bport)
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .ok_or_else(|| format!("no address for {}:{bport}", opts.bridge_host))?;
        let sock = UdpSocket::bind((opts.listen_host.as_str(), port))
            .await
            .map_err(|e| format!("bind {}:{port}: {e}", opts.listen_host))?;
        let stats = Arc::new(
            LinkStats::new(link.meta.clone())
                .with_sat(link.sat_shared(), link.params().latency_ns as f64 / 1e6)
                .with_nominal(link.nominal_json()),
        );
        all.push(stats.clone());
        tokio::spawn(run_link(link, sock, bridge, stats, start));
    }
    Ok(all)
}

/// Whether a datagram from `from` is the bridge's (its port for this link, from its address
/// or, when the bridge is on loopback, from any loopback address); anything else is the
/// vehicle's. The bridge holds its port bound, so no vehicle on this host can send from it.
pub fn from_bridge(from: SocketAddr, bridge: SocketAddr) -> bool {
    from.port() == bridge.port()
        && (from.ip() == bridge.ip() || (from.ip().is_loopback() && bridge.ip().is_loopback()))
}

fn now_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos() as u64
}

async fn run_link(
    mut link: Link,
    sock: UdpSocket,
    bridge: SocketAddr,
    stats: Arc<LinkStats>,
    start: Instant,
) {
    let mut buf = vec![0u8; 2048];
    let mut peer: Option<SocketAddr> = None;
    let mut delay: [Delay; 2] = Default::default();
    let mut last_gauge = 0u64;

    loop {
        let next = delay
            .iter()
            .filter_map(|d| d.heap.peek().map(|p| p.at_ns))
            .min();
        let timer = async {
            match next {
                Some(at) => tokio::time::sleep_until(start + Duration::from_nanos(at)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            r = sock.recv_from(&mut buf) => {
                if let Ok((n, from)) = r {
                    let now = now_ns(start);
                    if from_bridge(from, bridge) {
                        if peer.is_none() {
                            let d = stats.d(Dir::Down);
                            d.in_pkts.fetch_add(1, Relaxed);
                            d.in_bytes.fetch_add(n as u64, Relaxed);
                            d.dropped[Cause::NoPeer.idx()].fetch_add(1, Relaxed);
                        } else {
                            admit(&mut link, &stats, &mut delay[1], Dir::Down, &buf[..n], now);
                        }
                    } else {
                        if peer != Some(from) {
                            stats.set_vehicle_addr(from);
                        }
                        peer = Some(from);
                        link.observe_up(&buf[..n]);
                        if let Some(p) = link.position() {
                            for (i, v) in [p.lat, p.lon, p.alt_m].into_iter().enumerate() {
                                stats.pos[i].store(v.to_bits(), Relaxed);
                            }
                        }
                        admit(&mut link, &stats, &mut delay[0], Dir::Up, &buf[..n], now);
                    }
                }
            }
            () = timer => {}
        }

        let now = now_ns(start);
        for dir in [Dir::Up, Dir::Down] {
            let dl = &mut delay[dir.idx()];
            while dl.heap.peek().is_some_and(|p| p.at_ns <= now) {
                let Some(p) = dl.heap.pop() else { break };
                dl.bytes -= p.data.len() as u64;
                let d = stats.d(dir);
                let to = match dir {
                    Dir::Up => Some(bridge),
                    Dir::Down => peer,
                };
                let sent = match to {
                    Some(to) => sock.send_to(&p.data, to).await.is_ok(),
                    None => false,
                };
                if sent {
                    d.out_pkts.fetch_add(1, Relaxed);
                    d.out_bytes.fetch_add(p.data.len() as u64, Relaxed);
                } else {
                    d.dropped[Cause::Socket.idx()].fetch_add(1, Relaxed);
                }
            }
        }
        if now.saturating_sub(last_gauge) > 250_000_000 {
            last_gauge = now;
            gauge(&mut link, &stats, &delay, now);
        }
    }
}

/// The link's gauges for `/stats` (queues, depth, blocking cause, events, gate, line of
/// sight), refreshed every 250 ms by the link task. Public for the headless simulator's
/// in-memory proxy (crates/murmur-headless), which queues with [`admit`] and [`Delay`] too.
pub fn gauge(link: &mut Link, stats: &LinkStats, delay: &[Delay; 2], now: u64) {
    for dir in [Dir::Up, Dir::Down] {
        let d = stats.d(dir);
        d.queued_pkts
            .store(delay[dir.idx()].heap.len() as u64, Relaxed);
        d.queued_bytes.store(delay[dir.idx()].bytes, Relaxed);
    }
    stats.depth_state.store(
        link.submerged().map_or(0, |s| if s { 2 } else { 1 }),
        Relaxed,
    );
    let b = link.blocked_now(now).map_or(0, |c| c.idx() as u64 + 1);
    stats.blocked.store(b, Relaxed);
    stats.set_events(link.active_events(now));
    stats.on_gated.store(
        u64::from(link.active_profile() != link.meta.profile),
        Relaxed,
    );
    let (los, obs) = match link.los_state() {
        LosState::Off => (0, f64::NAN),
        LosState::Unknown => (1, f64::NAN),
        LosState::Clear(m) => (2, -m),
        LosState::Obstructed(m) => (3, m),
    };
    stats.los_state.store(los, Relaxed);
    stats.los_obstruction.store(obs.to_bits(), Relaxed);
    stats.terrain_unknown.store(link.los_unknown_pkts, Relaxed);
}

pub fn admit(link: &mut Link, stats: &LinkStats, dl: &mut Delay, dir: Dir, data: &[u8], now: u64) {
    let d = stats.d(dir);
    d.in_pkts.fetch_add(1, Relaxed);
    d.in_bytes.fetch_add(data.len() as u64, Relaxed);
    match link.decide(dir, now, data.len(), dl.heap.len()) {
        Verdict::Drop(cause) => {
            d.dropped[cause.idx()].fetch_add(1, Relaxed);
        }
        Verdict::Forward { at_ns, dup_at_ns } => {
            stats.record_latency(dir, at_ns.saturating_sub(now) / 1000);
            let copies = 1 + usize::from(dup_at_ns.is_some());
            if dup_at_ns.is_some() {
                d.duplicated.fetch_add(1, Relaxed);
            }
            for _ in 0..copies {
                dl.seq += 1;
                dl.bytes += data.len() as u64;
                dl.heap.push(Pending {
                    at_ns,
                    seq: dl.seq,
                    data: data.into(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_is_told_from_the_vehicle_by_its_port() {
        let at = |s: &str| s.parse::<SocketAddr>().expect("addr");
        let bridge = at("127.0.0.1:29000");
        assert!(from_bridge(at("127.0.0.1:29000"), bridge));
        // loopback is loopback, whichever address the reply's source took
        assert!(from_bridge(at("127.0.0.2:29000"), bridge));
        // a vehicle on any other port: a stand-in's fixed one, a SITL's ephemeral one
        assert!(!from_bridge(at("127.0.0.1:10000"), bridge));
        assert!(!from_bridge(at("127.0.0.1:41000"), bridge));
        // the bridge's port number on another host is a vehicle there
        assert!(!from_bridge(at("10.0.0.5:29000"), bridge));
        assert!(from_bridge(at("10.0.0.5:29000"), at("10.0.0.5:29000")));
    }
}
