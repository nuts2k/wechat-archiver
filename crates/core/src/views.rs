use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{IoContext, Result};
use crate::report::archive_report;

#[derive(Debug, Clone)]
pub struct ViewsConfig {
    pub archive_dir: PathBuf,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewsSummary {
    pub archive_dir: PathBuf,
    pub views_dir: PathBuf,
    pub dry_run: bool,
    pub scanned_records: u64,
    pub planned_links: u64,
    pub created_links: u64,
    pub existing_links: u64,
    pub skipped_records: u64,
    pub failed_links: u64,
    pub links: Vec<ViewLink>,
    pub failures: Vec<ViewFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewLink {
    pub media_item_id: i64,
    pub view_kind: String,
    pub view_path: PathBuf,
    pub object_path: PathBuf,
    pub link_target: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewFailure {
    pub media_item_id: Option<i64>,
    pub source_path: Option<String>,
    pub archive_path: Option<String>,
    pub view_path: Option<PathBuf>,
    pub error: String,
}

pub fn generate_views(config: ViewsConfig) -> Result<ViewsSummary> {
    let report = archive_report(&config.archive_dir)?;
    let views_dir = config.archive_dir.join("views");
    let mut summary = ViewsSummary {
        archive_dir: config.archive_dir.clone(),
        views_dir,
        dry_run: config.dry_run,
        scanned_records: report.records.len() as u64,
        planned_links: 0,
        created_links: 0,
        existing_links: 0,
        skipped_records: 0,
        failed_links: 0,
        links: Vec::new(),
        failures: Vec::new(),
    };

    for record in &report.records {
        if record.verify_status != "ok" {
            continue;
        }

        let Some(archive_path) = record.archive_path.as_deref() else {
            summary.skipped_records += 1;
            summary.failures.push(ViewFailure {
                media_item_id: Some(record.id),
                source_path: Some(record.source_path.clone()),
                archive_path: None,
                view_path: None,
                error: "missing_archive_path".to_string(),
            });
            continue;
        };
        if !is_safe_object_path(archive_path) {
            summary.skipped_records += 1;
            summary.failures.push(ViewFailure {
                media_item_id: Some(record.id),
                source_path: Some(record.source_path.clone()),
                archive_path: Some(archive_path.to_string()),
                view_path: None,
                error: "invalid_archive_path".to_string(),
            });
            continue;
        }

        let object_rel_path = PathBuf::from(archive_path);
        let object_path = config.archive_dir.join(&object_rel_path);
        if !object_path.is_file() {
            summary.skipped_records += 1;
            summary.failures.push(ViewFailure {
                media_item_id: Some(record.id),
                source_path: Some(record.source_path.clone()),
                archive_path: Some(archive_path.to_string()),
                view_path: None,
                error: "missing_archive_object".to_string(),
            });
            continue;
        }

        let leaf = view_leaf_name(record.id, &record.source_relative_path, archive_path);
        let view_specs = [
            (
                "by-type",
                PathBuf::from("views")
                    .join("by-type")
                    .join(safe_segment(&record.media_type))
                    .join(&leaf),
            ),
            (
                "by-year",
                PathBuf::from("views")
                    .join("by-year")
                    .join(year_bucket(record.message_create_time))
                    .join(&leaf),
            ),
            (
                "by-chat",
                PathBuf::from("views")
                    .join("by-chat")
                    .join(chat_bucket(record.message_talker.as_deref()))
                    .join(&leaf),
            ),
        ];

        for (view_kind, view_rel_path) in view_specs {
            let link_target = relative_link_target(&view_rel_path, &object_rel_path);
            let link = ViewLink {
                media_item_id: record.id,
                view_kind: view_kind.to_string(),
                view_path: view_rel_path.clone(),
                object_path: object_rel_path.clone(),
                link_target: link_target.clone(),
            };
            summary.planned_links += 1;
            if config.dry_run {
                summary.links.push(link);
                continue;
            }

            match ensure_view_link(&config.archive_dir, &view_rel_path, &link_target) {
                Ok(ViewLinkState::Created) => {
                    summary.created_links += 1;
                    summary.links.push(link);
                }
                Ok(ViewLinkState::Existing) => {
                    summary.existing_links += 1;
                    summary.links.push(link);
                }
                Err(error) => {
                    summary.failed_links += 1;
                    summary.failures.push(ViewFailure {
                        media_item_id: Some(record.id),
                        source_path: Some(record.source_path.clone()),
                        archive_path: Some(archive_path.to_string()),
                        view_path: Some(view_rel_path),
                        error,
                    });
                }
            }
        }
    }

    Ok(summary)
}

enum ViewLinkState {
    Created,
    Existing,
}

fn ensure_view_link(
    archive_dir: &Path,
    view_rel_path: &Path,
    link_target: &Path,
) -> std::result::Result<ViewLinkState, String> {
    if !is_safe_relative_path(view_rel_path) {
        return Err("invalid_view_path".to_string());
    }

    let view_path = archive_dir.join(view_rel_path);
    if let Ok(metadata) = std::fs::symlink_metadata(&view_path) {
        if metadata.file_type().is_symlink()
            && std::fs::read_link(&view_path).ok().as_deref() == Some(link_target)
        {
            return Ok(ViewLinkState::Existing);
        }
        return Err("view_path_exists".to_string());
    }

    if let Some(parent) = view_path.parent() {
        std::fs::create_dir_all(parent)
            .with_path(parent)
            .map_err(|error| error.to_string())?;
    }
    create_symlink(link_target, &view_path)
        .with_path(&view_path)
        .map_err(|error| error.to_string())?;
    Ok(ViewLinkState::Created)
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "views only supports symlinks on unix targets",
    ))
}

fn relative_link_target(view_rel_path: &Path, object_rel_path: &Path) -> PathBuf {
    let parent_depth = view_rel_path
        .parent()
        .map(|parent| parent.components().count())
        .unwrap_or(0);
    let mut target = PathBuf::new();
    for _ in 0..parent_depth {
        target.push("..");
    }
    target.push(object_rel_path);
    target
}

fn view_leaf_name(id: i64, source_relative_path: &str, archive_path: &str) -> String {
    let source_leaf = Path::new(source_relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            Path::new(archive_path)
                .file_name()
                .and_then(|name| name.to_str())
        })
        .unwrap_or("item");
    format!("{id:012}-{}", safe_segment(source_leaf))
}

