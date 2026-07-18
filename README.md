# poltergeist

A session manager for the [Ghostty](https://ghostty.org) terminal on macOS.

A poltergeist is a ghost that moves objects around — this one moves windows
and panes. `geist` turns declarative *workflows* into live Ghostty state —
tabs, split panes, running commands, titles — and keeps a registry of
everything it created, so sessions can be listed, switched to, and killed.

## Install

```sh
cargo install --path .
```

This installs two equivalent binaries: `poltergeist` and the short alias
`geist`. Examples below use `geist`.

> **First run:** the first time `geist` drives Ghostty, macOS shows an
> Automation consent dialog for the host application (your terminal, agent,
> etc.). That's a one-time OS prompt per host — approve it once and `geist`
> works unattended from then on.

## Commands

### `geist up` — spin up a session

Ad-hoc, straight from flags — one panel per command after `--`:

```sh
geist up --name dev -- vim . lazygit          # two panes, side by side
geist up --direction horizontal -- top htop   # stacked instead
geist up                                      # a single plain shell pane
```

Or from a named workflow defined in a config file (see
[Workflows](#workflows)):

```sh
geist up review --param branch=feat/login
```

Combining a workflow with `--` commands is an error. Commands are *typed into
an interactive shell*, so quitting vim or exiting an agent returns you to a
shell — the pane survives.

| Flag | Meaning |
|---|---|
| `--name N` | Session name. Default: workflow name, else basename of the invocation cwd. |
| `--cwd DIR` | Working directory inherited by all panels. Default: invocation cwd. |
| `--direction vertical\|horizontal` | Split order for ad-hoc panels. `vertical` = side by side (like `:vsplit`), `horizontal` = stacked (like `:split`). Default `vertical`. |
| `--label k=v` | Attach a label (queryable metadata). Repeatable; overrides workflow labels on key conflict. |
| `--window TARGET` | `front` (default), `new`, or a Ghostty window ID (from `geist ls --json`). |
| `--pre CMD` | Pre-spin-up hook, same contract as workflow hooks (below). |
| `--param k=v` | Supply a workflow param. Repeatable; workflow mode only. |
| `--json` | Print the created session's full record as JSON. |

Spin-up is all-or-nothing: params, hooks, and `${var}` interpolation are
resolved *before* any Ghostty mutation, and a failure rolls the tab back.

### `geist ls` — list sessions

```sh
geist ls
geist ls --label role=review --label branch=feat/login   # AND semantics
geist ls --json                                          # full records for machines
```

```
  NAME                LABELS                           CWD                    AGE
→ review-feat-login   role=review,branch=feat/login    ~/r/dasei/.wt/feat…   15m
  dev                 project=dasei                    ~/r/dasei             2h
```

`→` marks the session whose tab is currently selected in Ghostty. Human
tables are for humans; scripts should always use `--json`.

### `geist switch` — focus a session

```sh
geist switch review-feat-login
geist switch rev          # unambiguous prefixes work
geist switch              # interactive picker
```

Names resolve exact → unique prefix → error listing candidates. With no
argument: `fzf` if it's on PATH, else a numbered list — but only on a TTY.
From scripts (non-TTY stdout) the picker never fires; pass a name.

### `geist kill` — close a session

```sh
geist kill review-feat-login
```

Closes the session's tab and removes it from the registry. Same name
resolution as `switch`; no confirmation prompt (prompts break scriptability).

### Exit codes & output rules

- `0` success · `1` runtime error · `2` usage error
- Errors and cleanup notices (`cleared N closed session(s)`) go to stderr.
- When stdout is not a TTY, `geist` is fully non-interactive: no pickers, no
  prompts.

## Workflows

A workflow is a named, parameterized, declarative session description:

```yaml
# .poltergeist.yml (project) or ~/.config/poltergeist/config.yml (global)
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
    cwd: "${worktree}"                     # session cwd; panels inherit
    layout:
      direction: vertical                  # side by side
      panels:
        - run: pi
          active: true                  # focused after spin-up (at most one)
        - layout:
            direction: horizontal          # stacked
            panels:
              - run: vim .
              - run: lazygit
```

Project config (`.poltergeist.yml`, discovered by walking up from the
invocation cwd) is checked first and shadows the global config on name
conflict.

**Layout** is a recursive tree: a `panels` item is either a leaf (`run`,
optional `cwd`, `env: [KEY=VALUE]`, and `active: true`) or a nested `layout`.
Panel order expresses placement (first = left/top); Ghostty equalizes split sizes
after creation. At most one panel per workflow may be `active: true`; it's
focused after spin-up (default: the last-created split).

**Params** are declared in the workflow and supplied by, in precedence order:
`--param k=v` → environment variable `$k` → YAML default → error if
`required`. `${name}` interpolation works in every string field; `${cwd}`
(invocation directory) and `${session}` (resolved session name) are always
available. Undeclared params are an error — except keys emitted by the
pre-hook, which enter dynamically.

**Hooks** — `hooks.pre` runs before any Ghostty mutation via `$SHELL -lc` in
the invocation cwd. stdout is protocol (each `KEY=VALUE` line becomes a
param, overriding everything else); stderr passes through for progress;
non-zero exit aborts the spin-up:

```bash
#!/bin/sh
# new-worktree.sh <branch> <base>
git worktree add ".wt/$1" -b "$1" "$2" >&2
echo "worktree=$PWD/.wt/$1"
```

**In-pane awareness:** every panel gets `GEIST_SESSION=<session-name>` in its
environment, so anything running inside a managed pane can discover its
session and compose further `geist` calls.

## How it works (briefly)

- Single static Rust binary, no runtime dependencies.
- All Ghostty manipulation goes through Ghostty's AppleScript API via
  `osascript` — no private interfaces.
- State lives in a SQLite registry at `~/.config/poltergeist/state`
  (override the whole directory with `GEIST_HOME`). **Ghostty is the source
  of truth**; the registry is a cache of claims reconciled against live
  Ghostty state on every command — closing a tab by hand is automatically
  consistent by the next `geist` invocation. There is no daemon.

## Development

```sh
cargo test                   # unit + fake-bridge tests
GEIST_INTEGRATION=1 cargo test --test live -- --ignored   # against a live Ghostty
```

See `spec.md` for the behavior contract and `tdd.spec.md` for the
implementation design.
