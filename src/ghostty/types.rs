use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use clap::ValueEnum;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::ghostty::wire;

/// Split direction vocabulary (vim semantics, spec §1): `vertical` = side by
/// side (Ghostty `right`), `horizontal` = stacked (Ghostty `down`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Vertical,
    Horizontal,
}

impl Direction {
    pub fn ghostty(self) -> &'static str {
        match self {
            Direction::Vertical => "right",
            Direction::Horizontal => "down",
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Direction::Vertical => write!(f, "vertical"),
            Direction::Horizontal => write!(f, "horizontal"),
        }
    }
}

/// Where a new session tab lands (spec §5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowTarget {
    Front,
    New,
    Id(String),
}

impl FromStr for WindowTarget {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        Ok(match s {
            "front" => WindowTarget::Front,
            "new" => WindowTarget::New,
            id => WindowTarget::Id(id.to_string()),
        })
    }
}

impl std::fmt::Display for WindowTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WindowTarget::Front => write!(f, "front"),
            WindowTarget::New => write!(f, "new"),
            WindowTarget::Id(id) => write!(f, "{id}"),
        }
    }
}

impl<'de> Deserialize<'de> for WindowTarget {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Surface configuration applied at pane creation. Absent keys are omitted
/// from the AppleScript record entirely (absent ≠ empty).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SurfaceCfg {
    pub cwd: Option<String>,
    pub env: Vec<String>,
}

