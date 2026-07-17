use crate::error::{Error, Result};
use crate::state::{SessionRow, StateStore};

/// Name resolution shared by `switch` and `kill` (spec §2.3): exact match →
/// unambiguous prefix → error listing candidates.
pub fn resolve_session(store: &StateStore, name_or_prefix: &str) -> Result<SessionRow> {
    if let Some(row) = store.find_exact(name_or_prefix)? {
        return Ok(row);
    }
    let candidates = store.names_with_prefix(name_or_prefix)?;
    match candidates.len() {
        0 => Err(Error::SessionNotFound(name_or_prefix.to_string())),
        1 => store
            .find_exact(&candidates[0])?
            .ok_or_else(|| Error::SessionNotFound(name_or_prefix.to_string())),
        _ => Err(Error::Ambiguous { name: name_or_prefix.to_string(), candidates }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use crate::state::NewSession;

    fn store_with(names: &[&str]) -> StateStore {
        let store = StateStore::open_memory().unwrap();
        for (i, name) in names.iter().enumerate() {
            let terminals = vec![format!("t{i}")];
            store
                .register(&NewSession {
                    name,
                    window_id: "w1",
                    tab_id: &format!("tab{i}"),
                    workflow: None,
                    cwd: None,
                    params: "{}",
                    terminals: &terminals,
                    labels: &BTreeMap::new(),
                })
                .unwrap();
        }
        store
    }

    #[test]
    fn exact_match_wins() {
        let store = store_with(&["dev", "dev-long"]);
        assert_eq!(resolve_session(&store, "dev").unwrap().name, "dev");
    }

    #[test]
    fn unique_prefix_matches() {
        let store = store_with(&["review-feat-login", "dev"]);
        assert_eq!(resolve_session(&store, "rev").unwrap().name, "review-feat-login");
    }

    #[test]
    fn ambiguous_prefix_lists_candidates() {
        let store = store_with(&["review-a", "review-b", "dev"]);
        match resolve_session(&store, "review").unwrap_err() {
            Error::Ambiguous { candidates, .. } => assert_eq!(candidates, vec!["review-a", "review-b"]),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn unknown_name_not_found() {
        let store = store_with(&["dev"]);
        assert!(matches!(
            resolve_session(&store, "nope").unwrap_err(),
            Error::SessionNotFound(_)
        ));
    }
}
