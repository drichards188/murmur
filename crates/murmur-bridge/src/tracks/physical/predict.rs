//! The physical model's predictions, without its state (WORLD-V3-PLAN 2.8 item 5; the v3
//! oracle): for one sensor of one observer and one contact, whether it is in line of sight
//! (horizon, hull-down, terrain) and how likely a sensor pointed at it is to detect, recognise
//! and identify it under the conditions. The same functions the running model draws against
//! ([`super::Engine`]); what they leave out is the running model's state: where a camera
//! points, how long it has looked, the scan or report a draw falls on, misclassification.
//! Served to Python by `murmur-headless` (`sense`; eval/headless/sensing.py).

use serde::Serialize;

use super::conditions::Conditions;
use super::geometry::{Grid, K_OPTICAL, K_RADIO, horizon_m, visible_height_m};
use super::signature::{AisClass, SignatureOverride};
use super::spec::{Resolved, SonarMode, resolve};
use super::{PASSIVE_IDENTIFY_S, PASSIVE_RECOGNISE_S, ACTIVE_RECOGNISE_S, Target, eo, pair, radar, rf, sensor_height_m, sight, sonar};
use crate::tracks::sensing::{Observer, SensorSpec};
use crate::tracks::truth::{ContactTruth, Domain};

#[derive(Debug, Clone, Serialize)]
pub struct Prediction {
    pub sensor: &'static str,
    /// The sensor runs on placeholders derived from its `range_m`.
    pub placeholder: bool,
    pub ground_m: f64,
    pub range_m: f64,
    /// The sum of the two horizons at this sensor's k, m.
    pub horizon_m: f64,
    /// The contact's height above the sensor's horizon line (hull-down), m.
    pub visible_height_m: f64,
    /// Horizon and terrain both clear (for sonar: in the water and within its range).
    pub line_of_sight: bool,
    /// The terrain alone; none without a grid.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terrain_clear: Option<bool>,
    /// A pointed sensor's chance to detect, recognise, identify, given the time it takes (an
    /// EO/IR sensor in its best field for each).
    pub p_detect: f64,
    pub p_recognise: f64,
    pub p_identify: f64,
    /// EO/IR: the chance to detect it in the wide field a search sweeps.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p_search: Option<f64>,
    /// EO/IR: the mean time to detect in the wide field, s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tau_s: Option<f64>,
    /// A radar's or scanning sonar's revolution, AIS's report interval, s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revisit_s: Option<f64>,
    /// EO/IR: the channel that identifies best.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<&'static str>,
    /// Sonar: the contact it takes to recognise and to identify, s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recognise_after_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identify_after_s: Option<f64>,
    pub sun_elevation_deg: f64,
}

/// One question.
pub struct Ask<'a> {
    pub spec: &'a SensorSpec,
    pub observer: &'a Observer,
    pub target: &'a ContactTruth,
    pub over: Option<&'a SignatureOverride>,
    pub conditions: Conditions,
    /// Unix seconds of the scenario clock (the sun, unless the conditions state it).
    pub t: f64,
    pub terrain: Option<&'a Grid>,
    pub bathymetry: Option<&'a Grid>,
    /// AIS: the reports a minute the receiver hears besides this one (slot load).
    pub ais_load_per_min: f64,
}

/// The ranges along the line from the observer through the contact (its altitude kept) at
/// which a pointed sensor's chance to detect, recognise and identify falls to one half, m:
/// the D/R/I ranges under these conditions, horizon and terrain included (0: not even next to
/// it).
pub fn ranges(a: &Ask) -> [f64; 3] {
    let o = a.observer;
    let (de, dn) = super::geometry::en(o.lat, o.lon, a.target.lat, a.target.lon);
    let bearing = de.atan2(dn);
    let at = |d: f64| {
        let mut c = a.target.clone();
        c.lat = o.lat + d * bearing.cos() / super::geometry::M_PER_DEG;
        c.lon = o.lon + d * bearing.sin() / (super::geometry::M_PER_DEG * o.lat.to_radians().cos());
        predict(&Ask { target: &c, ..*a })
    };
    [
        eo::range_at(0.5, |d| at(d).p_detect),
        eo::range_at(0.5, |d| at(d).p_recognise),
        eo::range_at(0.5, |d| at(d).p_identify),
    ]
}

