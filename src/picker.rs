use std::io::{IsTerminal, Write};
use std::process::{Command, Stdio};

use crate::error::{Error, Result};
use crate::state::SessionRow;

/// Interactive session picker for bare `gtb switch` (spec §2.3). Never fires
/// from scripts or agents: non-TTY stdout is an error.
pub fn pick(sessions: &[SessionRow]) -> Result<String> {
    if sessions.is_empty() {
        return Err(Error::Message("no managed sessions — gtb up <workflow>".into()));
    }
    if !std::io::stdout().is_terminal() {
        return Err(Error::Usage("pass a session name".into()));
    }
    if which::which("fzf").is_ok() {
        pick_fzf(sessions)
    } else {
        pick_numbered(sessions)
    }
}

fn line(s: &SessionRow) -> String {
    let labels = s
        .labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{}\t{}\t{}", s.name, labels, s.cwd.as_deref().unwrap_or(""))
}

fn pick_fzf(sessions: &[SessionRow]) -> Result<String> {
    let mut child = Command::new("fzf")
        .arg("--with-nth=1,2,3")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(sessions.iter().map(line).collect::<Vec<_>>().join("\n").as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(Error::Message("no session selected".into()));
    }
    let selected = String::from_utf8_lossy(&output.stdout);
    selected
        .split('\t')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::Message("no session selected".into()))
}

fn pick_numbered(sessions: &[SessionRow]) -> Result<String> {
    for (i, s) in sessions.iter().enumerate() {
        eprintln!("{:>3}) {}", i + 1, line(s).replace('\t', "  "));
    }
    eprint!("switch to [1-{}]: ", sessions.len());
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let n: usize = input
        .trim()
        .parse()
        .ok()
        .filter(|&n| (1..=sessions.len()).contains(&n))
        .ok_or_else(|| Error::Usage(format!("invalid selection '{}'", input.trim())))?;
    Ok(sessions[n - 1].name.clone())
}
