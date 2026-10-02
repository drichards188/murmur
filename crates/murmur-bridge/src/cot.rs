//! Cursor-on-Target (CoT) export: the fleet as a TAK picture. Read-only by construction:
//! this module only reads the world model and writes CoT out. Nothing received from a TAK
//! client or server is ever turned into a task (the optional inbound side only lists markers
//! at `/api/cot/inbound`).
//!
//! # Event shape
//!
//! One CoT event (version 2.0) per vehicle that has a position and a live link:
//!
//! ```xml
//! <event version="2.0" uid="murmur-main-3" type="a-f-A-M-F-Q" how="m-g"
//!        time="2026-10-02T12:00:00.000Z" start="2026-10-02T12:00:00.000Z"
//!        stale="2026-10-02T12:00:04.000Z">
//!   <point lat="37.8068000" lon="-122.4460000" hae="105.0" ce="10.0" le="15.0"/>
//!   <detail>
//!     <contact callsign="HAWK 1"/><uid Droid="HAWK 1"/>
//!     <track course="90.0" speed="21.50"/><status battery="87"/>
//!     <remarks>...</remarks><_murmur .../>
//!   </detail>
//! </event>
//! ```
//!
//! Shape checked against the public references: the CoT 2.0 base schema and the MITRE "Cursor
//! on Target Message Router Developer Guide" (event attributes `version uid type time start
//! stale how`, required `point` with `lat lon hae ce le`, free-form `detail`); the ATAK/TAK
//! `detail` conventions `contact callsign`, `track course speed`, `status battery`, `remarks`,
//! `__group` (as documented in the TAK Product Center CoT "Detail" notes and used by PyTAK's
//! `gen_cot` and FreeTAKServer's `Event`/`Point`/`Detail` classes); and the type taxonomy of
//! TAK's `CoTtypes.xml` (`a-f-A-M-F-Q` is "friendly military fixed-wing UAV"). `how="m-g"` is
//! machine-generated, GPS-derived. Unknown detail children (`_murmur`) are ignored by TAK
//! clients, so Murmur-specific fields ride there.
//!
//! # SIDC to CoT type
//!
//! The 15-character legacy MIL-STD-2525B/C SIDC is `S F A P M F Q - - - - - - - -`: scheme,
//! affiliation, battle dimension, status, function ID (6), modifiers, country, order of
//! battle. The CoT atom type is `a` (atom) then the affiliation, the battle dimension, and the
//! function ID letters up to the first `-`, each as its own segment:
//!
//! | SIDC (platforms.toml)  | CoT type        | meaning                          |
//! |------------------------|-----------------|----------------------------------|
//! | `SFAPMFQ--------`      | `a-f-A-M-F-Q`   | friendly fixed-wing UAV          |
//! | `SFAPMHQ--------`      | `a-f-A-M-H-Q`   | friendly rotary-wing UAV         |
//! | `SFAPML---------`      | `a-f-A-M-L`     | friendly lighter-than-air        |
//! | `SFGPEV---------`      | `a-f-G-E-V`     | friendly ground vehicle          |
//! | `SFGPE----------`      | `a-f-G-E`       | friendly ground equipment        |
//! | `SFSP-----------`      | `a-f-S`         | friendly surface                 |
//! | `SFUP-----------`      | `a-f-U`         | friendly subsurface              |
//!
//! Affiliation letters: F/D friend, A/M assumed friend, H hostile, S suspect, N/L neutral,
//! U/W unknown, P/G pending, J joker, K faker, `-` unknown. Battle dimensions: P space,
//! A air, G ground, S surface, U subsurface, F SOF, X other.
//!
//! Limits: the status character (present, planned) and everything after the function ID
//! (modifiers, country, echelon) is dropped, since CoT types carry none of it. Non-warfighting
//! schemes (tactical graphics, weather, METOC, ...) have no `a-` atom form and fall back to
//! the platform's domain and autopilot kind. 20-digit 2525D SIDCs are only mapped to
//! affiliation and dimension (their numeric entity codes have no letter equivalent without the
//! full 2525D-to-B translation table); the fleet's own SIDCs are all 15-character legacy.
//! A function ID that CoTtypes.xml does not list still goes out as a valid atom path; TAK
//! clients show the nearest listed ancestor icon.
//!
//! # Height
//!
//! CoT `hae` is height above the WGS84 ellipsoid. MAVLink gives MSL (AMSL). Without a geoid
//! model in the bridge, `hae = alt_msl + MURMUR_COT_GEOID_M` (default 0, so the approximation
//! error is the local geoid undulation: about -32 m near San Francisco, within +-100 m
//! worldwide). Set the variable per theatre if it matters. Subsurface vehicles report negative
//! altitude as depth: it goes out as negative `hae` and a `depth N m` remark.
//! `ce` is 10 m with a 3D GPS fix and 100 m otherwise (inertial navigation); `le` 15 m and
//! 100 m. Those are conservative nominal figures, not measured accuracy.
//!
//! # Tracks (Phase C2)
//!
//! Contacts that are not ours go out too, one event per bridge track (`tracks` module),
//! uid `murmur-<tag>-track-<n>`. The type comes from the track's own 2525 SIDC through the
//! same table, so the affiliation the tracker estimated (or an operator declared) is the
//! TAK affiliation: `a-h-S` hostile surface, `a-s-S` suspect, `a-n-S` neutral, `a-u-S`
//! unknown, `a-a-S` assumed friend; `A` air, `U` subsurface, `G` ground. `how` is `m-f`
//! (machine fused: the bridge fuses detections over time and, when it has them, across
//! sensors). `ce` is the track's 1-sigma position uncertainty at that moment, growing with
//! age; `le` is 50 m (or unknown for a bearing-only or surface track where height isn't
//! estimated). The remarks carry the estimated kind, confidence, sources and age; `_murmur`
//! the same as attributes, plus `predicted="1"` once the track is stale and the point is
//! dead-reckoned from its last fix, and `fused="1"` when more than one sensor feeds it. A
//! track is published until the bridge drops it, going stale on the client (`stale = time
//! + period + pad`) when the bridge stops sending it.
//!
//! # Staleness and lost links
//!
//! `stale = time + period + MURMUR_COT_STALE_PAD_S` (default 1 s + 3 s). A vehicle whose link
//! has been silent for more than `MURMUR_COT_LOST_S` (default 5 s) is not published at all, so
//! its last marker goes stale on every client rather than showing an invented position.
//!
//! Both limits follow the link. A vehicle on a low-rate link (an acoustic AUV reporting every
//! 20 s) is lost after `max(MURMUR_COT_LOST_S, 3 x its report interval)` (60 s), the same
//! rule the decision layer uses, and its events carry `stale = time + max(period, report
//! interval) + pad`, so the TAK marker lasts a full report cycle instead of flickering.
//!
//! # Configuration (environment; every output is off unless set)
//!
//! | variable | meaning |
//! |----------|---------|
//! | `MURMUR_COT_MULTICAST` | `1` for 239.2.3.1:6969 (the TAK SA address) or `group:port` |
//! | `MURMUR_COT_MCAST_TTL` | multicast TTL (default 1) |
//! | `MURMUR_COT_MCAST_IFACE` | local IPv4 to send multicast from (default: OS choice) |
//! | `MURMUR_COT_UDP` | unicast UDP targets, `host:port[,host:port]` |
//! | `MURMUR_COT_TCP` | TAK Server plain-CoT input, `host:port` (e.g. 8087) |
//! | `MURMUR_COT_RATE_HZ` | steady rate per vehicle (default 1) |
//! | `MURMUR_COT_MAX_HZ` | cap for change-driven early updates (default 2) |
//! | `MURMUR_COT_MOVE_M` | move this far to send early (default 5 m; heading change 10 deg) |
//! | `MURMUR_COT_STALE_PAD_S`, `MURMUR_COT_LOST_S` | see above |
//! | `MURMUR_COT_TAG` | uid prefix part, `murmur-<tag>-<uid>` (default `main`) |
//! | `MURMUR_COT_GROUP` | optional ATAK team colour for `__group` (e.g. `Cyan`) |
//! | `MURMUR_COT_GEOID_M` | MSL to HAE offset in metres (default 0) |
//! | `MURMUR_COT_LISTEN` | inbound: UDP `host:port`, joins the group when it is multicast |
//! | `MURMUR_COT_INBOUND` | `1`: also read CoT coming back on the TCP connection |
//!
//! TLS (TAK Server 8089, client certificates) is not implemented: plain TCP 8087, UDP and
//! multicast only. Use a TAK Server plain input on a trusted network, or stunnel in front.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::routing::get;
use chrono::{DateTime, SecondsFormat, Utc};
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;

