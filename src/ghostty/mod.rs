pub mod scripts;
pub mod types;
pub mod wire;

use std::process::Command;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use types::{CreatedRef, Direction, Snapshot, SurfaceCfg};

/// Seam against the live app; `OsascriptBridge` is the only production
/// implementation, tests use in-memory fakes (TDD §4.2).
pub trait GhosttyBridge {
    fn is_running(&self) -> Result<bool>;
    fn launch(&self) -> Result<()>;
    fn snapshot(&self) -> Result<Snapshot>;
    fn front_window_id(&self) -> Result<Option<String>>;
    fn new_window(&self, cfg: &SurfaceCfg) -> Result<CreatedRef>;
    fn new_tab(&self, window: &str, cfg: &SurfaceCfg) -> Result<CreatedRef>;
    fn split(&self, terminal: &str, dir: Direction, cfg: &SurfaceCfg) -> Result<String>;
    fn perform_action(&self, terminal: &str, action: &str) -> Result<()>;
    fn input_text(&self, terminal: &str, text: &str) -> Result<()>;
    fn send_enter(&self, terminal: &str) -> Result<()>;
    fn activate_app(&self) -> Result<()>;
    fn activate_window(&self, window: &str) -> Result<()>;
    fn select_tab(&self, window: &str, tab: &str) -> Result<()>;
    fn close_tab(&self, window: &str, tab: &str) -> Result<()>;
    fn focus(&self, terminal: &str) -> Result<()>;
}

/// Drives Ghostty through its AppleScript API. Scripts run as
/// `osascript -e <script> <args…>` — argv array, no shell, no quoting bugs.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsascriptBridge;

impl OsascriptBridge {
    pub fn new() -> Self {
        OsascriptBridge
    }

    fn run(script: &str, args: &[&str]) -> Result<String> {
        let out = Command::new("osascript").arg("-e").arg(script).args(args).output()?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            if stderr.contains("-1743") {
                return Err(Error::Ghostty(
                    "macOS blocked Apple Events to Ghostty — grant permission in \
                     System Settings → Privacy & Security → Automation"
                        .into(),
                ));
            }
            return Err(Error::Ghostty(stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_string())
    }
}

fn spawn_args<'a>(op: &'a str, target: &'a str, dir: &'a str, cfg: &'a SurfaceCfg, env: &'a str) -> [&'a str; 5] {
    [op, target, dir, cfg.cwd.as_deref().unwrap_or(""), env]
}

impl GhosttyBridge for OsascriptBridge {
    fn is_running(&self) -> Result<bool> {
        Ok(Self::run(scripts::IS_RUNNING, &[])? == "true")
    }

