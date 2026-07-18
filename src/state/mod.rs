pub mod schema;

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, ErrorCode, OptionalExtension, params};
use ulid::Ulid;

use crate::error::{Error, Result};

/// The registry: a cache of claims reconciled against Ghostty, which is the
/// source of truth (spec §5).
pub struct StateStore {
    conn: Connection,
}

/// Everything needed to register a session in one transaction.
pub struct NewSession<'a> {
    pub name: &'a str,
    pub window_id: &'a str,
    pub tab_id: &'a str,
    pub workflow: Option<&'a str>,
    pub cwd: Option<&'a str>,
    /// JSON object of resolved params.
    pub params: &'a str,
    /// Ghostty terminal ids in pane order (ordinal = index).
    pub terminals: &'a [String],
    pub labels: &'a BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub name: String,
    pub window_id: String,
    pub tab_id: String,
    pub workflow: Option<String>,
    pub cwd: Option<String>,
    /// Raw JSON object text.
    pub params: String,
    pub created_at: String,
    pub updated_at: String,
    /// Ghostty terminal ids ordered by ordinal.
    pub terminals: Vec<String>,
    pub labels: BTreeMap<String, String>,
}

impl StateStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Insert session + terminals + labels atomically. A taken name maps the
    /// UNIQUE constraint to `Error::NameTaken` (TDD §6).
    pub fn register(&self, s: &NewSession) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let session_id = Ulid::new().to_string();
        tx.execute(
            "INSERT INTO Session (session_id, session_name, session_ghostty_window_id, \
                 session_ghostty_tab_id, session_workflow, session_cwd, session_params) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![session_id, s.name, s.window_id, s.tab_id, s.workflow, s.cwd, s.params],
        )
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(ref f, _) if f.code == ErrorCode::ConstraintViolation => {
                Error::NameTaken(s.name.to_string())
            }
            e => Error::Sqlite(e),
        })?;
        for (ordinal, ghostty_id) in s.terminals.iter().enumerate() {
            tx.execute(
                "INSERT INTO Terminal (terminal_id, terminal_session_id, terminal_ghostty_id, terminal_ordinal) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![Ulid::new().to_string(), session_id, ghostty_id, ordinal as i64],
            )?;
        }
        for (key, value) in s.labels {
            tx.execute(
                "INSERT INTO Label (label_id, label_session_id, label_key, label_value) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![Ulid::new().to_string(), session_id, key, value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// All registered sessions with terminals and labels, oldest first.
    pub fn live_sessions(&self) -> Result<Vec<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, session_name, session_ghostty_window_id, session_ghostty_tab_id, \
                    session_workflow, session_cwd, session_params, session_created_at, session_updated_at \
             FROM Session ORDER BY session_created_at, session_name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, String>(8)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut sessions = Vec::with_capacity(rows.len());
        for (id, name, window_id, tab_id, workflow, cwd, params, created_at, updated_at) in rows {
            sessions.push(SessionRow {
                terminals: self.terminals_of(&id)?,
                labels: self.labels_of(&id)?,
                id,
                name,
                window_id,
                tab_id,
                workflow,
                cwd,
                params,
                created_at,
                updated_at,
            });
        }
        Ok(sessions)
    }

    fn terminals_of(&self, session_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT terminal_ghostty_id FROM Terminal \
             WHERE terminal_session_id = ?1 ORDER BY terminal_ordinal",
        )?;
        let ids = stmt
            .query_map(params![session_id], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(ids)
    }

    fn labels_of(&self, session_id: &str) -> Result<BTreeMap<String, String>> {
        let mut stmt = self.conn.prepare(
            "SELECT label_key, label_value FROM Label \
             WHERE label_session_id = ?1 ORDER BY label_key",
        )?;
        let labels = stmt
            .query_map(params![session_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<BTreeMap<String, String>>>()?;
        Ok(labels)
    }

    pub fn find_exact(&self, name: &str) -> Result<Option<SessionRow>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT session_id FROM Session WHERE session_name = ?1",
                params![name],
                |r| r.get(0),
            )
            .optional()?;
        match id {
            None => Ok(None),
            Some(id) => Ok(self.live_sessions()?.into_iter().find(|s| s.id == id)),
        }
    }

    /// Names starting with `prefix` — matched in Rust so user input never
    /// becomes a LIKE pattern.
    pub fn names_with_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT session_name FROM Session ORDER BY session_name")?;
        let names = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(names.into_iter().filter(|n| n.starts_with(prefix)).collect())
    }

    pub fn session_exists(&self, name: &str) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM Session WHERE session_name = ?1 LIMIT 1",
                params![name],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// AND semantics across pairs (spec §2.2).
    pub fn filter_by_labels(&self, kv: &[(String, String)]) -> Result<Vec<SessionRow>> {
        Ok(self
            .live_sessions()?
            .into_iter()
            .filter(|s| kv.iter().all(|(k, v)| s.labels.get(k) == Some(v)))
            .collect())
    }

    /// Delete sessions whose tab is gone; cascades to Terminal/Label.
    pub fn delete_dead_sessions(&self, live_tab_ids: &HashSet<&str>) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT session_ghostty_tab_id FROM Session")?;
        let tabs = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        for tab in tabs {
            if !live_tab_ids.contains(tab.as_str()) {
                tx.execute("DELETE FROM Session WHERE session_ghostty_tab_id = ?1", params![tab])?;
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Delete Terminal rows for panes closed by hand inside live tabs.
    pub fn delete_dead_terminals(&self, live_terminal_ids: &HashSet<&str>) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT terminal_ghostty_id FROM Terminal")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        for id in ids {
            if !live_terminal_ids.contains(id.as_str()) {
                tx.execute("DELETE FROM Terminal WHERE terminal_ghostty_id = ?1", params![id])?;
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Refresh stored parent window for tabs dragged between windows by hand;
    /// bumps `session_updated_at` (spec §5.3).
    pub fn refresh_windows(&self, moves: &[(String, String)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for (tab_id, window_id) in moves {
            tx.execute(
                "UPDATE Session SET session_ghostty_window_id = ?2, \
                    session_updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') \
                 WHERE session_ghostty_tab_id = ?1",
                params![tab_id, window_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_session(&self, name: &str) -> Result<()> {
        self.conn.execute("DELETE FROM Session WHERE session_name = ?1", params![name])?;
        Ok(())
    }

    /// Ghostty not running → every session is definitionally dead (spec §5.3).
    pub fn delete_all(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM Session", [])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StateStore {
        StateStore::open_memory().unwrap()
    }

    fn register(store: &StateStore, name: &str, tab: &str, labels: &BTreeMap<String, String>) {
        let terminals = vec!["t1".to_string(), "t2".to_string()];
        store
            .register(&NewSession {
                name,
                window_id: "w1",
                tab_id: tab,
                workflow: Some("review"),
                cwd: Some("/tmp"),
                params: "{\"branch\":\"main\"}",
                terminals: &terminals,
                labels,
            })
            .unwrap();
    }

    #[test]
    fn register_and_read_back() {
        let s = store();
        let labels = BTreeMap::from([("role".to_string(), "review".to_string())]);
        register(&s, "review-x", "tab1", &labels);
        let rows = s.live_sessions().unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.name, "review-x");
        assert_eq!(row.window_id, "w1");
        assert_eq!(row.tab_id, "tab1");
        assert_eq!(row.workflow.as_deref(), Some("review"));
        assert_eq!(row.cwd.as_deref(), Some("/tmp"));
        assert_eq!(row.params, "{\"branch\":\"main\"}");
        assert_eq!(row.terminals, vec!["t1", "t2"]);
        assert_eq!(row.labels, labels);
        assert!(!row.created_at.is_empty());
    }

    #[test]
    fn duplicate_name_maps_to_name_taken() {
        let s = store();
        let labels = BTreeMap::new();
        register(&s, "dev", "tab1", &labels);
        let err = {
            let terminals = vec!["t9".to_string()];
            s.register(&NewSession {
                name: "dev",
                window_id: "w1",
                tab_id: "tab2",
                workflow: None,
                cwd: None,
                params: "{}",
                terminals: &terminals,
                labels: &labels,
            })
            .unwrap_err()
        };
        assert!(matches!(err, Error::NameTaken(n) if n == "dev"));
    }

    #[test]
    fn delete_dead_sessions_cascades() {
        let s = store();
        let labels = BTreeMap::new();
        register(&s, "a", "tab-a", &labels);
        register(&s, "b", "tab-b", &labels);
        let live = HashSet::from(["tab-b"]);
        assert_eq!(s.delete_dead_sessions(&live).unwrap(), 1);
        let rows = s.live_sessions().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "b");
        assert_eq!(rows[0].terminals.len(), 2); // cascade kept live row intact
    }

    #[test]
    fn delete_dead_terminals() {
        let s = store();
        let labels = BTreeMap::new();
        register(&s, "a", "tab-a", &labels);
        let live = HashSet::from(["t1"]);
        assert_eq!(s.delete_dead_terminals(&live).unwrap(), 1);
        assert_eq!(s.live_sessions().unwrap()[0].terminals, vec!["t1"]);
    }

    #[test]
    fn refresh_windows_updates_parent() {
        let s = store();
        let labels = BTreeMap::new();
        register(&s, "a", "tab-a", &labels);
        s.refresh_windows(&[("tab-a".to_string(), "w2".to_string())]).unwrap();
        assert_eq!(s.live_sessions().unwrap()[0].window_id, "w2");
    }

    #[test]
    fn prefix_and_exact_lookup() {
        let s = store();
        let labels = BTreeMap::new();
        register(&s, "review-one", "tab1", &labels);
        register(&s, "review-two", "tab2", &labels);
        register(&s, "dev", "tab3", &labels);
        assert!(s.find_exact("review-one").unwrap().is_some());
        assert!(s.find_exact("review").unwrap().is_none());
        assert_eq!(s.names_with_prefix("review").unwrap().len(), 2);
        assert_eq!(s.names_with_prefix("dev").unwrap(), vec!["dev"]);
        assert!(s.session_exists("dev").unwrap());
        assert!(!s.session_exists("nope").unwrap());
    }

    #[test]
    fn label_filter_and_semantics() {
        let s = store();
        register(&s, "a", "tab1", &BTreeMap::from([
            ("role".to_string(), "review".to_string()),
            ("branch".to_string(), "feat".to_string()),
        ]));
        register(&s, "b", "tab2", &BTreeMap::from([
            ("role".to_string(), "review".to_string()),
        ]));
        let kv = vec![("role".to_string(), "review".to_string())];
        assert_eq!(s.filter_by_labels(&kv).unwrap().len(), 2);
        let kv = vec![
            ("role".to_string(), "review".to_string()),
            ("branch".to_string(), "feat".to_string()),
        ];
        let rows = s.filter_by_labels(&kv).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "a");
        let kv = vec![("role".to_string(), "dev".to_string())];
        assert!(s.filter_by_labels(&kv).unwrap().is_empty());
    }

    #[test]
    fn delete_all_and_delete_session() {
        let s = store();
        let labels = BTreeMap::new();
        register(&s, "a", "tab1", &labels);
        register(&s, "b", "tab2", &labels);
        s.delete_session("a").unwrap();
        assert_eq!(s.live_sessions().unwrap().len(), 1);
        assert_eq!(s.delete_all().unwrap(), 1);
        assert!(s.live_sessions().unwrap().is_empty());
    }

    #[test]
    fn file_backed_open_in_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("geist").join("state");
        {
            let s = StateStore::open(&path).unwrap();
            let labels = BTreeMap::new();
            register(&s, "persisted", "tab1", &labels);
        }
        let s = StateStore::open(&path).unwrap();
        assert_eq!(s.live_sessions().unwrap()[0].name, "persisted");
    }
}
