use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::ghostty::types::{Direction, WindowTarget};
use crate::params::ParamDecl;

pub const PROJECT_CONFIG_NAME: &str = ".ghosttbusterr.yml";
pub const GLOBAL_CONFIG_NAME: &str = "config.yml";
pub const STATE_NAME: &str = "state";

/// `~/.config/ghosttbusterr` (spec §4.1 — explicitly *not* the macOS
/// `dirs::config_dir()`). `GTB_HOME` replaces the whole directory.
pub fn gtb_home() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("GTB_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir().ok_or_else(|| Error::Config("could not determine home directory".into()))?;
    Ok(home.join(".config").join("ghosttbusterr"))
}

/// Project-local config discovered by walking up from `start` (like git).
pub fn discover(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(PROJECT_CONFIG_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub workflows: BTreeMap<String, Workflow>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub name: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub params: BTreeMap<String, ParamDecl>,
    #[serde(default)]
    pub hooks: Hooks,
    pub window: Option<WindowTarget>,
    pub cwd: Option<String>,
    pub layout: Option<Layout>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    pub pre: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub direction: Direction,
    pub panels: Vec<Panel>,
}

/// Untagged: nested nodes always carry `layout`, leaves never do (TDD §3.3).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Panel {
    Leaf(PanelLeaf),
    Node(PanelNode),
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelLeaf {
    pub run: Option<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelNode {
    pub layout: Layout,
}

impl Panel {
    pub fn leaf(run: Option<String>) -> Panel {
        Panel::Leaf(PanelLeaf { run, cwd: None, env: Vec::new() })
    }
}

/// Missing file = empty config; malformed file = error.
pub fn load(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path)?;
    let config: Config = serde_yaml::from_str(&text)?;
    validate(&config)?;
    Ok(config)
}

/// Project workflows shadow global ones on name conflict (spec §4.1).
pub fn find_workflow<'a>(
    name: &str,
    project: Option<&'a Config>,
    global: &'a Config,
) -> Option<&'a Workflow> {
    project
        .and_then(|c| c.workflows.get(name))
        .or_else(|| global.workflows.get(name))
}

fn validate(config: &Config) -> Result<()> {
    for (key, wf) in &config.workflows {
        for leaf in leaves(wf) {
            for entry in &leaf.env {
                if !entry.contains('=') {
                    return Err(Error::Config(format!(
                        "workflow '{key}': env entry '{entry}' is not KEY=VALUE"
                    )));
                }
            }
        }
        if let Some(layout) = &wf.layout {
            validate_layout(key, layout)?;
        }
    }
    Ok(())
}

fn validate_layout(key: &str, layout: &Layout) -> Result<()> {
    if layout.panels.is_empty() {
        return Err(Error::Config(format!("workflow '{key}': layout node has no panels")));
    }
    for panel in &layout.panels {
        if let Panel::Node(node) = panel {
            validate_layout(key, &node.layout)?;
        }
    }
    Ok(())
}

fn leaves(wf: &Workflow) -> Vec<&PanelLeaf> {
    fn walk<'a>(layout: &'a Layout, out: &mut Vec<&'a PanelLeaf>) {
        for panel in &layout.panels {
            match panel {
                Panel::Leaf(l) => out.push(l),
                Panel::Node(n) => walk(&n.layout, out),
            }
        }
    }
    let mut out = Vec::new();
    if let Some(layout) = &wf.layout {
        walk(layout, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = r#"
workflows:
  review:
    name: "review-${branch}"
    labels: { role: review, branch: "${branch}" }
    params:
      branch: { required: true }
      base:   { default: "main" }
    hooks:
      pre: ./scripts/new-worktree.sh "${branch}" "${base}"
    window: front
    cwd: "${worktree}"
    layout:
      direction: vertical
      panels:
        - run: pi
        - layout:
            direction: horizontal
            panels:
              - run: vim .
              - run: lazygit
"#;

    #[test]
    fn parses_spec_example() {
        let cfg: Config = serde_yaml::from_str(SPEC_EXAMPLE).unwrap();
        let wf = &cfg.workflows["review"];
        assert_eq!(wf.name.as_deref(), Some("review-${branch}"));
        assert_eq!(wf.labels["role"], "review");
        assert_eq!(wf.labels["branch"], "${branch}");
        assert!(wf.params["branch"].required);
        assert_eq!(wf.params["base"].default.as_deref(), Some("main"));
        assert_eq!(wf.hooks.pre.as_deref(), Some("./scripts/new-worktree.sh \"${branch}\" \"${base}\""));
        assert_eq!(wf.window, Some(WindowTarget::Front));
        assert_eq!(wf.cwd.as_deref(), Some("${worktree}"));
        let layout = wf.layout.as_ref().unwrap();
        assert_eq!(layout.direction, Direction::Vertical);
        assert_eq!(layout.panels.len(), 2);
        match &layout.panels[0] {
            Panel::Leaf(l) => assert_eq!(l.run.as_deref(), Some("pi")),
            other => panic!("expected leaf, got {other:?}"),
        }
        match &layout.panels[1] {
            Panel::Node(n) => {
                assert_eq!(n.layout.direction, Direction::Horizontal);
                assert_eq!(n.layout.panels.len(), 2);
            }
            other => panic!("expected node, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_fields() {
        let yaml = "workflows:\n  x:\n    naem: typo\n";
        assert!(serde_yaml::from_str::<Config>(yaml).is_err());
    }

    #[test]
    fn window_target_from_yaml() {
        let yaml = "workflows:\n  a:\n    window: new\n  b:\n    window: tab-group-xyz\n";
        let cfg: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.workflows["a"].window, Some(WindowTarget::New));
        assert_eq!(
            cfg.workflows["b"].window,
            Some(WindowTarget::Id("tab-group-xyz".into()))
        );
    }

    #[test]
    fn discover_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.path().join("a/.ghosttbusterr.yml"), "workflows: {}").unwrap();
        assert_eq!(discover(&nested), Some(dir.path().join("a/.ghosttbusterr.yml")));
        assert_eq!(discover(dir.path()), None);
    }

    #[test]
    fn load_missing_file_is_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load(&dir.path().join("nope.yml")).unwrap();
        assert!(cfg.workflows.is_empty());
    }

    #[test]
    fn env_entry_without_equals_is_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PROJECT_CONFIG_NAME);
        std::fs::write(
            &path,
            "workflows:\n  x:\n    layout:\n      direction: vertical\n      panels:\n        - run: vim\n          env: [\"NOEQUALS\"]\n",
        )
        .unwrap();
        assert!(matches!(load(&path), Err(Error::Config(_))));
    }

    #[test]
    fn empty_layout_node_is_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PROJECT_CONFIG_NAME);
        std::fs::write(
            &path,
            "workflows:\n  x:\n    layout:\n      direction: vertical\n      panels: []\n",
        )
        .unwrap();
        assert!(matches!(load(&path), Err(Error::Config(_))));
    }

    #[test]
    fn project_shadows_global() {
        let project: Config = serde_yaml::from_str("workflows:\n  x:\n    name: project\n").unwrap();
        let global: Config = serde_yaml::from_str("workflows:\n  x:\n    name: global\n  y:\n    name: global-y\n").unwrap();
        assert_eq!(
            find_workflow("x", Some(&project), &global).unwrap().name.as_deref(),
            Some("project")
        );
        assert_eq!(
            find_workflow("y", Some(&project), &global).unwrap().name.as_deref(),
            Some("global-y")
        );
    }
}