use crate::clock;
use crate::vehicle::Uid;
use crate::world::World;

pub const SA_MULTICAST: (Ipv4Addr, u16) = (Ipv4Addr::new(239, 2, 3, 1), 6969);
const TICK: Duration = Duration::from_millis(250);
/// Unknown circular/linear error in CoT.
pub const UNKNOWN_ERR: f64 = 9_999_999.0;
const INBOUND_KEEP: usize = 2000;
const MAX_EVENT_BYTES: usize = 64 * 1024;

// ------------------------------------------------------------------ SIDC -> CoT type

/// CoT type for a SIDC, or `None` when it has no atom form (not a warfighting symbol, or no
/// dimension). See the module doc for the table and limits.
pub fn type_from_sidc(sidc: &str) -> Option<String> {
    let s = sidc.trim();
    if s.len() >= 10 && s.bytes().all(|b| b.is_ascii_digit()) {
        return type_from_sidc_2525d(s);
    }
    let c: Vec<char> = s.to_ascii_uppercase().chars().collect();
    if c.len() < 3 || c[0] != 'S' {
        return None;
    }
    let aff = match c[1] {
        'F' | 'D' => 'f',
        'A' | 'M' => 'a',
        'H' => 'h',
        'S' => 's',
        'N' | 'L' => 'n',
        'U' | 'W' | '-' => 'u',
        'P' | 'G' => 'p',
        'J' => 'j',
        'K' => 'k',
        _ => return None,
    };
    let dim = match c[2] {
        d @ ('P' | 'A' | 'G' | 'S' | 'U' | 'F' | 'X') => d,
        _ => return None,
    };
    let mut out = format!("a-{aff}-{dim}");
    // function ID: chars 5..11 (index 4..10), up to the first filler
    for &f in c.iter().skip(4).take(6) {
        if !f.is_ascii_alphabetic() {
            break;
        }
        out.push('-');
        out.push(f);
    }
    Some(out)
}

/// 2525D: `VV C I SS ...` (version, context, standard identity, symbol set): affiliation and
/// dimension only.
fn type_from_sidc_2525d(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let aff = match b[3] {
        b'0' => 'p',
        b'1' => 'u',
        b'2' => 'a',
        b'3' => 'f',
        b'4' => 'n',
        b'5' => 's',
        b'6' => 'h',
        b'7' => 'j',
        b'8' => 'k',
        _ => return None,
    };
    let dim = match &s[4..6] {
        "01" | "02" => 'A',
        "05" | "06" => 'P',
        "10" | "11" | "15" | "20" => 'G',
        "30" => 'S',
        "35" => 'U',
        _ => return None,
    };
    Some(format!("a-{aff}-{dim}"))
}

/// When the SIDC is missing or unusable: friendly, with the dimension from the platform's
/// domain and the function from the autopilot kind.
pub fn fallback_type(domain: Option<&str>, kind: Option<&str>) -> String {
    match (domain, kind) {
        (_, Some("plane" | "vtol")) => "a-f-A-M-F-Q",
        (_, Some("copter")) => "a-f-A-M-H-Q",
        (_, Some("blimp")) => "a-f-A-M-L",
        (_, Some("ugv")) => "a-f-G-E-V",
        (_, Some("tracker")) => "a-f-G-E",
        (_, Some("boat")) => "a-f-S",
        (_, Some("sub")) => "a-f-U",
        (Some("air"), _) => "a-f-A",
        (Some("surface"), _) => "a-f-S",
        (Some("subsurface"), _) => "a-f-U",
        _ => "a-f-G",
    }
    .to_string()
}

// ------------------------------------------------------------------ configuration

#[derive(Debug, Clone)]
pub struct CotConfig {
    pub multicast: Option<SocketAddr>,
    pub mcast_ttl: u32,
    pub mcast_iface: Option<Ipv4Addr>,
    pub udp: Vec<String>,
    pub tcp: Option<String>,
    pub listen: Option<String>,
    pub tcp_inbound: bool,
    pub rate_hz: f64,
    pub max_hz: f64,
    pub move_m: f64,
    pub stale_pad_s: f64,
    pub lost_s: f64,
    pub tag: String,
    pub group: Option<String>,
    pub geoid_m: f64,
}

impl Default for CotConfig {
    fn default() -> Self {
        CotConfig {
            multicast: None,
            mcast_ttl: 1,
            mcast_iface: None,
            udp: Vec::new(),
            tcp: None,
            listen: None,
            tcp_inbound: false,
            rate_hz: 1.0,
            max_hz: 2.0,
            move_m: 5.0,
            stale_pad_s: 3.0,
            lost_s: 5.0,
            tag: "main".into(),
            group: None,
            geoid_m: 0.0,
        }
    }
}

