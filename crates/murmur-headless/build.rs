//! Sets `cfg(murmur_headless)`: the headless simulator's seams in the bridge's files (its clock,
//! the in-memory link, `World::attach_link`) are compiled here and nowhere else; the bridge's own
//! build has no build script and never sets it (`clock::tests::the_bridge_build_has_no_headless_seams`).
//!
//! It also holds this crate's dependency table to the bridge's: the bridge's files are compiled
//! here by path, so every crate they use must be here with the same version and at least the
//! same features. A drift fails the build and says which.

use std::path::Path;

fn table(path: &Path) -> toml::Table {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let doc: toml::Table = text.parse().unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    doc.get("dependencies")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default()
}

fn field<'a>(v: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    match v {
        toml::Value::Table(t) => t.get(key),
        toml::Value::String(_) if key == "version" => Some(v),
        _ => None,
    }
}

fn features(v: &toml::Value) -> Vec<String> {
    field(v, "features")
        .and_then(toml::Value::as_array)
        .map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn main() {
    println!("cargo:rustc-check-cfg=cfg(murmur_headless)");
    println!("cargo:rustc-cfg=murmur_headless");
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bridge_manifest = here.join("../murmur-bridge/Cargo.toml");
    println!("cargo:rerun-if-changed={}", bridge_manifest.display());
    println!("cargo:rerun-if-changed=Cargo.toml");
    let bridge = table(&bridge_manifest);
    let mine = table(&here.join("Cargo.toml"));
    let mut problems = Vec::new();
    for (name, want) in &bridge {
        let Some(have) = mine.get(name) else {
            problems.push(format!("{name} is missing"));
            continue;
        };
        for key in ["version", "path", "default-features"] {
            if field(want, key) != field(have, key) {
                problems.push(format!("{name}: {key} {:?} here, {:?} in the bridge", field(have, key), field(want, key)));
            }
        }
        let have_f = features(have);
        for f in features(want) {
            if !have_f.contains(&f) {
                problems.push(format!("{name}: feature {f:?} missing"));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "crates/murmur-headless/Cargo.toml no longer has the bridge's dependencies (it compiles the bridge's files by path): {}",
        problems.join("; ")
    );
}
