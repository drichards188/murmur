//! Real sockets, real clock: a vehicle, the emulator and a fake bridge on loopback.

use murmur_linkemu::config::{Config, VehicleSpec};
use murmur_linkemu::geo::NoTerrain;
use murmur_linkemu::link::Env;
use murmur_linkemu::proxy::{Options, build_links, start};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

fn spec(uid: u32, port: u16, class: &str) -> VehicleSpec {
    VehicleSpec {
        uid,
        sysid: uid,
        port,
        link: class.into(),
        domain: "air".into(),
        platform: "t".into(),
        callsign: String::new(),
    }
}

#[tokio::test]
async fn latency_both_ways_and_reply_returns_to_the_vehicle() {
    let cfg =
        Config::parse("[class.slow]\nlatency_ms = 60\n[class.dead]\nloss = 1.0\n").expect("cfg");
    let fleet = vec![spec(1, 19901, "slow"), spec(2, 19902, "dead")];
    let env = Arc::new(Env::new(&cfg, Arc::new(NoTerrain), 1).expect("env"));
    let links = build_links(&fleet, &cfg, &env).expect("links");
    let opts = Options {
        listen_host: "127.0.0.1".into(),
        bridge_host: "127.0.0.1".into(),
        bridge_base: 19951,
        vehicle_base: 19901,
    };
    let stats = start(links, &opts, tokio::time::Instant::now())
        .await
        .expect("start");

    let bridge1 = UdpSocket::bind("127.0.0.1:19951").await.expect("bridge1");
    let bridge2 = UdpSocket::bind("127.0.0.1:19952").await.expect("bridge2");
    let v1 = UdpSocket::bind("127.0.0.1:0").await.expect("v1");
    v1.connect("127.0.0.1:19901").await.expect("connect");
    let v2 = UdpSocket::bind("127.0.0.1:0").await.expect("v2");
    v2.connect("127.0.0.1:19902").await.expect("connect");

    let t0 = Instant::now();
    v1.send(b"ping").await.expect("send");
    v2.send(b"lost").await.expect("send");
    let mut buf = [0u8; 64];
    let (n, from) = tokio::time::timeout(Duration::from_secs(2), bridge1.recv_from(&mut buf))
        .await
        .expect("timeout")
        .expect("recv");
    assert_eq!(&buf[..n], b"ping");
    let up = t0.elapsed();
    assert!(
        up >= Duration::from_millis(58) && up < Duration::from_millis(150),
        "{up:?}"
    );

    bridge1.send_to(b"pong", from).await.expect("reply");
    let n = tokio::time::timeout(Duration::from_secs(2), v1.recv(&mut buf))
        .await
        .expect("timeout")
        .expect("recv");
    assert_eq!(&buf[..n], b"pong");
    let rtt = t0.elapsed();
    assert!(
        rtt >= Duration::from_millis(118) && rtt < Duration::from_millis(300),
        "{rtt:?}"
    );

    // The fully lossy link delivers nothing and counts the drop by cause.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), bridge2.recv_from(&mut buf))
            .await
            .is_err()
    );
    let j = stats[1].json();
    assert_eq!(j["up"]["dropped"]["loss"], 1);
    assert_eq!(stats[0].json()["up"]["out_pkts"], 1);
}

#[test]
fn shipped_links_file_parses_and_every_profile_resolves() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sim/links.toml");
    let cfg = Config::load(&path).expect("sim/links.toml");
    for class in [
        "los",
        "mesh",
        "satcom",
        "satcom_leo",
        "satcom_iridium",
        "acoustic",
        "tether",
    ] {
        let raw = cfg.resolve(class).expect("resolve");
        murmur_linkemu::model::Params::from_profile(&raw).expect("params");
    }
    // GEO satcom: 300 ms each way is the ~600 ms round trip.
    assert_eq!(
        cfg.resolve("satcom").expect("satcom").latency_ms,
        Some(300.0)
    );
}
