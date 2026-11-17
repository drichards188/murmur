//! The guard on the scenario's settings in the Rust sources (docs/SCENARIO-SETTINGS.md section 5;
//! stage S7; eval/tests/test_settings_guard.py is the Python one). Each watch setting has one
//! source per scenario, its policy file, read through `RulesConfig::watch`,
//! `RulesConfig::silent_drift` and `EngageRules`; what older rules meant lives in
//! `crates/murmur-decide/src/legacy.rs` and the parsers' defaults in `watch.rs`. It reads every
//! `crates/*/src` file as text (comments set aside, test code left out: `tests.rs`, `tests/`
//! directories, `#[cfg(test)]` items and modules, and `tracks/tests/before_oho/`, never edited)
//! and fails on:
//!
//! - **a copied literal** of the inventory's values (section 1, stages S3 and S4), each found by its
//!   known pattern, as the Python guard finds a copy by its name:
//!   - a `const` whose name ends in `ON_STATION_S`, `OUT_OF_VIEW_S`, `STALE_S`, `DRIFT_MPS` or
//!     `EFFECT_RADIUS_M`, set to a number;
//!   - a field or JSON key `stale_s`, `stale_after_s`, `out_of_view_s`, `identify_on_station_s`,
//!     `on_station_s`, `drift_mps` or `default_effect_radius_m` given a number, a `SilentDrift { .. }`
//!     with a number in it, and an `effect_radius_m` or `radius_m` given 75 (the default effect
//!     radius);
//!   - `unwrap_or(75.0)` and friends (`unwrap_or_else(|| 75.0)`), and any `unwrap_or` of a number
//!     in a statement about the stale age, out of view, time on station or drift (`v["stale_after_s"]
//!     .as_f64().unwrap_or(10.0)` was one);
//!   - a string that says "180 s" (the drop time told to agents as a literal: `get_tracks` names the
//!     run's);
//!   - `include_str!` of `sim/policy-v0-watch.toml` anywhere but watch.rs;
//! - **an inventory site that stops reading the settings** (`SITES`): each reader names, in its own
//!   body, what it reads now.
//!
//! Allowed: watch.rs and legacy.rs whole; the `[rules.engage]` parser's default for its radius
//! (`PARSER_DEFAULTS`). Listed apart, each with its reason and each required to be still there
//! (so a list that has gone stale fails too): constants of the same name for another quantity
//! (`ANOTHER_QUANTITY`), and copies found and not yet fixed (`FOUND_COPIES`: the three this guard
//! found when it was written, stage S7, were fixed by bd murmur-u1q4.8; an entry goes when its copy
//! is fixed).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The files allowed to hold any of these literals: the parser's defaults and the old constants.
const ALLOWED_FILES: [&str; 2] = ["crates/murmur-decide/src/watch.rs", "crates/murmur-decide/src/legacy.rs"];

/// The parsers' defaults outside watch.rs: (file, finding, why).
const PARSER_DEFAULTS: [(&str, &str, &str); 1] = [(
    "crates/murmur-decide/src/engage.rs",
    "const DEFAULT_EFFECT_RADIUS_M",
    "the [rules.engage] parser's default for default_effect_radius_m (EngageRules::default_effect_radius), as watch.rs holds [watch]'s",
)];

/// Constants of an inventory name for another quantity: (file, finding, why).
const ANOTHER_QUANTITY: [(&str, &str, &str); 3] = [
    (
        "crates/murmur-bridge/src/emu.rs",
        "const STALE_S",
        "the link panel's: no answer from the link emulator for this long clears its briefs",
    ),
    (
        "crates/murmur-linkemu/src/coverage.rs",
        "const ON_STATION_S",
        "the link predictor's time on an order's task path for its failsafe chance, but an identify order's, whose own time on station the bridge passes (decision/mod.rs link_coverage)",
    ),
    (
        "crates/murmur-bridge/src/estimate.rs",
        "const DRIFT_MPS",
        "the console's estimate of a vehicle left to the water (1.0 m/s), not the decisions' drift bound",
    ),
];

