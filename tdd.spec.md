# ghosttbusterr — Technical Design Document

Implementation design for the behavior defined in `spec.md`. Where the two disagree,
`spec.md` wins.

---

## 1. Stack

| Concern | Choice | Rationale |
|---|---|---|
| Language | **Rust** (edition 2024, recent stable) | Single static binary; no runtime requirement for hooks/agents that shell out |
| CLI | `clap` 4 (derive) | Two `[[bin]]` targets over one lib |
| SQLite | `rusqlite` (`bundled` feature) | Compiles SQLite in-tree; zero system deps; `DatabaseSync`-class API |
| ULID | `ulid` 1 | Registry primary keys (schema convention) |
| Config | `serde` 1 + `serde_yaml` 0.9 | `serde_yaml` is archived but stable and ubiquitous; isolated behind `config` module so a swap to `serde_yml`/`yaml-rust2` is a one-file change |
| JSON | `serde_json` 1 | `--json` output, `session_params` column |
| Errors | `thiserror` 2 | Library error types; exit-code mapping at the CLI edge |
| Time | `jiff` 0.2 | Parse `created_at` for AGE column; timestamp math |
| fzf detection | `which` | PATH lookup for the picker |
| TTY detection | `std::io::IsTerminal` | In std since 1.70 — no crate |
| Ghostty bridge | `osascript` (AppleScript) via `std::process::Command` | See §4 |

No async anywhere; every operation is synchronous and short-lived.

**Build/install:** `cargo install --path .` installs both bins. Release profile:
`lto = true`, `strip = true`, `opt-level = 3`.

---

## 2. Crate layout

One crate, library-first (the future pi extension shells out to the CLI, but lib-first
keeps the core testable and reusable):

```
Cargo.toml
src/
  lib.rs
  main.rs            # thin: fn main() { std::process::exit(cli::run()) }
  error.rs           # Error enum, exit-code mapping
  cli.rs             # clap definitions, dispatch, flag/arg model
  config.rs          # discovery, YAML models, workflow merge
  params.rs          # declaration model, resolution, ${var} interpolation
  hooks.rs           # pre-hook execution, KEY=VALUE protocol parsing
  ghostty/
    mod.rs           # GhosttyBridge trait + OsascriptBridge
    scripts.rs       # AppleScript sources (snapshot, spawn, …)
    wire.rs          # delimited-text encode/decode
    types.rs         # snapshot/ref types, SurfaceConfig, SplitDirection
  state/
    mod.rs           # StateStore (repository API)
    schema.rs        # DDL, migrations
  reconcile.rs       # registry ↔ Ghostty diff
  session.rs         # `up` orchestration: split planner + executor
  resolve.rs         # session name resolution (exact → prefix)
  output.rs          # human tables, JSON emission, TTY rules
  picker.rs          # fzf / numbered fallback
```

**Binaries:** package `[[bin]] name = "ghosttbusterr"` (default `src/main.rs`) plus a second
`[[bin]] name = "gtb"` with `path = "src/main.rs"`. Identical behavior; no argv[0] sniffing.

**Home directory:** spec mandates `~/.config/ghosttbusterr/` — *not* `dirs::config_dir()`
(which is `~/Library/Application Support` on macOS). Construct from `dirs::home_dir()`.
Env override `GTB_HOME` replaces the whole directory (tests, power users).

---

## 3. Module designs

### 3.1 `error.rs`

```rust
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("config: {0}")] Config(String),
    #[error("workflow '{0}' not found")] WorkflowNotFound(String),
    #[error("missing required param '{0}'")] MissingParam(String),
    #[error("unresolved variable '${{{0}}}'")] UnresolvedVar(String),
    #[error("hook failed (exit {code})")] HookFailed { code: i32, stderr: String },
    #[error("session '{0}' not found")] SessionNotFound(String),
    #[error("ambiguous session '{0}': matches {}", .1.join(", "))] Ambiguous(String, Vec<String>),
    #[error("session name '{0}' already in use")] NameTaken(String),
    #[error("ghostty: {0}")] Ghostty(String),
    #[error(transparent)] Io(#[from] std::io::Error),
    #[error(transparent)] Sqlite(#[from] rusqlite::Error),
    #[error(transparent)] Yaml(#[from] serde_yaml::Error),
    #[error(transparent)] Json(#[from] serde_json::Error),
}
```

CLI edge maps to exit codes: usage errors (clap handles itself → `2`), everything else → `1`
with the message on stderr. Success → `0`.

### 3.2 `cli.rs`

clap derive mirrors spec §2 exactly. Notable modeling:

