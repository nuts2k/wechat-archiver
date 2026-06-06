use std::path::Path;

use rusqlite::Connection;
use walkdir::WalkDir;

use crate::archive::{store_bytes, store_file, StoreOutcome};
use crate::config::{create_archive_dirs, ArchiveConfig, DatDecodeOptions};
use crate::error::{ArchiverError, Result};
use crate::hash::{sha256_bytes, sha256_file};
use crate::image::{decode_dat, direct_image_extension, is_dat_file, DatDecode};
use crate::index::{index_path, insert_record, open_index, MediaRecord};
use crate::manifest::ManifestWriter;
use crate::types::{now_epoch_ms, ExtractSummary, ManifestEvent, ScanAction};

pub fn extract_images(config: ArchiveConfig) -> Result<ExtractSummary> {
    let resolved = config.resolve()?;
    let run_id = format!("{}", now_epoch_ms());
    let mut summary = ExtractSummary::new(
        run_id.clone(),
        resolved.source_dir.clone(),
        resolved.archive_dir.clone(),
        resolved.dry_run,
    );

    let mut conn = None;
    let mut manifest = None;
    if !resolved.dry_run {
        create_archive_dirs(&resolved.archive_dir)?;
        let opened = open_index(&resolved.archive_dir)?;
        summary.index_path = Some(index_path(&resolved.archive_dir));
        let writer = ManifestWriter::create(&resolved.archive_dir, &run_id)?;
        summary.manifest_path = Some(writer.path().to_path_buf());
        conn = Some(opened);
        manifest = Some(writer);
    }

    for entry in WalkDir::new(&resolved.source_dir).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        summary.scanned_files += 1;

        let path = entry.path();
        if let Some(extension) = direct_image_extension(path) {
            summary.candidates += 1;
            let result = process_direct_image(
                path,
                extension,
                &resolved.source_dir,
                &resolved.archive_dir,
                &run_id,
                resolved.dry_run,
                conn.as_ref(),
                manifest.as_mut(),
            );
            apply_result(&mut summary, result)?;
        } else if is_dat_file(path) {
            summary.candidates += 1;
            let result = process_dat_image(
                path,
                &resolved.source_dir,
                &resolved.archive_dir,
                &run_id,
                resolved.dry_run,
                &resolved.dat_options,
                conn.as_ref(),
                manifest.as_mut(),
            );
            apply_result(&mut summary, result)?;
        }
    }

    if let Some(writer) = manifest.as_mut() {
        writer.flush()?;
    }

    Ok(summary)
}