/// Copies of a setting found and not yet fixed: (file, finding, what it copies). Fixing one fails
/// `every_listed_site_is_still_there` until its entry goes. The three this guard found when it was
/// written (stage S7) are fixed (bd murmur-u1q4.8), each now a reader in `SITES`: the tracker's
/// second `STALE_S` (tracks/model.rs, now `legacy::OUT_OF_VIEW_S` itself), the stale age of
/// `TrackerMemory::default` (keepout.rs, now `TrackerMemory::under` the run's settings) and the
/// scenario runner's proxy nomination's 75 m (scenario.rs `proxies`, now the policy's default; its
/// 600 s window, a copy of `[rules.engage] default_window_s` the scan does not look for, too).
const FOUND_COPIES: [(&str, &str, &str); 0] = [];

/// Each inventory site and what its body reads now: (file, function or `Type::function`, or ""
/// for the whole file; the text it holds).
const SITES: [(&str, &str, &[&str]); 18] = [
    ("crates/murmur-decide/src/identify.rs", "on_station_s", &["cfg.watch().identify_on_station_s"]),
    ("crates/murmur-decide/src/identify.rs", "deadline", &["on_station_s(cfg)"]),
    ("crates/murmur-decide/src/keepout.rs", "TrackerMemory::under", &["cfg.watch().stale_s"]),
    ("crates/murmur-decide/src/keepout.rs", "behaving_at", &["Options::under(cfg)"]),
    ("crates/murmur-decide/src/scenario.rs", "proxies", &["engage_rules().default_effect_radius()", "engage_rules().default_window_s"]),
    ("crates/murmur-bridge/src/decision/mod.rs", "link_coverage", &["murmur_decide::identify::on_station_s(&self.rules)"]),
    ("crates/murmur-bridge/src/decision/engage.rs", "", &["TrackerMemory::under(&self.rules)"]),
    ("crates/murmur-decide/src/engage.rs", "EngageRules::out_of_view_s", &["self.stale_s"]),
    ("crates/murmur-decide/src/engage.rs", "out_of_view", &["ec.out_of_view_s()"]),
    ("crates/murmur-decide/src/engage.rs", "EngageRules::default_effect_radius", &["self.default_effect_radius_m"]),
    ("crates/murmur-decide/src/silent.rs", "carried_mps", &["cfg.silent_drift()"]),
    ("crates/murmur-decide/src/rules.rs", "RulesConfig::silent_drift", &["self.silent_rules().drift_mps"]),
    ("crates/murmur-bridge/src/decision/agent.rs", "default_effect_radius_m", &["engage_rules().default_effect_radius()"]),
    ("crates/murmur-bridge/src/decision/blocking.rs", "find_blocking", &["self.default_effect_radius_m()"]),
    ("crates/murmur-bridge/src/mcp/tools.rs", "", &["mcp.decision.default_effect_radius_m()"]),
    ("crates/murmur-bridge/src/mcp/world.rs", "tracks", &["mcp.decision.watch().stale_s", "{drop_after}"]),
    ("crates/murmur-bridge/src/tracks/model.rs", "TrackerWatch::of", &["stale_s: w.stale_s"]),
    ("crates/murmur-bridge/src/tracks/mod.rs", "stale_s", &["physical.stale_s"]),
];

const CONST_SUFFIXES: [&str; 5] = ["ON_STATION_S", "OUT_OF_VIEW_S", "STALE_S", "DRIFT_MPS", "EFFECT_RADIUS_M"];
const FIELDS: [&str; 7] =
    ["stale_s", "stale_after_s", "out_of_view_s", "identify_on_station_s", "on_station_s", "drift_mps", "default_effect_radius_m"];
const RADIUS_FIELDS: [&str; 2] = ["effect_radius_m", "radius_m"];
const DEFAULT_RADIUS: f64 = 75.0;
/// The names a statement about a setting holds (whole, as identifiers or JSON keys).
const STATEMENT_WORDS: [&str; 6] = ["stale_s", "stale_after_s", "out_of_view_s", "on_station_s", "identify_on_station_s", "drift_mps"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("the workspace root")
}