pub fn predict(a: &Ask) -> Prediction {
    let o = a.observer;
    let cond = a.conditions.sane();
    let h_s = sensor_height_m(o);
    let t = Target::new(a.target, a.over, a.terrain);
    let p = pair(o, h_s, &t);
    let sun = cond.sun_at(a.t, o.lat, o.lon);
    let resolved = resolve(a.spec, h_s > 20.0);
    let k = if matches!(resolved, Resolved::EoIr(_)) { K_OPTICAL } else { K_RADIO };
    let obstruction = a.terrain.map(|g| g.obstruction_m((o.lat, o.lon, h_s), (t.c.lat, t.c.lon, t.top_msl), [k])[0]);
    let mut out = Prediction {
        sensor: a.spec.kind.as_str(),
        placeholder: resolved.placeholder(),
        ground_m: p.ground_m,
        range_m: p.slant_m,
        horizon_m: horizon_m(h_s, k) + horizon_m(t.top_msl.max(0.0), k),
        visible_height_m: visible_height_m(t.top_msl, t.vis_height_m, p.ground_m, h_s, k),
        line_of_sight: false,
        terrain_clear: obstruction.map(|x| x <= 0.0),
        p_detect: 0.0,
        p_recognise: 0.0,
        p_identify: 0.0,
        p_search: None,
        tau_s: None,
        revisit_s: None,
        channel: None,
        recognise_after_s: None,
        identify_after_s: None,
        sun_elevation_deg: sun,
    };
    match &resolved {
        Resolved::EoIr(eo) => {
            let Some(vis) = sight(h_s, &t, &p, K_OPTICAL, obstruction) else {
                return out;
            };
            out.line_of_sight = true;
            if t.sig.dc_m <= 0.0 {
                return out;
            }
            let dc = if t.c.domain == Domain::Air || t.vis_height_m <= 0.0 {
                t.sig.dc_m
            } else {
                t.sig.dc_m * (vis / t.vis_height_m).clamp(0.0, 1.0).sqrt()
            };
            let mut search: f64 = 0.0;
            for ch in &eo.channels {
                let wide = eo::channel_p(ch, false, p.slant_m, [h_s, t.top_msl], dc, t.sig.thermal_k, &cond, sun);
                let narrow = eo::channel_p(ch, ch.zooms(), p.slant_m, [h_s, t.top_msl], dc, t.sig.thermal_k, &cond, sun);
                search = search.max(wide[0]);
                out.p_detect = out.p_detect.max(wide[0]).max(narrow[0]);
                out.p_recognise = out.p_recognise.max(wide[1]).max(narrow[1]);
                if narrow[2] > out.p_identify {
                    out.p_identify = narrow[2];
                    out.channel = Some(ch.band.as_str());
                }
            }
            out.p_search = Some(search);
            out.tau_s = (search > 0.0).then(|| eo::TAU_K_S / search);
        }
        Resolved::Radar(r) => {
            if sight(h_s, &t, &p, K_RADIO, obstruction).is_none() {
                return out;
            }
            out.line_of_sight = true;
            let h_t = if t.c.domain == Domain::Air {
                visible_height_m(t.top_msl, t.top_msl, p.ground_m, h_s, K_RADIO)
            } else {
                visible_height_m(t.top_msl, t.vis_height_m, p.ground_m, h_s, K_RADIO)
            };
            let sea = matches!(t.c.domain, Domain::Surface | Domain::Subsurface).then_some(cond.sea_state);
            out.p_detect = radar::pd(r, t.sig.rcs_m2, p.slant_m, p.ground_m, h_s, h_t, sea);
            out.revisit_s = Some(r.scan_s);
        }
        Resolved::Ais => {
            let Some(_) = t.c.ais.as_ref().filter(|_| t.c.domain == Domain::Surface) else {
                return out;
            };
            let class = if t.sig.ais_class == AisClass::None { AisClass::B } else { t.sig.ais_class };
            let interval = rf::ais_interval_s(class, t.c.speed_mps);
            let reach = rf::vhf_range_m(h_s, t.sig.height_m.max(1.0));
            out.horizon_m = reach;
            out.line_of_sight = p.ground_m <= reach && out.terrain_clear != Some(false);
            if out.line_of_sight {
                let own = interval.map_or(0.0, |i| 60.0 / i);
                out.p_detect = rf::ais_decode(a.ais_load_per_min + own);
            }
            out.revisit_s = interval;
        }
        Resolved::Sonar(sn) => {
            if !matches!(t.c.domain, Domain::Surface | Domain::Subsurface) {
                return out;
            }
            let depth_t = if t.c.domain == Domain::Subsurface { t.c.alt_m.min(0.0) } else { 0.0 };
            let r = p.ground_m.hypot(depth_t - o.alt_msl.min(0.0));
            out.range_m = r;
            out.horizon_m = sn.max_m;
            out.visible_height_m = 0.0;
            out.line_of_sight = r <= sn.max_m;
            out.terrain_clear = None;
            let water = a
                .bathymetry
                .and_then(|g| g.height(0.5 * (o.lat + t.c.lat), 0.5 * (o.lon + t.c.lon)))
                .filter(|h| *h < -1.0)
                .map_or(cond.water_depth_m, |h| -h);
            let dnl = sonar::noise_db(sn.freq_khz, cond.wind_mps, cond.shipping)
                - sonar::noise_db(sn.freq_khz, sonar::REF_WIND_MPS, sonar::REF_SHIPPING);
            if let Some(fom) = sonar::fom_db(sn, &t.c.kind, &t.sig) {
                out.p_detect = sonar::pd(sn, fom, r, water, dnl);
                out.p_recognise = out.p_detect;
                match sn.mode {
                    SonarMode::Passive => {
                        out.p_identify = out.p_detect;
                        out.recognise_after_s = Some(PASSIVE_RECOGNISE_S);
                        out.identify_after_s = Some(PASSIVE_IDENTIFY_S);
                    }
                    SonarMode::Active => out.recognise_after_s = Some(ACTIVE_RECOGNISE_S),
                }
            }
            out.revisit_s = sn.scan_s;
        }
        Resolved::Esm(e) => {
            let Some(em) = t.sig.emitter.filter(|_| t.c.emits) else {
                return out;
            };
            if sight(h_s, &t, &p, K_RADIO, obstruction).is_none() {
                return out;
            }
            out.line_of_sight = true;
            out.p_detect = 0.95 * sonar::phi((rf::received_dbm(&em, p.slant_m) - e.s_min_dbm) / 3.0);
        }
    }
    out
}
