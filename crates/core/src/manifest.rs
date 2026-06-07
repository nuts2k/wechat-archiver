use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::error::{IoContext, Result};
use crate::types::ManifestEvent;

pub(crate) struct ManifestWriter {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl ManifestWriter {
    pub(crate) fn create(archive_dir: &Path, run_id: &str, label: &str) -> Result<Self> {
        let path = archive_dir
            .join("manifests")
            .join(format!("{run_id}-{label}.jsonl"));
        let file = File::create(&path).with_path(&path)?;
        Ok(Self {
            path,
            writer: BufWriter::new(file),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn write_event(&mut self, event: &ManifestEvent) -> Result<()> {
        serde_json::to_writer(&mut self.writer, event)?;
        self.writer.write_all(b"\n").with_path(&self.path)?;
        Ok(())
    }

    pub(crate) fn flush(&mut self) -> Result<()> {
        self.writer.flush().with_path(&self.path)
    }
}
