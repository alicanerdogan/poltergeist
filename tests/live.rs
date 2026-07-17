//! End-to-end tests against a live Ghostty. Excluded from normal runs;
//! execute manually with:
//!
//!     GTB_INTEGRATION=1 cargo test --test live -- --ignored --test-threads=1
//!
//! Requires Ghostty installed and Automation consent for the host terminal.

#![cfg(target_os = "macos")]

use std::process::Command;

fn gtb_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gtb")
}

fn enabled() -> bool {
    std::env::var("GTB_INTEGRATION").is_ok()
}

fn run_gtb(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(gtb_bin())
        .args(args)
        .env("GTB_HOME", home)
        .output()
        .expect("spawn gtb")
}

#[test]
#[ignore = "requires live Ghostty"]
fn up_ls_kill_roundtrip() {
    if !enabled() {
        return;
    }
    let home = tempfile::tempdir().unwrap();

    let out = run_gtb(home.path(), &["up", "--name", "gtb-itest", "--json"]);
    assert!(out.status.success(), "up failed: {}", String::from_utf8_lossy(&out.stderr));
    let record: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(record["name"], "gtb-itest");
    assert_eq!(record["terminals"].as_array().unwrap().len(), 1);

    let out = run_gtb(home.path(), &["ls", "--json"]);
    assert!(out.status.success());
    let sessions: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(sessions.as_array().unwrap().iter().any(|s| s["name"] == "gtb-itest"));

    let out = run_gtb(home.path(), &["switch", "gtb-itest"]);
    assert!(out.status.success(), "switch failed: {}", String::from_utf8_lossy(&out.stderr));

    let out = run_gtb(home.path(), &["kill", "gtb-itest"]);
    assert!(out.status.success(), "kill failed: {}", String::from_utf8_lossy(&out.stderr));

    let out = run_gtb(home.path(), &["ls", "--json"]);
    let sessions: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!sessions.as_array().unwrap().iter().any(|s| s["name"] == "gtb-itest"));
}
