use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::error::{Error, Result};
use crate::ghostty::{GhosttyBridge, OsascriptBridge};
use crate::ghostty::types::{Direction, WindowTarget};
use crate::state::StateStore;
use crate::{config, output, picker, reconcile, resolve, session};

#[derive(Parser)]
#[command(
    name = "geist",
    version,
    about = "poltergeist — session manager for the Ghostty terminal"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Spin up a session — ad-hoc from flags, or from a named workflow
    Up(UpArgs),
    /// Adopt the current tab as a session and apply the layout to it
    Adopt(AdoptArgs),
    /// List live managed sessions
    Ls(LsArgs),
    /// Make a session's tab active (interactive picker when no name is given)
    Switch(SwitchArgs),
    /// Close a session's tab and remove it from the registry
    Kill(KillArgs),
}

/// Flags shared by `up` and `adopt`.
#[derive(Args)]
struct SpinArgs {
    /// Workflow name from config (project-local first, then global)
    workflow: Option<String>,
    /// Session name (default: workflow name, else basename of cwd)
    #[arg(long)]
    name: Option<String>,
    /// Working directory inherited by all panels (default: invocation cwd)
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Ad-hoc split direction: side by side (vertical) or stacked (horizontal)
    #[arg(long, value_enum)]
    direction: Option<Direction>,
    /// Attach a label to the session (repeatable)
    #[arg(long = "label", value_parser = parse_kv)]
    labels: Vec<(String, String)>,
    /// Pre-spin-up hook command (same contract as workflow hooks)
    #[arg(long)]
    pre: Option<String>,
    /// Supply a workflow param (repeatable; workflow mode only)
    #[arg(long = "param", value_parser = parse_kv)]
    params: Vec<(String, String)>,
    /// Print the session's full record as JSON
    #[arg(long)]
    json: bool,
    /// Ad-hoc panel commands, one panel per command (after --)
    #[arg(last = true)]
    commands: Vec<String>,
}

impl SpinArgs {
    fn into_request(self, window: Option<WindowTarget>, invocation_cwd: PathBuf, home: PathBuf) -> session::UpRequest {
        session::UpRequest {
            workflow: self.workflow,
            name: self.name,
            cwd: self.cwd,
            direction: self.direction,
            labels: self.labels,
            window,
            pre: self.pre,
            params: self.params,
            commands: self.commands,
            invocation_cwd,
            home,
        }
    }
}

#[derive(Args)]
struct UpArgs {
    #[command(flatten)]
    spin: SpinArgs,
    /// Window placement: front | new | <ghostty-window-id>
    #[arg(long)]
    window: Option<WindowTarget>,
}

#[derive(Args)]
struct AdoptArgs {
    #[command(flatten)]
    spin: SpinArgs,
}

#[derive(Args)]
struct LsArgs {
    /// Filter by label, AND semantics (repeatable)
    #[arg(long = "label", value_parser = parse_kv)]
    labels: Vec<(String, String)>,
    /// Emit complete records as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct SwitchArgs {
    /// Session name or unambiguous prefix
    name: Option<String>,
}

#[derive(Args)]
struct KillArgs {
    /// Session name or unambiguous prefix
    name: String,
}

fn parse_kv(s: &str) -> std::result::Result<(String, String), String> {
    s.split_once('=')
        .filter(|(k, _)| !k.is_empty())
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected KEY=VALUE, got '{s}'"))
}

/// Entry point. Exit codes: 0 success, 1 runtime error, 2 usage error (clap
/// parse failures exit 2 on their own).
pub fn run() -> i32 {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            if let Error::HookFailed { stderr, .. } = &e {
                let stderr = stderr.trim();
                if !stderr.is_empty() {
                    eprintln!("{stderr}");
                }
            }
            e.exit_code()
        }
    }
}

fn dispatch(cli: Cli) -> Result<()> {
    let invocation_cwd = std::env::current_dir()?;
    let home = config::home()?;
    let store = StateStore::open(&home.join(config::STATE_NAME))?;
    let bridge = OsascriptBridge::new();
    match cli.cmd {
        Cmd::Up(args) => cmd_up(args, &store, &bridge, invocation_cwd, home),
        Cmd::Adopt(args) => cmd_adopt(args, &store, &bridge, invocation_cwd, home),
        Cmd::Ls(args) => cmd_ls(args, &store, &bridge),
        Cmd::Switch(args) => cmd_switch(args, &store, &bridge),
        Cmd::Kill(args) => cmd_kill(args, &store, &bridge),
    }
}

fn reconcile_first(store: &StateStore, bridge: &dyn GhosttyBridge) -> Result<()> {
    reconcile::report_notice(&reconcile::reconcile(store, bridge)?);
    Ok(())
}

/// Cross-flag usage rules shared by `up` and `adopt` (spec §2.1).
fn validate_spin(spin: &SpinArgs) -> Result<()> {
    if spin.workflow.is_some() && !spin.commands.is_empty() {
        return Err(Error::Usage(
            "cannot combine a workflow with panel commands (after `--`)".into(),
        ));
    }
    if spin.workflow.is_none() && !spin.params.is_empty() {
        return Err(Error::Usage("--param only applies to workflow spins".into()));
    }
    if spin.workflow.is_some() && spin.direction.is_some() {
        return Err(Error::Usage("--direction only applies to ad-hoc spins".into()));
    }
    Ok(())
}