```rust
#[derive(Parser)]
struct Cli { #[command(subcommand)] cmd: Cmd }

enum Cmd {
    Up(UpArgs),            // [workflow] + trailing var-arg panel commands (trailing_var_arg)
    Ls { label: Vec<String>, json: bool },
    Switch { name: Option<String> },
    Kill { name: String },
}

struct UpArgs {
    workflow: Option<String>,
    #[arg(long)] name: Option<String>,
    #[arg(long)] cwd: Option<PathBuf>,
    #[arg(long)] direction: Option<Direction>,     // vertical | horizontal
    #[arg(long = "label")] labels: Vec<String>,    // k=v, validated at parse
    #[arg(long)] window: Option<WindowTarget>,     // front | new | <id>
    #[arg(long)] pre: Option<String>,
    #[arg(long = "param")] params: Vec<String>,    // k=v
    #[arg(long)] json: bool,
    #[arg(last = true)] commands: Vec<String>,     // after `--`
}
```

Dispatch validates cross-flag rules (workflow + `--` commands together → usage error).

### 3.3 `config.rs`

- `discover(start: &Path) -> (Option<PathBuf /*project*/>, PathBuf /*global*/)` — walk up
  from cwd looking for `.ghosttbusterr.yml`; global is `$GTB_HOME/config.yml` (may not exist).
- Serde model: `Config { workflows: BTreeMap<String, Workflow> }`; `Workflow { name, labels,
  params: BTreeMap<String, ParamDecl>, hooks: Hooks, window, cwd, layout: Layout }`;
  `Layout { direction: Direction, panels: Vec<Panel> }`; `Panel` is an untagged enum:
  leaf `{ run, cwd, env }` vs node `{ layout: Layout }` — untagged works because node
  items always carry `layout`, leaves never do. Unknown YAML keys rejected
  (`deny_unknown_fields`) for typo safety.
- Lookup: project map first, then global (project shadows on key conflict).

### 3.4 `params.rs`

- `ParamDecl { required: bool, default: Option<String> }` (custom Deserialize so both
  `branch: { required: true }` and `install: { default: "…" }` parse).
- `resolve(decls, cli_params, env, hook_out) -> Params` implementing spec precedence:
  flag > env (`std::env::var`) > default > `MissingParam`. Two-phase per spec: pre-hook pass
  without hook keys, post-hook re-check at interpolation time.
- `interpolate(template, &Params) -> Result<String>` — scans for `${name}`; `$${` is an
  escaped literal `${`. Unknown name → `UnresolvedVar` (never silent empty strings).
  Implicit `cwd` (invocation dir) and `session` (resolved name) are seeded into the map.
- Interpolation is applied to: workflow name, label values, panel `run`/`cwd`/`env` values,
  workflow `cwd`, hook command, `--pre` command.

### 3.5 `hooks.rs`

```rust
fn run_pre(cmd: &str, cwd: &Path, env: &Env) -> Result<Vec<(String, String)>>
```

- Shell: `$SHELL` (fallback `/bin/zsh`) with `-lc <cmd>`; `current_dir(cwd)`; inherit env.
- Capture stdout/stderr (no streaming needed — hooks are short).
- Exit ≠ 0 → `HookFailed { code, stderr }` (stderr shown verbatim).
- Parse stdout lines as `KEY=VALUE` (first `=` splits; lines without `=` ignored but
  warned to stderr — catches scripts accidentally logging to stdout).

### 3.6 `ghostty/` — the bridge (see §4)

### 3.7 `state/`

`StateStore { conn: rusqlite::Connection }`, opened with:

```rust
conn.pragma_update(None, "journal_mode", "WAL")?;
conn.pragma_update(None, "foreign_keys", "ON")?;
migrate(&conn)?;   // user_version-driven
```

Repository API (all single-statement or explicit tx):

```rust
impl StateStore {
    fn register(&self, s: &NewSession) -> Result<()>;            // tx: Session + Terminals + Labels
    fn live_sessions(&self) -> Result<Vec<SessionRow>>;          // joined, labels aggregated
    fn find(&self, name_or_prefix: &str) -> Result<SessionRow>;  // exact → unique prefix → Ambiguous/NotFound
    fn filter_by_labels(&self, kv: &[(String, String)]) -> Result<Vec<SessionRow>>;  // INTERSECT
    fn delete_by_tab_ids(&self, keep: &HashSet<String>) -> Result<usize>;            // reconciliation deletes
    fn delete_dead_terminals(&self, keep: &HashSet<String>) -> Result<usize>;
    fn refresh_windows(&self, moves: &[(String /*tab*/, String /*window*/)]) -> Result<()>;
    fn delete_session(&self, name: &str) -> Result<()>;
    fn delete_all(&self) -> Result<usize>;                        // Ghostty-not-running case
}
```

