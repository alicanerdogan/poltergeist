use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{self, Layout, Panel, PanelLeaf, PanelNode, Workflow};
use crate::error::{Error, Result};
use crate::ghostty::GhosttyBridge;
use crate::ghostty::types::{Direction, SurfaceCfg, WindowTarget};
use crate::params::{self, Params};
use crate::state::{NewSession, SessionRow, StateStore};
use crate::{hooks, reconcile};

/// Everything `geist up` knows at invocation time (flags already parsed).
pub struct UpRequest {
    pub workflow: Option<String>,
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
    pub direction: Option<Direction>,
    pub labels: Vec<(String, String)>,
    pub window: Option<WindowTarget>,
    pub pre: Option<String>,
    pub params: Vec<(String, String)>,
    pub commands: Vec<String>,
    pub invocation_cwd: PathBuf,
    /// poltergeist home (global config + state dir); `GEIST_HOME`-aware.
    pub home: PathBuf,
}

/// Placeholder for a pane, resolved to a real terminal id at execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaneRef {
    Root,
    Child(usize),
}

/// Window placement decided before planning (Front already resolved to an id).
#[derive(Debug, Clone, PartialEq)]
pub enum PlannedWindow {
    New,
    Existing(String),
}

/// Operations folded over the bridge in order (TDD §3.10).
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    NewWindow { cfg: SurfaceCfg },
    NewTab { window_id: String, cfg: SurfaceCfg },
    Split { target: PaneRef, dir: Direction, cfg: SurfaceCfg, into: PaneRef },
    Equalize { pane: PaneRef },
    SetTitle { pane: PaneRef, title: String },
    TypeRun { pane: PaneRef, text: String },
}

pub struct PlanCtx<'a> {
    pub window: &'a PlannedWindow,
    pub layout: &'a Layout,
    pub title: &'a str,
    pub session_cwd: &'a str,
    pub session_name: &'a str,
}

/// Layout tree → op list. Pure; unit-tested without Ghostty.
///
/// Newest-pane-first per node: for panels `[p0, p1..pn]`, `p0` occupies the
/// current pane and each subsequent panel splits the pane created just before
/// it. Creation ops come first, then equalize, title, and run delivery.
pub fn plan(ctx: &PlanCtx) -> Result<Vec<Op>> {
    let root_cfg = first_cfg(&ctx.layout.panels[0], ctx);
    let mut ops = vec![match ctx.window {
        PlannedWindow::New => Op::NewWindow { cfg: root_cfg },
        PlannedWindow::Existing(id) => Op::NewTab { window_id: id.clone(), cfg: root_cfg },
    }];
    let mut runs = Vec::new();
    let mut counter = 0;
    plan_node(ctx, ctx.layout, PaneRef::Root, &mut ops, &mut runs, &mut counter);
    ops.push(Op::Equalize { pane: PaneRef::Root });
    ops.push(Op::SetTitle { pane: PaneRef::Root, title: ctx.title.to_string() });
    ops.extend(runs);
    Ok(ops)
}

fn plan_node(
    ctx: &PlanCtx,
    layout: &Layout,
    current: PaneRef,
    ops: &mut Vec<Op>,
    runs: &mut Vec<Op>,
    counter: &mut usize,
) {
    let mut panes = vec![current];
    let mut prev = current;
    for panel in &layout.panels[1..] {
        let into = PaneRef::Child(*counter);
        *counter += 1;
        ops.push(Op::Split { target: prev, dir: layout.direction, cfg: first_cfg(panel, ctx), into });
        panes.push(into);
        prev = into;
    }
    for (panel, pane) in layout.panels.iter().zip(panes) {
        match panel {
            Panel::Leaf(leaf) => {
                if let Some(run) = &leaf.run {
                    runs.push(Op::TypeRun { pane, text: run.clone() });
                }
            }
            Panel::Node(node) => plan_node(ctx, &node.layout, pane, ops, runs, counter),
        }
    }
}

/// The cfg for the pane a panel first occupies: a leaf's own, or — for a
/// nested layout — its first panel's (recursively).
fn first_cfg(panel: &Panel, ctx: &PlanCtx) -> SurfaceCfg {
    match panel {
        Panel::Leaf(leaf) => leaf_cfg(leaf, ctx),
        Panel::Node(node) => first_cfg(&node.layout.panels[0], ctx),
    }
}

