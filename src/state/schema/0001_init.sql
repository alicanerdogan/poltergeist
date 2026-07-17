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