impl CotConfig {
    pub fn from_env() -> CotConfig {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> CotConfig {
        let text = |k: &str| {
            get(k)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let num = |k: &str, d: f64| text(k).and_then(|s| s.parse().ok()).unwrap_or(d);
        let flag = |k: &str| {
            text(k).is_some_and(|s| {
                !matches!(
                    s.to_ascii_lowercase().as_str(),
                    "0" | "false" | "off" | "no"
                )
            })
        };
        let d = CotConfig::default();
        let multicast = text("MURMUR_COT_MULTICAST")
            .filter(|_| flag("MURMUR_COT_MULTICAST"))
            .map(|s| {
                s.parse::<SocketAddr>()
                    .unwrap_or_else(|_| SocketAddr::new(IpAddr::V4(SA_MULTICAST.0), SA_MULTICAST.1))
            });
        CotConfig {
            multicast,
            mcast_ttl: num("MURMUR_COT_MCAST_TTL", 1.0) as u32,
            mcast_iface: text("MURMUR_COT_MCAST_IFACE").and_then(|s| s.parse().ok()),
            udp: text("MURMUR_COT_UDP")
                .map(|s| {
                    s.split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            tcp: text("MURMUR_COT_TCP"),
            listen: text("MURMUR_COT_LISTEN"),
            tcp_inbound: flag("MURMUR_COT_INBOUND"),
            rate_hz: num("MURMUR_COT_RATE_HZ", d.rate_hz).clamp(0.05, 20.0),
            max_hz: num("MURMUR_COT_MAX_HZ", d.max_hz).clamp(0.05, 20.0),
            move_m: num("MURMUR_COT_MOVE_M", d.move_m).max(0.0),
            stale_pad_s: num("MURMUR_COT_STALE_PAD_S", d.stale_pad_s).max(0.0),
            lost_s: num("MURMUR_COT_LOST_S", d.lost_s).max(0.5),
            tag: text("MURMUR_COT_TAG").unwrap_or(d.tag),
            group: text("MURMUR_COT_GROUP"),
            geoid_m: num("MURMUR_COT_GEOID_M", 0.0),
        }
    }

    pub fn outputs_enabled(&self) -> bool {
        self.multicast.is_some() || !self.udp.is_empty() || self.tcp.is_some()
    }
}

// ------------------------------------------------------------------ event building

/// What the exporter reads from the world model, one vehicle.
#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub uid: Uid,
    pub callsign: Option<String>,
    pub platform: Option<String>,
    pub name: Option<String>,
    pub role: Option<String>,
    pub domain: Option<String>,
    pub sidc: Option<String>,
    pub link_kind: Option<String>,
    pub kind: Option<String>,
    pub mode: Option<String>,
    pub armed: bool,
    pub lat: f64,
    pub lon: f64,
    pub alt_msl: Option<f64>,
    pub heading: Option<f64>,
    pub groundspeed: Option<f64>,
    pub battery_pct: Option<i8>,
    pub gps_fix: u8,
    /// wall-clock seconds of the last packet from the vehicle
    pub last_rx: Option<f64>,
    pub loss_pct: Option<i64>,
    pub rtt_ms: Option<i64>,
    /// How often the link should report, ms (1000 streaming, 20000 acoustic); 0 = unknown,
    /// which behaves as streaming.
    pub expected_interval_ms: u32,
}

impl Sample {
    fn interval_s(&self) -> f64 {
        f64::from(self.expected_interval_ms) / 1000.0
    }
}

pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // not legal in XML 1.0
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

fn iso(t: f64) -> String {
    let secs = t.floor();
    let nanos = ((t - secs) * 1e9) as u32;
    DateTime::<Utc>::from_timestamp(secs as i64, nanos.min(999_999_999))
        .unwrap_or_default()
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn uid_for(cfg: &CotConfig, uid: Uid) -> String {
    format!("murmur-{}-{}", xml_escape(&cfg.tag), uid)
}

/// The CoT type for this vehicle: SIDC when usable, else the domain/kind fallback.
pub fn type_for(s: &Sample) -> String {
    s.sidc
        .as_deref()
        .and_then(type_from_sidc)
        .unwrap_or_else(|| fallback_type(s.domain.as_deref(), s.kind.as_deref()))
}

/// One CoT event as XML. `now` is wall seconds; `period` the steady update interval.
pub fn build_event(cfg: &CotConfig, s: &Sample, now: f64, period: f64) -> String {
    let uid = uid_for(cfg, s.uid);
    let callsign = s
        .callsign
        .clone()
        .unwrap_or_else(|| format!("MURMUR-{}", s.uid));
    let hae = s.alt_msl.map(|a| a + cfg.geoid_m);
    let (ce, le) = if s.gps_fix >= 3 {
        (10.0, 15.0)
    } else {
        (100.0, 100.0)
    };
    let time = iso(now);
    let stale = iso(now + period + cfg.stale_pad_s);

    let mut x = String::with_capacity(900);
    x.push_str(&format!(
        "<event version=\"2.0\" uid=\"{uid}\" type=\"{}\" how=\"m-g\" time=\"{time}\" start=\"{time}\" stale=\"{stale}\">",
        type_for(s)
    ));
    x.push_str(&format!(
        "<point lat=\"{:.7}\" lon=\"{:.7}\" hae=\"{}\" ce=\"{ce:.1}\" le=\"{le:.1}\"/>",
        s.lat,
        s.lon,
        hae.map_or_else(|| format!("{UNKNOWN_ERR:.1}"), |h| format!("{h:.1}")),
    ));
    x.push_str("<detail>");
    x.push_str(&format!(
        "<contact callsign=\"{c}\"/><uid Droid=\"{c}\"/>",
        c = xml_escape(&callsign)
    ));
    if let Some(g) = &cfg.group {
        x.push_str(&format!(
            "<__group name=\"{}\" role=\"Team Member\"/>",
            xml_escape(g)
        ));
    }
    x.push_str("<precisionlocation geopointsrc=\"GPS\" altsrc=\"GPS\"/>");
    if s.heading.is_some() || s.groundspeed.is_some() {
        x.push_str(&format!(
            "<track course=\"{:.1}\" speed=\"{:.2}\"/>",
            s.heading.unwrap_or(0.0).rem_euclid(360.0),
            s.groundspeed.unwrap_or(0.0).max(0.0)
        ));
    }
    if let Some(b) = s.battery_pct.filter(|b| (0..=100).contains(b)) {
        x.push_str(&format!("<status battery=\"{b}\"/>"));
    }

    // remarks: human-readable, what an operator on a TAK handset sees
    let mut r: Vec<String> = Vec::new();
    r.push(
        s.name
            .clone()
            .or_else(|| s.platform.clone())
            .unwrap_or_else(|| "Murmur node".into()),
    );
    if let Some(role) = &s.role {
        r.push(role.clone());
    }
    let age = s.last_rx.map(|t| (now - t).max(0.0));
    let mut link = format!("link {}", s.link_kind.as_deref().unwrap_or("?"));
    if let Some(a) = age {
        link.push_str(&format!(" rx {a:.1} s ago"));
    }
    if let Some(l) = s.loss_pct {
        link.push_str(&format!(" loss {l}%"));
    }
    if let Some(rtt) = s.rtt_ms {
        link.push_str(&format!(" rtt {rtt} ms"));
    }
    r.push(link);
    if let Some(m) = &s.mode {
        r.push(format!("mode {m}{}", if s.armed { " armed" } else { "" }));
    }
    let subsurface = s.domain.as_deref() == Some("subsurface") || s.kind.as_deref() == Some("sub");
    if let Some(a) = s.alt_msl.filter(|a| subsurface && *a < -0.5) {
        r.push(format!("depth {:.0} m", -a));
    }
    x.push_str(&format!(
        "<remarks>{}</remarks>",
        xml_escape(&r.join(" | "))
    ));

    // Murmur-specific fields in their own element; TAK clients ignore unknown detail children
    x.push_str(&format!("<_murmur uid=\"{}\"", s.uid));
    for (k, v) in [
        ("platform", s.platform.as_deref()),
        ("link", s.link_kind.as_deref()),
        ("mode", s.mode.as_deref()),
    ] {
        if let Some(v) = v {
            x.push_str(&format!(" {k}=\"{}\"", xml_escape(v)));
        }
    }
    if let Some(l) = s.loss_pct {
        x.push_str(&format!(" loss_pct=\"{l}\""));
    }
    if let Some(a) = age {
        x.push_str(&format!(" rx_age_s=\"{a:.1}\""));
    }
    x.push_str("/></detail></event>");
    x
}

/// A bridge track (a contact that is not ours) as the exporter reads it.
#[derive(Debug, Clone, Default)]
pub struct TrackSample {
    pub no: u64,
    pub id: String,
    pub sidc: String,
    /// predicted position at the sample time
    pub lat: f64,
    pub lon: f64,
    pub alt_m: Option<f64>,
    /// 1-sigma horizontal uncertainty now, metres
    pub sigma_m: f64,
    pub course: Option<f64>,
    pub speed: f64,
    pub confidence: f64,
    pub affiliation: &'static str,
    pub reason: String,
    pub kind: Option<String>,
    pub identity_confidence: f64,
    pub ais_name: Option<String>,
    pub sources: Vec<String>,
    pub last_seen_t: f64,
    pub stale: bool,
    pub fused: bool,
}

/// CoT type for a track: its SIDC through the table, else an unknown atom in the dimension
/// the SIDC names (the tracker always writes a dimension).
pub fn track_type(sidc: &str) -> String {
    type_from_sidc(sidc).unwrap_or_else(|| {
        let dim = sidc.chars().nth(2).unwrap_or('X');
        format!("a-u-{dim}")
    })
}

/// One CoT event for a track. Same envelope as a vehicle's; see the module doc.
pub fn build_track_event(cfg: &CotConfig, t: &TrackSample, now: f64, period: f64) -> String {
    let uid = format!("murmur-{}-track-{}", xml_escape(&cfg.tag), t.no);
    let callsign = t.id.clone();
    let how = "m-f";
    let hae = t.alt_m.map(|a| a + cfg.geoid_m);
    let le = if t.alt_m.is_some() { 50.0 } else { UNKNOWN_ERR };
    let time = iso(now);
    let stale = iso(now + period + cfg.stale_pad_s);
    let age = (now - t.last_seen_t).max(0.0);

    let mut x = String::with_capacity(900);
    x.push_str(&format!(
        "<event version=\"2.0\" uid=\"{uid}\" type=\"{}\" how=\"{how}\" time=\"{time}\" start=\"{time}\" stale=\"{stale}\">",
        track_type(&t.sidc)
    ));
    x.push_str(&format!(
        "<point lat=\"{:.7}\" lon=\"{:.7}\" hae=\"{}\" ce=\"{:.1}\" le=\"{le:.1}\"/>",
        t.lat,
        t.lon,
        hae.map_or_else(|| format!("{UNKNOWN_ERR:.1}"), |h| format!("{h:.1}")),
        t.sigma_m.max(1.0),
    ));
    x.push_str("<detail>");
    x.push_str(&format!(
        "<contact callsign=\"{c}\"/><uid Droid=\"{c}\"/>",
        c = xml_escape(&callsign)
    ));
    if t.course.is_some() || t.speed > 0.0 {
        x.push_str(&format!(
            "<track course=\"{:.1}\" speed=\"{:.2}\"/>",
            t.course.unwrap_or(0.0).rem_euclid(360.0),
            t.speed.max(0.0)
        ));
    }
    let mut r: Vec<String> = Vec::new();
    r.push(format!(
        "{} {}",
        t.affiliation.replace('_', " "),
        t.kind.as_deref().unwrap_or("unclassified")
    ));
    if let Some(n) = &t.ais_name {
        r.push(format!("AIS {n}"));
    }
    r.push(format!(
        "track conf {:.0}% id conf {:.0}%",
        100.0 * t.confidence,
        100.0 * t.identity_confidence
    ));
    r.push(format!(
        "sources {}",
        if t.sources.is_empty() {
            "none".into()
        } else {
            t.sources.join(" ")
        }
    ));
    r.push(format!("seen {age:.0} s ago, 1-sigma {:.0} m", t.sigma_m));
    r.push(t.reason.clone());
    x.push_str(&format!("<remarks>{}</remarks>", xml_escape(&r.join(" | "))));
    x.push_str(&format!(
        "<_murmur track=\"{}\" affiliation=\"{}\" confidence=\"{:.2}\" identity_confidence=\"{:.2}\" age_s=\"{age:.1}\" sigma_m=\"{:.0}\" sources=\"{}\"",
        xml_escape(&t.id),
        t.affiliation,
        t.confidence,
        t.identity_confidence,
        t.sigma_m,
        xml_escape(&t.sources.join(" "))
    ));
    if let Some(k) = &t.kind {
        x.push_str(&format!(" kind=\"{}\"", xml_escape(k)));
    }
    if t.stale {
        x.push_str(" predicted=\"1\"");
    }
    if t.fused {
        x.push_str(" fused=\"1\"");
    }
    x.push_str("/></detail></event>");
    x
}

/// Per-vehicle send state: decides when an event is due.
#[derive(Default)]
pub struct Emitter {
    last: HashMap<Uid, Last>,
}

struct Last {
    t: f64,
    lat: f64,
    lon: f64,
    heading: Option<f64>,
    loss: Option<i64>,
}

impl Emitter {
    /// Events due at `now`: every `1/rate_hz`, or earlier (never faster than `max_hz`) when the
    /// vehicle moved, turned or its link-loss figure changed. Vehicles without a position or
    /// with a silent link produce nothing, so their markers go stale.
    pub fn step(&mut self, cfg: &CotConfig, samples: &[Sample], now: f64) -> Vec<String> {
        let period = 1.0 / cfg.rate_hz;
        let min_gap = 1.0 / cfg.max_hz.max(cfg.rate_hz);
        let mut out = Vec::new();
        for s in samples {
            // link-aware: 3 report intervals of silence (60 s on an acoustic link), never
            // less than lost_s
            let lost_s = cfg
                .lost_s
                .max(crate::link::STALE_INTERVALS * s.interval_s());
            let fresh = s.last_rx.is_some_and(|t| now - t <= lost_s);
            if !fresh {
                self.last.remove(&s.uid); // on return, send straight away
                continue;
            }
            let due = match self.last.get(&s.uid) {
                None => true,
                Some(l) => {
                    let dt = now - l.t;
                    let moved = crate::geo::haversine_m(l.lat, l.lon, s.lat, s.lon) > cfg.move_m;
                    let turned = match (l.heading, s.heading) {
                        (Some(a), Some(b)) => {
                            let d = (a - b).rem_euclid(360.0);
                            d.min(360.0 - d) > 10.0
                        }
                        _ => false,
                    };
                    dt >= period - 1e-6
                        || ((moved || turned || l.loss != s.loss_pct) && dt >= min_gap)
                }
            };
            if due {
                out.push(build_event(cfg, s, now, period.max(s.interval_s())));
                self.last.insert(
                    s.uid,
                    Last {
                        t: now,
                        lat: s.lat,
                        lon: s.lon,
                        heading: s.heading,
                        loss: s.loss_pct,
                    },
                );
            }
        }
        out
    }

    /// Track events due at `now`: the same rate and change rules as vehicles, keyed by
    /// track number (a separate `Emitter` from the vehicles' keeps the key spaces apart).
    pub fn step_tracks(
        &mut self,
        cfg: &CotConfig,
        tracks: &[TrackSample],
        now: f64,
    ) -> Vec<String> {
        let period = 1.0 / cfg.rate_hz;
        let min_gap = 1.0 / cfg.max_hz.max(cfg.rate_hz);
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for t in tracks {
            seen.insert(t.no);
            let due = match self.last.get(&t.no) {
                None => true,
                Some(l) => {
                    let dt = now - l.t;
                    let moved = crate::geo::haversine_m(l.lat, l.lon, t.lat, t.lon) > cfg.move_m;
                    dt >= period - 1e-6 || (moved && dt >= min_gap)
                }
            };
            if due {
                out.push(build_track_event(cfg, t, now, period));
                self.last.insert(
                    t.no,
                    Last {
                        t: now,
                        lat: t.lat,
                        lon: t.lon,
                        heading: t.course,
                        loss: None,
                    },
                );
            }
        }
        // dropped tracks are forgotten, so a later track starts fresh
        self.last.retain(|k, _| seen.contains(k));
        out
    }
}

fn collect(world: &World) -> Vec<Sample> {
    let mono = clock::mono();
    world
        .vehicles()
        .into_iter()
        .filter_map(|cell| {
            let link = cell.link_snapshot(mono);
            cell.read(|v| {
                let (lat, lon) = v.pos?;
                let id = v.identity.as_deref();
                let text = |k: &str| id.and_then(|i| i.str(k)).map(str::to_string);
                Some(Sample {
                    uid: cell.uid,
                    callsign: text("callsign"),
                    platform: text("platform"),
                    name: text("name"),
                    role: text("role"),
                    domain: text("domain"),
                    sidc: text("sidc"),
                    link_kind: text("link"),
                    kind: v.kind.map(|k| format!("{k:?}").to_lowercase()),
                    mode: v.mode.clone(),
                    armed: v.armed,
                    lat,
                    lon,
                    alt_msl: v.alt_msl,
                    heading: v.heading,
                    groundspeed: v.groundspeed,
                    battery_pct: v.battery_pct,
                    gps_fix: v.gps_fix,
                    last_rx: link.last_rx_t,
                    loss_pct: link.loss_pct,
                    rtt_ms: link.rtt_ms,
                    expected_interval_ms: link.expected_interval_ms,
                })
            })
        })
        .collect()
}

// ------------------------------------------------------------------ transports

enum Sink {
    Udp(UdpSocket, Vec<SocketAddr>),
    Tcp(mpsc::Sender<String>),
}

fn multicast_sender(cfg: &CotConfig) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_multicast_ttl_v4(cfg.mcast_ttl)?;
    sock.set_multicast_loop_v4(true)?;
    if let Some(i) = cfg.mcast_iface {
        sock.set_multicast_if_v4(&i)?;
    }
    sock.bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).into())?;
    sock.set_nonblocking(true)?;
    UdpSocket::from_std(sock.into())
}

async fn resolve(target: &str) -> Option<SocketAddr> {
    tokio::net::lookup_host(target)
        .await
        .ok()?
        .find(SocketAddr::is_ipv4)
}

/// TCP to a TAK Server: reconnect forever with backoff; events are concatenated (the server
/// splits on `</event>`); a slow or down server drops events instead of stalling the loop.
async fn tcp_task(target: String, mut rx: mpsc::Receiver<String>, inbound: Option<Arc<Inbound>>) {
    let mut backoff = 1u64;
    loop {
        match TcpStream::connect(&target).await {
            Ok(stream) => {
                println!("cot: connected to TAK server {target}");
                backoff = 1;
                let (mut rd, mut wr) = stream.into_split();
                let mut buf = vec![0u8; 8192];
                let mut acc = String::new();
                loop {
                    tokio::select! {
                        ev = rx.recv() => {
                            let Some(ev) = ev else { return };
                            if wr.write_all(ev.as_bytes()).await.is_err() { break; }
                        }
                        n = rd.read(&mut buf) => match n {
                            Ok(0) | Err(_) => break,
                            Ok(n) => if let Some(inb) = &inbound {
                                acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                                inb.feed_stream(&mut acc, clock::wall());
                            },
                        },
                    }
                }
                println!("cot: TAK server {target} disconnected");
            }
            Err(e) => eprintln!("cot: TCP {target}: {e} (retry in {backoff} s)"),
        }
        // drop whatever queued while down: stale positions are worse than none
        while rx.try_recv().is_ok() {}
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}

async fn build_sinks(cfg: &CotConfig, inbound: Option<Arc<Inbound>>) -> Vec<Sink> {
    let mut sinks = Vec::new();
    if let Some(mc) = cfg.multicast {
        match multicast_sender(cfg) {
            Ok(sock) => {
                println!("cot: multicast to {mc} (ttl {})", cfg.mcast_ttl);
                sinks.push(Sink::Udp(sock, vec![mc]));
            }
            Err(e) => eprintln!("cot: multicast: {e}"),
        }
    }
    let mut targets = Vec::new();
    for t in &cfg.udp {
        match resolve(t).await {
            Some(a) => targets.push(a),
            None => eprintln!("cot: cannot resolve UDP target {t}"),
        }
    }
    if !targets.is_empty() {
        match UdpSocket::bind("0.0.0.0:0").await {
            Ok(sock) => {
                println!("cot: unicast UDP to {targets:?}");
                sinks.push(Sink::Udp(sock, targets));
            }
            Err(e) => eprintln!("cot: UDP socket: {e}"),
        }
    }
    if let Some(t) = &cfg.tcp {
        let (tx, rx) = mpsc::channel(512);
        tokio::spawn(tcp_task(t.clone(), rx, inbound.filter(|_| cfg.tcp_inbound)));
        sinks.push(Sink::Tcp(tx));
    }
    sinks
}

async fn export_loop(world: Arc<World>, cfg: CotConfig, sinks: Vec<Sink>) {
    let mut emitter = Emitter::default();
    let mut track_emitter = Emitter::default();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let now = clock::wall();
        let mut events = emitter.step(&cfg, &collect(&world), now);
        if let Some(tracks) = crate::tracks::store() {
            events.extend(track_emitter.step_tracks(&cfg, &tracks.cot_samples(now), now));
        }
        for ev in &events {
            for sink in &sinks {
                match sink {
                    Sink::Udp(sock, targets) => {
                        for t in targets {
                            let _ = sock.send_to(ev.as_bytes(), t).await;
                        }
                    }
                    Sink::Tcp(tx) => {
                        let _ = tx.try_send(ev.clone());
                    }
                }
            }
        }
    }
}

/// Start the exporter and the inbound listener if the environment asks for them.
pub fn spawn(world: Arc<World>) {
    let cfg = CotConfig::from_env();
    let inbound = inbound();
    let own_prefix = format!("murmur-{}-", cfg.tag);
    inbound.set_own_prefix(own_prefix);
    if let Some(listen) = cfg.listen.clone() {
        tokio::spawn(listen_udp(listen, inbound.clone()));
    }
    if cfg.outputs_enabled() {
        let inb = inbound.clone();
        tokio::spawn(async move {
            let sinks = build_sinks(&cfg, Some(inb)).await;
            export_loop(world, cfg, sinks).await;
        });
    }
}

async fn listen_udp(listen: String, inbound: Arc<Inbound>) {
    use socket2::{Domain, Protocol, Socket, Type};
    let Some(addr) = resolve(&listen).await else {
        eprintln!("cot: cannot resolve MURMUR_COT_LISTEN {listen}");
        return;
    };
    let bound = (|| -> std::io::Result<UdpSocket> {
        let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        sock.set_reuse_address(true)?;
        let bind_ip = if addr.ip().is_multicast() {
            Ipv4Addr::UNSPECIFIED.into()
        } else {
            addr.ip()
        };
        sock.bind(&SocketAddr::new(bind_ip, addr.port()).into())?;
        if let IpAddr::V4(g) = addr.ip()
            && g.is_multicast()
        {
            sock.join_multicast_v4(&g, &Ipv4Addr::UNSPECIFIED)?;
        }
        sock.set_nonblocking(true)?;
        UdpSocket::from_std(sock.into())
    })();
    let sock = match bound {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cot: listen {listen}: {e}");
            return;
        }
    };
    println!("cot: listening for CoT on {addr}");
    let mut buf = vec![0u8; MAX_EVENT_BYTES];
    loop {
        if let Ok(n) = sock.recv(&mut buf).await {
            inbound.ingest(&String::from_utf8_lossy(&buf[..n]), clock::wall());
        }
    }
}