fn leaf_cfg(leaf: &PanelLeaf, ctx: &PlanCtx) -> SurfaceCfg {
    let cwd = leaf.cwd.clone().unwrap_or_else(|| ctx.session_cwd.to_string());
    let mut env = leaf.env.clone();
    env.push(format!("GEIST_SESSION={}", ctx.session_name));
    SurfaceCfg { cwd: Some(cwd), env }
}

// ---------------------------------------------------------------------------
// `geist up` orchestration (spec §2.1 lifecycle: all-or-nothing)
// ---------------------------------------------------------------------------

pub fn up(req: &UpRequest, store: &StateStore, ghostty: &dyn GhosttyBridge) -> Result<SessionRow> {
    // Config discovery: project-local walking up from the invocation cwd,
    // then global; project shadows global (spec §4.1).
    let project = config::discover(&req.invocation_cwd).map(|p| config::load(&p)).transpose()?;
    let global = config::load(&req.home.join(config::GLOBAL_CONFIG_NAME))?;
    let workflow: Option<Workflow> = match &req.workflow {
        Some(name) => Some(
            config::find_workflow(name, project.as_ref(), &global)
                .cloned()
                .ok_or_else(|| Error::WorkflowNotFound(name.clone()))?,
        ),
        None => None,
    };

    // 1. Resolve params; error on missing required. ${cwd} is implicit.
    //    Undeclared params warn (typo-prone but harmless) and are dropped.
    let decls = workflow.as_ref().map(|w| w.params.clone()).unwrap_or_default();
    let (mut params, undeclared) = params::resolve(&decls, &req.params, &|k| std::env::var(k).ok())?;
    for key in &undeclared {
        eprintln!("warning: ignoring undeclared param '{key}'");
    }
    let invocation_cwd = req.invocation_cwd.to_string_lossy().into_owned();
    params.set("cwd", invocation_cwd.clone());

    // 2. Pre-hook (flag wins over workflow hook); abort on failure.
    let hook_cmd = req.pre.clone().or_else(|| workflow.as_ref().and_then(|w| w.hooks.pre.clone()));
    if let Some(cmd) = hook_cmd {
        let cmd = params::interpolate(&cmd, &params)?;
        let pairs = hooks::run_pre(&cmd, &req.invocation_cwd)?;
        params.merge_hook_output(&pairs);
    }

    // 3. Session name: --name > workflow name (interpolated) > workflow key >
    //    basename of invocation cwd. Control chars stripped (CLI-edge
    //    sanitization, TDD §4.1).
    let name = sanitize_name(match req.name.clone() {
        Some(n) => n,
        None => match workflow.as_ref().and_then(|w| w.name.clone()) {
            Some(template) => params::interpolate(&template, &params)?,
            None => req
                .workflow
                .clone()
                .unwrap_or_else(|| basename(&req.invocation_cwd)),
        },
    });
    params.set("session", name.clone());

    // 4. Interpolate all remaining fields; any unresolved ${var} aborts
    //    before any Ghostty mutation.
    let mut labels = BTreeMap::new();
    if let Some(wf) = &workflow {
        for (k, v) in &wf.labels {
            labels.insert(k.clone(), params::interpolate(v, &params)?);
        }
    }
    for (k, v) in &req.labels {
        labels.insert(k.clone(), v.clone()); // flag labels are literal, and win
    }

    let session_cwd = match (&req.cwd, workflow.as_ref().and_then(|w| w.cwd.clone())) {
        (Some(flag), _) => absolutize(flag, &req.invocation_cwd)?,
        (None, Some(template)) => {
            absolutize(Path::new(&params::interpolate(&template, &params)?), &req.invocation_cwd)?
        }
        (None, None) => absolutize(&req.invocation_cwd, &req.invocation_cwd)?,
    };

    let layout = match &workflow {
        Some(wf) => match &wf.layout {
            Some(l) => interpolate_layout(l, &params)?,
            None => Layout { direction: Direction::Vertical, panels: vec![Panel::leaf(None)] },
        },
        None => Layout {
            direction: req.direction.unwrap_or(Direction::Vertical),
            panels: if req.commands.is_empty() {
                vec![Panel::leaf(None)]
            } else {
                req.commands.iter().map(|c| Panel::leaf(Some(c.clone()))).collect()
            },
        },
    };
    let layout = absolutize_layout(layout, &session_cwd);

    // 5. Reconcile, then check name uniqueness.
    let report = reconcile::reconcile(store, ghostty)?;
    reconcile::report_notice(&report);
    if store.session_exists(&name)? {
        return Err(Error::NameTaken(name));
    }

    // 6. Create the tab and splits, equalize, set the tab title.
    if !ghostty.is_running()? {
        ghostty.launch()?;
    }
    let window_target = req
        .window
        .clone()
        .or_else(|| workflow.as_ref().and_then(|w| w.window.clone()))
        .unwrap_or(WindowTarget::Front);
    let planned_window = match window_target {
        WindowTarget::New => PlannedWindow::New,
        WindowTarget::Front => match ghostty.front_window_id()? {
            // Ghostty running with zero windows behaves as `new` (spec §5.4).
            Some(id) => PlannedWindow::Existing(id),
            None => PlannedWindow::New,
        },
        WindowTarget::Id(id) => {
            // Validated up front; an unknown id is an early error (spec §5.4).
            if !ghostty.snapshot()?.window_exists(&id) {
                return Err(Error::Ghostty(format!("window '{id}' not found")));
            }
            PlannedWindow::Existing(id)
        }
    };

    let ctx = PlanCtx {
        window: &planned_window,
        layout: &layout,
        title: &name,
        session_cwd: &session_cwd,
        session_name: &name,
    };
    let ops = plan(&ctx)?;
    let created = execute(&ops, ghostty)?;

    // 7. Register (one tx); on failure — e.g. a name race — roll the tab back.
    let params_json = serde_json::to_string(&params.0)?;
    if let Err(e) = store.register(&NewSession {
        name: &name,
        window_id: &created.window_id,
        tab_id: &created.tab_id,
        workflow: req.workflow.as_deref(),
        cwd: Some(&session_cwd),
        params: &params_json,
        terminals: &created.terminals,
        labels: &labels,
    }) {
        let _ = ghostty.close_tab(&created.window_id, &created.tab_id);
        return Err(e);
    }

    // 8. Read the registered record back for output.
    store
        .find_exact(&name)?
        .ok_or_else(|| Error::Message(format!("session '{name}' vanished after registration")))
}