/// A source file as the guard reads it, each the same length in bytes as the file: `code` with
/// comments and string contents blanked, `text` with comments blanked and strings kept (newlines
/// kept in both), and each string literal: (byte offset of its opening quote, of its closing one,
/// its contents).
struct Lexed {
    code: String,
    text: String,
    strings: Vec<(usize, usize, String)>,
}

fn ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn blank(out: &mut String, c: char) {
    if c == '\n' {
        out.push('\n');
    } else {
        out.extend(std::iter::repeat_n(' ', c.len_utf8()));
    }
}

fn lex(src: &str) -> Lexed {
    let b: Vec<(usize, char)> = src.char_indices().collect();
    let n = b.len();
    let ch = |i: usize| b.get(i).map(|x| x.1);
    let at = |i: usize| b.get(i).map_or(src.len(), |x| x.0);
    let (mut code, mut text, mut strings) = (String::with_capacity(src.len()), String::with_capacity(src.len()), Vec::new());
    let mut i = 0;
    while i < n {
        let c = b[i].1;
        let prev_ident = i > 0 && ident(b[i - 1].1);
        if c == '/' && ch(i + 1) == Some('/') {
            while i < n && b[i].1 != '\n' {
                blank(&mut code, b[i].1);
                blank(&mut text, b[i].1);
                i += 1;
            }
            continue;
        }
        if c == '/' && ch(i + 1) == Some('*') {
            let mut depth = 0;
            while i < n {
                if b[i].1 == '/' && ch(i + 1) == Some('*') {
                    depth += 1;
                } else if b[i].1 == '*' && ch(i + 1) == Some('/') {
                    depth -= 1;
                    for _ in 0..2 {
                        blank(&mut code, b[i].1);
                        blank(&mut text, b[i].1);
                        i += 1;
                    }
                    if depth == 0 {
                        break;
                    }
                    continue;
                }
                blank(&mut code, b[i].1);
                blank(&mut text, b[i].1);
                i += 1;
            }
            continue;
        }
        // a string: "..", b"..", r"..", r#".."#, br".."
        let mut j = i;
        if !prev_ident && c == 'b' {
            j += 1;
        }
        let raw = !prev_ident && ch(j) == Some('r');
        if raw {
            j += 1;
        }
        let mut hashes = 0;
        if raw {
            while ch(j) == Some('#') {
                hashes += 1;
                j += 1;
            }
        }
        if ch(j) == Some('"') && (j == i || !prev_ident) {
            for &(_, d) in &b[i..=j] {
                code.push(d);
                text.push(d);
            }
            let open = at(j);
            let mut k = j + 1;
            let mut content = String::new();
            while k < n {
                let d = b[k].1;
                if d == '"' && (0..hashes).all(|h| ch(k + 1 + h) == Some('#')) {
                    break;
                }
                // an escape: the quote it may hold is not the string's end
                let len = if !raw && d == '\\' && k + 1 < n { 2 } else { 1 };
                for &(_, e) in &b[k..k + len] {
                    blank(&mut code, e);
                    text.push(e);
                    content.push(e);
                }
                k += len;
            }
            let close = at(k);
            for &(_, d) in &b[k.min(n)..(k + 1 + hashes).min(n)] {
                code.push(d);
                text.push(d);
            }
            strings.push((open, close, content));
            i = k + 1 + hashes;
            continue;
        }
        // a char literal ('x', '\n', '\u{..}'), not a lifetime
        if c == '\'' {
            let end = if ch(i + 1) == Some('\\') {
                (i + 3..n).find(|&k| b[k].1 == '\'')
            } else if ch(i + 2) == Some('\'') {
                Some(i + 2)
            } else {
                None
            };
            if let Some(end) = end {
                code.push('\'');
                text.push('\'');
                for &(_, d) in &b[i + 1..end] {
                    blank(&mut code, d);
                    blank(&mut text, d);
                }
                code.push('\'');
                text.push('\'');
                i = end + 1;
                continue;
            }
        }
        code.push(c);
        text.push(c);
        i += 1;
    }
    debug_assert_eq!(code.len(), src.len());
    Lexed { code, text, strings }
}

