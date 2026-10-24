Open-source sensor parameters and models for Murmur's sensing (research, 2026-10-23). [n] cited, [E] estimated with its basis. Unclassified public sources only. Input to docs/WORLD-V3-PLAN.md.

## Sensing model for Murmur: open-source parameters and simple models

**Tags:** [n] is a cited value (reference list at the end). [E] is my estimate, with its basis. Ranges are in km and are 50% points unless noted. "D/R/I" means detect/recognize/identify. Several sources are secondary or vendor claims; I mark those. The prose is about 1,500 words; the references come on top.

### (a) Models, one per sensor, cheap enough for a per-second tick

**0. Geometry (applies to every sensor)**
- **Horizon:** d = √(2kRₑh).
  - Radio (radar, AIS, ESM): k = 4/3, giving 4.12√h km.
  - Optical (EO/IR): k = 7/6, giving 3.86√h km. Both constants are from gdal_viewshed's curvature table [1].
  - The pair is in line of sight only if R ≤ d(h_sensor) + d(h_target).
- **Terrain:** sample a DEM (SRTM, 30 m) along the path. The path is blocked where terrain + s(R−s)/(2kRₑ) ≥ ray height, which is the same correction GDAL uses [1].
  - [E] Recompute a pair only when either end moves more than 50 m, or every 5 s. Golden Gate headlands and Angel Island make this matter.
- **Hull-down:** the visible part of a target is h_vis = h_t − (R − d_obs)²/(2kRₑ). Use h_vis both for the EO critical dimension and for the radar target height.
- **Pointing:** EO only sees inside its current FOV.
  - [E] Two modes: WIDE to search, NARROW to identify a cued target.
  - [E] Slew time = Δθ / slew rate + about 1 s to settle.

**1. EO/IR (Johnson criteria, ACQUIRE/NVESD form)**
- **Pixels on target:** p = D_c / (R · IFOV_eff).
  - D_c = √(W·H) of the visible silhouette.
  - IFOV_eff = max(HFOV/N_px, blur floor).
  - [E] Blur floors: 5-10 µrad for a large turret in the visible, about 20 µrad for MWIR on a 15-20" turret, 25-50 µrad for a small-UAS gimbal. For comparison, MX-20 line-of-sight stabilization is under 4 µrad [3] and MX-10 is under 15 µrad [4].
- **50% thresholds across D_c:** detection 1.5 px, recognition 6 px, identification 12 px [5]. Johnson's original figures are 1.0/4.0/6.4 line pairs [6].
- **Probability curve:** P∞ = x^E / (1 + x^E), with x = p/p₅₀ and E = 2.7 + 0.7x (the ACQUIRE target transfer probability function) [7,8].
- **Atmosphere:** multiply by A = 1 / (1 + (R/R_atm)⁶). The shape is [E].
  - Visible: R_atm = V·ln(C₀/0.02)/3.912 (Koschmieder [9]). With inherent contrast C₀ = 0.3 this is about 0.7 × visibility.
  - IR: R_atm = ln(ΔT₀/0.1 K)/β. β ≈ 0.2 km⁻¹ in clear air (a vendor DRI assumption [10]), so about 15 km for ΔT₀ = 2 K.
  - Haze and fog: IR reaches 3-6× the visual range [11].
- **Time to detect:** P(t) = P∞(1 − e^(−t/τ)), with τ ≈ 3.4/P∞ s per FOV dwell. The functional form is NVESD's [12]; the 3.4 constant is the classic NVL value and I did not confirm it in a fetched source.
  - Implementation: draw once per look whether the target is detectable at all (probability P∞), then draw the detection time from an exponential with mean τ.
  - [E] Identification needs the NARROW FOV on the target for at least 2-3 s.
- **Day and night [E]:** visible cameras work in daylight only; low-light cameras give about 0.3-0.5× range at night. Thermal works day and night, but ΔT collapses at dawn/dusk thermal crossover.
- **Check against vendor datasheets:**
  - FLIR M364C LR (640 px, 18°): human detection 0.97 km published vs 1.02 computed; NATO 2.3 m target 2.34 vs 3.1 [13].
  - EDGE G270 (uncooled, 7.3°): vehicle D/R 5.5/1.8 published vs 7.7/1.9 computed [14].
  - EOS Guardian 2.0 day camera: vehicle D/R/I 15/10.3/8.4 published vs 89/22/11 computed [15].
  - So thermal comes out at roughly 0.6-1.1× pure geometry, and long day-camera ranges are capped by the atmosphere.