struct Created {
    window_id: String,
    tab_id: String,
    /// Ghostty terminal ids in pane order (ordinal = index).
    terminals: Vec<String>,
}

/// Fold ops over the bridge. Any failure rolls the half-built tab back —
/// spin-up is all-or-nothing.
fn execute(ops: &[Op], ghostty: &dyn GhosttyBridge) -> Result<Created> {
    let mut panes: std::collections::HashMap<PaneRef, String> = std::collections::HashMap::new();
    let mut created: Option<Created> = None;
    match run_ops(ops, ghostty, &mut panes, &mut created) {
        Ok(()) => created.ok_or_else(|| Error::Message("plan created no tab".into())),
        Err(e) => {
            if let Some(c) = created {
                let _ = ghostty.close_tab(&c.window_id, &c.tab_id);
            }
            Err(e)
        }
    }
}

fn run_ops(
    ops: &[Op],
    ghostty: &dyn GhosttyBridge,
    panes: &mut std::collections::HashMap<PaneRef, String>,
    created: &mut Option<Created>,
) -> Result<()> {
    for op in ops {
        match op {
            Op::NewWindow { cfg } => {
                let c = ghostty.new_window(cfg)?;
                panes.insert(PaneRef::Root, c.terminal_id.clone());
                *created = Some(Created {
                    window_id: c.window_id,
                    tab_id: c.tab_id,
                    terminals: vec![c.terminal_id],
                });
            }
            Op::NewTab { window_id, cfg } => {
                let c = ghostty.new_tab(window_id, cfg)?;
                panes.insert(PaneRef::Root, c.terminal_id.clone());
                *created = Some(Created {
                    window_id: c.window_id,
                    tab_id: c.tab_id,
                    terminals: vec![c.terminal_id],
                });
            }
            Op::Split { target, dir, cfg, into } => {
                let target_id = panes
                    .get(target)
                    .ok_or_else(|| Error::Message("plan referenced an unknown pane".into()))?
                    .clone();
                let id = ghostty.split(&target_id, *dir, cfg)?;
                panes.insert(*into, id.clone());
                created.as_mut().expect("root created first").terminals.push(id);
            }
            Op::Equalize { pane } => {
                ghostty.perform_action(pane_id(panes, *pane)?, "equalize_splits")?;
            }
            Op::SetTitle { pane, title } => {
                ghostty.perform_action(pane_id(panes, *pane)?, &format!("set_tab_title:{title}"))?;
            }
            Op::TypeRun { pane, text } => {
                let id = pane_id(panes, *pane)?;
                ghostty.input_text(id, text)?;
                ghostty.send_enter(id)?;
            }
        }
    }
    Ok(())
}

