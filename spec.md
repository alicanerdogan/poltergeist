# ghosttbusterr — Specification

**ghosttbusterr** is a session manager for the [Ghostty](https://ghostty.org) terminal on macOS.
It translates declarative *workflows* into live Ghostty state: tabs, split panes, running
commands, and titles — and keeps a registry of everything it created so sessions can be
listed, switched to, and killed.

**Agnosticism principle.** ghosttbusterr knows nothing about its callers. It is not aware of
pi, AI agents, or any other program. It exposes generic primitives (layouts, commands, hooks,
params, labels) that any caller — a human, a script, or a separate adapter such as a pi
extension — composes to build higher-level behavior.

**Control plane.** All Ghostty manipulation goes through Ghostty's AppleScript API
(`Ghostty.sdef`), invoked via `osascript`. Ghostty is the source of truth for what exists;
ghosttbusterr's own state is a registry of claims that is continuously reconciled against it.

**Binaries.** Ships two equivalent entry points: `ghosttbusterr` and the short alias `gtb`.

---

## 1. Concepts

| Term | Meaning |
|---|---|
| **Session** | One ghosttbusterr-managed Ghostty tab: its name, labels, panes, and Ghostty IDs. The unit of listing, switching, and killing. |
| **Panel** | One terminal surface (split pane) within a session's tab. |
| **Workflow** | A named, parameterized, declarative description of a session, defined in a config file. |
| **Label** | A key-value pair attached to a session; queryable metadata (`project=dasei`, `role=review`). |
| **Param** | A declared workflow input, supplied by flag, environment, or default; interpolated into workflow fields. |
| **Registry** | The SQLite record of live sessions, reconciled against Ghostty on every command. |

**Direction vocabulary (vim semantics).** `vertical` means panes arranged **side by side**
(vertical divider, like `:vsplit`; maps to Ghostty `split right`). `horizontal` means panes
**stacked** (like `:split`; maps to Ghostty `split down`).

---

## 2. Commands

### 2.1 `gtb up`

Spin up a session — ad-hoc from flags, or from a named workflow.

```
gtb up [workflow] [--name N] [--cwd DIR] [--direction vertical|horizontal]
       [--label k=v]... [--window TARGET] [--pre CMD] [--param k=v]...
       [--json] [--] [CMD ...]
```

- A positional argument **before** `--` is a workflow name. Panel commands only ever appear
  **after** `--`. Combining both is an error.
- **Ad-hoc panels** (after `--`): one panel per command, split in `--direction` order
  (default `vertical`). No commands → a single plain shell pane. Ad-hoc layouts are flat;
  nested layouts require a workflow file.
- **`--name`**: session name. Default for ad-hoc spins: basename of the invocation cwd.
  Overrides the workflow's `name` (after interpolation).
- **`--cwd`**: session working directory, inherited by all panels. Default: invocation cwd.
  In workflow mode, overrides the workflow's `cwd`.
- **`--label k=v`**: repeatable; attaches labels to the session. Merged with (and overrides)
  workflow labels on key conflict.
- **`--window TARGET`**: `front` (default) | `new` | a Ghostty window ID. Overrides the
  workflow's `window` field. See §5.4.
- **`--pre CMD`**: ad-hoc pre-spin-up hook with the same contract as workflow hooks (§4.4).
- **`--param k=v`**: repeatable; supplies workflow params (§4.3). Only meaningful in
  workflow mode.
- **`--json`**: on success, prints the created session's full record (see §6.2).

**Lifecycle (all-or-nothing):**

1. Resolve params (§4.3); error on missing required params.
2. Run the pre-hook, if any (§4.4); abort on failure.
3. Interpolate all fields; any unresolved `${var}` aborts **before any Ghostty mutation**.
4. Reconcile the registry (§5.3), then check name uniqueness (§5.2).
5. Create the tab and splits via AppleScript, equalize splits, set the tab title.
6. Deliver each panel's `run` command (§5.5).
7. Focus the active panel, if any (§4.2 `active`).
8. Register the session in the registry; print the result.

**Variant: `gtb adopt`.**

```
gtb adopt [workflow] [--name N] [--cwd DIR] [--direction vertical|horizontal]
          [--label k=v]... [--pre CMD] [--param k=v]... [--json] [--] [CMD ...]
```

Like `up`, but instead of creating a tab it **adopts the current tab** — the selected
tab of the front Ghostty window — and applies the layout to it. All `up` flags apply
except `--window`. Guards, checked before any mutation:

- Ghostty is running with a current tab.
- The tab has exactly **one pane** (adopt can't guess which split is the main one).
- The tab is not already managed by a session (checked after reconciliation).

The tab's existing terminal becomes the root pane: the first panel's `run` is typed
into that shell, remaining panels split off of it, splits are equalized, and the tab
title is set. Lifecycle steps 1–4 and 8 are identical to `up`; steps 5–7 differ in
that nothing is created for the root. Two consequences of the root shell pre-existing:
its cwd and environment are untouched (no `GEIST_SESSION` there), and a mid-spin
failure cannot roll the tab back — the user's tab is never closed, though partial
splits may remain.

### 2.2 `gtb ls`

```
gtb ls [--label k=v]... [--json]
```

List live managed sessions. `--label` filters are repeatable with AND semantics.

Human output — `→` marks the session whose tab is currently selected in Ghostty:

```
  NAME                LABELS                           CWD                    AGE
→ review-feat-login   role=review,branch=feat/login    ~/r/dasei/.wt/feat…   15m
  dev                 project=dasei                    ~/r/dasei             2h
```

CWD is home-collapsed (`~`). Ghostty IDs, terminal IDs, params, and exact timestamps appear
only in `--json` output. With no live sessions, prints a hint
(`no managed sessions — gtb up <workflow>`).

### 2.3 `gtb switch`

```
gtb switch [<name>]
```

Make a session's tab active: bring Ghostty frontmost if needed, `activate window`, then
`select tab`.

**Name resolution** (shared with `kill`): exact match → unambiguous prefix
(`gtb switch rev` matches `review-feat-login` if it is the only `rev*`) → error listing
candidates.

**No argument** → interactive picker:

- TTY and `fzf` on PATH → fzf over sessions (name, labels, cwd per line); Enter switches.
- TTY without fzf → numbered list; read a number from stdin.
- Non-TTY → error (`pass a session name`). The picker never fires from scripts or agents.

### 2.4 `gtb kill`

```
gtb kill <name>
```

Close the session's tab (`close tab`) and remove it from the registry. Same name resolution
as `switch`. No confirmation prompt: names are explicit, and prompts break scriptability.

---

## 3. Output & process contract

- **`--json`** exists on `up` and `ls` and emits complete records (§6). Human tables are for
  humans; machines always get `--json`.
- **Non-interactive by default when stdout is not a TTY:** no pickers, no prompts.
- **Errors** go to stderr with a non-zero exit code (`0` success; `1` runtime error;
  `2` usage error).
- **Cleanup notices:** when a command prunes stale registry entries (§5.3), it prints
  `cleared N closed session(s)` to stderr.

---

## 4. Configuration

### 4.1 Locations & discovery

- **Global:** `~/.config/ghosttbusterr/config.yml`
- **Project-local:** `.ghosttbusterr.yml`, discovered by walking up from the invocation cwd
  (like git finds `.git`).
- Workflow lookup: project config first, then global; project workflows shadow global ones
  on name conflict.

### 4.2 Workflow schema

```yaml
workflows:
  review:
    name: "review-${branch}"              # session name (interpolated)
    labels: { role: review, branch: "${branch}" }
    params:
      branch: { required: true }
      base:   { default: "main" }
    hooks:
      pre: ./scripts/new-worktree.sh "${branch}" "${base}"
    window: front                          # front | new | <ghostty-window-id>
    cwd: "${worktree}"                     # session cwd; panels inherit, may override
    layout:
      direction: vertical                  # side by side (vim semantics, §1)
      panels:
        - run: pi
        - layout:
            direction: horizontal          # stacked
            panels:
              - run: vim .
              - run: lazygit
```

**Layout** is a recursive list tree. A `panels` item is either a **panel leaf** or a nested
`layout` node. Panel fields:

| Field | Meaning |
|---|---|
| `run` | Shell command typed into the pane's interactive shell (§5.5). Optional; omitted → plain shell. |
| `cwd` | Panel working directory. Default: the session cwd. |
| `env` | List of `KEY=VALUE` entries injected into that surface's environment. |
| `active` | Boolean; mark one panel to be Ghostty-focused after spin-up. At most one per workflow; none → Ghostty's native focus (the last-created split). |

Splits are created newest-pane-first per node, then Ghostty's `equalize_splits` action
normalizes pane sizes. Panel *order* expresses placement (first = left/top); there are no
`left`/`up` splits. The active panel (if any) is focused **last**, after run delivery, so it
overrides the focus a `run` command may leave behind.

### 4.3 Params

Params are **declared in the workflow** (its interface) and supplied from three sources with
this precedence:

```
--param key=value  (flag)   >   $key  (environment)   >   YAML default   >   error if required
```

- `${name}` interpolation works in every string field: `name`, label values, `run`, `cwd`,
  `env` values, and hook commands.
- Two implicit params always exist without declaration: `${cwd}` (invocation directory) and
  `${session}` (resolved session name).
- Undeclared params warn (typo safety without aborting) and are dropped from the
  resolved set, except keys emitted by the pre-hook (§4.4), which enter the param set
  dynamically.
- Declared params make workflows self-documenting; help output can list what a workflow
  accepts.

### 4.4 Hooks

`hooks.pre` runs **before any Ghostty mutation**, via `$SHELL -lc` (login shell, so the
user's normal PATH/tools apply), in the invocation cwd, inheriting the caller's environment,
with already-resolved params interpolated into the command string.

**Contract:**

- **stdout is protocol, stderr is human output.** Each stdout line of `KEY=VALUE` becomes a
  param (or overrides one — hook output wins over flag/env/default, since it is computed
  last with full knowledge of them).
- stderr passes through to the terminal for progress logging.
- Non-zero exit → **abort**; hook stderr is surfaced; nothing is created in Ghostty.
- After the hook, all fields are interpolated; any still-unresolved `${var}` → abort.
  Spin-up is all-or-nothing.

Example — a worktree-producing hook:

```bash
#!/bin/sh
# new-worktree.sh <branch> <base>
git worktree add ".wt/$1" -b "$1" "$2" >&2
echo "worktree=$PWD/.wt/$1"
```

```yaml
hooks:
  pre: ./scripts/new-worktree.sh "${branch}" "${base}"
cwd: "${worktree}"
```

---

## 5. State

### 5.1 Storage

SQLite database at `~/.config/ghosttbusterr/state` (no extension; WAL sidecars
`state-wal`/`state-shm` appear alongside). `PRAGMA journal_mode = WAL`,
`PRAGMA foreign_keys = ON`. Schema versioning via `PRAGMA user_version` migrations.

Transactions make registration atomic (session + terminals + labels in one commit), and WAL
serializes concurrent `gtb` processes (e.g. two hooks spinning sessions simultaneously).

### 5.2 Schema

Conventions: tables are PascalCase singular; columns are prefixed with the snake_case table
name; primary keys are ULIDs; every table carries `created_at`/`updated_at`; hard deletes
only (no soft-delete flags); indexes are named `idx__{table}__{columns}`.

```sql
CREATE TABLE Session (
  session_id                TEXT PRIMARY KEY,   -- ULID
  session_name              TEXT NOT NULL,
  session_ghostty_window_id TEXT NOT NULL,
  session_ghostty_tab_id    TEXT NOT NULL,
  session_workflow          TEXT,
  session_cwd               TEXT,
  session_params            TEXT NOT NULL DEFAULT '{}',   -- JSON object
  session_created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  session_updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX idx__session__session_name ON Session(session_name);
CREATE INDEX idx__session__session_ghostty_tab_id ON Session(session_ghostty_tab_id);

CREATE TABLE Terminal (
  terminal_id         TEXT PRIMARY KEY,   -- ULID
  terminal_session_id TEXT NOT NULL REFERENCES Session(session_id) ON DELETE CASCADE,
  terminal_ghostty_id TEXT NOT NULL,
  terminal_ordinal    INTEGER NOT NULL,   -- pane order within the tab
  terminal_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  terminal_updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX idx__terminal__terminal_ghostty_id ON Terminal(terminal_ghostty_id);

CREATE TABLE Label (
  label_id         TEXT PRIMARY KEY,   -- ULID
  label_session_id TEXT NOT NULL REFERENCES Session(session_id) ON DELETE CASCADE,
  label_key        TEXT NOT NULL,
  label_value      TEXT NOT NULL,
  label_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  label_updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE UNIQUE INDEX idx__label__label_session_id_label_key ON Label(label_session_id, label_key);
CREATE INDEX idx__label__label_key_label_value ON Label(label_key, label_value);
```

**Name uniqueness:** `session_name` is unique across the registry. Spinning up with a taken
name errors (suggesting `kill` or `--name`). Names of manually closed sessions free up
automatically via reconciliation (§5.3).

`session_ghostty_window_id` is load-bearing, not informational: AppleScript can only address
a tab as `tab id … of window id …`, and `switch` needs the window for `activate window`.

### 5.3 Reconciliation

The registry is a cache of claims; **Ghostty is the source of truth**. Every command begins
with a reconciliation pass:

1. Check Ghostty liveness via `System Events` — this check does **not** launch the app.
2. Ghostty not running → all sessions are definitionally dead: delete all rows.
3. Ghostty running → fetch all live tab IDs (and their parent window IDs, and terminal IDs)
   in one AppleScript call:
   - Rows whose `session_ghostty_tab_id` is absent → `DELETE` (cascades to `Terminal`,
     `Label`).
   - Rows whose tab is live but under a different window (tab dragged by hand) → refresh
     `session_ghostty_window_id`, bump `session_updated_at`.
   - `Terminal` rows whose ghostty ID is gone (pane closed by hand) → `DELETE`.
4. If anything was pruned, print `cleared N closed session(s)` to stderr.

There is no daemon, watcher, or `prune` command: cleanup is lazy and automatic. Manual tab
closure (Cmd+W, `exit`, quitting Ghostty, window restoration after relaunch with fresh IDs)
is always consistent by the next `gtb` invocation.

### 5.4 Window placement

`window` (workflow field) / `--window` (flag; flag wins) accepts:

- `front` (default) — the current front Ghostty window.
- `new` — a freshly created window.
- `<ghostty-window-id>` — a specific existing window (validated up front; unknown ID is an
  early error). Discoverable from `gtb ls --json` records, enabling "spawn into the window
  where session X lives".

Edge cases: Ghostty running with zero windows → behave as `new`. Ghostty not running →
launch via `open -a Ghostty`, wait briefly for readiness, error only if it never comes up.

### 5.5 Session mechanics

- **Tab title** = session name, set via `perform action "set_tab_title:<name>"`. Verified
  against Ghostty 1.3.1: a manually set tab title is pinned — escape-sequence titles from
  apps (vim, agents) change the surface title but not the tab title.
- **`run` delivery:** commands are *typed into an interactive shell* (`initial input` on the
  surface configuration; fallback: `input text` + `send key enter` after creation), not
  executed as the surface's replacement process. The pane therefore survives command exit —
  quitting vim or exiting an agent returns you to a shell. Mechanics verified at build time.
- **In-pane awareness:** every panel surface gets `GTB_SESSION=<session-name>` injected into
  its environment (via surface configuration `environment variables`), so any process inside
  a managed pane — a script, a hook, an agent — can discover which session it belongs to and
  compose further `gtb` calls.

---

## 6. JSON contracts

### 6.1 `gtb ls --json`

```json
[
  {
    "name": "review-feat-login",
    "window_id": "tab-group-90c0c0140",
    "tab_id": "tab-90ab40c00",
    "terminals": ["8E2111E9-…", "67EF5607-…"],
    "labels": { "role": "review", "branch": "feat/login" },
    "workflow": "review",
    "params": { "branch": "feat/login", "base": "main", "worktree": "/Users/alican/r/dasei/.wt/feat-login" },
    "cwd": "/Users/alican/r/dasei/.wt/feat-login",
    "selected": true,
    "created_at": "2026-07-17T15:02:11.123Z"
  }
]
```

### 6.2 `gtb up --json`

The single created session record, same shape as above (with `"selected"` omitted).

---

## 7. Environment & platform notes

- **macOS Automation consent:** the first time any given host process (terminal app, agent)
  runs `gtb` and it drives Ghostty via Apple Events, macOS shows a consent dialog for that
  host. This is a one-time OS-level prompt per host application, outside ghosttbusterr's
  control.
- **Ghostty version:** developed against Ghostty 1.3.x and its `Ghostty.sdef` AppleScript
  dictionary (`new tab`, `split`, `input text`, `send key`, `perform action`,
  `select tab`, `activate window`, `close tab`, `new window`; stable IDs on
  window/tab/terminal).

---

## 8. Non-goals (v1)

Deliberately excluded; all addable later without breaking contracts:

- `post` hooks; global (config-level) hooks; hook timeouts
- A `gtb windows` view; index on `session_ghostty_window_id`
- Per-pane focus/send commands against existing sessions
- Mouse events, split resizing beyond `equalize_splits`
- Any awareness of pi or other callers (adapters live outside this tool)
- `ls --all` / session history (hard deletes mean closed sessions are gone, by decision)
