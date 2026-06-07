use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{ArchiverError, IoContext, Result};

#[derive(Debug, Clone)]
pub struct ArchiveConfig {
    pub source_dir: PathBuf,
    pub archive_dir: PathBuf,
    pub dry_run: bool,
    pub dat_options: DatDecodeOptions,
    pub explain_unsupported: bool,
}

#[derive(Debug, Clone)]
pub struct DatDecodeOptions {
    pub image_aes_key: Option<Vec<u8>>,
    pub image_xor_key: u8,
    pub wxgf_mode: WxgfMode,
    pub wxgf_ffmpeg_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WxgfMode {
    /// 不处理微信 wxgf 私有图片格式，保持旧行为。
    Off,
    /// 归档解密后的 wxgf 原始内容。
    Raw,
    /// 提取 wxgf 内 HEVC 分片，并调用 ffmpeg 转出首帧 JPG。
    Jpg,
    /// 提取 wxgf 内 HEVC 分片，并调用 ffmpeg 封装成 MP4。
    Mp4,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub source_dir: PathBuf,
    pub archive_dir: PathBuf,
    pub dry_run: bool,
    pub dat_options: DatDecodeOptions,
    pub explain_unsupported: bool,
}

impl Default for DatDecodeOptions {
    fn default() -> Self {
        Self {
            image_aes_key: None,
            image_xor_key: 0x88,
            wxgf_mode: WxgfMode::Off,
            wxgf_ffmpeg_path: None,
        }
    }
}

impl ArchiveConfig {
    pub fn resolve(&self) -> Result<ResolvedConfig> {
        let source_dir = normalize_existing_dir(&self.source_dir)?;
        let archive_dir = normalize_target_dir(&self.archive_dir)?;

        if source_dir == archive_dir
            || archive_dir.starts_with(&source_dir)
            || source_dir.starts_with(&archive_dir)
        {
            return Err(ArchiverError::OverlappingPaths {
                source_dir,
                archive: archive_dir,
            });
        }

        Ok(ResolvedConfig {
            source_dir,
            archive_dir,
            dry_run: self.dry_run,
            dat_options: self.dat_options.clone(),
            explain_unsupported: self.explain_unsupported,
        })
    }
}

pub(crate) fn create_archive_dirs(archive_dir: &Path) -> Result<()> {
    for rel in ["objects/sha256", "manifests", "staging", "logs", "views"] {
        let path = archive_dir.join(rel);
        std::fs::create_dir_all(&path).with_path(path)?;
    }
    Ok(())
}

fn normalize_existing_dir(path: &Path) -> Result<PathBuf> {
    let abs = absolutize(path)?;
    let normalized = abs.canonicalize().with_path(&abs)?;
    if !normalized.is_dir() {
        return Err(ArchiverError::InvalidSource(normalized));
    }
    Ok(normalized)
}

fn normalize_target_dir(path: &Path) -> Result<PathBuf> {
    let abs = absolutize(path)?;
    if abs.exists() {
        let normalized = abs.canonicalize().with_path(&abs)?;
        if !normalized.is_dir() {
            return Err(ArchiverError::InvalidArchive(normalized));
        }
        return Ok(normalized);
    }

    let mut missing = Vec::<OsString>::new();
    let mut cursor = abs.as_path();
    while !cursor.exists() {
        let file_name = cursor
            .file_name()
            .ok_or_else(|| ArchiverError::InvalidArchive(abs.clone()))?;
        missing.push(file_name.to_os_string());
        cursor = cursor
            .parent()
            .ok_or_else(|| ArchiverError::InvalidArchive(abs.clone()))?;
    }

    let mut normalized = cursor.canonicalize().with_path(cursor)?;
    for part in missing.iter().rev() {
        normalized.push(part);
    }
    Ok(normalized)
}

fn absolutize(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        let cwd = env::current_dir().with_path(".")?;
        Ok(cwd.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_archive_inside_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat");
        std::fs::create_dir(&source).unwrap();
        let archive = source.join("archive");

        let config = ArchiveConfig {
            source_dir: source,
            archive_dir: archive,
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        };

        assert!(matches!(
            config.resolve(),
            Err(ArchiverError::OverlappingPaths { .. })
        ));
    }

    #[test]
    fn rejects_source_inside_existing_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("archive");
        let source = archive.join("wechat");
        std::fs::create_dir(&archive).unwrap();
        std::fs::create_dir(&source).unwrap();

        let config = ArchiveConfig {
            source_dir: source,
            archive_dir: archive,
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        };

        assert!(matches!(
            config.resolve(),
            Err(ArchiverError::OverlappingPaths { .. })
        ));
    }
}