**2. Radar**
- **Detection range:** R₅₀ = R_ref(σ/σ_ref)^¼ (fourth-root law [16]), cut off at the radio horizon computed with h_vis.
- **Low surface targets [E, two-ray model]:** beyond R_x = 4πh_r h_t/λ, use R₅₀ = √(R_fs·R_x), because received power falls as R⁻⁸ there.
- **Sea clutter [E]:** clutter cross-section σ_c = σ₀ · R · θ_az · (cτ/2) · sec ψ. Require target/clutter ≥ 10 dB in addition to SNR.
  - σ₀ for X-band HH, in dB (rounded Nathanson-type values, to be replaced by the NRL model [17]):

    | Grazing angle | SS1 | SS3 | SS5 |
    |---|---|---|---|
    | ≤1° | −55 | −45 | −38 |
    | 3° | −48 | −40 | −33 |
    | 10° | −42 | −34 | −28 |

  - Worked example: a navigation radar (1.2° beam, 37 m range cell) at 5 km in sea state 3 sees σ_c ≈ 0.13 m². A 2.5 m² boat then has 13 dB of margin; at sea state 5 the margin is about 3 dB and the boat is lost.
- **Revisit:** navigation radars turn at 24 or 48 rpm, i.e. 2.5 or 1.25 s [18]. [E] Airborne 360° surface search: 6-12 rpm.
- **Accuracy:** 30 m or 1% of range scale, and 1° in bearing (IMO minimum) [19].
- **IMO Table 2 calibration anchors** (15 m antenna, Pd 0.8, Pfa 10⁻⁴; X/S band, NM) [19]:

  | Target | Height | RCS (X/S) | Range (X/S) |
  |---|---|---|---|
  | Ship >5,000 GT | 10 m | 10,000 m² | 11/11 |
  | Ship >500 GT | 5 m | 1,000 m² | 8/8 |
  | Small vessel with IMO reflector | 4 m | 7.5/0.5 m² | 5.0/3.7 |
  | Navigation buoy | 3.5 m | 5/0.5 m² | 4.6/3.0 |
  | 10 m vessel, no reflector | 2 m | 2.5/1.4 m² | 3.4/3.0 |

  These are minimum performance; real radars do better.

**3. AIS**
- **Who reports:** cooperating vessels only. Carriage is required on SOLAS ships ≥300 GT on international voyages, cargo ships ≥500 GT on domestic voyages, and all passenger ships [20]. Everyone else carries Class B or nothing, and any vessel can switch off or spoof.
- **Range:** 4.12(√h₁ + √h₂) km. From a 15 m antenna, ships are typically heard at 15-20 NM [21].
- **Aircraft [E]:** an aircraft is horizon-limited. The MQ-9B at 7,600 m hears a 10 m antenna at about 370 km, and even a 2 W transmitter closes the link at that range (about 128 dB path loss vs roughly −107 dBm receiver sensitivity).
  - Slot collisions are the real limit: about 4,500 slots/min across both channels.
  - Model it as P_decode ≈ exp(−reports heard per min / 4,500).
- **Reporting intervals [22]:**
  - Class A: moored 3 min; 0-14 kn 10 s (3⅓ s while turning); 14-23 kn 6 s (2 s turning); over 23 kn 2 s; static data every 6 min.
  - Class B CS: 30 s above 2 kn, 3 min below.
  - Class B SO: 30, 15 or 5 s depending on speed.
- **Transmit power:** Class A 12.5 W; Class B 2 W (CS) or 5 W (SO) [23].

**4. Sonar**
- **Detection:** signal excess SE = FOM − TL(r) − ΔNL, and Pd = Φ(SE/σ) with σ ≈ 8 dB [24].
  - Passive FOM = SL − NL + DI − DT.
  - Active FOM = SL + TS − NL + DI − DT, compared against two-way TL [25].
- **Transmission loss:** TL = 20 log r up to a transition range r_t, then + 10 log(r/r_t), plus αr. r_t ≈ water depth in shallow water [25]. Alternatively use 15 log r ("practical spreading") [26].
- **Absorption α (Thorp, dB/km, f in kHz):** 0.11f²/(1+f²) + 44f²/(4100+f²) + 2.75·10⁻⁴f² + 0.003 [26].
  - Values: 0.07 at 1 kHz, 0.2 at 3, 1.2 at 10, 8 at 30, 32 at 90 kHz.