/// The index of the brace that closes the one at `open` (strings and comments already blanked).
fn closing(code: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (k, c) in code[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + k);
                }
            }
            _ => {}
        }
    }
    None
}

fn blank_range(s: &mut String, from: usize, to: usize) {
    let part: String = s[from..to].chars().map(|c| if c == '\n' { '\n' } else { ' ' }).collect();
    s.replace_range(from..to, &part);
}

/// `#[cfg(test)]` items blanked out of both texts; the modules a `#[cfg(test)] mod name;` declares.
fn without_tests(l: &mut Lexed) -> Vec<String> {
    let mut mods = Vec::new();
    const ATTR: &str = "#[cfg(test)]";
    while let Some(start) = l.code.find(ATTR) {
        let rest = &l.code[start + ATTR.len()..];
        let semi = rest.find(';');
        let brace = rest.find('{');
        let end = match (semi, brace) {
            (Some(s), Some(b)) if s < b => start + ATTR.len() + s + 1,
            (Some(s), None) => start + ATTR.len() + s + 1,
            (_, Some(b)) => closing(&l.code, start + ATTR.len() + b).map_or(l.code.len(), |c| c + 1),
            (None, None) => l.code.len(),
        };
        let item = l.code[start + ATTR.len()..end].trim_start();
        if let Some(name) = item.strip_prefix("mod ").or_else(|| item.split_once(" mod ").map(|(_, m)| m))
            && let Some(name) = name.trim().strip_suffix(';')
        {
            mods.push(name.trim().to_string());
        }
        blank_range(&mut l.code, start, end);
        blank_range(&mut l.text, start, end);
    }
    mods
}

/// One copy found: the file, its line, and what (`const NAME`, `field NAME`, `field radius_m 75`,
/// `unwrap_or 75`, `unwrap_or NUMBER in a statement about ...`, `string "180 s"`, `SilentDrift
/// literal`, `include_str policy-v0-watch.toml`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Finding {
    path: String,
    line: usize,
    what: String,
}

fn line_of(s: &str, at: usize) -> usize {
    s[..at].matches('\n').count() + 1
}

/// The number at the start of `s` (after spaces), with its type suffix: `75.0`, `10`, `2.5f64`.
fn number(s: &str) -> Option<f64> {
    let s = s.trim_start();
    let len = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '_')).unwrap_or(s.len());
    if len == 0 || !s.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    s[..len].replace('_', "").trim_end_matches('.').parse().ok()
}

fn has_number(s: &str) -> bool {
    let cs: Vec<char> = s.chars().collect();
    (0..cs.len()).any(|k| cs[k].is_ascii_digit() && (k == 0 || !(ident(cs[k - 1]) || cs[k - 1] == '.')))
}

/// Each occurrence of `word` in `s` as a whole identifier.
fn words<'a>(s: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    s.match_indices(word).map(|(k, _)| k).filter(move |&k| {
        let before = s[..k].chars().next_back();
        let after = s[k + word.len()..].chars().next();
        before.is_none_or(|c| !ident(c)) && after.is_none_or(|c| !ident(c))
    })
}

/// After a field or key ending at `end`: `: <number>` (not a path's `::`), the number.
fn field_value(code: &str, end: usize) -> Option<f64> {
    let rest = code[end..].trim_start();
    let rest = rest.strip_prefix(':')?;
    if rest.starts_with(':') {
        return None;
    }
    number(rest)
}

