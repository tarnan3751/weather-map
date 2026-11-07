//! Proof that a refactoring of the registry (`families/`, `layer.rs`,
//! `source.rs`) changed no value: the registry as JSON and as `Debug` text,
//! compared with a snapshot taken before.
//!
//! `REGISTRY_SNAPSHOT=/path/snap.txt cargo test -p wx-core --test registry_snapshot -- --ignored`
//! writes the snapshot when the file does not exist and compares against it
//! when it does.

use wx_core::families::{self, ROWS};
use wx_core::{LAYERS, SOURCES};

fn snapshot() -> String {
    let mut s = String::new();
    s += &serde_json::to_string_pretty(SOURCES).unwrap();
    s += &serde_json::to_string_pretty(LAYERS).unwrap();
    s += &format!("{SOURCES:#?}\n{LAYERS:#?}\n{ROWS:#?}\n");
    for (family, _) in ROWS {
        s += &format!("{family:?} enabled: {}\n", families::enabled(*family));
    }
    s
}

#[test]
#[ignore]
fn registry_matches_the_snapshot() {
    let Ok(path) = std::env::var("REGISTRY_SNAPSHOT") else {
        panic!("set REGISTRY_SNAPSHOT to the snapshot file");
    };
    let now = snapshot();
    match std::fs::read_to_string(&path) {
        Ok(before) => assert!(before == now, "the registry differs from {path}"),
        Err(_) => std::fs::write(&path, now).unwrap(),
    }
}