- **Ambient noise (dB re µPa²/Hz, power-summed) [26]:**
  - Turbulence: 17 − 30 log f.
  - Shipping: 40 + 20(s − 0.5) + 26 log f − 60 log(f + 0.03), with shipping activity s from 0 to 1.
  - Wind/waves: 50 + 7.5√w + 20 log f − 40 log(f + 0.4), with w the wind speed in m/s.
  - Thermal: −15 + 20 log f.
  - At 1 kHz this gives about 61 dB at w = 5 m/s and about 68 dB at 10 m/s.
- **Calibrate per pair:** source levels (SL) and detection thresholds (DT) are mostly not public. Set each pair's FOM from a reference R₅₀ (the table below) in a reference environment (50 m water, w = 5 m/s), then let noise level and water depth shift it.
- **Quiet targets:** a submarine on battery is effectively inaudible to one omnidirectional hydrophone.
- **Classification:**
  - Passive spectra separate ship / submarine / UUV after minutes [E].
  - High-frequency diver sonars classify divers by their track [27].

**5. ESM**
- **Detect** an emitter only if it is transmitting, is in radio line of sight, and received power Pt·Gt·Gr·λ²/(4πR)² ≥ S_min, with S_min between −60 and −90 dBm [E].
- **Range:** one-way propagation (R²) against the radar's two-way (R⁴), so ESM hears a radar beyond that radar's own detection range, except low-probability-of-intercept radars [28].
  - Ship navigation radars: limited only by line of sight.
  - Drone links (0.1-1 W at 2.4/5.8 GHz): about 1-5 km [E].
- **Bearing only:**
  - Interferometer: under 1° RMS [29].
  - Installed V/UHF: under 2°; HF: under 5° RMS [30].
  - A rotating radar is intercepted once per rotation; position comes from triangulation over time or across platforms.

### (b) Platform table

Ranges are from the geometric EO model (vehicle-size target 2.3 m, person 0.75 m) and are capped at runtime by atmosphere and horizon. A small boat (D_c ≈ 3.5 m) gets 1.5× the vehicle range.