Label filter SQL (AND semantics across pairs):

```sql
SELECT … FROM Session
WHERE session_name IN (
  SELECT label_session_id FROM Label WHERE label_key = ?1 AND label_value = ?2
  INTERSECT
  SELECT label_session_id FROM Label WHERE label_key = ?3 AND label_value = ?4
)
```

(built dynamically; each pair adds one INTERSECT arm).

### 3.8 `state/schema.rs`

DDL exactly as spec §5.2. Migrations:

```rust
const MIGRATIONS: &[&str] = &[ include_str!("schema/0001_init.sql") ];

fn migrate(conn: &Connection) -> Result<()> {
    let v: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(v as usize) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}
```

### 3.9 `reconcile.rs`

```rust
fn reconcile(state: &StateStore, ghostty: &dyn GhosttyBridge) -> Result<ReconcileReport>
```

1. `ghostty.is_running()?` (System Events — never launches Ghostty).
2. Not running → `state.delete_all()`.
3. Running → `ghostty.snapshot()?` once; then:
   - `delete_by_tab_ids(live_tab_ids)` — cascades remove sessions' children,
   - `delete_dead_terminals(live_terminal_ids)` for panes closed inside live tabs,
   - `refresh_windows(moved)` where a live tab's parent window ≠ stored
     `session_ghostty_window_id` (also bumps `session_updated_at`).
4. Return counts; caller prints `cleared N closed session(s)` to stderr when N > 0.

`up`, `ls`, `switch`, `kill` all call `reconcile` first, always.

### 3.10 `session.rs` — `up` orchestration

Two phases, with the pure one separated for testing:

**Plan (pure):**

```rust
enum Op {
    NewTab { cfg: SurfaceCfg },                        // always op 0 (or NewWindow)
    Split { target: PaneRef, dir: Direction, cfg: SurfaceCfg, into: PaneRef },
    SetTitle { title: String },                        // set_tab_title:<name>
    Equalize,                                          // equalize_splits
    TypeRun { pane: PaneRef, text: String },           // run delivery
}

fn plan(wf: &ResolvedWorkflow, target: &WindowTarget) -> Vec<Op>
```

Layout tree → op list, recursive, newest-pane-first: for a node with panels
`[p0, p1..pn]`, `p0` occupies the current pane; each subsequent panel splits the pane
created just before it (`vertical` → Ghostty `right`, `horizontal` → `down`). Nested
layouts recurse into their freshly split pane. `PaneRef` placeholders (`Root`, `Child(n)`)
are resolved to real terminal IDs at execution. Sibling `SetTitle`/`Equalize`/`TypeRun`
ops follow creation order: create all panes → equalize → set title → type runs.

**Execute:** fold ops over the bridge, mapping `PaneRef` → returned terminal IDs, then
`StateStore::register` in one tx, then print (table line or `--json` record).

**Surface config mapping** (per pane):

| Workflow field | Surface configuration property |
|---|---|
| `cwd` (panel > session > invocation) | `initial working directory` |
| `env` + `GTB_SESSION=<name>` | `environment variables` (`KEY=VALUE` list) |
| `run` | delivered post-creation via `input text` + `send key "enter"` (V2-resolved) |

### 3.11 `resolve.rs`

`find` semantics shared by `switch`/`kill`: exact match → rows with `name LIKE prefix%` →
0 → `SessionNotFound`; >1 → `Ambiguous` (candidate list in the error); 1 → row.

### 3.12 `output.rs`

- TTY check via `std::io::IsTerminal`; `--json` short-circuits all formatting.
- Table: manual column sizing (no table crate) — NAME, `→`/space marker, LABELS
  (`k=v` comma-joined), CWD (`~`-collapsed via `dirs::home_dir`), AGE (`jiff` span →
  `15m`, `2h`, `3d`).
- Empty state → the spec'd hint line.

### 3.13 `picker.rs`

```rust
fn pick(sessions: &[SessionRow]) -> Result<String>
```

- `!stdout.is_terminal()` → `Error::Ghostty("pass a session name".into())`-class usage error.
- `which::which("fzf").is_ok()` → spawn `fzf` with stdin = lines
  `name\tlabels\tcwd`, read selection, parse name from first field.
- Else numbered list on stderr (stdout stays clean), `read_line` a number, validate range.

---