fn scan(path: &str, src: &str) -> Vec<Finding> {
    let mut l = lex(src);
    without_tests(&mut l);
    let (code, text) = (&l.code, &l.text);
    let mut out = Vec::new();
    let mut found = |at: usize, what: String| out.push(Finding { path: path.to_string(), line: line_of(code, at), what });
    // a const of an inventory name set to a number
    for k in words(code, "const") {
        let rest = &code[k + 5..];
        let name: String = rest.trim_start().chars().take_while(|&c| ident(c)).collect();
        // a bound on the policy's own key (MAX_EFFECT_RADIUS_M) is another quantity
        if !CONST_SUFFIXES.iter().any(|s| name.ends_with(s)) || name.starts_with("MAX_") || name.starts_with("MIN_") {
            continue;
        }
        let Some(semi) = rest.find(';') else { continue };
        if let Some((_, rhs)) = rest[..semi].split_once('=')
            && has_number(rhs)
        {
            found(k, format!("const {name}"));
        }
    }
    // a field given a number, or the default effect radius
    for f in FIELDS {
        for k in words(code, f) {
            if field_value(code, k + f.len()).is_some() {
                found(k, format!("field {f}"));
            }
        }
    }
    for f in RADIUS_FIELDS {
        for k in words(code, f) {
            if field_value(code, k + f.len()) == Some(DEFAULT_RADIUS) {
                found(k, format!("field {f} 75"));
            }
        }
    }
    // a JSON key given a number ({"stale_after_s": 10.0}), or the default effect radius
    for (open, close, s) in &l.strings {
        if let Some(v) = field_value(code, close + 1) {
            if FIELDS.contains(&s.as_str()) {
                found(*open, format!("field {s}"));
            } else if RADIUS_FIELDS.contains(&s.as_str()) && v == DEFAULT_RADIUS {
                found(*open, format!("field {s} 75"));
            }
        }
    }
    // a SilentDrift with a number in it
    for k in words(code, "SilentDrift") {
        let rest = &code[k + "SilentDrift".len()..];
        if rest.trim_start().starts_with('{') {
            let open = k + "SilentDrift".len() + rest.find('{').unwrap_or(0);
            if let Some(close) = closing(code, open)
                && code[open..close].split(':').skip(1).any(|v| number(v).is_some())
            {
                found(k, "SilentDrift literal".into());
            }
        }
    }
    // unwrap_or(75.0) and friends; a number in a statement about the settings
    for pat in [".unwrap_or(", ".unwrap_or_else("] {
        for (k, _) in code.match_indices(pat) {
            let arg = code[k + pat.len()..].trim_start();
            let arg = arg.strip_prefix("||").unwrap_or(arg);
            let Some(v) = number(arg) else { continue };
            // the expression it ends: back to the statement's, the block's or the list item's start
            let start = text[..k].rfind([';', '{', '}', ',']).map_or(0, |s| s + 1);
            let statement = &text[start..k];
            if v == DEFAULT_RADIUS {
                found(k, "unwrap_or 75".into());
            } else if let Some(w) = STATEMENT_WORDS.iter().find(|w| words(statement, w).next().is_some()) {
                found(k, format!("unwrap_or {v} in a statement about {w}"));
            }
        }
    }
    // the drop time told as a literal
    for (open, _, s) in &l.strings {
        if s.contains("180 s") || s.contains("180s") {
            found(*open, "string \"180 s\"".into());
        }
    }
    // the frozen v0 file read anywhere else
    for (k, _) in code.match_indices("include_str!") {
        if let Some((_, _, s)) = l.strings.iter().find(|(o, _, _)| *o > k && text[k..*o].trim_end().ends_with('('))
            && s.ends_with("policy-v0-watch.toml")
        {
            found(k, "include_str policy-v0-watch.toml".into());
        }
    }
    out.sort();
    out
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// Every non-test source file of the workspace's crates: (path from the root, text).
fn sources() -> Vec<(String, String)> {
    let root = root();
    let mut files = Vec::new();
    let mut crates: Vec<PathBuf> = std::fs::read_dir(root.join("crates")).expect("crates/").filter_map(|e| e.ok().map(|e| e.path())).collect();
    crates.sort();
    for c in crates {
        rs_files(&c.join("src"), &mut files);
    }
    let rel = |p: &Path| p.strip_prefix(&root).expect("under the root").to_string_lossy().replace('\\', "/");
    let texts: Vec<(String, String)> = files.iter().map(|p| (rel(p), std::fs::read_to_string(p).expect("a source file"))).collect();
    // the modules a file declares `#[cfg(test)] mod name;`: test code too
    let mut test_paths: BTreeSet<String> = BTreeSet::new();
    for (path, src) in &texts {
        let mut l = lex(src);
        let dir = match path.rsplit_once('/') {
            Some((d, f)) if ["mod.rs", "lib.rs", "main.rs"].contains(&f) => d.to_string(),
            Some((d, f)) => format!("{d}/{}", f.trim_end_matches(".rs")),
            None => continue,
        };
        for m in without_tests(&mut l) {
            test_paths.insert(format!("{dir}/{m}.rs"));
            test_paths.insert(format!("{dir}/{m}/"));
        }
    }
    texts
        .into_iter()
        .filter(|(p, _)| {
            !p.split('/').any(|part| part == "tests" || part == "tests.rs")
                && !p.contains("/before_oho/")
                && !test_paths.contains(p)
                && !test_paths.iter().any(|t| t.ends_with('/') && p.starts_with(t.as_str()))
        })
        .collect()
}

fn findings() -> Vec<Finding> {
    sources().iter().flat_map(|(p, s)| scan(p, s)).collect()
}

fn listed(f: &Finding) -> bool {
    PARSER_DEFAULTS.iter().chain(&ANOTHER_QUANTITY).chain(&FOUND_COPIES).any(|(p, w, _)| f.path == *p && f.what == *w)
}

#[test]
fn no_setting_is_a_literal_outside_its_places() {
    let all = findings();
    let copies: Vec<&Finding> = all.iter().filter(|f| !ALLOWED_FILES.contains(&f.path.as_str()) && !listed(f)).collect();
    assert!(
        copies.is_empty(),
        "a copy of a scenario setting (docs/SCENARIO-SETTINGS.md): read it from the run's settings \
         (RulesConfig::watch, silent_drift, EngageRules) or, for older rules, legacy.rs; never write the \
         number again:\n{copies:#?}"
    );
}

#[test]
fn every_listed_site_is_still_there() {
    let all = findings();
    for (p, w, why) in PARSER_DEFAULTS.iter().chain(&ANOTHER_QUANTITY).chain(&FOUND_COPIES) {
        let n = all.iter().filter(|f| f.path == *p && f.what == *w).count();
        assert_eq!(n, 1, "{p}: {w} ({why}) is found {n} times: fixed or moved? update the guard's list");
    }
    // and the allowed files hold the old constants (a scan that saw nothing would pass the guard)
    for w in ["const IDENTIFY_ON_STATION_S", "const OUT_OF_VIEW_S", "const SURFACE_DRIFT_MPS", "const AIR_DRIFT_MPS"] {
        assert!(all.iter().any(|f| f.path == "crates/murmur-decide/src/legacy.rs" && f.what == w), "legacy.rs: no {w}");
    }
    assert!(all.iter().any(|f| f.path == "crates/murmur-decide/src/watch.rs" && f.what == "include_str policy-v0-watch.toml"));
}

#[test]
fn the_guard_finds_a_planted_copy_and_nothing_else() {
    let planted = r##"
//! A doc comment saying the tracker drops a track after 180 s, and const STALE_S: f64 = 10.0;
use crate::legacy::OUT_OF_VIEW_S;
pub const TRACK_STALE_S: f64 = 10.0;
pub const OUT_OF_VIEW_S2: f64 = crate::legacy::OUT_OF_VIEW_S;
const MAX_EFFECT_RADIUS_M: f64 = 250.0;
/* const ON_STATION_S: f64 = 300.0; */
fn f(p: &P, v: &Value, w: &WatchSettings) -> (f64, f64, String) {
    let r = p.effect_radius_m.unwrap_or(75.0);
    let s = v["stale_after_s"].as_f64().unwrap_or(10.0);
    let fine = v["radius"].as_f64().unwrap_or(200.0);
    let ok = w.stale_s;
    let o = Order { radius_m: 75.0, stale_s: 12.0, drift_mps: SilentDrift { surface: 2.5, ..SilentDrift::legacy() } };
    let j = json!({"identify_on_station_s": 300, "effect_radius_m": 75.0, "radius_m": 200.0});
    let t = format!("dropped after 180 s; a lifetime 'a and a char '{{'");
    let q = r#"no detection for 180 s"#;
    let v0 = include_str!("../../../sim/policy-v0-watch.toml");
    let g = Report { stale_skipped: n, visible_max: v.iter().max().unwrap_or(0), plan_drift_max_m: d.unwrap_or(300.0) };
    (r, s, t)
}
#[cfg(test)]
mod tests {
    const STALE_S: f64 = 10.0;
    fn g() { let x = Some(1.0).unwrap_or(75.0); }
}
"##;
    let got: Vec<(usize, String)> = scan("crates/x/src/planted.rs", planted).into_iter().map(|f| (f.line, f.what)).collect();
    let want: Vec<(usize, String)> = vec![
        (4, "const TRACK_STALE_S".into()),
        (9, "unwrap_or 75".into()),
        (10, "unwrap_or 10 in a statement about stale_after_s".into()),
        (13, "SilentDrift literal".into()),
        (13, "field radius_m 75".into()),
        (13, "field stale_s".into()),
        (14, "field effect_radius_m 75".into()),
        (14, "field identify_on_station_s".into()),
        (15, "string \"180 s\"".into()),
        (16, "string \"180 s\"".into()),
        (17, "include_str policy-v0-watch.toml".into()),
    ];
    let mut got_sorted = got.clone();
    got_sorted.sort();
    assert_eq!(got_sorted, want);
    // and the same file, unplanted, is clean
    let clean = "pub const STALE_S: f64 = crate::legacy::OUT_OF_VIEW_S;\nfn f(w: &W) -> f64 { w.stale_s.max(1.0) }\n";
    assert!(scan("crates/x/src/clean.rs", clean).is_empty());
}

/// The body of `fn name` (or `Type::name`, within `impl Type`) in `path`, comments blanked and
/// strings kept; the whole file for "".
fn body(path: &str, qual: &str) -> Option<String> {
    let src = std::fs::read_to_string(root().join(path)).ok()?;
    let mut l = lex(&src);
    without_tests(&mut l);
    if qual.is_empty() {
        return Some(l.text);
    }
    // where to look: each `impl Type` block for `Type::name`, else the file
    let (within, name): (Vec<std::ops::Range<usize>>, &str) = match qual.split_once("::") {
        Some((ty, f)) => {
            let blocks = words(&l.code, "impl")
                .filter(|&k| {
                    let rest = l.code[k + 4..].trim_start();
                    let rest = rest.strip_prefix('<').map_or(rest, |r| r.split_once('>').map_or(r, |x| x.1).trim_start());
                    rest.starts_with(ty) && !rest[ty.len()..].starts_with(ident)
                })
                .filter_map(|k| {
                    let open = k + l.code[k..].find('{')?;
                    Some(open..closing(&l.code, open)?)
                })
                .collect();
            (blocks, f)
        }
        None => (std::iter::once(0..l.code.len()).collect(), qual),
    };
    let k = within.iter().find_map(|r| {
        words(&l.code[r.clone()], name)
            .map(|k| k + r.start)
            .find(|&k| l.code[..k].trim_end().ends_with("fn") && l.code[k + name.len()..].trim_start().starts_with(['(', '<']))
    })?;
    let open = k + l.code[k..].find('{')?;
    Some(l.text[k..=closing(&l.code, open)?].to_string())
}

#[test]
fn each_inventory_site_reads_the_settings() {
    for (path, qual, needles) in SITES {
        let b = body(path, qual).unwrap_or_else(|| panic!("{path}: no {qual} (renamed? update SITES with what reads the settings now)"));
        let missing: Vec<&&str> = needles.iter().filter(|n| !b.contains(**n)).collect();
        assert!(missing.is_empty(), "{path} {qual} no longer reads {missing:?}: a setting must come from the settings");
    }
}