fn cmd_up(
    args: UpArgs,
    store: &StateStore,
    bridge: &dyn GhosttyBridge,
    invocation_cwd: PathBuf,
    home: PathBuf,
) -> Result<()> {
    validate_spin(&args.spin)?;
    let json = args.spin.json;
    let req = args.spin.into_request(args.window, invocation_cwd, home);
    let row = session::up(&req, store, bridge)?;
    output::print_up(&row, json, "created")
}

fn cmd_adopt(
    args: AdoptArgs,
    store: &StateStore,
    bridge: &dyn GhosttyBridge,
    invocation_cwd: PathBuf,
    home: PathBuf,
) -> Result<()> {
    validate_spin(&args.spin)?;
    let json = args.spin.json;
    let req = args.spin.into_request(None, invocation_cwd, home);
    let row = session::adopt(&req, store, bridge)?;
    output::print_up(&row, json, "adopted")
}

fn cmd_ls(args: LsArgs, store: &StateStore, bridge: &dyn GhosttyBridge) -> Result<()> {
    reconcile_first(store, bridge)?;
    let sessions = store.filter_by_labels(&args.labels)?;
    let selected = bridge.snapshot().ok().and_then(|s| s.selected_tab_id().map(str::to_string));
    output::print_ls(&sessions, selected.as_deref(), args.json)
}

fn cmd_switch(args: SwitchArgs, store: &StateStore, bridge: &dyn GhosttyBridge) -> Result<()> {
    reconcile_first(store, bridge)?;
    let name = match args.name {
        Some(name) => name,
        None => picker::pick(&store.live_sessions()?)?,
    };
    let row = resolve::resolve_session(store, &name)?;
    bridge.activate_app()?;
    bridge.activate_window(&row.window_id)?;
    bridge.select_tab(&row.window_id, &row.tab_id)?;
    println!("switched to '{}'", row.name);
    Ok(())
}

fn cmd_kill(args: KillArgs, store: &StateStore, bridge: &dyn GhosttyBridge) -> Result<()> {
    reconcile_first(store, bridge)?;
    let row = resolve::resolve_session(store, &args.name)?;
    bridge.close_tab(&row.window_id, &row.tab_id)?;
    store.delete_session(&row.name)?;
    println!("killed '{}'", row.name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_shape_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_up_adhoc() {
        let cli = Cli::try_parse_from(["geist", "up", "--", "vim", "lazygit"]).unwrap();
        match cli.cmd {
            Cmd::Up(args) => {
                assert_eq!(args.spin.workflow, None);
                assert_eq!(args.spin.commands, vec!["vim", "lazygit"]);
            }
            _ => panic!("expected up"),
        }
    }

    #[test]
    fn parses_adopt_and_rejects_window_flag() {
        let cli = Cli::try_parse_from(["geist", "adopt", "--name", "dev", "--", "vim"]).unwrap();
        match cli.cmd {
            Cmd::Adopt(args) => {
                assert_eq!(args.spin.name.as_deref(), Some("dev"));
                assert_eq!(args.spin.commands, vec!["vim"]);
            }
            _ => panic!("expected adopt"),
        }
        // --window is `up`-only: adopt always targets the current tab.
        assert!(Cli::try_parse_from(["geist", "adopt", "--window", "new"]).is_err());
    }

    #[test]
    fn parses_up_workflow_with_flags() {
        let cli = Cli::try_parse_from([
            "geist", "up", "review",
            "--param", "branch=feat",
            "--label", "role=review",
            "--window", "new",
            "--json",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Up(args) => {
                assert_eq!(args.spin.workflow.as_deref(), Some("review"));
                assert_eq!(args.spin.params, vec![("branch".to_string(), "feat".to_string())]);
                assert_eq!(args.spin.labels, vec![("role".to_string(), "review".to_string())]);
                assert_eq!(args.window, Some(WindowTarget::New));
                assert!(args.spin.json);
            }
            _ => panic!("expected up"),
        }
    }

    #[test]
    fn bad_kv_is_usage_error() {
        assert!(Cli::try_parse_from(["geist", "up", "--param", "noequals"]).is_err());
        assert!(Cli::try_parse_from(["geist", "ls", "--label", "=v"]).is_err());
    }

    #[test]
    fn direction_value_enum() {
        let cli = Cli::try_parse_from(["geist", "up", "--direction", "horizontal", "--", "top"]).unwrap();
        match cli.cmd {
            Cmd::Up(args) => assert_eq!(args.spin.direction, Some(Direction::Horizontal)),
            _ => panic!("expected up"),
        }
        assert!(Cli::try_parse_from(["geist", "up", "--direction", "sideways"]).is_err());
    }

    #[test]
    fn workflow_and_commands_together_is_rejected() {
        let cli = Cli::try_parse_from(["geist", "up", "review", "--", "vim"]).unwrap();
        let Cmd::Up(args) = cli.cmd else { panic!("expected up") };
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::open_memory().unwrap();
        let bridge = OsascriptBridge::new();
        let err = cmd_up(args, &store, &bridge, dir.path().to_path_buf(), dir.path().join("h"))
            .unwrap_err();
        assert!(matches!(err, Error::Usage(_)));
        assert_eq!(err.exit_code(), 2);
    }
}
