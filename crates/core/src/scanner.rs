use std::path::{Path, PathBuf};

use rusqlite::Connection;
use walkdir::WalkDir;

use crate::archive::{store_bytes, store_file, StoreOutcome};
use crate::config::{create_archive_dirs, ArchiveConfig, DatDecodeOptions};
use crate::error::{ArchiverError, Result};
use crate::hash::{sha256_bytes, sha256_file};
use crate::image::{decode_dat, direct_image_extension, is_dat_file, validate_dat, DatDecode};
use crate::index::{index_path, insert_record, open_index, MediaRecord};
use crate::manifest::ManifestWriter;
use crate::media::{direct_file_extension, direct_video_extension, direct_voice_extension};
use crate::types::{now_epoch_ms, ExtractSummary, ManifestEvent, ScanAction};

#[derive(Debug, Clone, Default)]
pub(crate) struct MessageSource {
    pub talker: Option<String>,
    pub sender: Option<String>,
    pub local_id: Option<i64>,
    pub create_time: Option<i64>,
}

pub fn extract_images(config: ArchiveConfig) -> Result<ExtractSummary> {
    let resolved = config.resolve()?;
    let mut run = ScanRun::new(&resolved, "extract-images")?;

    for entry in WalkDir::new(&resolved.source_dir).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        run.summary.scanned_files += 1;

        let path = entry.path();
        if let Some(extension) = direct_image_extension(path) {
            run.summary.candidates += 1;
            let result = process_direct_media(
                path,
                extension,
                &resolved.source_dir,
                &resolved.archive_dir,
                &run.run_id,
                resolved.dry_run,
                run.conn.as_ref(),
                run.manifest.as_mut(),
                "direct_image",
                "image",
            );
            apply_result(&mut run.summary, result)?;
        } else if is_dat_file(path) {
            run.summary.candidates += 1;
            let result = process_dat_image(
                path,
                &resolved.source_dir,
                &resolved.archive_dir,
                &run.run_id,
                "dat_image",
                resolved.dry_run,
                &resolved.dat_options,
                run.conn.as_ref(),
                run.manifest.as_mut(),
            );
            apply_result(&mut run.summary, result)?;
        }
    }

    run.finish()
}

pub fn extract_videos(config: ArchiveConfig) -> Result<ExtractSummary> {
    let resolved = config.resolve()?;
    let mut run = ScanRun::new(&resolved, "extract-videos")?;
    let video_sources = account_media_scan_sources(&resolved.source_dir, "video");

    for source in video_sources {
        for entry in WalkDir::new(&source.scan_dir).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            run.summary.scanned_files += 1;

            let path = entry.path();
            if let Some(extension) = direct_video_extension(path) {
                run.summary.candidates += 1;
                let result = process_direct_media(
                    path,
                    extension,
                    &source.relative_root,
                    &resolved.archive_dir,
                    &run.run_id,
                    resolved.dry_run,
                    run.conn.as_ref(),
                    run.manifest.as_mut(),
                    "direct_video",
                    "video",
                );
                apply_result(&mut run.summary, result)?;
            }
        }
    }

    run.finish()
}

#[derive(Debug, Clone)]
struct AccountMediaScanSource {
    scan_dir: PathBuf,
    relative_root: PathBuf,
}

pub fn extract_voices(config: ArchiveConfig) -> Result<ExtractSummary> {
    let resolved = config.resolve()?;
    let mut run = ScanRun::new(&resolved, "extract-voices")?;
    let voice_sources = voice_scan_sources(&resolved.source_dir);

    for source in voice_sources {
        for entry in WalkDir::new(&source.scan_dir).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            run.summary.scanned_files += 1;

            let path = entry.path();
            if let Some(extension) = direct_voice_extension(path) {
                run.summary.candidates += 1;
                let result = process_direct_media(
                    path,
                    extension,
                    &source.relative_root,
                    &resolved.archive_dir,
                    &run.run_id,
                    resolved.dry_run,
                    run.conn.as_ref(),
                    run.manifest.as_mut(),
                    "direct_voice",
                    "voice",
                );
                apply_result(&mut run.summary, result)?;
            }
        }
    }

    run.finish()
}