fn pane_id(panes: &std::collections::HashMap<PaneRef, String>, pane: PaneRef) -> Result<&str> {
    panes
        .get(&pane)
        .map(String::as_str)
        .ok_or_else(|| Error::Message("plan referenced an unknown pane".into()))
}

fn interpolate_layout(layout: &Layout, params: &Params) -> Result<Layout> {
    Ok(Layout {
        direction: layout.direction,
        panels: layout
            .panels
            .iter()
            .map(|p| interpolate_panel(p, params))
            .collect::<Result<Vec<_>>>()?,
    })
}

fn interpolate_panel(panel: &Panel, params: &Params) -> Result<Panel> {
    match panel {
        Panel::Leaf(l) => Ok(Panel::Leaf(PanelLeaf {
            run: l.run.as_deref().map(|r| params::interpolate(r, params)).transpose()?,
            cwd: l.cwd.as_deref().map(|c| params::interpolate(c, params)).transpose()?,
            env: l.env.iter().map(|e| params::interpolate(e, params)).collect::<Result<Vec<_>>>()?,
        })),
        Panel::Node(n) => Ok(Panel::Node(PanelNode { layout: interpolate_layout(&n.layout, params)? })),
    }
}

/// Panel cwds are relative to the session cwd.
fn absolutize_layout(layout: Layout, session_cwd: &str) -> Layout {
    Layout {
        direction: layout.direction,
        panels: layout.panels.into_iter().map(|p| absolutize_panel(p, session_cwd)).collect(),
    }
}

fn absolutize_panel(panel: Panel, session_cwd: &str) -> Panel {
    match panel {
        Panel::Leaf(mut l) => {
            if let Some(cwd) = l.cwd {
                l.cwd = Some(join_if_relative(&cwd, session_cwd));
            }
            Panel::Leaf(l)
        }
        Panel::Node(n) => Panel::Node(PanelNode { layout: absolutize_layout(n.layout, session_cwd) }),
    }
}

fn join_if_relative(path: &str, base: &str) -> String {
    let p = Path::new(path);
    if p.is_absolute() { path.to_string() } else { Path::new(base).join(p).to_string_lossy().into_owned() }
}

fn absolutize(path: &Path, base: &Path) -> Result<String> {
    let p = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
    Ok(p.to_string_lossy().into_owned())
}

fn basename(cwd: &Path) -> String {
    cwd.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "session".into())
}