    fn launch(&self) -> Result<()> {
        let status = Command::new("open").args(["-a", "Ghostty"]).status()?;
        if !status.success() {
            return Err(Error::Ghostty("failed to launch Ghostty (`open -a Ghostty`)".into()));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.is_running().unwrap_or(false) && self.snapshot().is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(Error::Ghostty("ghostty did not start".into()))
    }

    fn snapshot(&self) -> Result<Snapshot> {
        Snapshot::parse(&Self::run(scripts::SNAPSHOT, &[])?)
    }

    fn front_window_id(&self) -> Result<Option<String>> {
        let id = Self::run(scripts::FRONT_WINDOW, &[])?;
        Ok(if id.is_empty() { None } else { Some(id) })
    }

    fn new_window(&self, cfg: &SurfaceCfg) -> Result<CreatedRef> {
        let env = cfg.env.join(&wire::US.to_string());
        let out = Self::run(scripts::SPAWN, &spawn_args("new_window", "", "", cfg, &env))?;
        parse_created(&out)
    }

    fn new_tab(&self, window: &str, cfg: &SurfaceCfg) -> Result<CreatedRef> {
        let env = cfg.env.join(&wire::US.to_string());
        let out = Self::run(scripts::SPAWN, &spawn_args("new_tab", window, "", cfg, &env))?;
        parse_created(&out)
    }

    fn split(&self, terminal: &str, dir: Direction, cfg: &SurfaceCfg) -> Result<String> {
        let env = cfg.env.join(&wire::US.to_string());
        Self::run(scripts::SPAWN, &spawn_args("split", terminal, dir.ghostty(), cfg, &env))
    }

    fn perform_action(&self, terminal: &str, action: &str) -> Result<()> {
        Self::run(scripts::ACTION, &["perform_action", terminal, action])?;
        Ok(())
    }

    fn input_text(&self, terminal: &str, text: &str) -> Result<()> {
        Self::run(scripts::ACTION, &["input_text", terminal, text])?;
        Ok(())
    }

    fn send_enter(&self, terminal: &str) -> Result<()> {
        Self::run(scripts::ACTION, &["send_enter", terminal])?;
        Ok(())
    }

    fn activate_app(&self) -> Result<()> {
        Self::run(scripts::ACTION, &["activate_app"])?;
        Ok(())
    }

    fn activate_window(&self, window: &str) -> Result<()> {
        Self::run(scripts::ACTION, &["activate_window", window])?;
        Ok(())
    }

    fn select_tab(&self, window: &str, tab: &str) -> Result<()> {
        Self::run(scripts::ACTION, &["select_tab", window, tab])?;
        Ok(())
    }

    fn close_tab(&self, window: &str, tab: &str) -> Result<()> {
        Self::run(scripts::ACTION, &["close_tab", window, tab])?;
        Ok(())
    }

    fn focus(&self, terminal: &str) -> Result<()> {
        Self::run(scripts::ACTION, &["focus", terminal])?;
        Ok(())
    }
}

fn parse_created(out: &str) -> Result<CreatedRef> {
    let fields: Vec<&str> = out.split(wire::US).collect();
    if fields.len() != 3 {
        return Err(Error::Ghostty(format!("malformed spawn result: '{out}'")));
    }
    Ok(CreatedRef {
        window_id: fields[0].to_string(),
        tab_id: fields[1].to_string(),
        terminal_id: fields[2].to_string(),
    })
}

#[cfg(test)]
pub mod fake {
    //! In-memory bridge for reconcile/session tests.
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Debug, Clone, PartialEq)]
    pub struct FakeTerminal {
        pub id: String,
        pub cwd: String,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct FakeTab {
        pub id: String,
        pub selected: bool,
        pub terminals: Vec<FakeTerminal>,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct FakeWindow {
        pub id: String,
        pub front: bool,
        pub tabs: Vec<FakeTab>,
    }

    #[derive(Debug, Default)]
    pub struct FakeBridge {
        pub running: Cell<bool>,
        pub next_id: Cell<u64>,
        pub windows: RefCell<Vec<FakeWindow>>,
        /// Log of side effects for assertions: "action:<term>:<action>",
        /// "input:<term>:<text>", "enter:<term>", "activate_app", ...
        pub log: RefCell<Vec<String>>,
        /// Surface cfgs seen at creation, keyed by terminal id.
        pub cfgs: RefCell<Vec<(String, SurfaceCfg)>>,
    }

    impl FakeBridge {
        fn gen_id(&self, prefix: &str) -> String {
            let n = self.next_id.get();
            self.next_id.set(n + 1);
            format!("{prefix}-{n}")
        }

        pub fn with_window() -> Self {
            let b = FakeBridge {
                running: Cell::new(true),
                ..FakeBridge::default()
            };
            b.windows.borrow_mut().push(FakeWindow {
                id: "w-100".into(),
                front: true,
                tabs: vec![FakeTab {
                    id: "t-100".into(),
                    selected: true,
                    terminals: vec![FakeTerminal { id: "term-100".into(), cwd: "/".into() }],
                }],
            });
            b.next_id.set(200);
            b
        }
    }

    impl GhosttyBridge for FakeBridge {
        fn is_running(&self) -> Result<bool> {
            Ok(self.running.get())
        }

        fn launch(&self) -> Result<()> {
            self.running.set(true);
            Ok(())
        }

        fn snapshot(&self) -> Result<Snapshot> {
            if !self.running.get() {
                return Err(Error::Ghostty("not running".into()));
            }
            Ok(Snapshot {
                windows: self
                    .windows
                    .borrow()
                    .iter()
                    .map(|w| types::WindowInfo {
                        id: w.id.clone(),
                        front: w.front,
                        tabs: w
                            .tabs
                            .iter()
                            .enumerate()
                            .map(|(i, t)| types::TabInfo {
                                id: t.id.clone(),
                                window_id: w.id.clone(),
                                index: (i + 1) as u32,
                                selected: t.selected,
                                name: String::new(),
                                terminals: t
                                    .terminals
                                    .iter()
                                    .map(|x| types::TerminalInfo {
                                        id: x.id.clone(),
                                        cwd: x.cwd.clone(),
                                        name: String::new(),
                                    })
                                    .collect(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
        }

        fn front_window_id(&self) -> Result<Option<String>> {
            Ok(self.windows.borrow().iter().find(|w| w.front).map(|w| w.id.clone()))
        }

        fn new_window(&self, cfg: &SurfaceCfg) -> Result<CreatedRef> {
            let wid = self.gen_id("w");
            let tid = self.gen_id("t");
            let term = self.gen_id("term");
            for w in self.windows.borrow_mut().iter_mut() {
                w.front = false;
            }
            self.windows.borrow_mut().push(FakeWindow {
                id: wid.clone(),
                front: true,
                tabs: vec![FakeTab {
                    id: tid.clone(),
                    selected: true,
                    terminals: vec![FakeTerminal { id: term.clone(), cwd: cfg.cwd.clone().unwrap_or_default() }],
                }],
            });
            self.cfgs.borrow_mut().push((term.clone(), cfg.clone()));
            Ok(CreatedRef { window_id: wid, tab_id: tid, terminal_id: term })
        }

        fn new_tab(&self, window: &str, cfg: &SurfaceCfg) -> Result<CreatedRef> {
            let tid = self.gen_id("t");
            let term = self.gen_id("term");
            let mut windows = self.windows.borrow_mut();
            let w = windows
                .iter_mut()
                .find(|w| w.id == window)
                .ok_or_else(|| Error::Ghostty(format!("window '{window}' not found")))?;
            for t in w.tabs.iter_mut() {
                t.selected = false;
            }
            w.tabs.push(FakeTab {
                id: tid.clone(),
                selected: true,
                terminals: vec![FakeTerminal { id: term.clone(), cwd: cfg.cwd.clone().unwrap_or_default() }],
            });
            drop(windows);
            self.cfgs.borrow_mut().push((term.clone(), cfg.clone()));
            Ok(CreatedRef { window_id: window.to_string(), tab_id: tid, terminal_id: term })
        }

        fn split(&self, terminal: &str, _dir: Direction, cfg: &SurfaceCfg) -> Result<String> {
            let term = self.gen_id("term");
            let mut windows = self.windows.borrow_mut();
            let tab = windows
                .iter_mut()
                .flat_map(|w| w.tabs.iter_mut())
                .find(|t| t.terminals.iter().any(|x| x.id == terminal))
                .ok_or_else(|| Error::Ghostty(format!("terminal '{terminal}' not found")))?;
            tab.terminals.push(FakeTerminal { id: term.clone(), cwd: cfg.cwd.clone().unwrap_or_default() });
            drop(windows);
            self.cfgs.borrow_mut().push((term.clone(), cfg.clone()));
            Ok(term)
        }

        fn perform_action(&self, terminal: &str, action: &str) -> Result<()> {
            self.log.borrow_mut().push(format!("action:{terminal}:{action}"));
            Ok(())
        }

        fn input_text(&self, terminal: &str, text: &str) -> Result<()> {
            self.log.borrow_mut().push(format!("input:{terminal}:{text}"));
            Ok(())
        }

        fn send_enter(&self, terminal: &str) -> Result<()> {
            self.log.borrow_mut().push(format!("enter:{terminal}"));
            Ok(())
        }

        fn activate_app(&self) -> Result<()> {
            self.log.borrow_mut().push("activate_app".into());
            Ok(())
        }

        fn activate_window(&self, window: &str) -> Result<()> {
            self.log.borrow_mut().push(format!("activate_window:{window}"));
            Ok(())
        }

        fn select_tab(&self, window: &str, tab: &str) -> Result<()> {
            self.log.borrow_mut().push(format!("select_tab:{window}:{tab}"));
            Ok(())
        }

        fn close_tab(&self, window: &str, tab: &str) -> Result<()> {
            self.log.borrow_mut().push(format!("close_tab:{window}:{tab}"));
            let mut windows = self.windows.borrow_mut();
            if let Some(w) = windows.iter_mut().find(|w| w.id == window) {
                w.tabs.retain(|t| t.id != tab);
            }
            windows.retain(|w| !w.tabs.is_empty());
            Ok(())
        }

        fn focus(&self, terminal: &str) -> Result<()> {
            self.log.borrow_mut().push(format!("focus:{terminal}"));
            Ok(())
        }
    }
}