## 4. Ghostty bridge (`ghostty/`)

### 4.1 Wire format: AppleScript + delimited text

Bridge operations are AppleScript run via `osascript <script> <args…>`, spawned by Rust with
`Command` (argv array — **no shell**, no quoting bugs).

- **Inputs** travel as plain argv elements received by the script's `on run argv` handler.
  User data (session names, `run` commands containing arbitrary shell text) is **never**
  string-interpolated into script source — injection is a non-issue.
- **Outputs** are delimited text, not JSON. Every bridge query is fixed-shape and flat, so
  nesting is unnecessary: fields are separated by `ASCII character 31` (unit separator) and
  records by `ASCII character 30` (record separator) — control characters that cannot
  collide with real titles/paths. Rust decodes with `split('\x1e')`/`split('\x1f')`
  (~15 lines in `wire.rs`, unit-tested). The one string we ever *write* into Ghostty (the
  session name as tab title) is sanitized of control characters at the CLI edge.

Rationale over JXA: the snapshot's nesting flattens losslessly (one record per terminal,
window/tab fields denormalized), so JSON buys nothing; AppleScript is the canonical language
of the sdef and its `whose` filters (`first tab whose id is …`) are the standard, reliable
form — JXA's `whose()` is a known quirk zone and JXA itself is frozen at Apple. Scripts are
`include_str!` constants in `scripts.rs`. (AppleScript enumeration was already validated
against the live app during design.)

### 4.2 Trait (seams for tests)

```rust
trait GhosttyBridge {
    fn is_running(&self) -> Result<bool>;
    fn launch(&self) -> Result<()>;                       // open -a Ghostty + poll
    fn snapshot(&self) -> Result<Snapshot>;               // windows → tabs → terminals (+cwd)
    fn front_window_id(&self) -> Result<Option<String>>;
    fn new_window(&self, cfg: &SurfaceCfg) -> Result<CreatedRef>;   // window + first terminal
    fn new_tab(&self, window: &str, cfg: &SurfaceCfg) -> Result<CreatedRef>; // tab + first terminal
    fn split(&self, terminal: &str, dir: Direction, cfg: &SurfaceCfg) -> Result<String /*new terminal id*/>;
    fn perform_action(&self, terminal: &str, action: &str) -> Result<()>;
    fn input_text(&self, terminal: &str, text: &str) -> Result<()>;
    fn send_enter(&self, terminal: &str) -> Result<()>;
    fn activate_window(&self, window: &str) -> Result<()>;
    fn select_tab(&self, window: &str, tab: &str) -> Result<()>;
    fn close_tab(&self, window: &str, tab: &str) -> Result<()>;
}
```

`SurfaceCfg { cwd: Option<String>, env: Vec<String>, wait_after_command: bool }`
maps to the sdef record (`workingDirectory`, `environmentVariables`, `waitAfterCommand`).
`initialInput` is deliberately unused (V2).

### 4.3 Scripts

- **`snapshot.scpt`** — walks `windows` → `tabs` → `terminals`, emitting **one record per
  terminal** with fields denormalized:
  `window-id ␟ tab-id ␟ tab-index ␟ tab-selected ␟ terminal-id ␟ cwd ␟ tab-name ␟ terminal-name`.
  Rust groups by tab/window client-side. Empty Ghostty (zero windows) → empty output.
- **`is_running.scpt`** — `tell application "System Events" to (name of processes) contains
  "Ghostty"`. Never `tell`s Ghostty, so it cannot launch it.
- **`spawn.scpt`** — one script handles `new_tab`/`new_window`/`split` (argv[1] selects the
  op), building the surface configuration record only from provided keys (absent ≠ empty),
  returning the created refs' IDs as one delimited record. `new tab` with no target uses
  `front window`.
- **`action.scpt`** — `perform action` (used for `set_tab_title:<name>` and
  `equalize_splits`), plus `input text` / `send key "enter"`, `activate window` /
  `select tab` / `close tab`. Tab/terminal references are rebuilt per call from IDs:
  `first tab of first window whose id is …` — the sdef exposes stable IDs precisely so
  references survive across separate osascript invocations.

### 4.4 Launch & readiness

`launch()`: `open -a Ghostty` (via `Command`, detached), then poll `is_running` +
`snapshot` succeeds, 50 ms interval, 10 s ceiling → `Error::Ghostty("ghostty did not
start")`. `up` calls `ensure_running` before planning execution.

---

## 5. Build-time verification items

Behavioral assumptions that must be proven against the live app before/while implementing
the modules that depend on them (each gates the listed module):