/// Names are identifiers: control chars stripped (TDD §4.1) and `/`
/// normalized to `-` — spec §6.1 derives "review-feat-login" from
/// `review-${branch}` with branch `feat/login`.
fn sanitize_name(name: String) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == '/' { '-' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghostty::fake::FakeBridge;

    fn leaf(run: Option<&str>) -> Panel {
        Panel::leaf(run.map(str::to_string))
    }

    fn nested(direction: Direction, panels: Vec<Panel>) -> Panel {
        Panel::Node(PanelNode { layout: Layout { direction, panels } })
    }

    fn ctx<'a>(
        window: &'a PlannedWindow,
        layout: &'a Layout,
    ) -> PlanCtx<'a> {
        PlanCtx { window, layout, title: "s", session_cwd: "/cwd", session_name: "s" }
    }

    #[test]
    fn flat_vertical_three_panels() {
        let layout = Layout {
            direction: Direction::Vertical,
            panels: vec![leaf(Some("a")), leaf(Some("b")), leaf(Some("c"))],
        };
        let window = PlannedWindow::Existing("w1".into());
        let ops = plan(&ctx(&window, &layout)).unwrap();
        let cfg = || SurfaceCfg {
            cwd: Some("/cwd".into()),
            env: vec!["GEIST_SESSION=s".into()],
        };
        assert_eq!(
            ops,
            vec![
                Op::NewTab { window_id: "w1".into(), cfg: cfg() },
                Op::Split { target: PaneRef::Root, dir: Direction::Vertical, cfg: cfg(), into: PaneRef::Child(0) },
                Op::Split { target: PaneRef::Child(0), dir: Direction::Vertical, cfg: cfg(), into: PaneRef::Child(1) },
                Op::Equalize { pane: PaneRef::Root },
                Op::SetTitle { pane: PaneRef::Root, title: "s".into() },
                Op::TypeRun { pane: PaneRef::Root, text: "a".into() },
                Op::TypeRun { pane: PaneRef::Child(0), text: "b".into() },
                Op::TypeRun { pane: PaneRef::Child(1), text: "c".into() },
            ]
        );
    }

    #[test]
    fn nested_layout_matches_spec_example() {
        // vertical: [pi, horizontal: [vim ., lazygit]] — spec §4.2 example.
        let layout = Layout {
            direction: Direction::Vertical,
            panels: vec![
                leaf(Some("pi")),
                nested(Direction::Horizontal, vec![leaf(Some("vim .")), leaf(Some("lazygit"))]),
            ],
        };
        let window = PlannedWindow::Existing("w1".into());
        let ops = plan(&ctx(&window, &layout)).unwrap();
        let kinds: Vec<String> = ops
            .iter()
            .map(|op| match op {
                Op::NewWindow { .. } => "new_window".into(),
                Op::NewTab { .. } => "new_tab".into(),
                Op::Split { target, dir, into, .. } => format!("split {target:?} {} {into:?}", dir.ghostty()),
                Op::Equalize { .. } => "equalize".into(),
                Op::SetTitle { .. } => "title".into(),
                Op::TypeRun { pane, text } => format!("run {pane:?} {text}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "new_tab",
                "split Root right Child(0)",       // node pane, hosts vim .
                "split Child(0) down Child(1)",    // lazygit below vim .
                "equalize",
                "title",
                "run Root pi",
                "run Child(0) vim .",
                "run Child(1) lazygit",
            ]
        );
    }

    #[test]
    fn single_plain_pane_has_no_runs() {
        let layout = Layout { direction: Direction::Vertical, panels: vec![leaf(None)] };
        let window = PlannedWindow::New;
        let ops = plan(&ctx(&window, &layout)).unwrap();
        assert!(matches!(ops[0], Op::NewWindow { .. }));
        assert!(!ops.iter().any(|op| matches!(op, Op::TypeRun { .. } | Op::Split { .. })));
    }

    #[test]
    fn panel_cwd_and_env_reach_cfg() {
        let layout = Layout {
            direction: Direction::Horizontal,
            panels: vec![
                leaf(None),
                Panel::Leaf(PanelLeaf {
                    run: Some("x".into()),
                    cwd: Some("/elsewhere".into()),
                    env: vec!["A=1".into()],
                }),
            ],
        };
        let window = PlannedWindow::Existing("w1".into());
        let ops = plan(&ctx(&window, &layout)).unwrap();
        match &ops[1] {
            Op::Split { dir, cfg, .. } => {
                assert_eq!(*dir, Direction::Horizontal);
                assert_eq!(cfg.cwd.as_deref(), Some("/elsewhere"));
                assert_eq!(cfg.env, vec!["A=1".to_string(), "GEIST_SESSION=s".to_string()]);
            }
            other => panic!("expected split, got {other:?}"),
        }
    }

    #[test]
    fn execute_types_runs_after_creation() {
        let layout = Layout {
            direction: Direction::Vertical,
            panels: vec![leaf(Some("vim")), leaf(Some("lazygit"))],
        };
        let window = PlannedWindow::Existing("w-100".into());
        let ops = plan(&ctx(&window, &layout)).unwrap();
        let bridge = FakeBridge::with_window();
        let created = execute(&ops, &bridge).unwrap();
        assert_eq!(created.terminals.len(), 2);
        let log = bridge.log.borrow();
        let root = &created.terminals[0];
        let split = &created.terminals[1];
        let expected = vec![
            format!("action:{root}:equalize_splits"),
            format!("action:{root}:set_tab_title:s"),
            format!("input:{root}:vim"),
            format!("enter:{root}"),
            format!("input:{split}:lazygit"),
            format!("enter:{split}"),
        ];
        assert_eq!(*log, expected);
    }

    // ---- full `up` flow (fake bridge + in-memory store) ----

    fn req(dir: &Path) -> UpRequest {
        UpRequest {
            workflow: None,
            name: None,
            cwd: None,
            direction: None,
            labels: vec![],
            window: None,
            pre: None,
            params: vec![],
            commands: vec![],
            invocation_cwd: dir.to_path_buf(),
            home: dir.join("geist-home"),
        }
    }

    #[test]
    fn up_adhoc_registers_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open_memory().unwrap();
        let bridge = FakeBridge::with_window();
        let mut r = req(dir.path());
        r.commands = vec!["vim".into(), "lazygit".into()];

        let row = up(&r, &store, &bridge).unwrap();
        assert_eq!(row.name, dir.path().file_name().unwrap().to_string_lossy());
        assert_eq!(row.terminals.len(), 2);
        assert_eq!(row.cwd.as_deref(), Some(dir.path().to_string_lossy().as_ref()));
        // every pane got GEIST_SESSION injected
        for (_, cfg) in bridge.cfgs.borrow().iter() {
            assert!(cfg.env.iter().any(|e| e == &format!("GEIST_SESSION={}", row.name)));
        }
        // same name again -> NameTaken
        let err = up(&req(dir.path()), &store, &bridge).unwrap_err();
        assert!(matches!(err, Error::NameTaken(_)));
    }

    #[test]
    fn up_workflow_with_hook_and_params() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(config::PROJECT_CONFIG_NAME),
            r#"