pub fn extract_files(config: ArchiveConfig) -> Result<ExtractSummary> {
    let resolved = config.resolve()?;
    let mut run = ScanRun::new(&resolved, "extract-files")?;
    let file_sources = account_media_scan_sources(&resolved.source_dir, "file");

    for source in file_sources {
        for entry in WalkDir::new(&source.scan_dir).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            run.summary.scanned_files += 1;

            let path = entry.path();
            if let Some(extension) = direct_file_extension(path) {
                run.summary.candidates += 1;
                let result = process_direct_media(
                    path,
                    &extension,
                    &source.relative_root,
                    &resolved.archive_dir,
                    &run.run_id,
                    resolved.dry_run,
                    run.conn.as_ref(),
                    run.manifest.as_mut(),
                    "direct_file",
                    "file",
                );
                apply_result(&mut run.summary, result)?;
            }
        }
    }

    run.finish()
}

fn account_media_scan_sources(
    source_dir: &Path,
    media_dir_name: &str,
) -> Vec<AccountMediaScanSource> {
    if let Some(sources) = account_media_scan_sources_for_names(source_dir, &[media_dir_name]) {
        if !sources.is_empty() {
            return sources;
        }
    }

    vec![AccountMediaScanSource {
        scan_dir: source_dir.to_path_buf(),
        relative_root: source_dir.to_path_buf(),
    }]
}

fn voice_scan_sources(source_dir: &Path) -> Vec<AccountMediaScanSource> {
    if let Some(sources) = account_media_scan_sources_for_names(source_dir, &["voice", "audio"]) {
        return sources;
    }

    vec![AccountMediaScanSource {
        scan_dir: source_dir.to_path_buf(),
        relative_root: source_dir.to_path_buf(),
    }]
}

fn account_media_scan_sources_for_names(
    source_dir: &Path,
    media_dir_names: &[&str],
) -> Option<Vec<AccountMediaScanSource>> {
    if let Some(account_dir) = account_dir_from_attach_dir(source_dir) {
        return Some(existing_account_media_sources(
            &account_dir,
            media_dir_names,
            &account_dir,
        ));
    }

    let msg_dir = source_dir.join("msg");
    if msg_dir.is_dir() {
        return Some(existing_account_media_sources(
            source_dir,
            media_dir_names,
            source_dir,
        ));
    }

    None
}

fn existing_account_media_sources(
    account_dir: &Path,
    media_dir_names: &[&str],
    relative_root: &Path,
) -> Vec<AccountMediaScanSource> {
    let mut sources = Vec::new();
    for media_dir_name in media_dir_names {
        let media_dir = account_dir.join("msg").join(media_dir_name);
        if media_dir.is_dir() {
            sources.push(AccountMediaScanSource {
                scan_dir: media_dir,
                relative_root: relative_root.to_path_buf(),
            });
        }
    }

    sources
}

fn account_dir_from_attach_dir(source_dir: &Path) -> Option<PathBuf> {
    let attach = source_dir.file_name()?.to_str()?;
    let msg_dir = source_dir.parent()?;
    let msg = msg_dir.file_name()?.to_str()?;
    if attach.eq_ignore_ascii_case("attach") && msg.eq_ignore_ascii_case("msg") {
        return msg_dir.parent().map(Path::to_path_buf);
    }
    None
}

struct ScanRun {
    run_id: String,
    summary: ExtractSummary,
    conn: Option<Connection>,
    manifest: Option<ManifestWriter>,
}

impl ScanRun {
    fn new(resolved: &crate::config::ResolvedConfig, manifest_label: &str) -> Result<Self> {
        let run_id = format!("{}", now_epoch_ms());
        let mut summary = ExtractSummary::new(
            run_id.clone(),
            resolved.source_dir.clone(),
            resolved.archive_dir.clone(),
            resolved.dry_run,
        );
        if resolved.explain_unsupported {
            summary.enable_unsupported_explanation();
        }

        let mut conn = None;
        let mut manifest = None;
        if !resolved.dry_run {
            create_archive_dirs(&resolved.archive_dir)?;
            let opened = open_index(&resolved.archive_dir)?;
            summary.index_path = Some(index_path(&resolved.archive_dir));
            let writer = ManifestWriter::create(&resolved.archive_dir, &run_id, manifest_label)?;
            summary.manifest_path = Some(writer.path().to_path_buf());
            conn = Some(opened);
            manifest = Some(writer);
        }

        Ok(Self {
            run_id,
            summary,
            conn,
            manifest,
        })
    }