| # | Assumption | Gates |
|---|---|---|
| ~~V1~~ | **RESOLVED**: probes exercised `new tab` (with surface configuration incl. `initial working directory`), `split right/down`, `input text`, `send key enter`, `perform action` (`set_tab_title`, `equalize_splits`), `whose`-based reference rebuilding across separate osascript invocations, `close tab`, and full enumeration. Residual: `new window` / `activate window` / `select tab` unexercised — same command family, accepted risk | ~~`ghostty/`~~ |
| ~~V2~~ | **RESOLVED** (probed on 1.3.1): `initial input` types but never auto-submits, and its delivery timing is Ghostty-controlled → not used. `run` delivery is `input text` + `send key "enter"`, post-creation, strictly ordered by us. Residual: `input text` sent before shell-ready relies on pty buffering (accepted risk; observed safe) | ~~`session.rs`~~ |
| ~~V3~~ | **RESOLVED** (probed on 1.3.1): `set_tab_title` pins the tab title; app escape sequences (OSC 0) do not override it. No re-assertion logic needed | ~~`session.rs`~~ |
| ~~V4~~ | **RESOLVED**: (a) IDs byte-identical across osascript processes an hour apart, surviving manual reordering; (b) user drag test — two tabs dragged to other windows kept their tab IDs, only parent window changed (covered by reconcile's `window_id` refresh) | ~~`reconcile.rs`~~ |
| ~~V5~~ | **RESOLVED** (probed on 1.3.1): `split` returns the new terminal (verified against live enumeration); splits without explicit config inherit cwd; focus moves to the newest pane; `equalize_splits` works | ~~`session.rs`~~ |

---

## 6. Concurrency & consistency

- WAL + a single write transaction per command; registration is one tx covering
  `Session`/`Terminal`/`Label` inserts.
- Two `gtb up` racing on the same name: both reconcile, both plan, second `INSERT` hits
  `idx__session__session_name` UNIQUE → mapped to `Error::NameTaken`. SQLite serializes the
  writers; no app-level locking needed.
- Reconciliation deletes and refreshes run in the same tx as the command's writes where
  they coexist (`up`), otherwise standalone.

---

## 7. Testing strategy

| Layer | Approach |
|---|---|
| `params` | Pure unit tests: precedence matrix, interpolation, escapes, error cases |
| `session::plan` | Pure unit tests: flat/nested layout → op sequence, direction mapping, pane targeting |
| `resolve` | Unit tests over in-memory SQLite: exact/prefix/ambiguous/missing |
| `state` | In-memory SQLite (`:memory:` + same migrations): register, label INTERSECT, cascade deletes, name uniqueness, `GTB_HOME` tempdir for file-backed open |
| `reconcile` | `FakeBridge` (in-memory `Snapshot`): dead tabs purged, window moves refresh, Ghostty-down wipes, stderr notice |
| `hooks` | Real `/bin/sh` scripts in tempdirs: KEY=VALUE parsing, non-zero exit, stdout-vs-stderr separation |
| `cli` dispatch | `clap::Command::debug_assert` + flag-combination validation tests |
| End-to-end | `#[ignore = "requires live Ghostty"]` integration tests driving a real app: up → ls → switch → kill round-trip, manual-close reconciliation. Run manually via `GTB_INTEGRATION=1 cargo test -- --ignored` |

CI runs everything except the ignored end-to-end tier.

---

## 8. Dependency manifest

```toml
[dependencies]
clap        = { version = "4", features = ["derive"] }
rusqlite    = { version = "0.32", features = ["bundled"] }
ulid        = "1"
serde       = { version = "1", features = ["derive"] }
serde_yaml  = "0.9"
serde_json  = "1"
thiserror   = "2"
jiff        = "0.2"
which       = "7"
dirs        = "6"

[profile.release]
lto = true
strip = true
```

(`dev-dependencies`: `tempfile` for state/hook tests.)

---

## 9. Risks & mitigations

| Risk | Mitigation |
|---|---|
| sdef command behavior diverges from documentation (parameter labels, record keys) | V1; bridge scripts are the only coupling point, small and swappable |
| macOS Automation consent per host app blocks unattended first run | Documented in spec §7; errors from osascript (`-1743`) mapped to a clear "grant permission in System Settings → Privacy → Automation" message |
| `initial input` timing on slow shells | V2; fallback path already designed |
| serde_yaml archived | Isolated behind `config.rs`; swap is one file |
| Ghostty sdef changes in future versions | Bridge scripts are the only coupling point; version checked via `ghostty +version` in diagnostics |