pub(crate) fn apply_result(summary: &mut ExtractSummary, result: Result<ScanAction>) -> Result<()> {
    match result? {
        ScanAction::Archived => summary.archived += 1,
        ScanAction::AlreadyArchived => summary.already_archived += 1,
        ScanAction::Unsupported => summary.unsupported += 1,
        ScanAction::Failed => summary.failed += 1,
        ScanAction::WouldArchive => summary.would_archive += 1,
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_direct_image(
    path: &Path,
    extension: &str,
    source_root: &Path,
    archive_root: &Path,
    run_id: &str,
    dry_run: bool,
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
) -> Result<ScanAction> {
    let rel = relative_path(path, source_root)?;
    let (sha256, size_bytes) = sha256_file(path)?;

    let (action, archive_path, verify_status) = if dry_run {
        (ScanAction::WouldArchive, None, "not_run".to_string())
    } else {
        match store_file(archive_root, run_id, path, &sha256, extension)? {
            StoreOutcome::Stored { archive_path } => {
                (ScanAction::Archived, Some(archive_path), "ok".to_string())
            }
            StoreOutcome::AlreadyExists { archive_path } => (
                ScanAction::AlreadyArchived,
                Some(archive_path),
                "ok".to_string(),
            ),
        }
    };

    let event = build_event(
        run_id,
        path,
        &rel,
        "direct_image",
        action.clone(),
        archive_path,
        Some(sha256),
        Some(size_bytes),
        Some(extension.to_string()),
        "not_needed",
        &verify_status,
        None,
    );
    persist(conn, manifest, &event)?;
    Ok(action)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_dat_image(
    path: &Path,
    source_root: &Path,
    archive_root: &Path,
    run_id: &str,
    dry_run: bool,
    dat_options: &DatDecodeOptions,
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
) -> Result<ScanAction> {
    let rel = relative_path(path, source_root)?;

    match decode_dat(path, dat_options)? {
        DatDecode::Decoded {
            bytes,
            extension,
            decoder,
        } => {
            let (sha256, size_bytes) = sha256_bytes(&bytes);
            let (action, archive_path, verify_status) = if dry_run {
                (ScanAction::WouldArchive, None, "not_run".to_string())
            } else {
                match store_bytes(archive_root, run_id, &bytes, &sha256, extension)? {
                    StoreOutcome::Stored { archive_path } => {
                        (ScanAction::Archived, Some(archive_path), "ok".to_string())
                    }
                    StoreOutcome::AlreadyExists { archive_path } => (
                        ScanAction::AlreadyArchived,
                        Some(archive_path),
                        "ok".to_string(),
                    ),
                }
            };

            let event = build_event(
                run_id,
                path,
                &rel,
                decoder,
                action.clone(),
                archive_path,
                Some(sha256),
                Some(size_bytes),
                Some(extension.to_string()),
                "decoded",
                &verify_status,
                None,
            );
            persist(conn, manifest, &event)?;
            Ok(action)
        }
        DatDecode::Unsupported { reason } => {
            let event = build_event(
                run_id,
                path,
                &rel,
                "dat_image",
                ScanAction::Unsupported,
                None,
                None,
                None,
                None,
                "unsupported",
                "not_run",
                Some(reason.to_string()),
            );
            persist(conn, manifest, &event)?;
            Ok(ScanAction::Unsupported)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_event(
    run_id: &str,
    source_path: &Path,
    source_relative_path: &str,
    source_kind: &str,
    action: ScanAction,
    archive_path: Option<String>,
    sha256: Option<String>,
    size_bytes: Option<u64>,
    extension: Option<String>,
    decrypt_status: &str,
    verify_status: &str,
    error: Option<String>,
) -> ManifestEvent {
    ManifestEvent {
        event: "media_item".to_string(),
        run_id: run_id.to_string(),
        timestamp_epoch_ms: now_epoch_ms(),
        source_path: source_path.to_string_lossy().to_string(),
        source_relative_path: source_relative_path.to_string(),
        source_kind: source_kind.to_string(),
        media_type: "image".to_string(),
        action,
        archive_path,
        sha256,
        size_bytes,
        extension,
        decrypt_status: decrypt_status.to_string(),
        verify_status: verify_status.to_string(),
        error,
    }
}

pub(crate) fn persist(
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
    event: &ManifestEvent,
) -> Result<()> {
    if let Some(conn) = conn {
        insert_record(
            conn,
            &MediaRecord {
                source_path: event.source_path.clone(),
                source_relative_path: event.source_relative_path.clone(),
                source_kind: event.source_kind.clone(),
                media_type: event.media_type.clone(),
                archive_path: event.archive_path.clone(),
                sha256: event.sha256.clone(),
                size_bytes: event.size_bytes,
                extension: event.extension.clone(),
                decrypt_status: event.decrypt_status.clone(),
                verify_status: event.verify_status.clone(),
                error: event.error.clone(),
                timestamp_epoch_ms: event.timestamp_epoch_ms,
            },
        )?;
    }

    if let Some(manifest) = manifest {
        manifest.write_event(event)?;
    }

    Ok(())
}

fn relative_path(path: &Path, source_root: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(source_root)
        .map_err(|_| ArchiverError::StripPrefix {
            path: path.to_path_buf(),
            source_dir: source_root.to_path_buf(),
        })?;
    Ok(relative.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ArchiveConfig;
    use crate::status::archive_status;
    use crate::verify::verify_archive;

    #[test]
    fn extracts_direct_and_xor_images_without_touching_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(source.join("attach/hash/2026-01/Img")).unwrap();

        let direct_path = source.join("image.jpg");
        let direct_bytes = b"\xff\xd8\xffdirect\xff\xd9";
        std::fs::write(&direct_path, direct_bytes).unwrap();

        let dat_path = source.join("attach/hash/2026-01/Img/sample.dat");
        let decoded = b"\x89PNG\r\nsynthetic-png";
        let encrypted: Vec<u8> = decoded.iter().map(|byte| byte ^ 0x88).collect();
        std::fs::write(&dat_path, encrypted.clone()).unwrap();

        let summary = extract_images(ArchiveConfig {
            source_dir: source.clone(),
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 2);
        assert_eq!(summary.candidates, 2);
        assert_eq!(summary.archived, 2);
        assert_eq!(std::fs::read(&direct_path).unwrap(), direct_bytes);
        assert_eq!(std::fs::read(&dat_path).unwrap(), encrypted);
        assert!(archive.join("index.sqlite").exists());
        assert!(summary.manifest_path.unwrap().exists());

        let verify = verify_archive(&archive).unwrap();
        assert_eq!(verify.checked, 2);
        assert_eq!(verify.ok, 2);
        assert_eq!(verify.missing, 0);
        assert_eq!(verify.mismatched, 0);
    }

    #[test]
    fn dry_run_writes_nothing_to_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("image.jpg"), b"\xff\xd8\xffdirect\xff\xd9").unwrap();

        let summary = extract_images(ArchiveConfig {
            source_dir: source,
            archive_dir: archive.clone(),
            dry_run: true,
            dat_options: DatDecodeOptions::default(),
        })
        .unwrap();

        assert_eq!(summary.archived, 0);
        assert_eq!(summary.would_archive, 1);
        assert!(!archive.exists());
    }

    #[test]
    fn unsupported_dat_keeps_index_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("unknown.dat"), b"not-a-supported-dat").unwrap();

        for _ in 0..2 {
            let summary = extract_images(ArchiveConfig {
                source_dir: source.clone(),
                archive_dir: archive.clone(),
                dry_run: false,
                dat_options: DatDecodeOptions::default(),
            })
            .unwrap();

            assert_eq!(summary.candidates, 1);
            assert_eq!(summary.unsupported, 1);
        }

        let status = archive_status(&archive).unwrap();
        assert_eq!(status.total_records, 1);
        assert_eq!(status.unsupported_records, 1);
        assert_eq!(status.archived_records, 0);
    }
}
