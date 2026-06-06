use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, ArchiverError>;

#[derive(Debug, thiserror::Error)]
pub enum ArchiverError {
    #[error("source directory does not exist or is not a directory: {0}")]
    InvalidSource(PathBuf),

    #[error("archive path exists but is not a directory: {0}")]
    InvalidArchive(PathBuf),

    #[error("source directory and archive directory must not overlap: source={source_dir}, archive={archive}")]
    OverlappingPaths {
        source_dir: PathBuf,
        archive: PathBuf,
    },

    #[error("index database does not exist: {0}")]
    MissingIndex(PathBuf),

    #[error("failed to strip source prefix: path={path}, source={source_dir}")]
    StripPrefix { path: PathBuf, source_dir: PathBuf },

    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("walkdir error: {0}")]
    Walkdir(#[from] walkdir::Error),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("hash mismatch at {path}: expected {expected}, got {actual}")]
    HashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },

    #[error("{0}")]
    Other(String),
}

pub(crate) trait IoContext<T> {
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| ArchiverError::Io {
            path: path.into(),
            source,
        })
    }
}