fn chat_bucket(message_talker: Option<&str>) -> String {
    message_talker
        .filter(|talker| !talker.trim().is_empty())
        .map(safe_segment)
        .unwrap_or_else(|| "unknown".to_string())
}

fn year_bucket(message_create_time: Option<i64>) -> String {
    let Some(timestamp) = message_create_time else {
        return "unknown".to_string();
    };
    if timestamp <= 0 {
        return "unknown".to_string();
    }
    let seconds = if timestamp > 10_000_000_000 {
        timestamp / 1000
    } else {
        timestamp
    };
    year_from_unix_seconds(seconds)
        .map(|year| year.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn year_from_unix_seconds(seconds: i64) -> Option<i32> {
    let days = seconds.checked_div_euclid(86_400)?;
    let z = days.checked_add(719_468)?;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    i32::try_from(year).ok()
}

fn safe_segment(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch == '/' || ch == '\\' || ch == ':' || ch.is_control() {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim_matches([' ', '.']).trim();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized.chars().take(120).collect()
    }
}

fn is_safe_relative_path(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    let mut components = path.components().peekable();
    !path.is_absolute()
        && components.peek().is_some()
        && components.all(|component| matches!(component, Component::Normal(_)))
}

fn is_safe_object_path(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    is_safe_relative_path(path)
        && path
            .components()
            .next()
            .and_then(|component| match component {
                Component::Normal(value) => Some(value),
                _ => None,
            })
            == Some(std::ffi::OsStr::new("objects"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::object_rel_path;
    use crate::hash::sha256_bytes;
    use crate::index::{insert_record, open_index, MediaRecord};

    fn record(
        source_path: &str,
        media_type: &str,
        message_talker: Option<&str>,
        message_create_time: Option<i64>,
        archive_path: Option<String>,
        sha256: Option<String>,
    ) -> MediaRecord {
        MediaRecord {
            source_path: source_path.to_string(),
            source_relative_path: Path::new(source_path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            source_kind: "direct_image".to_string(),
            media_type: media_type.to_string(),
            message_talker: message_talker.map(str::to_string),
            message_sender: None,
            message_local_id: Some(42),
            message_create_time,
            decoder: None,
            archive_path,
            sha256,
            size_bytes: Some(4),
            extension: Some("jpg".to_string()),
            decrypt_status: "not_needed".to_string(),
            verify_status: "ok".to_string(),
            error: None,
            timestamp_epoch_ms: 1_700_000_000_000,
        }
    }

    fn write_object(archive_dir: &Path, bytes: &[u8], extension: &str) -> (String, String) {
        let (sha256, _) = sha256_bytes(bytes);
        let rel_path = object_rel_path(&sha256, extension);
        let full_path = archive_dir.join(&rel_path);
        std::fs::create_dir_all(full_path.parent().unwrap()).unwrap();
        std::fs::write(&full_path, bytes).unwrap();
        (rel_path.to_string_lossy().to_string(), sha256)
    }

    #[test]
    fn generate_views_dry_run_plans_links_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let conn = open_index(archive).unwrap();
        let (archive_path, sha256) = write_object(archive, b"good", "jpg");
        insert_record(
            &conn,
            &record(
                "/tmp/source/image.jpg",
                "image",
                Some("chat/a"),
                Some(1_700_000_000),
                Some(archive_path),
                Some(sha256),
            ),
        )
        .unwrap();
        drop(conn);

        let summary = generate_views(ViewsConfig {
            archive_dir: archive.to_path_buf(),
            dry_run: true,
        })
        .unwrap();

        assert!(summary.dry_run);
        assert_eq!(summary.scanned_records, 1);
        assert_eq!(summary.planned_links, 3);
        assert_eq!(summary.created_links, 0);
        assert_eq!(summary.skipped_records, 0);
        assert!(!archive.join("views").exists());
        let paths = summary
            .links
            .iter()
            .map(|link| link.view_path.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(paths.iter().any(|path| path.contains("by-type/image")));
        assert!(paths.iter().any(|path| path.contains("by-year/2023")));
        assert!(paths.iter().any(|path| path.contains("by-chat/chat_a")));
    }

    #[cfg(unix)]
    #[test]
    fn generate_views_writes_relative_symlinks_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let conn = open_index(archive).unwrap();
        let (archive_path, sha256) = write_object(archive, b"good", "jpg");
        insert_record(
            &conn,
            &record(
                "/tmp/source/image.jpg",
                "image",
                None,
                None,
                Some(archive_path),
                Some(sha256),
            ),
        )
        .unwrap();
        drop(conn);

        let first = generate_views(ViewsConfig {
            archive_dir: archive.to_path_buf(),
            dry_run: false,
        })
        .unwrap();
        let second = generate_views(ViewsConfig {
            archive_dir: archive.to_path_buf(),
            dry_run: false,
        })
        .unwrap();

        assert_eq!(first.created_links, 3);
        assert_eq!(first.failed_links, 0);
        assert_eq!(second.created_links, 0);
        assert_eq!(second.existing_links, 3);
        let link = first
            .links
            .iter()
            .find(|link| link.view_kind == "by-type")
            .unwrap();
        let link_path = archive.join(&link.view_path);
        assert!(std::fs::symlink_metadata(&link_path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_link(&link_path).unwrap(), link.link_target);
    }

    #[test]
    fn generate_views_skips_invalid_or_missing_objects() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path();
        let conn = open_index(archive).unwrap();
        insert_record(
            &conn,
            &record(
                "/tmp/source/invalid.jpg",
                "image",
                None,
                None,
                Some("../outside.jpg".to_string()),
                Some("abc123".to_string()),
            ),
        )
        .unwrap();
        insert_record(
            &conn,
            &record(
                "/tmp/source/not-object.jpg",
                "image",
                None,
                None,
                Some("index.sqlite".to_string()),
                Some("abc123".to_string()),
            ),
        )
        .unwrap();
        insert_record(
            &conn,
            &record(
                "/tmp/source/missing.jpg",
                "image",
                None,
                None,
                Some("objects/sha256/ab/missing.jpg".to_string()),
                Some("missing".to_string()),
            ),
        )
        .unwrap();
        drop(conn);

        let summary = generate_views(ViewsConfig {
            archive_dir: archive.to_path_buf(),
            dry_run: true,
        })
        .unwrap();

        assert_eq!(summary.planned_links, 0);
        assert_eq!(summary.skipped_records, 3);
        let errors = summary
            .failures
            .iter()
            .map(|failure| failure.error.as_str())
            .collect::<Vec<_>>();
        assert!(errors.contains(&"invalid_archive_path"));
        assert_eq!(
            errors
                .iter()
                .filter(|error| **error == "invalid_archive_path")
                .count(),
            2
        );
        assert!(errors.contains(&"missing_archive_object"));
    }

    #[test]
    fn year_bucket_handles_seconds_and_milliseconds() {
        assert_eq!(year_bucket(Some(1_700_000_000)), "2023");
        assert_eq!(year_bucket(Some(1_700_000_000_000)), "2023");
        assert_eq!(year_bucket(None), "unknown");
    }
}
