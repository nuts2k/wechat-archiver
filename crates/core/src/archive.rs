use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{ArchiverError, IoContext, Result};
use crate::hash::{sha256_file, write_all};

#[derive(Debug, Clone)]
pub(crate) enum StoreOutcome {
    Stored { archive_path: String },
    AlreadyExists { archive_path: String },
}

pub(crate) fn object_rel_path(sha256: &str, extension: &str) -> PathBuf {
    let prefix = sha256.get(0..2).unwrap_or("00");
    PathBuf::from("objects")
        .join("sha256")
        .join(prefix)
        .join(format!("{sha256}.{extension}"))
}

pub(crate) fn store_file(
    archive_dir: &Path,
    run_id: &str,
    source_path: &Path,
    sha256: &str,
    extension: &str,
) -> Result<StoreOutcome> {
    let rel_path = object_rel_path(sha256, extension);
    let final_path = archive_dir.join(&rel_path);
    if final_path.exists() {
        verify_existing(&final_path, sha256)?;
        return Ok(StoreOutcome::AlreadyExists {
            archive_path: rel_path.to_string_lossy().to_string(),
        });
    }

    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent).with_path(parent)?;
    }

    let staging_dir = archive_dir.join("staging").join(run_id);
    fs::create_dir_all(&staging_dir).with_path(&staging_dir)?;
    let temp_path = staging_dir.join(format!("{sha256}.{extension}.tmp"));

    fs::copy(source_path, &temp_path).with_path(&temp_path)?;
    verify_existing(&temp_path, sha256)?;
    fs::rename(&temp_path, &final_path).with_path(&final_path)?;

    Ok(StoreOutcome::Stored {
        archive_path: rel_path.to_string_lossy().to_string(),
    })
}

pub(crate) fn store_bytes(
    archive_dir: &Path,
    run_id: &str,
    bytes: &[u8],
    sha256: &str,
    extension: &str,
) -> Result<StoreOutcome> {
    let rel_path = object_rel_path(sha256, extension);
    let final_path = archive_dir.join(&rel_path);
    if final_path.exists() {
        verify_existing(&final_path, sha256)?;
        return Ok(StoreOutcome::AlreadyExists {
            archive_path: rel_path.to_string_lossy().to_string(),
        });
    }

    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent).with_path(parent)?;
    }

    let staging_dir = archive_dir.join("staging").join(run_id);
    fs::create_dir_all(&staging_dir).with_path(&staging_dir)?;
    let temp_path = staging_dir.join(format!("{sha256}.{extension}.tmp"));
    write_all(&temp_path, bytes)?;
    verify_existing(&temp_path, sha256)?;
    fs::rename(&temp_path, &final_path).with_path(&final_path)?;

    Ok(StoreOutcome::Stored {
        archive_path: rel_path.to_string_lossy().to_string(),
    })
}

fn verify_existing(path: &Path, expected: &str) -> Result<()> {
    let (actual, _) = sha256_file(path)?;
    if actual != expected {
        return Err(ArchiverError::HashMismatch {
            path: path.to_path_buf(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}