// ------------------------------------------------------------------ inbound (read-only)

/// A marker some other TAK participant published. Display only: nothing here is ever a task.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Marker {
    pub uid: String,
    #[serde(rename = "type")]
    pub cot_type: String,
    pub callsign: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub hae: Option<f64>,
    pub how: Option<String>,
    pub time: Option<String>,
    pub stale: Option<String>,
    pub course: Option<f64>,
    pub speed: Option<f64>,
    /// wall seconds when the bridge received it
    pub received_t: f64,
}

#[derive(Default)]
pub struct Inbound {
    markers: Mutex<BTreeMap<String, Marker>>,
    own_prefix: Mutex<String>,
}

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name)
        .and_then(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// Parse CoT events out of text. Lenient: bad events are skipped. DOCTYPE (entity tricks) is
/// rejected outright; quick-xml does not resolve external entities in any case.
pub fn parse_events(text: &str, now: f64) -> Vec<Marker> {
    let mut out = Vec::new();
    let mut rd = Reader::from_str(text);
    let mut cur: Option<Marker> = None;
    loop {
        match rd.read_event() {
            Ok(Event::DocType(_)) | Ok(Event::Eof) | Err(_) => break,
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.name().as_ref() {
                "event" => {
                    cur = Some(Marker {
                        uid: attr(&e, "uid").unwrap_or_default(),
                        cot_type: attr(&e, "type").unwrap_or_default(),
                        callsign: None,
                        lat: f64::NAN,
                        lon: f64::NAN,
                        hae: None,
                        how: attr(&e, "how"),
                        time: attr(&e, "time"),
                        stale: attr(&e, "stale"),
                        course: None,
                        speed: None,
                        received_t: now,
                    });
                    // a self-closing <event/> has no point; it is dropped at the end
                }
                "point" => {
                    if let Some(m) = cur.as_mut() {
                        m.lat = attr(&e, "lat")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(f64::NAN);
                        m.lon = attr(&e, "lon")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(f64::NAN);
                        m.hae = attr(&e, "hae").and_then(|v| v.parse().ok());
                    }
                }
                "contact" => {
                    if let Some(m) = cur.as_mut() {
                        m.callsign = attr(&e, "callsign");
                    }
                }
                "track" => {
                    if let Some(m) = cur.as_mut() {
                        m.course = attr(&e, "course").and_then(|v| v.parse().ok());
                        m.speed = attr(&e, "speed").and_then(|v| v.parse().ok());
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) if e.name().as_ref() == "event" => {
                if let Some(m) = cur.take()
                    && !m.uid.is_empty()
                    && m.lat.is_finite()
                    && m.lon.is_finite()
                    && (-90.0..=90.0).contains(&m.lat)
                    && (-180.0..=180.0).contains(&m.lon)
                {
                    out.push(m);
                }
            }
            Ok(_) => {}
        }
    }
    out
}

impl Inbound {
    fn set_own_prefix(&self, p: String) {
        *self.own_prefix.lock().unwrap_or_else(|e| e.into_inner()) = p;
    }

    /// Store the markers in `text` (a datagram or a complete chunk).
    pub fn ingest(&self, text: &str, now: f64) {
        if text.len() > MAX_EVENT_BYTES {
            return;
        }
        let own = self
            .own_prefix
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut map = self.markers.lock().unwrap_or_else(|e| e.into_inner());
        for m in parse_events(text, now) {
            // never list our own export echoed back by a server
            if !own.is_empty() && m.uid.starts_with(&own) {
                continue;
            }
            if map.len() >= INBOUND_KEEP && !map.contains_key(&m.uid) {
                // drop the oldest to bound memory
                if let Some(k) = map
                    .iter()
                    .min_by(|a, b| a.1.received_t.total_cmp(&b.1.received_t))
                    .map(|(k, _)| k.clone())
                {
                    map.remove(&k);
                }
            }
            map.insert(m.uid.clone(), m);
        }
    }

    /// TCP stream framing: consume everything up to the last `</event>` in `acc`.
    pub fn feed_stream(&self, acc: &mut String, now: f64) {
        const END: &str = "</event>";
        if let Some(i) = acc.rfind(END) {
            let rest = acc.split_off(i + END.len());
            for chunk in acc.split_inclusive(END) {
                self.ingest(chunk, now);
            }
            *acc = rest;
        }
        if acc.len() > 4 * MAX_EVENT_BYTES {
            acc.clear();
        }
    }

    pub fn snapshot(&self, now: f64) -> Vec<Value> {
        let map = self.markers.lock().unwrap_or_else(|e| e.into_inner());
        map.values()
            .map(|m| {
                let stale = m
                    .stale
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.timestamp_millis() as f64 / 1000.0 < now);
                let mut v = serde_json::to_value(m).unwrap_or(Value::Null);
                if let Some(o) = v.as_object_mut() {
                    o.insert("is_stale".into(), json!(stale));
                }
                v
            })
            .collect()
    }
}

/// The one inbound store (process-wide, so the world model and the other routers stay
/// untouched).
pub fn inbound() -> Arc<Inbound> {
    static STORE: OnceLock<Arc<Inbound>> = OnceLock::new();
    STORE.get_or_init(|| Arc::new(Inbound::default())).clone()
}

/// `GET /api/cot/inbound`: markers received from TAK (read-only).
pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new().route(
        "/api/cot/inbound",
        get(|| async {
            let markers = inbound().snapshot(clock::wall());
            Json(json!({ "count": markers.len(), "markers": markers }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sample {
        Sample {
            uid: 3,
            callsign: Some("HAWK 1".into()),
            platform: Some("rq4".into()),
            name: Some("RQ-4 Global Hawk-class".into()),
            role: Some("HALE ISR UAS".into()),
            domain: Some("air".into()),
            sidc: Some("SFAPMFQ--------".into()),
            link_kind: Some("satcom".into()),
            kind: Some("plane".into()),
            mode: Some("AUTO".into()),
            armed: true,
            lat: 37.8068,
            lon: -122.446,
            alt_msl: Some(105.0),
            heading: Some(90.0),
            groundspeed: Some(21.5),
            battery_pct: Some(87),
            gps_fix: 3,
            last_rx: Some(1000.0),
            loss_pct: Some(2),
            rtt_ms: Some(40),
            expected_interval_ms: 1000,
        }
    }

    fn attrs_of(xml: &str, tag: &str) -> HashMap<String, String> {
        let mut rd = Reader::from_str(xml);
        loop {
            match rd.read_event().expect("well-formed") {
                Event::Start(e) | Event::Empty(e) if e.name().as_ref() == tag => {
                    return e
                        .attributes()
                        .flatten()
                        .map(|a| {
                            (
                                a.key.as_ref().to_string(),
                                a.normalized_value(XmlVersion::Implicit1_0)
                                    .expect("attr")
                                    .into_owned(),
                            )
                        })
                        .collect();
                }
                Event::Eof => panic!("no <{tag}>"),
                _ => {}
            }
        }
    }

    fn well_formed(xml: &str) {
        let mut rd = Reader::from_str(xml);
        let mut depth = 0i32;
        loop {
            match rd.read_event().expect("XML error") {
                Event::Start(_) => depth += 1,
                Event::End(_) => depth -= 1,
                Event::Eof => break,
                _ => {}
            }
        }
        assert_eq!(depth, 0);
    }

    #[test]
    fn sidc_table() {
        let cases = [
            ("SFAPMFQ--------", Some("a-f-A-M-F-Q")),
            ("SFAPMHQ--------", Some("a-f-A-M-H-Q")),
            ("SFAPML---------", Some("a-f-A-M-L")),
            ("SFGPEV---------", Some("a-f-G-E-V")),
            ("SFGPE----------", Some("a-f-G-E")),
            ("SFSP-----------", Some("a-f-S")),
            ("SFUP-----------", Some("a-f-U")),
            ("SHAPMFF--------", Some("a-h-A-M-F-F")),
            ("SNGPUCI--------", Some("a-n-G-U-C-I")),
            ("SUPP-----------", Some("a-u-P")),
            ("sfapmfq--------", Some("a-f-A-M-F-Q")),
            ("SF", None),
            ("GFGPGP---------", None), // tactical graphic: no atom
            ("SFXPMFQ--------", Some("a-f-X-M-F-Q")),
            ("SF-P-----------", None), // no dimension
            ("", None),
            // 2525D: affiliation and dimension only
            ("10033000001100000000", Some("a-f-S")),
            ("10060100001100000000", Some("a-h-A")),
        ];
        for (sidc, want) in cases {
            assert_eq!(type_from_sidc(sidc).as_deref(), want, "{sidc}");
        }
    }

    #[test]
    fn every_catalog_sidc_maps() {
        let text = include_str!("../../../sim/platforms.toml");
        let table: toml::Table = text.parse().expect("platforms.toml");
        let mut n = 0;
        for (id, spec) in &table {
            let Some(sidc) = spec.get("sidc").and_then(toml::Value::as_str) else {
                continue;
            };
            let t = type_from_sidc(sidc).unwrap_or_else(|| panic!("{id}: {sidc}"));
            assert!(t.starts_with("a-f-"), "{id}: {t}");
            n += 1;
        }
        assert!(n >= 20);
    }

    #[test]
    fn fallback_when_no_sidc() {
        assert_eq!(fallback_type(Some("air"), Some("copter")), "a-f-A-M-H-Q");
        assert_eq!(fallback_type(Some("surface"), None), "a-f-S");
        assert_eq!(fallback_type(None, None), "a-f-G");
        let mut s = sample();
        s.sidc = Some("GFGPGP---------".into());
        assert_eq!(type_for(&s), "a-f-A-M-F-Q"); // via kind
    }

    #[test]
    fn event_is_well_formed_and_complete() {
        let cfg = CotConfig::default();
        let xml = build_event(&cfg, &sample(), 1000.5, 1.0);
        well_formed(&xml);
        let ev = attrs_of(&xml, "event");
        assert_eq!(ev["version"], "2.0");
        assert_eq!(ev["uid"], "murmur-main-3");
        assert_eq!(ev["type"], "a-f-A-M-F-Q");
        assert_eq!(ev["how"], "m-g");
        assert_eq!(ev["time"], ev["start"]);
        assert!(ev["time"].ends_with('Z'));
        let pt = attrs_of(&xml, "point");
        for k in ["lat", "lon", "hae", "ce", "le"] {
            assert!(pt.contains_key(k), "{k}");
        }
        assert_eq!(pt["hae"], "105.0");
        assert_eq!(attrs_of(&xml, "contact")["callsign"], "HAWK 1");
        let tr = attrs_of(&xml, "track");
        assert_eq!(
            (tr["course"].as_str(), tr["speed"].as_str()),
            ("90.0", "21.50")
        );
        assert!(xml.contains("link satcom rx 0.5 s ago loss 2%"));
    }

    #[test]
    fn stale_follows_period_and_pad() {
        let cfg = CotConfig::default();
        let xml = build_event(&cfg, &sample(), 1000.0, 1.0);
        let ev = attrs_of(&xml, "event");
        let t = DateTime::parse_from_rfc3339(&ev["time"]).expect("time");
        let s = DateTime::parse_from_rfc3339(&ev["stale"]).expect("stale");
        assert_eq!((s - t).num_milliseconds(), 4000);
    }

    #[test]
    fn subsurface_depth_and_geoid() {
        let cfg = CotConfig {
            geoid_m: -32.0,
            ..CotConfig::default()
        };
        let mut s = sample();
        s.domain = Some("subsurface".into());
        s.sidc = Some("SFUP-----------".into());
        s.alt_msl = Some(-45.0);
        let xml = build_event(&cfg, &s, 1000.0, 1.0);
        assert_eq!(attrs_of(&xml, "point")["hae"], "-77.0");
        assert!(xml.contains("depth 45 m"));
        assert_eq!(attrs_of(&xml, "event")["type"], "a-f-U");
    }

    #[test]
    fn escaping() {
        let mut s = sample();
        s.callsign = Some("A&B <\"x\"> 'y'".into());
        let xml = build_event(&CotConfig::default(), &s, 1000.0, 1.0);
        well_formed(&xml);
        assert_eq!(attrs_of(&xml, "contact")["callsign"], "A&B <\"x\"> 'y'");
        assert_eq!(xml_escape("a\u{1}b"), "ab");
    }

    #[test]
    fn rate_and_change_driven() {
        let cfg = CotConfig::default(); // 1 Hz steady, 2 Hz cap
        let mut em = Emitter::default();
        let mut s = sample();
        s.last_rx = Some(1000.0);
        assert_eq!(em.step(&cfg, &[s.clone()], 1000.0).len(), 1);
        s.last_rx = Some(1000.2);
        assert_eq!(em.step(&cfg, &[s.clone()], 1000.25).len(), 0, "too soon");
        assert_eq!(em.step(&cfg, &[s.clone()], 1000.5).len(), 0, "unchanged");
        s.lat += 0.0005; // ~55 m
        assert_eq!(
            em.step(&cfg, &[s.clone()], 1000.5).len(),
            1,
            "moved, 2 Hz cap"
        );
        s.lat += 0.0005;
        assert_eq!(em.step(&cfg, &[s.clone()], 1000.75).len(), 0, "cap holds");
        s.last_rx = Some(1001.4);
        assert_eq!(
            em.step(&cfg, &[s.clone()], 1001.5).len(),
            1,
            "steady period"
        );
    }

    #[test]
    fn lost_link_goes_stale_not_faked() {
        let cfg = CotConfig::default();
        let mut em = Emitter::default();
        let mut s = sample();
        s.last_rx = Some(1000.0);
        assert_eq!(em.step(&cfg, &[s.clone()], 1000.0).len(), 1);
        // silent for longer than lost_s: nothing is sent, however long it lasts
        for t in [1006.0, 1010.0, 1100.0] {
            assert!(em.step(&cfg, &[s.clone()], t).is_empty());
        }
        // link returns: published at once
        s.last_rx = Some(1200.0);
        assert_eq!(em.step(&cfg, &[s.clone()], 1200.1).len(), 1);
        // never heard from: nothing
        s.last_rx = None;
        assert!(em.step(&cfg, &[s], 1300.0).is_empty());
    }

    #[test]
    fn low_rate_link_is_kept_and_stays_valid() {
        let cfg = CotConfig::default();
        let mut em = Emitter::default();
        let mut s = sample();
        s.expected_interval_ms = 20_000;
        s.last_rx = Some(1000.0);
        assert_eq!(em.step(&cfg, &[s.clone()], 1000.0).len(), 1);
        // 50 s of silence is under 3 x 20 s: still published, unlike a streaming vehicle
        let xml = em.step(&cfg, &[s.clone()], 1050.0);
        assert_eq!(xml.len(), 1);
        let mut st = s.clone();
        st.expected_interval_ms = 1000;
        assert!(Emitter::default().step(&cfg, &[st], 1050.0).is_empty());
        // stale covers a whole report cycle plus the pad
        let ev = attrs_of(&xml[0], "event");
        let t = DateTime::parse_from_rfc3339(&ev["time"]).expect("time");
        let stale = DateTime::parse_from_rfc3339(&ev["stale"]).expect("stale");
        assert_eq!((stale - t).num_seconds(), 23);
        // after 3 missed reports it goes quiet
        assert!(em.step(&cfg, &[s], 1061.0).is_empty());
    }

    #[test]
    fn config_from_env_lookup() {
        let env: HashMap<&str, &str> = [
            ("MURMUR_COT_MULTICAST", "1"),
            ("MURMUR_COT_UDP", "127.0.0.1:18969, 10.0.0.2:4242"),
            ("MURMUR_COT_TCP", "tak:8087"),
            ("MURMUR_COT_RATE_HZ", "2"),
            ("MURMUR_COT_MCAST_TTL", "4"),
        ]
        .into();
        let c = CotConfig::from_lookup(|k| env.get(k).map(|s| s.to_string()));
        assert_eq!(c.multicast, Some("239.2.3.1:6969".parse().expect("addr")));
        assert_eq!(c.udp.len(), 2);
        assert_eq!(c.tcp.as_deref(), Some("tak:8087"));
        assert_eq!((c.rate_hz, c.mcast_ttl), (2.0, 4));
        let off = CotConfig::from_lookup(|_| None);
        assert!(!off.outputs_enabled());
        let c = CotConfig::from_lookup(|k| (k == "MURMUR_COT_MULTICAST").then(|| "0".into()));
        assert!(c.multicast.is_none());
    }

    #[test]
    fn inbound_parses_and_skips_own() {
        let inb = Inbound::default();
        inb.set_own_prefix("murmur-main-".into());
        let ours = build_event(&CotConfig::default(), &sample(), 1000.0, 1.0);
        let theirs = r#"<?xml version="1.0"?><event version="2.0" uid="ANDROID-abc" type="a-f-G-U-C" how="h-e" time="2026-10-02T12:00:00Z" start="2026-10-02T12:00:00Z" stale="2026-10-02T12:05:00Z"><point lat="37.8" lon="-122.4" hae="12" ce="9999999" le="9999999"/><detail><contact callsign="Alpha 1"/><track course="12" speed="1.5"/></detail></event>"#;
        let mut stream = format!("{ours}{theirs}<event version=\"2.0\" uid=\"par");
        inb.feed_stream(&mut stream, 5000.0);
        assert_eq!(stream, "<event version=\"2.0\" uid=\"par"); // partial kept
        let snap = inb.snapshot(1.9e9);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0]["uid"], "ANDROID-abc");
        assert_eq!(snap[0]["callsign"], "Alpha 1");
        assert_eq!(snap[0]["is_stale"], true);
        // bad coordinates and DOCTYPE are rejected
        inb.ingest(
            r#"<event uid="x" type="a-f-G"><point lat="95" lon="0"/></event>"#,
            1.0,
        );
        inb.ingest(r#"<!DOCTYPE e [<!ENTITY x "y">]><event uid="y" type="a"><point lat="1" lon="1"/></event>"#, 1.0);
        assert_eq!(inb.snapshot(1.9e9).len(), 1);
    }

    #[tokio::test]
    async fn udp_sink_delivers_event() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        let addr = rx.local_addr().expect("addr");
        let tx = UdpSocket::bind("127.0.0.1:0").await.expect("bind");
        let xml = build_event(&CotConfig::default(), &sample(), 1000.0, 1.0);
        tx.send_to(xml.as_bytes(), addr).await.expect("send");
        let mut buf = vec![0u8; 4096];
        let n = rx.recv(&mut buf).await.expect("recv");
        let got = String::from_utf8_lossy(&buf[..n]).into_owned();
        assert_eq!(got, xml);
        assert_eq!(parse_events(&got, 0.0)[0].cot_type, "a-f-A-M-F-Q");
    }
}
