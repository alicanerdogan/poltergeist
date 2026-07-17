use std::collections::HashSet;

use crate::error::Result;
use crate::ghostty::GhosttyBridge;
use crate::state::StateStore;

#[derive(Debug, Default, PartialEq)]
pub struct ReconcileReport {
    pub pruned_sessions: usize,
    pub pruned_terminals: usize,
    pub moved_windows: usize,
}

/// The registry is a cache of claims; Ghostty is the source of truth. Every
/// command begins with this pass (spec §5.3) — lazy, automatic cleanup; no
/// daemon, no `prune` command.
pub fn reconcile(store: &StateStore, ghostty: &dyn GhosttyBridge) -> Result<ReconcileReport> {
    if !ghostty.is_running()? {
        let pruned = store.delete_all()?;
        return Ok(ReconcileReport { pruned_sessions: pruned, ..ReconcileReport::default() });
    }
    let snapshot = ghostty.snapshot()?;
    let live_tabs: HashSet<&str> = snapshot.tab_ids();
    let live_terminals: HashSet<&str> = snapshot.terminal_ids();
    let pruned_sessions = store.delete_dead_sessions(&live_tabs)?;
    let pruned_terminals = store.delete_dead_terminals(&live_terminals)?;

    // Tabs dragged to another window by hand keep their tab id; only the
    // parent window changes — refresh it so `switch` keeps working.
    let moves: Vec<(String, String)> = store
        .live_sessions()?
        .into_iter()
        .filter_map(|row| match snapshot.window_of_tab(&row.tab_id) {
            Some(w) if w != row.window_id => Some((row.tab_id, w.to_string())),
            _ => None,
        })
        .collect();
    let moved_windows = moves.len();
    store.refresh_windows(&moves)?;

    Ok(ReconcileReport { pruned_sessions, pruned_terminals, moved_windows })
}

/// Spec §3 cleanup notice, printed by every command after reconciling.
pub fn report_notice(report: &ReconcileReport) {
    if report.pruned_sessions > 0 {
        eprintln!("cleared {} closed session(s)", report.pruned_sessions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use crate::ghostty::fake::{FakeBridge, FakeTab, FakeTerminal, FakeWindow};
    use crate::state::NewSession;

    fn register(store: &StateStore, name: &str, window: &str, tab: &str, terminals: &[&str]) {
        let terms: Vec<String> = terminals.iter().map(|t| t.to_string()).collect();
        store
            .register(&NewSession {
                name,
                window_id: window,
                tab_id: tab,
                workflow: None,
                cwd: None,
                params: "{}",
                terminals: &terms,
                labels: &BTreeMap::new(),
            })
            .unwrap();
    }

    fn ghostty_with(tabs: &[(&str, &str, &[&str])]) -> FakeBridge {
        let bridge = FakeBridge::default();
        bridge.running.set(true);
        let mut windows: Vec<FakeWindow> = Vec::new();
        for (window, tab, terminals) in tabs {
            let w = windows.iter_mut().find(|w: &&mut FakeWindow| w.id == *window);
            let ft = FakeTab {
                id: tab.to_string(),
                selected: true,
                terminals: terminals
                    .iter()
                    .map(|t| FakeTerminal { id: t.to_string(), cwd: "/".into() })
                    .collect(),
            };
            match w {
                Some(w) => w.tabs.push(ft),
                None => windows.push(FakeWindow {
                    id: window.to_string(),
                    front: windows.is_empty(),
                    tabs: vec![ft],
                }),
            }
        }
        *bridge.windows.borrow_mut() = windows;
        bridge
    }

    #[test]
    fn ghostty_down_wipes_registry() {
        let store = StateStore::open_memory().unwrap();
        register(&store, "a", "w1", "tab-a", &["t1"]);
        let bridge = FakeBridge::default(); // running = false
        let report = reconcile(&store, &bridge).unwrap();
        assert_eq!(report.pruned_sessions, 1);
        assert!(store.live_sessions().unwrap().is_empty());
    }

    #[test]
    fn dead_tabs_pruned_live_kept() {
        let store = StateStore::open_memory().unwrap();
        register(&store, "a", "w1", "tab-a", &["t1", "t2"]);
        register(&store, "b", "w1", "tab-b", &["t3"]);
        let bridge = ghostty_with(&[("w1", "tab-b", &["t3"])]);
        let report = reconcile(&store, &bridge).unwrap();
        assert_eq!(report.pruned_sessions, 1);
        let rows = store.live_sessions().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "b");
        assert_eq!(rows[0].terminals, vec!["t3"]); // cascade cleaned a's terminals
    }

    #[test]
    fn hand_closed_panes_pruned() {
        let store = StateStore::open_memory().unwrap();
        register(&store, "a", "w1", "tab-a", &["t1", "t2"]);
        let bridge = ghostty_with(&[("w1", "tab-a", &["t1"])]); // t2 closed by hand
        let report = reconcile(&store, &bridge).unwrap();
        assert_eq!(report.pruned_sessions, 0);
        assert_eq!(report.pruned_terminals, 1);
        assert_eq!(store.live_sessions().unwrap()[0].terminals, vec!["t1"]);
    }

    #[test]
    fn dragged_tab_refreshes_window() {
        let store = StateStore::open_memory().unwrap();
        register(&store, "a", "w1", "tab-a", &["t1"]);
        let bridge = ghostty_with(&[("w2", "tab-a", &["t1"])]); // dragged to w2
        let report = reconcile(&store, &bridge).unwrap();
        assert_eq!(report.moved_windows, 1);
        assert_eq!(store.live_sessions().unwrap()[0].window_id, "w2");
    }

    #[test]
    fn everything_consistent_is_noop() {
        let store = StateStore::open_memory().unwrap();
        register(&store, "a", "w1", "tab-a", &["t1"]);
        let bridge = ghostty_with(&[("w1", "tab-a", &["t1"])]);
        let report = reconcile(&store, &bridge).unwrap();
        assert_eq!(report, ReconcileReport::default());
    }
}
