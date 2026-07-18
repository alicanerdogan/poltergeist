//! End-to-end tests against a live Ghostty. Excluded from normal runs;
//! execute manually with:
//!
//!     GEIST_INTEGRATION=1 cargo test --test live -- --ignored --test-threads=1
//!
//! Requires Ghostty installed and Automation consent for the host terminal.

#![cfg(target_os = "macos")]

use std::process::Command;

fn geist_bin() -> &'static str {
    env!("CARGO_BIN_EXE_geist")
}

fn enabled() -> bool {
    std::env::var("GEIST_INTEGRATION").is_ok()
}

fn run_geist(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(geist_bin())
        .args(args)
        .env("GEIST_HOME", home)
        .output()
        .expect("spawn geist")
}

#[test]
#[ignore = "requires live Ghostty"]
fn up_ls_kill_roundtrip() {
    if !enabled() {
        return;
    }
    let home = tempfile::tempdir().unwrap();

    let out = run_geist(home.path(), &["up", "--name", "geist-itest", "--json"]);
    assert!(out.status.success(), "up failed: {}", String::from_utf8_lossy(&out.stderr));
    let record: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(record["name"], "geist-itest");
    assert_eq!(record["terminals"].as_array().unwrap().len(), 1);

    let out = run_geist(home.path(), &["ls", "--json"]);
    assert!(out.status.success());
    let sessions: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(sessions.as_array().unwrap().iter().any(|s| s["name"] == "geist-itest"));

    let out = run_geist(home.path(), &["switch", "geist-itest"]);
    assert!(out.status.success(), "switch failed: {}", String::from_utf8_lossy(&out.stderr));

    let out = run_geist(home.path(), &["kill", "geist-itest"]);
    assert!(out.status.success(), "kill failed: {}", String::from_utf8_lossy(&out.stderr));

    let out = run_geist(home.path(), &["ls", "--json"]);
    let sessions: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!sessions.as_array().unwrap().iter().any(|s| s["name"] == "geist-itest"));
}
