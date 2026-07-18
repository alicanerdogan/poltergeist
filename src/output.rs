use std::path::Path;

use serde::Serialize;

use crate::error::Result;
use crate::state::SessionRow;

const CWD_MAX_WIDTH: usize = 24;

#[derive(Serialize)]
struct SessionJson<'a> {
    name: &'a str,
    window_id: &'a str,
    tab_id: &'a str,
    terminals: &'a [String],
    labels: &'a std::collections::BTreeMap<String, String>,
    workflow: Option<&'a str>,
    params: serde_json::Value,
    cwd: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected: Option<bool>,
    created_at: &'a str,
}

fn to_json(row: &SessionRow, selected: Option<bool>) -> Result<SessionJson<'_>> {
    Ok(SessionJson {
        name: &row.name,
        window_id: &row.window_id,
        tab_id: &row.tab_id,
        terminals: &row.terminals,
        labels: &row.labels,
        workflow: row.workflow.as_deref(),
        params: serde_json::from_str(&row.params)?,
        cwd: row.cwd.as_deref(),
        selected,
        created_at: &row.created_at,
    })
}

/// `geist up` result: the created session's full record with `--json`
/// (spec §6.2 — `selected` omitted), else a one-line confirmation.
pub fn print_up(row: &SessionRow, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&to_json(row, None)?)?);
    } else {
        let n = row.terminals.len();
        println!("created session '{}' ({} panel{})", row.name, n, if n == 1 { "" } else { "s" });
    }
    Ok(())
}

/// `geist ls`: human table, or complete records with `--json` (spec §6.1).
/// `selected_tab` is the front window's selected tab (drives `→`/`selected`).
pub fn print_ls(sessions: &[SessionRow], selected_tab: Option<&str>, json: bool) -> Result<()> {
    if json {
        let records: Vec<SessionJson> = sessions
            .iter()
            .map(|row| to_json(row, Some(selected_tab == Some(row.tab_id.as_str()))))
            .collect::<Result<Vec<_>>>()?;
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(());
    }
    if sessions.is_empty() {
        println!("no managed sessions — geist up <workflow>");
        return Ok(());
    }

    let home = dirs::home_dir();
    let rows: Vec<(String, String, String, String, String)> = sessions
        .iter()
        .map(|s| {
            let marker = if selected_tab == Some(s.tab_id.as_str()) { "→" } else { " " };
            let labels = s
                .labels
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(",");
            let cwd = s
                .cwd
                .as_deref()
                .map(|c| truncate(&collapse_home(c, home.as_deref()), CWD_MAX_WIDTH))
                .unwrap_or_default();
            (marker.to_string(), s.name.clone(), labels, cwd, age(&s.created_at))
        })
        .collect();

    let w_name = width("NAME", rows.iter().map(|r| &r.1));
    let w_labels = width("LABELS", rows.iter().map(|r| &r.2));
    let w_cwd = width("CWD", rows.iter().map(|r| &r.3));

    println!("  {:<w_name$}  {:<w_labels$}  {:<w_cwd$}  AGE", "NAME", "LABELS", "CWD");
    for (marker, name, labels, cwd, age) in &rows {
        println!(
            "{marker} {:<w_name$}  {:<w_labels$}  {:<w_cwd$}  {age}",
            name, labels, cwd
        );
    }
    Ok(())
}

fn width<'a>(header: &str, cells: impl Iterator<Item = &'a String>) -> usize {
    cells.map(|c| c.chars().count()).max().unwrap_or(0).max(header.len())
}

fn collapse_home(path: &str, home: Option<&Path>) -> String {
    match home.and_then(|h| h.to_str()) {
        Some(h) if path == h => "~".to_string(),
        Some(h) if path.starts_with(&format!("{h}/")) => format!("~/{}", &path[h.len() + 1..]),
        _ => path.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max - 1).collect();
    format!("{head}…")
}

/// `15m`, `2h`, `3d` (spec §2.2).
fn age(created_at: &str) -> String {
    let ts: jiff::Timestamp = match created_at.parse() {
        Ok(ts) => ts,
        Err(_) => return "?".into(),
    };
    let secs = jiff::Timestamp::now().duration_since(ts).as_secs().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_buckets() {
        let now = jiff::Timestamp::now();
        let at = |secs_ago: i64| (now - jiff::Span::new().seconds(secs_ago)).to_string();
        assert_eq!(age(&at(5)), "5s");
        assert_eq!(age(&at(15 * 60)), "15m");
        assert_eq!(age(&at(2 * 3600)), "2h");
        assert_eq!(age(&at(3 * 86400)), "3d");
        assert_eq!(age("not a timestamp"), "?");
    }

    #[test]
    fn home_collapse() {
        let home = Path::new("/Users/x");
        assert_eq!(collapse_home("/Users/x", Some(home)), "~");
        assert_eq!(collapse_home("/Users/x/r/dasei", Some(home)), "~/r/dasei");
        assert_eq!(collapse_home("/other", Some(home)), "/other");
        assert_eq!(collapse_home("/Users/xy", Some(home)), "/Users/xy");
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("short", 24), "short");
        let long = "a".repeat(30);
        let got = truncate(&long, 24);
        assert_eq!(got.chars().count(), 24);
        assert!(got.ends_with('…'));
    }
}
