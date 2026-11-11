use std::{env, fs, path::PathBuf};

fn main() {
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../../Proteus-core/crates/proteus-core/src/core");
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for (name, relative) in [
        ("core_history_capture", "session_journal/history_capture.rs"),
        ("core_replay_normalize", "workflow_replay/normalize.rs"),
    ] {
        let path = source.join(relative);
        println!("cargo:rerun-if-changed={}", path.display());
        let text = fs::read_to_string(path).unwrap();
        // Import exact production validator bodies, excluding their unrelated
        // Core unit-test modules. No changes to shared sources or logic.
        let body = text.split("#[cfg(test)]\nmod tests;").next().unwrap();
        fs::write(output.join(format!("{name}.rs")), body).unwrap();
    }
}