    fn finish(mut self) -> Result<ExtractSummary> {
        if let Some(writer) = self.manifest.as_mut() {
            writer.flush()?;
        }
        self.summary.finish_unsupported_explanation();
        Ok(self.summary)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ScanOutcome {
    pub action: ScanAction,
    pub unsupported_reason: Option<String>,
    pub unsupported_sample: Option<String>,
}

impl ScanOutcome {
    pub(crate) fn new(action: ScanAction) -> Self {
        Self {
            action,
            unsupported_reason: None,
            unsupported_sample: None,
        }
    }

    fn unsupported(reason: impl Into<String>, sample: impl Into<String>) -> Self {
        Self {
            action: ScanAction::Unsupported,
            unsupported_reason: Some(reason.into()),
            unsupported_sample: Some(sample.into()),
        }
    }
}

pub(crate) fn apply_result(
    summary: &mut ExtractSummary,
    result: Result<ScanOutcome>,
) -> Result<()> {
    let result = result?;
    match result.action {
        ScanAction::Archived => summary.archived += 1,
        ScanAction::AlreadyArchived => summary.already_archived += 1,
        ScanAction::Unsupported => {
            summary.unsupported += 1;
            if let Some(reason) = result.unsupported_reason {
                summary.record_unsupported(reason, result.unsupported_sample);
            }
        }
        ScanAction::Failed => summary.failed += 1,
        ScanAction::WouldArchive => summary.would_archive += 1,
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_direct_media(
    path: &Path,
    extension: &str,
    source_root: &Path,
    archive_root: &Path,
    run_id: &str,
    dry_run: bool,
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
    source_kind: &str,
    media_type: &str,
) -> Result<ScanOutcome> {
    process_direct_media_with_message_source(
        path,
        extension,
        source_root,
        archive_root,
        run_id,
        dry_run,
        conn,
        manifest,
        source_kind,
        media_type,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_direct_media_with_message_source(
    path: &Path,
    extension: &str,
    source_root: &Path,
    archive_root: &Path,
    run_id: &str,
    dry_run: bool,
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
    source_kind: &str,
    media_type: &str,
    message_source: Option<&MessageSource>,
) -> Result<ScanOutcome> {
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
        source_kind,
        media_type,
        message_source,
        None,
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
    Ok(ScanOutcome::new(action))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_dat_image(
    path: &Path,
    source_root: &Path,
    archive_root: &Path,
    run_id: &str,
    source_kind: &str,
    dry_run: bool,
    dat_options: &DatDecodeOptions,
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
) -> Result<ScanOutcome> {
    process_dat_image_with_message_source(
        path,
        source_root,
        archive_root,
        run_id,
        source_kind,
        dry_run,
        dat_options,
        conn,
        manifest,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_dat_image_with_message_source(
    path: &Path,
    source_root: &Path,
    archive_root: &Path,
    run_id: &str,
    source_kind: &str,
    dry_run: bool,
    dat_options: &DatDecodeOptions,
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
    message_source: Option<&MessageSource>,
) -> Result<ScanOutcome> {
    let rel = relative_path(path, source_root)?;

    let decoded = if dry_run {
        validate_dat(path, dat_options)?
    } else {
        decode_dat(path, dat_options)?
    };

    match decoded {
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
                source_kind,
                "image",
                message_source,
                Some(decoder),
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
            Ok(ScanOutcome::new(action))
        }
        DatDecode::Validated { extension, decoder } => {
            let event = build_event(
                run_id,
                path,
                &rel,
                source_kind,
                "image",
                message_source,
                Some(decoder),
                ScanAction::WouldArchive,
                None,
                None,
                None,
                Some(extension.to_string()),
                "validated",
                "not_run",
                None,
            );
            persist(conn, manifest, &event)?;
            Ok(ScanOutcome::new(ScanAction::WouldArchive))
        }
        DatDecode::Unsupported { reason } => {
            let event = build_event(
                run_id,
                path,
                &rel,
                source_kind,
                "image",
                message_source,
                None,
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
            Ok(ScanOutcome::unsupported(reason, rel))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_event(
    run_id: &str,
    source_path: &Path,
    source_relative_path: &str,
    source_kind: &str,
    media_type: &str,
    message_source: Option<&MessageSource>,
    decoder: Option<&str>,
    action: ScanAction,
    archive_path: Option<String>,
    sha256: Option<String>,
    size_bytes: Option<u64>,
    extension: Option<String>,
    decrypt_status: &str,
    verify_status: &str,
    error: Option<String>,
) -> ManifestEvent {
    let message_source = message_source.cloned().unwrap_or_default();
    ManifestEvent {
        event: "media_item".to_string(),
        run_id: run_id.to_string(),
        timestamp_epoch_ms: now_epoch_ms(),
        source_path: source_path.to_string_lossy().to_string(),
        source_relative_path: source_relative_path.to_string(),
        source_kind: source_kind.to_string(),
        media_type: media_type.to_string(),
        message_talker: message_source.talker,
        message_sender: message_source.sender,
        message_local_id: message_source.local_id,
        message_create_time: message_source.create_time,
        decoder: decoder.map(str::to_string),
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
                message_talker: event.message_talker.clone(),
                message_sender: event.message_sender.clone(),
                message_local_id: event.message_local_id,
                message_create_time: event.message_create_time,
                decoder: event.decoder.clone(),
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
    use rusqlite::Connection;

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
            explain_unsupported: false,
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
    fn records_dat_source_kind_and_decoder_separately() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(source.join("attach/hash/2026-01/Img")).unwrap();

        let dat_path = source.join("attach/hash/2026-01/Img/sample.dat");
        let decoded = b"\x89PNG\r\nsynthetic-png";
        let encrypted: Vec<u8> = decoded.iter().map(|byte| byte ^ 0x88).collect();
        std::fs::write(&dat_path, encrypted).unwrap();

        let summary = extract_images(ArchiveConfig {
            source_dir: source,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (source_kind, decoder): (String, Option<String>) = conn
            .query_row(
                r#"
                SELECT source_kind, decoder
                FROM media_items
                WHERE source_relative_path = 'attach/hash/2026-01/Img/sample.dat'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(source_kind, "dat_image");
        assert_eq!(decoder.as_deref(), Some("legacy_xor"));

        let manifest_path = summary.manifest_path.unwrap();
        let manifest = std::fs::read_to_string(manifest_path).unwrap();
        let event = manifest
            .lines()
            .map(|line| serde_json::from_str::<ManifestEvent>(line).unwrap())
            .find(|event| event.source_relative_path == "attach/hash/2026-01/Img/sample.dat")
            .unwrap();
        assert_eq!(event.source_kind, "dat_image");
        assert_eq!(event.decoder.as_deref(), Some("legacy_xor"));
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
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.archived, 0);
        assert_eq!(summary.would_archive, 1);
        assert!(!archive.exists());
    }

    #[test]
    fn extracts_direct_videos_without_touching_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();

        let video_path = source.join("clip.MP4");
        let video_bytes = b"synthetic-video";
        std::fs::write(&video_path, video_bytes).unwrap();
        std::fs::write(source.join("not-video.txt"), b"ignored").unwrap();

        let summary = extract_videos(ArchiveConfig {
            source_dir: source.clone(),
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 2);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);
        assert_eq!(std::fs::read(&video_path).unwrap(), video_bytes);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (source_kind, media_type, extension): (String, String, String) = conn
            .query_row(
                r#"
                SELECT source_kind, media_type, extension
                FROM media_items
                WHERE source_relative_path = 'clip.MP4'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(source_kind, "direct_video");
        assert_eq!(media_type, "video");
        assert_eq!(extension, "mp4");

        let manifest_path = summary.manifest_path.unwrap();
        assert!(manifest_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-extract-videos.jsonl"));
        let manifest = std::fs::read_to_string(manifest_path).unwrap();
        let event = manifest
            .lines()
            .map(|line| serde_json::from_str::<ManifestEvent>(line).unwrap())
            .find(|event| event.source_relative_path == "clip.MP4")
            .unwrap();
        assert_eq!(event.source_kind, "direct_video");
        assert_eq!(event.media_type, "video");
        assert_eq!(event.extension.as_deref(), Some("mp4"));

        let verify = verify_archive(&archive).unwrap();
        assert_eq!(verify.checked, 1);
        assert_eq!(verify.ok, 1);
    }

    #[test]
    fn video_extract_from_attach_scans_sibling_msg_video() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("xwechat_files").join("wxid");
        let attach = account.join("msg").join("attach");
        let video_dir = account.join("msg").join("video").join("2026-02");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&attach).unwrap();
        std::fs::create_dir_all(&video_dir).unwrap();
        std::fs::write(attach.join("ignored.mp4"), b"attach-video-is-not-used").unwrap();
        std::fs::write(video_dir.join("clip.mp4"), b"real-video").unwrap();

        let summary = extract_videos(ArchiveConfig {
            source_dir: attach,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let relative_path: String = conn
            .query_row(
                "SELECT source_relative_path FROM media_items WHERE media_type = 'video'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(relative_path, "msg/video/2026-02/clip.mp4");
    }

    #[test]
    fn video_extract_from_account_scans_msg_video() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("xwechat_files").join("wxid");
        let video_dir = account.join("msg").join("video").join("2026-02");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&video_dir).unwrap();
        std::fs::write(video_dir.join("clip.mov"), b"real-video").unwrap();

        let summary = extract_videos(ArchiveConfig {
            source_dir: account,
            archive_dir: archive,
            dry_run: true,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.would_archive, 1);
    }

    #[test]
    fn extracts_direct_files_without_touching_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();

        let file_path = source.join("report.PDF");
        let file_bytes = b"synthetic-file";
        std::fs::write(&file_path, file_bytes).unwrap();

        let summary = extract_files(ArchiveConfig {
            source_dir: source.clone(),
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);
        assert_eq!(std::fs::read(&file_path).unwrap(), file_bytes);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (source_kind, media_type, extension): (String, String, String) = conn
            .query_row(
                r#"
                SELECT source_kind, media_type, extension
                FROM media_items
                WHERE source_relative_path = 'report.PDF'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(source_kind, "direct_file");
        assert_eq!(media_type, "file");
        assert_eq!(extension, "pdf");

        let verify = verify_archive(&archive).unwrap();
        assert_eq!(verify.checked, 1);
        assert_eq!(verify.ok, 1);
    }

    #[test]
    fn file_extract_from_attach_scans_sibling_msg_file() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("xwechat_files").join("wxid");
        let attach = account.join("msg").join("attach");
        let file_dir = account.join("msg").join("file").join("2026-02");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&attach).unwrap();
        std::fs::create_dir_all(&file_dir).unwrap();
        std::fs::write(attach.join("ignored.pdf"), b"attach-file-is-not-used").unwrap();
        std::fs::write(file_dir.join("report.docx"), b"real-file").unwrap();

        let summary = extract_files(ArchiveConfig {
            source_dir: attach,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let relative_path: String = conn
            .query_row(
                "SELECT source_relative_path FROM media_items WHERE media_type = 'file'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(relative_path, "msg/file/2026-02/report.docx");
    }

    #[test]
    fn file_dry_run_writes_nothing_to_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("report.xlsx"), b"synthetic-file").unwrap();

        let summary = extract_files(ArchiveConfig {
            source_dir: source,
            archive_dir: archive.clone(),
            dry_run: true,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.archived, 0);
        assert_eq!(summary.would_archive, 1);
        assert!(!archive.exists());
    }

    #[test]
    fn extracts_direct_voices_without_touching_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("voice-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();

        let voice_path = source.join("sample.SILK");
        let voice_bytes = b"synthetic-voice";
        std::fs::write(&voice_path, voice_bytes).unwrap();
        std::fs::write(source.join("notes.txt"), b"ignored").unwrap();

        let summary = extract_voices(ArchiveConfig {
            source_dir: source.clone(),
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 2);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);
        assert_eq!(std::fs::read(&voice_path).unwrap(), voice_bytes);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (source_kind, media_type, extension): (String, String, String) = conn
            .query_row(
                r#"
                SELECT source_kind, media_type, extension
                FROM media_items
                WHERE source_relative_path = 'sample.SILK'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(source_kind, "direct_voice");
        assert_eq!(media_type, "voice");
        assert_eq!(extension, "silk");

        let manifest_path = summary.manifest_path.unwrap();
        assert!(manifest_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-extract-voices.jsonl"));

        let verify = verify_archive(&archive).unwrap();
        assert_eq!(verify.checked, 1);
        assert_eq!(verify.ok, 1);
    }

    #[test]
    fn voice_extract_from_attach_scans_sibling_msg_voice() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("xwechat_files").join("wxid");
        let attach = account.join("msg").join("attach");
        let voice_dir = account.join("msg").join("voice").join("2026-02");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&attach).unwrap();
        std::fs::create_dir_all(&voice_dir).unwrap();
        std::fs::write(attach.join("ignored.silk"), b"attach-voice-is-not-used").unwrap();
        std::fs::write(voice_dir.join("sample.amr"), b"real-voice").unwrap();

        let summary = extract_voices(ArchiveConfig {
            source_dir: attach,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let relative_path: String = conn
            .query_row(
                "SELECT source_relative_path FROM media_items WHERE media_type = 'voice'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(relative_path, "msg/voice/2026-02/sample.amr");
    }

    #[test]
    fn voice_extract_from_account_does_not_scan_msg_file_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("xwechat_files").join("wxid");
        let file_dir = account.join("msg").join("file").join("2026-02");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&file_dir).unwrap();
        std::fs::write(file_dir.join("music.mp3"), b"audio-attachment").unwrap();

        let summary = extract_voices(ArchiveConfig {
            source_dir: account,
            archive_dir: archive,
            dry_run: true,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 0);
        assert_eq!(summary.candidates, 0);
        assert_eq!(summary.would_archive, 0);
    }

    #[test]
    fn voice_dry_run_writes_nothing_to_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("voice-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("sample.opus"), b"synthetic-voice").unwrap();

        let summary = extract_voices(ArchiveConfig {
            source_dir: source,
            archive_dir: archive.clone(),
            dry_run: true,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.archived, 0);
        assert_eq!(summary.would_archive, 1);
        assert!(!archive.exists());
    }

    #[test]
    fn video_dry_run_writes_nothing_to_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("clip.mov"), b"synthetic-video").unwrap();

        let summary = extract_videos(ArchiveConfig {
            source_dir: source,
            archive_dir: archive.clone(),
            dry_run: true,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.archived, 0);
        assert_eq!(summary.would_archive, 1);
        assert!(!archive.exists());
    }

    #[test]
    fn duplicate_videos_share_one_archive_object() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("one.mp4"), b"same-video").unwrap();
        std::fs::write(source.join("two.mp4"), b"same-video").unwrap();

        let summary = extract_videos(ArchiveConfig {
            source_dir: source,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.candidates, 2);
        assert_eq!(summary.archived, 1);
        assert_eq!(summary.already_archived, 1);

        let status = archive_status(&archive).unwrap();
        assert_eq!(status.total_records, 2);
        assert_eq!(status.archived_records, 2);
        assert_eq!(status.unique_objects, 1);
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
                explain_unsupported: false,
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

    #[test]
    fn explain_unsupported_groups_reasons_and_samples() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("unknown.dat"), b"not-a-supported-dat").unwrap();
        std::fs::write(
            source.join("missing-key.dat"),
            [b"\x07\x08V2\x08\x07".as_slice(), &[0; 16]].concat(),
        )
        .unwrap();

        let summary = extract_images(ArchiveConfig {
            source_dir: source,
            archive_dir: archive,
            dry_run: true,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: true,
        })
        .unwrap();

        assert_eq!(summary.candidates, 2);
        assert_eq!(summary.unsupported, 2);
        let explanation = summary.unsupported_explanation.unwrap();
        assert_eq!(explanation.reasons.len(), 2);
        assert!(explanation.reasons.iter().any(|reason| {
            reason.reason == "xor_key_not_detected"
                && reason.count == 1
                && reason.samples == ["unknown.dat"]
        }));
        assert!(explanation.reasons.iter().any(|reason| {
            reason.reason == "v2_aes_key_missing"
                && reason.count == 1
                && reason.samples == ["missing-key.dat"]
        }));
    }
}