workflows:
  review:
    name: "review-${branch}"
    labels: { role: review, branch: "${branch}" }
    params:
      branch: { required: true }
      base:   { default: "main" }
    hooks:
      pre: echo "worktree=$PWD/.wt/${branch}"
    cwd: "${worktree}"
    layout:
      direction: vertical
      panels:
        - run: pi
        - run: "git log ${base}"
"#,
        )
        .unwrap();
        let store = StateStore::open_memory().unwrap();
        let bridge = FakeBridge::with_window();
        let mut r = req(dir.path());
        r.workflow = Some("review".into());
        r.params = vec![("branch".to_string(), "feat/login".to_string())];

        let row = up(&r, &store, &bridge).unwrap();
        assert_eq!(row.name, "review-feat-login");
        // hook emits $PWD (canonical; /var -> /private/var on macOS)
        let canonical = dir.path().canonicalize().unwrap();
        assert_eq!(
            row.cwd.as_deref(),
            Some(format!("{}/.wt/feat/login", canonical.to_string_lossy()).as_str())
        );
        assert_eq!(row.labels["branch"], "feat/login");
        assert_eq!(row.labels["role"], "review");
        assert_eq!(row.workflow.as_deref(), Some("review"));
        let params: serde_json::Value = serde_json::from_str(&row.params).unwrap();
        assert_eq!(params["base"], "main");
        assert!(params["worktree"].as_str().unwrap().ends_with(".wt/feat/login"));
        // missing required param aborts before any mutation
        let mut r2 = req(dir.path());
        r2.workflow = Some("review".into());
        let panes_before = bridge.cfgs.borrow().len();
        assert!(matches!(up(&r2, &store, &bridge), Err(Error::MissingParam(_))));
        assert_eq!(bridge.cfgs.borrow().len(), panes_before);
    }

    #[test]
    fn up_aborts_on_unresolved_var_before_mutation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(config::PROJECT_CONFIG_NAME),
            "workflows:\n  x:\n    name: fixed\n    layout:\n      direction: vertical\n      panels:\n        - run: \"echo ${nope}\"\n",
        )
        .unwrap();
        let store = StateStore::open_memory().unwrap();
        let bridge = FakeBridge::with_window();
        let mut r = req(dir.path());
        r.workflow = Some("x".into());
        assert!(matches!(up(&r, &store, &bridge), Err(Error::UnresolvedVar(_))));
        assert!(bridge.cfgs.borrow().is_empty()); // nothing created
    }

    #[test]
    fn up_window_new_creates_window() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open_memory().unwrap();
        let bridge = FakeBridge::with_window();
        let mut r = req(dir.path());
        r.window = Some(WindowTarget::New);
        let row = up(&r, &store, &bridge).unwrap();
        assert_ne!(row.window_id, "w-100");
        assert_eq!(bridge.windows.borrow().len(), 2);
    }
}