/// Result of creating a window or tab: the container ids plus the first
/// terminal's id.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedRef {
    pub window_id: String,
    pub tab_id: String,
    pub terminal_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalInfo {
    pub id: String,
    pub cwd: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TabInfo {
    pub id: String,
    pub window_id: String,
    pub index: u32,
    pub selected: bool,
    pub name: String,
    pub terminals: Vec<TerminalInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowInfo {
    pub id: String,
    pub front: bool,
    pub tabs: Vec<TabInfo>,
}

/// A point-in-time read of everything Ghostty contains.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub windows: Vec<WindowInfo>,
}

impl Snapshot {
    /// Parse the denormalized wire format from `snapshot.scpt`:
    /// one record per terminal, fields
    /// `window-id, tab-id, tab-index, tab-selected, terminal-id, cwd,
    ///  tab-name, terminal-name, window-front`.
    pub fn parse(output: &str) -> Result<Snapshot> {
        let mut windows: Vec<WindowInfo> = Vec::new();
        let mut win_idx: HashMap<String, usize> = HashMap::new();
        let mut tab_idx: HashMap<(String, String), usize> = HashMap::new();
        for rec in wire::decode(output) {
            if rec.len() != 9 {
                return Err(Error::Ghostty(format!(
                    "malformed snapshot record ({} fields, expected 9)",
                    rec.len()
                )));
            }
            let (wid, tid) = (rec[0].clone(), rec[1].clone());
            let index: u32 = rec[2]
                .parse()
                .map_err(|_| Error::Ghostty(format!("bad tab index '{}'", rec[2])))?;
            let selected = rec[3] == "true";
            let terminal = TerminalInfo { id: rec[4].clone(), cwd: rec[5].clone(), name: rec[7].clone() };
            let front = rec[8] == "true";

            let wpos = *win_idx.entry(wid.clone()).or_insert_with(|| {
                windows.push(WindowInfo { id: wid.clone(), front, tabs: Vec::new() });
                windows.len() - 1
            });
            let key = (wid.clone(), tid.clone());
            let tpos = *tab_idx.entry(key).or_insert_with(|| {
                windows[wpos].tabs.push(TabInfo {
                    id: tid.clone(),
                    window_id: wid.clone(),
                    index,
                    selected,
                    name: rec[6].clone(),
                    terminals: Vec::new(),
                });
                windows[wpos].tabs.len() - 1
            });
            windows[wpos].tabs[tpos].terminals.push(terminal);
        }
        Ok(Snapshot { windows })
    }

    pub fn tab_ids(&self) -> HashSet<&str> {
        self.windows.iter().flat_map(|w| w.tabs.iter().map(|t| t.id.as_str())).collect()
    }

    pub fn terminal_ids(&self) -> HashSet<&str> {
        self.windows
            .iter()
            .flat_map(|w| w.tabs.iter().flat_map(|t| t.terminals.iter().map(|x| x.id.as_str())))
            .collect()
    }

    /// Parent window of a live tab (for drift detection after manual drags).
    pub fn window_of_tab(&self, tab_id: &str) -> Option<&str> {
        self.windows
            .iter()
            .find(|w| w.tabs.iter().any(|t| t.id == tab_id))
            .map(|w| w.id.as_str())
    }

    pub fn window_exists(&self, window_id: &str) -> bool {
        self.windows.iter().any(|w| w.id == window_id)
    }

    /// The selected tab of the front window — the single tab Ghostty
    /// considers current (`adopt` targets it).
    pub fn selected_tab(&self) -> Option<&TabInfo> {
        self.windows
            .iter()
            .find(|w| w.front)
            .and_then(|w| w.tabs.iter().find(|t| t.selected))
    }

    /// Id of [`selected_tab`]. Drives the `→` marker and the `selected`
    /// JSON field.
    pub fn selected_tab_id(&self) -> Option<&str> {
        self.selected_tab().map(|t| t.id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghostty::wire::{RS, US};

    fn rec(fields: &[&str]) -> String {
        fields.join(&US.to_string()) + &RS.to_string()
    }

    #[test]
    fn parse_groups_by_window_and_tab() {
        let out = format!(
            "{}{}{}{}",
            rec(&["w1", "t1", "1", "true", "term1", "/a", "tab one", "shell", "true"]),
            rec(&["w1", "t1", "1", "true", "term2", "/b", "tab one", "vim", "true"]),
            rec(&["w1", "t2", "2", "false", "term3", "/c", "tab two", "shell", "true"]),
            rec(&["w2", "t3", "1", "true", "term4", "/d", "tab three", "shell", "false"]),
        );
        let snap = Snapshot::parse(&out).unwrap();
        assert_eq!(snap.windows.len(), 2);
        assert_eq!(snap.windows[0].tabs.len(), 2);
        assert_eq!(snap.windows[0].tabs[0].terminals.len(), 2);
        assert_eq!(snap.windows[0].tabs[0].name, "tab one");
        assert!(snap.windows[0].front);
        assert!(!snap.windows[1].front);
        assert_eq!(snap.tab_ids().len(), 3);
        assert_eq!(snap.terminal_ids().len(), 4);
        assert_eq!(snap.window_of_tab("t2"), Some("w1"));
        assert_eq!(snap.selected_tab_id(), Some("t1"));
        assert!(snap.window_exists("w2"));
        assert!(!snap.window_exists("w9"));
    }

    #[test]
    fn parse_empty_output() {
        let snap = Snapshot::parse("").unwrap();
        assert!(snap.windows.is_empty());
        assert_eq!(snap.selected_tab_id(), None);
    }

    #[test]
    fn parse_rejects_short_records() {
        let out = rec(&["w1", "t1", "1"]);
        assert!(Snapshot::parse(&out).is_err());
    }

    #[test]
    fn direction_ghostty_mapping() {
        assert_eq!(Direction::Vertical.ghostty(), "right");
        assert_eq!(Direction::Horizontal.ghostty(), "down");
    }

    #[test]
    fn window_target_parsing() {
        assert_eq!("front".parse::<WindowTarget>().unwrap(), WindowTarget::Front);
        assert_eq!("new".parse::<WindowTarget>().unwrap(), WindowTarget::New);
        assert_eq!(
            "tab-group-abc".parse::<WindowTarget>().unwrap(),
            WindowTarget::Id("tab-group-abc".into())
        );
    }
}