| Platform | Sensors / FOV (source) | Slew / revisit | Ranges, km |
|---|---|---|---|
| RQ-4 | EISS SAR/GMTI 20-200; EO NIIRS 6, IR 5.5 [31, secondary]; EISS has +50% range over the original suite [32] | n/a | [E, IFOV 10/30 µrad] EO vehicle I ≈ 19, IR vehicle I ≈ 6; ESM horizon ≈ 550 |
| MQ-9B | MX-20 (on SeaGuardian [33]): MWIR 640 at 18.2/3.7/0.73/0.24°; spotter 1080p, 1.07-0.2° [3]. Lynx radar: maritime ≤300, ISAR ≤150, SAR ≤90, MTI ≤50; ±180° az, −5 to −60° el [34] | 57°/s [3] | MWIR vehicle 77/19/9.6, person 25/6.3/3.1 (IR atmosphere cap about 15-25). [E, scaled from 300 km at 10⁴ m²] radar: 2.5 m² boat ≈ 40, fishing vessel ≈ 140 |
| CCA | Nothing published. [E] Pod like MX-15: MWIR 26.7-0.54°, spotter 1.1-0.43° [35] | 60°/s [35] | [E] AESA ≈ 100 vs 1 m² (fighter class); MWIR vehicle I ≈ 10; ESM horizon ≈ 430 |
| FVR-90 | MX-8: MWIR 640×480 at 28.4-2.75°; EO 1280 at 30-1.53° [36] | [E] 60°/s | MWIR vehicle 20/5.1/2.6, person 6.7/1.7/0.8; EO vehicle I ≈ 9 |
| Puma 3 AE | Mantis i45: EO 15 MP, 56-1.2° (50×); LWIR 640, 21.4-10.8° [37]. i45 N: LWIR 40-5.3° [38]. Pan 360°, tilt −90/+30 | [E] 60-120°/s | LWIR (5.3°) vehicle 10.6/2.6/1.3, person 3.5/0.9/0.4; EO vehicle I ≈ 7.7 |
| Switchblade 600 | "2-axis, 4-sensor dual EO/IR"; FOV not published [39] | [E] | [E, EO 3°/1280, IR 12°/640] IR vehicle 4.7/1.2/0.6 |
| Skydio X10 | Tele 48 MP, 13° diagonal; narrow 50°; wide 93°; Boson+ 640 at 13.6 mm (0.88 mrad) [40] | gimbal pitch ±90° | Tele (video) person 10/2.5/1.3. Thermal person 0.57/0.14/0.07, vehicle 1.7/0.44/0.22 |
| FPV | [E] ~150° FOV, ~720 px | fixed camera | vehicle 0.42/0.11/0.05; person 0.14/0.03/0.02 |
| Small airship | [E] MX-10 class: MWIR 640 at 30-1.8°; EO 31-1.2°; spotter 0.61° [4] | [E] 60°/s | MWIR vehicle 31/7.8/3.9, person 10/2.5/1.3 |
| UGVs (logistics, mine-laying) | [E] Drive cameras ~90°, 1080p; LWIR ~50° | none | Day vehicle 1.9/0.47/0.23, person 0.6/0.15/0.08. Terrain masking dominates |
| Armed UGV | EOS Guardian 2.0: uncooled 12/4°; MWIR 17.6/2.2°; day 38/1.9° [15] | [E] 60°/s | NATO 2.3 m target: uncooled 8.5/2.9/2.0; MWIR 17.3/5.4/3.6; day 15/10.3/8.4 (which column is which sensor is inferred from the FOVs) |
| Recon UGV | [E] Mast EO/IR | | [E] Like the uncooled RWS: vehicle 8.5/2.9/2.0, person about 3/1/0.5 |
| Magura V5 | Two EO guidance cameras; no radar published [41] | | [E] EO 20°/1080p: vehicle-size 8.4/2.1/1.0 |
| Corsair | Reported: passive 360° EO/IR + ESM, no radar [42, secondary] | | M364C-class thermal [13]: 30 ft boat D 7.9, person D 0.97 |
| Sea Hunter | Long- and short-range radars; 360° stereo EO/IR [43]; MS3 hull sonar, active and passive [44] | 24/48 rpm [18] | [E, IMO anchors ×1.5-2] radar: boat 8-10, ships ≈ 25 (horizon). [E; FAS's 80/95 dB FOM examples give 7.5/21 kyd direct path, ≈ 6.9/19 km [25]] sonar active: submarine 5-10, small UUV 1-2 |
| Saildrone Explorer | 4-16 overlapping cameras [45], imaging every ~5 s [46]; radar is documented on Voyager, not Explorer [47] | 5 s | [E] Cameras (60°/1080p each): boat D 4.3, I 0.5 |
| Buoy | SSQ-53-type hydrophones at 90-1,000 ft [48] | [E] 10-60 s integration | [E] Passive: cargo ship 10-40, fast boat 2-5, battery submarine ≤1, AUV ≤0.3, diver ≤0.1 |
| BlueROV2 | Ping360: 750 kHz, 0.75-50 m, 2°×25° beam [49] | 9-35 s per 360° [49] | Sonar ≤0.05 |
| REMUS 600 | Side-scan swath ≤300 m (Klein) or ≤600 m (EdgeTech) [50] | | 0.15-0.3 per side |
| Orca XLUUV | Sensors not published | | [E] Passive: ships 10-30, battery submarine 1-2 |

Diver detection sonar, for comparison: open-circuit diver 0.9-1.0 km, rebreather 0.7, UUV 1.2, mini-sub 1.5 [27]. A passive 10-hydrophone array detected a rebreather diver at 120 m [51].

### (c) Target table

| Contact | D_c, m | RCS (X-band), m² | Underwater |
|---|---|---|---|
| Fast boat, 7-10 m | 3.5 (about 2 head-on) [E] | 2-2.5 [16,19]; low-profile USV 0.5-2 [E] | SL 134-171 dB re 1 µPa @ 1 m at 3-16 m/s [52] |
| Fishing vessel | ≈ 11 [E] | 100-500. Skolnik's ship formula σ = 52·√f_MHz·D_kt^1.5 gives ≈ 450 for a 200 t vessel [53] | SL 150-170 [E] |
| Sailboat | 4-6 [E] | 2-10; ≥7.5 with an IMO reflector [E] | quiet under sail [E] |
| Ferry | ≈ 27 [E] | 1,000-5,000 [19,53] | SL 165-180 [E] |
| Cargo ship | ≈ 84 [E] | ≥10,000 [19] | SL 177-188 [54] |
| Small drone | 0.35 [E] | 0.01-0.03 [55] | n/a |
| Light aircraft | 3-5 [E] | 1 [16] | n/a |
| Person | 0.75, ΔT ≈ 5 K [5,10] | 1 on land [16]; 0.01-0.1 in water [E] | TS −20 to −10 dB [E] |
| Car / truck | 2.3 (NATO), ΔT 2 K [5] | 100 / 200 [16] | n/a |
| Periscope | ≈ 0.5 [E] | 0.1-1 [E]; real measurements are classified [56] | submarine TS 15-25 dB [25] |
| Small submarine, surfaced or on battery | ≈ 5 when surfaced [E] | 10-100 surfaced [E] | SL ≈ 100 on battery, ≈ 140 snorting (units unclear, secondary source) [57] |
| AUV | n/a | n/a | SL ≈ 124 dB (Autosub, 0.1-5 kHz) [58] |

### Conflicts with sim/platforms.toml
- **Corsair** has radar = 20 km, but it is reported to use passive sensing only [42].
- **Magura** has radar = 8 km; no radar is published [41].
- **Saildrone Explorer** has radar = 10 km; radar is documented on Voyager [47].
- **REMUS 600** has sonar = 3 km; its side-scan reaches about 150-300 m per side [50].
- **BlueROV2** has sonar = 500 m; Ping360 reaches 50 m [49].
- **Buoy (3 km) and Orca (8 km):** plausible against surface ships, far too long against quiet submarines and UUVs. Sonar range should be set per target class.
- **EO `range_m`:** replace the single range with per-task D/R/I derived from FOV, pixel count and the target's critical dimension. Identification is the range that matters for engagement decisions.

### Endurance

Added 2026-10-23 (murmur-4v7) for the eight platform types that had no `endurance_h` in sim/platforms.toml. Without one, the C2 cannot estimate reserves and sim/policy.toml routes every order on the platform to a person.

- **What the figure means:** hours at the catalog's cruise speed. The reserves check and a stand-in's battery both read it that way.
- **Range-only sources:** where a source gives a range but no hours, the figure is that range divided by the catalog's cruise speed [E].
  - Electric UGVs: energy per kilometre changes little with speed, so this holds.
  - Boats: fuel per mile rises with speed. If the range was quoted at a slower speed than the catalog's cruise, this overstates the hours at cruise. Saronic does not say what speed its range assumes.
- **Generic types:** the airship and the four UGVs are not models of a named vehicle. Each takes a named comparable's published figure, and the choice of comparable is the estimate.

| Platform | `endurance_h` | Source and basis |
|---|---|---|
| FVR-90 | 8 | Spec sheet: endurance 8-16 h, range 810 nmi, cruise 50 kt [36; archived copy 59]. The low end [E]: the catalog's MX-8 weighs 15 lb [36], most of the 22 lb nose payload. Trade press gives 12-18 h with an 8-22 lb payload in 2023 [60, secondary]. |
| Small airship | 12 | [E] Comparable: Kelluu's 12 m hydrogen airship, a "proven flight time of 12 hours throughout the year", payload at most 6 kg [61]. The catalog's MX-10-class turret weighs 16.8 kg [4], so carrying it takes a larger hull; the hours are taken as they are. |
| Tracked logistics UGV | 15 | [E] Comparable: Milrem's THeMIS, tracked, with cargo and CASEVAC payloads. Run time is up to 15 h hybrid and 1.5 h on batteries alone, at up to 20 km/h [62]; the 15 h is "at cruise speed with full fuel tanks" [63]. |
| Armed UGV | 15 | [E] Comparable: the same THeMIS carrying a remote weapon station; Kongsberg's PROTECTOR RS6 is one of its listed payloads [62]. |
| Mine-laying UGV | 5.5 | [E] Comparable: Tencore's TerMIT 2.0, a tracked UGV whose mine-laying module carries 20 TM-62 mines. Range up to 40 km at 15 km/h on hard ground or 12 km/h off-road, on four 73 Ah 20S batteries [64]. 40 km at the catalog's 7.2 km/h (2 m/s) is 5.6 h. |
| Small recon UGV | 3 | [E] Comparable: Rheinmetall's Mission Master SP, the smallest of its family, which has an Armed Reconnaissance variant. Endurance 54 km, top speed 40 km/h [65]. 54 km at the catalog's 18 km/h (5 m/s) is 3.0 h. |
| Magura V5 | 20 | [E] The declared range at the declared cruise speed, which is close to the catalog's 23 kt (12 m/s). IDEF 2023: 450 nmi at 22 kt [66], which is 20.5 h. The Eurosatory 2024 brochure: 960 km at 45 km/h [67], which is 21.3 h. |
| Corsair | 64 | [E] Saronic publishes a 1,000+ nmi range and 35+ kt top speed, but no endurance or cruise speed [69]. 1,000 nmi at the catalog's 15.6 kt (8 m/s) is 64 h. In Saronic's own trials it ran more than 92 continuous hours underway, at a speed not given, and loitered for five days [70]. |

**Magura V5's "60 hours":** the "up to 60 hours" of autonomy often repeated for the Magura V5 is not used here. In the sources found, it traces back to figures the Minister of Digital Transformation gave in November 2022 for an unnamed naval drone [68], not to a Magura V5 declaration.

### References
1. https://gdal.org/en/stable/programs/gdal_viewshed.html
3. https://www.militarysystems-tech.com/sites/militarysystems/files/supplier_docs//PDS-MX-20-20D-January-2017.pdf
4. https://www.militarysystems-tech.com/sites/militarysystems/files/supplier_docs/PDS-MX-10-January-2018.pdf
5. https://www.axis.com/dam/public/e1/df/a5/thermal-technology-glossary-en-US-111749.pdf
6. https://en.wikipedia.org/wiki/Johnson%27s_criteria
7. https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/7149652
8. https://apps.dtic.mil/sti/pdfs/ADA456742.pdf
9. https://old.wmo.int/extranet/pages/prog/www/IMOP/publications/CIMO-Guide/Prelim_2018_ed/8_I_9_en_MR_tc.pdf
10. https://www.ophiropt.com/f/foldir-40-450-mm-f-3-6
11. https://man.fas.org/dod-101/navy/docs/fun/part10.htm
12. https://nanophotonics.spiedigitallibrary.org/journals/optical-engineering/volume-52/issue-04/041108/Search-and-detection-modeling-of-military-imaging-systems/10.1117/1.OE.52.4.041108.full
13. https://marine.flir.com/en-us/marine-cameras/fixed-mount/flir-m364c
14. https://products-webfiles.edgegroup.ae/s3fs-public/2022-10/g270-gimbal.pdf
15. https://www.targikielce.pl/storage/file/core_files/2023/9/4/836600f2475ea9a2a5f4a2beb10e8404/DATASHEET-G2.0-EN-1.pdf
16. https://copradar.com/chapts/chapt3/ch3d6.html (Skolnik, Introduction to Radar Systems, 2nd ed., p. 44)
17. https://www.mathworks.com/help/radar/ref/seareflectivity.html
18. https://fcomwww.furuno.co.jp/en/products/radar/FAR-1426
19. https://www.transportstyrelsen.se/globalassets/global/sjofart/dokument/resolution_msc_192_79.pdf
20. https://www.imo.org/en/OurWork/Safety/Pages/AIS.aspx
21. https://support.marinetraffic.com/en/articles/9718923-what-is-the-typical-range-of-an-ais-station
22. https://docs.raymarine.com/87326/en-US/latest/Reference_ais500_reporting_interva-01E02213.html
23. https://www.dco.uscg.mil/Portals/9/DCO%20Documents/5p/CG-5PC/INV/Alerts/1008.pdf
24. https://apps.dtic.mil/sti/pdfs/ADA134078.pdf
25. https://man.fas.org/dod-101/navy/docs/fun/part08.htm
26. https://arxiv.org/abs/0801.0426
27. https://www.sonardyne.com/products/sentinel-diver-detection-sonar-head/ and https://www.sonardyne.com/case-study/sentinel-expeditionary-intruder-detection-where-its-needed/
28. https://www.emsopedia.org/entries/radar-electronic-support-measures-resm/
29. https://www.l3harris.com/sites/default/files/2022-07/L3Harris-SellSheet-Olympia.pdf
30. https://dam.hensoldt.net/m/2e0e366ca754ccbb/original/Skylark-N-English.pdf
31. https://www.globalsecurity.org/intell/systems/global_hawk.htm
32. https://raytheon.mediaroom.com/index.php?item=298
33. https://www.janes.com/defence-news/c4isr-command-tech/latest/ga-asi-leonardo-fly-seaspray-maritime-radar-on-mq-9-uav
34. https://www.ga-asi.com/radars/pdf/2606_LYNX_E.pdf
35. https://www.militarysystems-tech.com/sites/militarysystems/files/supplier_docs//PDS-MX-15-March-2018.pdf
36. https://l3harris.com/sites/default/files/2020-08/L3Harris_Collateral_FVR-90_SpecSheet_Web_0520.pdf
37. https://avinc.com/images/uploads/product_docs/Mantis_i45_Datasheet.pdf
38. https://www.avinc.com/images/uploads/product_docs/Mantis_i45N_Datasheet_v01.pdf
39. https://www.avinc.com/wp-content/uploads/2026/04/2025_Switchblade-600b2_Datasheet.pdf
40. https://skydio.com/x10/technical-specs
41. https://www.usni.org/magazines/proceedings/2025/september/ukraines-magura-naval-drones-black-sea-equalizers
42. https://thetexan.news/federal/texas-based-autonomous-surface-vessel-helped-rescue-helicopter-pilots-in-strait-of-hormuz/article_0a4f5dbd-c2a7-4fc0-a1d8-4e3f98503182.html and https://kureansiklopedi.com/en/detay/saronic-corsair-f2b9a
43. https://www.naval-technology.com/projects/sea-hunter/
44. https://raytheon.mediaroom.com/2015-11-18-Raytheon-delivers-sonar-for-DARPAs-unmanned-anti-submarine-warfare-vehicle-program
45. https://seapowermagazine.org/qa-ron-tremain-vp-maritime-domain-awareness-saildrone-inc/?print=pdf
46. https://seapowermagazine.org/saildrone-usvs-set-for-7th-arctic-mission/?print=pdf
47. https://blog.geogarage.com/2021/08/with-sight-sound-and-radar-picture.html
48. https://www.militaryaerospace.com/rf-analog/article/16719696/navy-bulks-up-on-submarine-hunting-sonobuoys-for-us-and-taiwan-maritime-security-forces
49. https://www.unmannedsystemstechnology.com/company/blue-robotics/ping360-scanning-imaging-sonar/
50. https://bluezonegroup.com.au/wp-content/uploads/2022/08/REMUS-600M-3-31-22_web.pdf
51. https://researchportal.bath.ac.uk/en/publications/passive-acoustic-detection-of-closed-circuit-underwater-breathing/
52. https://espace.curtin.edu.au/handle/20.500.11937/9798
53. https://apps.dtic.mil/sti/pdfs/AD0754408.pdf
54. https://www.cetus.ucsd.edu/docs/publications/McKennaJASA2012.pdf
55. https://www.sto.nato.int/publications/STO%20Meeting%20Proceedings/STO-MP-MSG-SET-183/MP-MSG-SET-183-04.pdf
56. https://www.navysbir.com/n19_2/N192-059.htm
57. https://www.globalsecurity.org/military/systems/ship/silent.htm
58. https://nora.nerc.ac.uk/id/eprint/100256/
59. https://web.archive.org/web/2023/https://l3harris.com/sites/default/files/2020-08/L3Harris_Collateral_FVR-90_SpecSheet_Web_0520.pdf (an archived copy of [36], whose link now answers 404; endurance is on its second page)
60. https://www.thedefensepost.com/2023/10/25/us-navy-fvr-drones
61. https://kelluu.com/ (its FAQ)
62. https://milremrobotics.com/themis-family/
63. https://www.edrmagazine.eu/milrem-robotics-themis-ground-robot-gets-new-teeth-edge-hunter-sp-loitering-munitions
64. https://tencore.com.ua/termit
65. https://www.rheinmetall.com/Rheinmetall%20Group/brochure-download/Uncrewed-Systems/B253e1024-Rheinmetall-Mission-Master-family.pdf
66. https://militarnyi.com/en/news/magura-v5-marine-drone-developed-in-ukraine/
67. https://euro-sd.com/2024/06/major-news/38905/magura-v5-at-eurosatory-2024-2/
68. https://militarnyi.com/uk/news/ukrayina-rozkryla-detali-pro-udarni-morski-bpa/
69. https://www.saronic.com/vessels
70. https://www.workboat.com/saronic-runs-endurance-trials-on-corsair-asv-for-navy-program
