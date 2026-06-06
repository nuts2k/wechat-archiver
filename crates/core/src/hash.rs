use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::{IoContext, Result};

pub(crate) fn sha256_file(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path).with_path(path)?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 1024 * 128];

    loop {
        let read = file.read(&mut buffer).with_path(path)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        hasher.update(&buffer[..read]);
    }

    Ok((hex::encode(hasher.finalize()), size))
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> (String, u64) {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    (hex::encode(hasher.finalize()), bytes.len() as u64)
}

pub(crate) fn write_all(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = File::create(path).with_path(path)?;
    file.write_all(bytes).with_path(path)?;
    file.sync_all().with_path(path)?;
    Ok(())
}
