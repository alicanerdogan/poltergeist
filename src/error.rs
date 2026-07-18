use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),
    #[error("workflow '{0}' not found")]
    WorkflowNotFound(String),
    #[error("missing required param '{0}'")]
    MissingParam(String),
    #[error("undeclared param '{0}'")]
    UndeclaredParam(String),
    #[error("unresolved variable '${{{0}}}'")]
    UnresolvedVar(String),
    #[error("hook failed (exit {code})")]
    HookFailed { code: i32, stderr: String },
    #[error("session '{0}' not found")]
    SessionNotFound(String),
    #[error("ambiguous session '{name}': matches {}", candidates.join(", "))]
    Ambiguous { name: String, candidates: Vec<String> },
    #[error("session name '{0}' already in use (kill it with `geist kill {0}` or pick another with --name)")]
    NameTaken(String),
    #[error("ghostty: {0}")]
    Ghostty(String),
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Process exit code: 2 for usage errors, 1 for everything else.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => 2,
            _ => 1,
        }
    }
}
