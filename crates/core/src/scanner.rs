use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use walkdir::WalkDir;

use crate::archive::{store_bytes, store_file, StoreOutcome};
use crate::audio::{
    audio_duration_supported_extension, detect_audio_metadata_from_file, AudioMetadata,
};
use crate::config::{create_archive_dirs, ArchiveConfig, DatDecodeOptions, WxgfMode};
use crate::error::{ArchiverError, IoContext, Result};
use crate::hash::{sha256_bytes, sha256_file};
use crate::image::{
    decode_dat, detect_image_dimensions, detect_image_dimensions_from_file, direct_image_extension,
    is_dat_file, validate_dat, DatDecode, ImageDimensions,
};
use crate::index::{
    index_path, insert_record, open_index, reusable_decoded_media_record, reusable_media_record,
    MediaRecord, ReusableMediaRecord,
};
use crate::manifest::ManifestWriter;
use crate::media::{
    direct_file_extension, direct_video_extension, direct_voice_extension, mime_type_for_extension,
};
use crate::task::{task_event, TaskEventKind, TaskOptions, TaskProgress};
use crate::types::{now_epoch_ms, ExtractSummary, ManifestEvent, ScanAction};
use crate::video::{detect_video_metadata_from_file, VideoMetadata};

#[derive(Debug, Clone, Default)]
pub(crate) struct MessageSource {
    pub talker: Option<String>,
    pub sender: Option<String>,
    pub local_id: Option<i64>,
    pub create_time: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct MediaMetadata {
    width_px: Option<u32>,
    height_px: Option<u32>,
    duration_ms: Option<u64>,
}

impl MediaMetadata {
    fn merge_missing(self, fallback: Self) -> Self {
        Self {
            width_px: self.width_px.or(fallback.width_px),
            height_px: self.height_px.or(fallback.height_px),
            duration_ms: self.duration_ms.or(fallback.duration_ms),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SourceFingerprint {
    size_bytes: u64,
    modified_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ObjectWriteStat {
    New,
    Existing,
}

pub fn extract_images(config: ArchiveConfig) -> Result<ExtractSummary> {
    extract_images_with_task(config, TaskOptions::default())
}

pub fn extract_images_with_task(
    config: ArchiveConfig,
    task_options: TaskOptions,
) -> Result<ExtractSummary> {
    let resolved = config.resolve()?;
    let mut run = ScanRun::new(&resolved, "extract-images", task_options)?;
    run.emit_scan_source_started(&resolved.source_dir);

    for entry in WalkDir::new(&resolved.source_dir).follow_links(false) {
        run.check_cancelled()?;
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        run.record_file_scanned(path);
        if let Some(extension) = direct_image_extension(path) {
            run.record_candidate(path);
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
            run.apply_result(Some(path), result)?;
        } else if is_dat_file(path) {
            run.record_candidate(path);
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
            run.apply_result(Some(path), result)?;
        }
    }

    run.finish()
}

pub fn extract_videos(config: ArchiveConfig) -> Result<ExtractSummary> {
    extract_videos_with_task(config, TaskOptions::default())
}

pub fn extract_videos_with_task(
    config: ArchiveConfig,
    task_options: TaskOptions,
) -> Result<ExtractSummary> {
    let resolved = config.resolve()?;
    let mut run = ScanRun::new(&resolved, "extract-videos", task_options)?;
    let video_sources = account_media_scan_sources(&resolved.source_dir, "video");

    for source in video_sources {
        run.check_cancelled()?;
        run.emit_scan_source_started(&source.scan_dir);
        for entry in WalkDir::new(&source.scan_dir).follow_links(false) {
            run.check_cancelled()?;
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            run.record_file_scanned(path);
            if let Some(extension) = direct_video_extension(path) {
                run.record_candidate(path);
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
                run.apply_result(Some(path), result)?;
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
    extract_voices_with_task(config, TaskOptions::default())
}

pub fn extract_voices_with_task(
    config: ArchiveConfig,
    task_options: TaskOptions,
) -> Result<ExtractSummary> {
    let resolved = config.resolve()?;
    let mut run = ScanRun::new(&resolved, "extract-voices", task_options)?;
    let voice_sources = voice_scan_sources(&resolved.source_dir);

    for source in voice_sources {
        run.check_cancelled()?;
        run.emit_scan_source_started(&source.scan_dir);
        for entry in WalkDir::new(&source.scan_dir).follow_links(false) {
            run.check_cancelled()?;
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            run.record_file_scanned(path);
            if let Some(extension) = direct_voice_extension(path) {
                run.record_candidate(path);
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
                run.apply_result(Some(path), result)?;
            }
        }
    }

    run.finish()
}

pub fn extract_files(config: ArchiveConfig) -> Result<ExtractSummary> {
    extract_files_with_task(config, TaskOptions::default())
}

pub fn extract_files_with_task(
    config: ArchiveConfig,
    task_options: TaskOptions,
) -> Result<ExtractSummary> {
    let resolved = config.resolve()?;
    let mut run = ScanRun::new(&resolved, "extract-files", task_options)?;
    let file_sources = account_media_scan_sources(&resolved.source_dir, "file");

    for source in file_sources {
        run.check_cancelled()?;
        run.emit_scan_source_started(&source.scan_dir);
        for entry in WalkDir::new(&source.scan_dir).follow_links(false) {
            run.check_cancelled()?;
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            run.record_file_scanned(path);
            if let Some(extension) = direct_file_extension(path) {
                run.record_candidate(path);
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
                run.apply_result(Some(path), result)?;
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
    task_name: String,
    summary: ExtractSummary,
    conn: Option<Connection>,
    manifest: Option<ManifestWriter>,
    task_options: TaskOptions,
}

impl ScanRun {
    fn new(
        resolved: &crate::config::ResolvedConfig,
        manifest_label: &str,
        task_options: TaskOptions,
    ) -> Result<Self> {
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

        task_options.emit(task_event(
            &run_id,
            manifest_label,
            TaskEventKind::Started,
            &summary,
            None,
            None,
            None,
        ));
        task_options.check_cancelled(&run_id, manifest_label, TaskProgress::from(&summary))?;

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
            task_name: manifest_label.to_string(),
            summary,
            conn,
            manifest,
            task_options,
        })
    }

    fn progress(&self) -> TaskProgress {
        TaskProgress::from(&self.summary)
    }

    fn emit(
        &self,
        kind: TaskEventKind,
        source_path: Option<&Path>,
        action: Option<ScanAction>,
        message: Option<String>,
    ) {
        self.task_options.emit(task_event(
            &self.run_id,
            &self.task_name,
            kind,
            &self.summary,
            source_path,
            action,
            message,
        ));
    }

    fn emit_scan_source_started(&self, path: &Path) {
        self.emit(TaskEventKind::ScanSourceStarted, Some(path), None, None);
    }

    fn record_file_scanned(&mut self, path: &Path) {
        self.summary.scanned_files += 1;
        self.emit(TaskEventKind::FileScanned, Some(path), None, None);
    }

    fn record_candidate(&mut self, path: &Path) {
        self.summary.candidates += 1;
        self.emit(TaskEventKind::CandidateFound, Some(path), None, None);
    }

    fn apply_result(
        &mut self,
        source_path: Option<&Path>,
        result: Result<ScanOutcome>,
    ) -> Result<()> {
        let outcome = result?;
        let action = outcome.action.clone();
        apply_outcome(&mut self.summary, outcome);
        self.emit(TaskEventKind::ItemFinished, source_path, Some(action), None);
        Ok(())
    }

    fn check_cancelled(&self) -> Result<()> {
        self.task_options
            .check_cancelled(&self.run_id, &self.task_name, self.progress())
    }

    fn finish(mut self) -> Result<ExtractSummary> {
        if let Some(writer) = self.manifest.as_mut() {
            writer.flush()?;
        }
        self.summary.finish_unsupported_explanation();
        self.emit(TaskEventKind::Completed, None, None, None);
        Ok(self.summary)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ScanOutcome {
    pub action: ScanAction,
    pub unsupported_reason: Option<String>,
    pub unsupported_sample: Option<String>,
    pub reused_records: u64,
    pub decoded_dat: u64,
    pub metadata_backfilled: u64,
    pub new_objects: u64,
    pub existing_objects: u64,
}

impl ScanOutcome {
    pub(crate) fn new(action: ScanAction) -> Self {
        Self {
            action,
            unsupported_reason: None,
            unsupported_sample: None,
            reused_records: 0,
            decoded_dat: 0,
            metadata_backfilled: 0,
            new_objects: 0,
            existing_objects: 0,
        }
    }

    fn reused_record(mut self) -> Self {
        self.reused_records += 1;
        self
    }

    fn decoded_dat(mut self) -> Self {
        self.decoded_dat += 1;
        self
    }

    fn metadata_backfilled(mut self) -> Self {
        self.metadata_backfilled += 1;
        self
    }

    pub(crate) fn new_object(mut self) -> Self {
        self.new_objects += 1;
        self
    }

    pub(crate) fn existing_object(mut self) -> Self {
        self.existing_objects += 1;
        self
    }

    fn unsupported(reason: impl Into<String>, sample: impl Into<String>) -> Self {
        Self {
            action: ScanAction::Unsupported,
            unsupported_reason: Some(reason.into()),
            unsupported_sample: Some(sample.into()),
            reused_records: 0,
            decoded_dat: 0,
            metadata_backfilled: 0,
            new_objects: 0,
            existing_objects: 0,
        }
    }
}

pub(crate) fn apply_outcome(summary: &mut ExtractSummary, result: ScanOutcome) {
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
    summary.reused_records += result.reused_records;
    summary.decoded_dat += result.decoded_dat;
    summary.metadata_backfilled += result.metadata_backfilled;
    summary.new_objects += result.new_objects;
    summary.existing_objects += result.existing_objects;
}

pub(crate) fn outcome_with_object_stat(
    outcome: ScanOutcome,
    object_write_stat: Option<ObjectWriteStat>,
) -> ScanOutcome {
    match object_write_stat {
        Some(ObjectWriteStat::New) => outcome.new_object(),
        Some(ObjectWriteStat::Existing) => outcome.existing_object(),
        None => outcome,
    }
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
    let source_fingerprint = source_fingerprint(path)?;

    let reusable = if !dry_run {
        if let (Some(conn), Some(modified_ms)) = (conn, source_fingerprint.modified_ms) {
            let source_path = path.to_string_lossy().to_string();
            reusable_media_record(
                conn,
                &source_path,
                source_kind,
                media_type,
                extension,
                source_fingerprint.size_bytes,
                modified_ms,
            )?
        } else {
            None
        }
    } else {
        None
    };

    if let Some(existing) = reusable.as_ref() {
        if safe_archive_path_exists(archive_root, &existing.archive_path)
            && metadata_complete_for_reuse(media_type, existing)
        {
            let event = build_reused_direct_media_event(
                run_id,
                path,
                &rel,
                source_kind,
                media_type,
                message_source,
                extension,
                source_fingerprint,
                metadata_from_reusable(existing),
                existing,
            );
            persist(conn, manifest, &event)?;
            return Ok(ScanOutcome::new(ScanAction::AlreadyArchived).reused_record());
        }
    }

    let metadata = direct_media_metadata(path, media_type);

    if let Some(existing) = reusable.as_ref() {
        if safe_archive_path_exists(archive_root, &existing.archive_path) {
            let metadata_backfilled = metadata_adds_missing(existing, metadata);
            let event = build_reused_direct_media_event(
                run_id,
                path,
                &rel,
                source_kind,
                media_type,
                message_source,
                extension,
                source_fingerprint,
                metadata.merge_missing(metadata_from_reusable(existing)),
                existing,
            );
            persist(conn, manifest, &event)?;
            let outcome = ScanOutcome::new(ScanAction::AlreadyArchived).reused_record();
            return Ok(if metadata_backfilled {
                outcome.metadata_backfilled()
            } else {
                outcome
            });
        }
    }

    let (sha256, size_bytes) = sha256_file(path)?;
    let (action, archive_path, verify_status, object_write_stat) = if dry_run {
        (ScanAction::WouldArchive, None, "not_run".to_string(), None)
    } else {
        match store_file(archive_root, run_id, path, &sha256, extension)? {
            StoreOutcome::Stored { archive_path } => (
                ScanAction::Archived,
                Some(archive_path),
                "ok".to_string(),
                Some(ObjectWriteStat::New),
            ),
            StoreOutcome::AlreadyExists { archive_path } => (
                ScanAction::AlreadyArchived,
                Some(archive_path),
                "ok".to_string(),
                Some(ObjectWriteStat::Existing),
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
        None,
        action.clone(),
        archive_path,
        Some(sha256),
        Some(size_bytes),
        Some(extension.to_string()),
        Some(source_fingerprint),
        metadata,
        "not_needed",
        &verify_status,
        None,
    );
    persist(conn, manifest, &event)?;
    Ok(outcome_with_object_stat(
        ScanOutcome::new(action),
        object_write_stat,
    ))
}

#[allow(clippy::too_many_arguments)]
fn build_reused_direct_media_event(
    run_id: &str,
    path: &Path,
    rel: &str,
    source_kind: &str,
    media_type: &str,
    message_source: Option<&MessageSource>,
    extension: &str,
    source_fingerprint: SourceFingerprint,
    metadata: MediaMetadata,
    existing: &ReusableMediaRecord,
) -> ManifestEvent {
    build_event(
        run_id,
        path,
        rel,
        source_kind,
        media_type,
        message_source,
        None,
        None,
        ScanAction::AlreadyArchived,
        Some(existing.archive_path.clone()),
        Some(existing.sha256.clone()),
        existing.size_bytes,
        Some(extension.to_string()),
        Some(source_fingerprint),
        metadata,
        "not_needed",
        "ok",
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_reused_dat_image_event(
    run_id: &str,
    path: &Path,
    rel: &str,
    source_kind: &str,
    message_source: Option<&MessageSource>,
    extension: &str,
    source_fingerprint: SourceFingerprint,
    decode_fingerprint: &str,
    metadata: MediaMetadata,
    existing: &ReusableMediaRecord,
) -> ManifestEvent {
    build_event(
        run_id,
        path,
        rel,
        source_kind,
        "image",
        message_source,
        existing.decoder.as_deref(),
        Some(decode_fingerprint.to_string()),
        ScanAction::AlreadyArchived,
        Some(existing.archive_path.clone()),
        Some(existing.sha256.clone()),
        existing.size_bytes,
        Some(extension.to_string()),
        Some(source_fingerprint),
        metadata,
        "decoded",
        "ok",
        None,
    )
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
    let source_fingerprint = source_fingerprint(path).ok();
    let decode_fingerprint = dat_decode_fingerprint(dat_options);

    let reusable = if !dry_run {
        if let (Some(conn), Some(source_fingerprint), Some(modified_ms)) = (
            conn,
            source_fingerprint,
            source_fingerprint.and_then(|value| value.modified_ms),
        ) {
            let source_path = path.to_string_lossy().to_string();
            reusable_decoded_media_record(
                conn,
                &source_path,
                source_kind,
                "image",
                source_fingerprint.size_bytes,
                modified_ms,
                &decode_fingerprint,
            )?
        } else {
            None
        }
    } else {
        None
    };

    if let (Some(existing), Some(source_fingerprint)) = (reusable.as_ref(), source_fingerprint) {
        if let Some(extension) = existing.extension.as_deref() {
            if safe_archive_path_exists(archive_root, &existing.archive_path)
                && metadata_complete_for_reuse("image", existing)
            {
                let event = build_reused_dat_image_event(
                    run_id,
                    path,
                    &rel,
                    source_kind,
                    message_source,
                    extension,
                    source_fingerprint,
                    &decode_fingerprint,
                    metadata_from_reusable(existing),
                    existing,
                );
                persist(conn, manifest, &event)?;
                return Ok(ScanOutcome::new(ScanAction::AlreadyArchived).reused_record());
            }
        }
    }

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
            let metadata = metadata_from_image_dimensions(detect_image_dimensions(&bytes));
            let (action, archive_path, verify_status, object_write_stat) = if dry_run {
                (ScanAction::WouldArchive, None, "not_run".to_string(), None)
            } else {
                match store_bytes(archive_root, run_id, &bytes, &sha256, extension)? {
                    StoreOutcome::Stored { archive_path } => (
                        ScanAction::Archived,
                        Some(archive_path),
                        "ok".to_string(),
                        Some(ObjectWriteStat::New),
                    ),
                    StoreOutcome::AlreadyExists { archive_path } => (
                        ScanAction::AlreadyArchived,
                        Some(archive_path),
                        "ok".to_string(),
                        Some(ObjectWriteStat::Existing),
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
                Some(decode_fingerprint.clone()),
                action.clone(),
                archive_path,
                Some(sha256),
                Some(size_bytes),
                Some(extension.to_string()),
                source_fingerprint,
                metadata,
                "decoded",
                &verify_status,
                None,
            );
            persist(conn, manifest, &event)?;
            Ok(outcome_with_object_stat(ScanOutcome::new(action), object_write_stat).decoded_dat())
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
                Some(decode_fingerprint.clone()),
                ScanAction::WouldArchive,
                None,
                None,
                None,
                Some(extension.to_string()),
                source_fingerprint,
                MediaMetadata::default(),
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
                Some(decode_fingerprint),
                ScanAction::Unsupported,
                None,
                None,
                None,
                None,
                source_fingerprint,
                MediaMetadata::default(),
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
    decode_fingerprint: Option<String>,
    action: ScanAction,
    archive_path: Option<String>,
    sha256: Option<String>,
    size_bytes: Option<u64>,
    extension: Option<String>,
    source_fingerprint: Option<SourceFingerprint>,
    metadata: MediaMetadata,
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
        original_filename: original_filename(source_path),
        mime_type: extension
            .as_deref()
            .and_then(mime_type_for_extension)
            .map(str::to_string),
        width_px: metadata.width_px,
        height_px: metadata.height_px,
        duration_ms: metadata.duration_ms,
        message_talker: message_source.talker,
        message_sender: message_source.sender,
        message_local_id: message_source.local_id,
        message_create_time: message_source.create_time,
        decoder: decoder.map(str::to_string),
        decode_fingerprint,
        action,
        archive_path,
        sha256,
        size_bytes,
        source_size_bytes: source_fingerprint.map(|fingerprint| fingerprint.size_bytes),
        source_modified_ms: source_fingerprint.and_then(|fingerprint| fingerprint.modified_ms),
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
                original_filename: event.original_filename.clone(),
                mime_type: event.mime_type.clone(),
                width_px: event.width_px,
                height_px: event.height_px,
                duration_ms: event.duration_ms,
                message_talker: event.message_talker.clone(),
                message_sender: event.message_sender.clone(),
                message_local_id: event.message_local_id,
                message_create_time: event.message_create_time,
                decoder: event.decoder.clone(),
                decode_fingerprint: event.decode_fingerprint.clone(),
                archive_path: event.archive_path.clone(),
                sha256: event.sha256.clone(),
                size_bytes: event.size_bytes,
                source_size_bytes: event.source_size_bytes,
                source_modified_ms: event.source_modified_ms,
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

fn metadata_from_image_dimensions(dimensions: Option<ImageDimensions>) -> MediaMetadata {
    dimensions
        .map(|dimensions| MediaMetadata {
            width_px: Some(dimensions.width_px),
            height_px: Some(dimensions.height_px),
            duration_ms: None,
        })
        .unwrap_or_default()
}

fn metadata_from_video_metadata(metadata: Option<VideoMetadata>) -> MediaMetadata {
    metadata
        .map(|metadata| MediaMetadata {
            width_px: metadata.width_px,
            height_px: metadata.height_px,
            duration_ms: metadata.duration_ms,
        })
        .unwrap_or_default()
}

fn metadata_from_audio_metadata(metadata: Option<AudioMetadata>) -> MediaMetadata {
    metadata
        .map(|metadata| MediaMetadata {
            width_px: None,
            height_px: None,
            duration_ms: metadata.duration_ms,
        })
        .unwrap_or_default()
}

fn metadata_from_reusable(record: &ReusableMediaRecord) -> MediaMetadata {
    MediaMetadata {
        width_px: record.width_px,
        height_px: record.height_px,
        duration_ms: record.duration_ms,
    }
}

fn metadata_adds_missing(record: &ReusableMediaRecord, metadata: MediaMetadata) -> bool {
    (record.width_px.is_none() && metadata.width_px.is_some())
        || (record.height_px.is_none() && metadata.height_px.is_some())
        || (record.duration_ms.is_none() && metadata.duration_ms.is_some())
}

fn metadata_complete_for_reuse(media_type: &str, record: &ReusableMediaRecord) -> bool {
    match media_type {
        "image" => record.width_px.is_some() && record.height_px.is_some(),
        "video" => {
            record.width_px.is_some() && record.height_px.is_some() && record.duration_ms.is_some()
        }
        "voice" => record.duration_ms.is_some(),
        _ => true,
    }
}

fn direct_media_metadata(path: &Path, media_type: &str) -> MediaMetadata {
    match media_type {
        "image" => metadata_from_image_dimensions(detect_image_dimensions_from_file(path)),
        "video" => metadata_from_video_metadata(detect_video_metadata_from_file(path)),
        "voice" => path
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| audio_duration_supported_extension(extension))
            .and_then(|extension| detect_audio_metadata_from_file(path, extension))
            .map(|metadata| metadata_from_audio_metadata(Some(metadata)))
            .unwrap_or_default(),
        _ => MediaMetadata::default(),
    }
}

fn dat_decode_fingerprint(options: &DatDecodeOptions) -> String {
    let mut canonical = String::from("wechat-archiver:dat-decode:v1\n");
    canonical.push_str("image_aes_key_sha256=");
    if let Some(key) = options.image_aes_key.as_deref() {
        let (key_hash, _) = sha256_bytes(key);
        canonical.push_str(&key_hash);
    } else {
        canonical.push_str("none");
    }
    canonical.push('\n');
    canonical.push_str(&format!("image_xor_key={:02x}\n", options.image_xor_key));
    canonical.push_str("wxgf_mode=");
    canonical.push_str(wxgf_mode_name(options.wxgf_mode));
    canonical.push('\n');
    canonical.push_str("wxgf_ffmpeg_path=");
    if let Some(path) = &options.wxgf_ffmpeg_path {
        canonical.push_str(&path.to_string_lossy());
    } else {
        canonical.push_str("default");
    }
    canonical.push('\n');

    let (fingerprint, _) = sha256_bytes(canonical.as_bytes());
    format!("dat-v1:{fingerprint}")
}

fn wxgf_mode_name(mode: WxgfMode) -> &'static str {
    match mode {
        WxgfMode::Off => "off",
        WxgfMode::Raw => "raw",
        WxgfMode::Jpg => "jpg",
        WxgfMode::Mp4 => "mp4",
    }
}

fn source_fingerprint(path: &Path) -> Result<SourceFingerprint> {
    let metadata = std::fs::metadata(path).with_path(path)?;
    Ok(SourceFingerprint {
        size_bytes: metadata.len(),
        modified_ms: metadata.modified().ok().and_then(system_time_ms),
    })
}

fn system_time_ms(time: SystemTime) -> Option<i64> {
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    let millis = duration.as_millis();
    (millis <= i64::MAX as u128).then_some(millis as i64)
}

fn safe_archive_path_exists(archive_root: &Path, archive_path: &str) -> bool {
    let path = Path::new(archive_path);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        && archive_root.join(path).is_file()
}

fn original_filename(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
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
    use crate::error::ArchiverError;
    use crate::status::archive_status;
    use crate::task::{
        CancelToken, TaskEvent, TaskEventKind, TaskOptions, TaskReporter, TaskRunner, TaskStatus,
    };
    use crate::verify::verify_archive;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    type DatIndexMetadataRow = (
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        Option<i64>,
        Option<i64>,
    );

    #[test]
    fn image_extract_with_task_emits_progress_events() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("image.jpg"), synthetic_jpeg(64, 32)).unwrap();

        let events = Arc::new(Mutex::new(Vec::<TaskEvent>::new()));
        let reporter = TaskReporter::new({
            let events = Arc::clone(&events);
            move |event| events.lock().unwrap().push(event)
        });

        let summary = extract_images_with_task(
            ArchiveConfig {
                source_dir: source,
                archive_dir: archive,
                dry_run: false,
                dat_options: DatDecodeOptions::default(),
                explain_unsupported: false,
            },
            TaskOptions::new().with_reporter(reporter),
        )
        .unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.first().unwrap().kind, TaskEventKind::Started);
        assert!(events
            .iter()
            .any(|event| event.kind == TaskEventKind::ScanSourceStarted));
        assert!(events
            .iter()
            .any(|event| event.kind == TaskEventKind::FileScanned));
        assert!(events
            .iter()
            .any(|event| event.kind == TaskEventKind::CandidateFound));
        assert!(events
            .iter()
            .any(|event| event.kind == TaskEventKind::ItemFinished
                && event.action == Some(ScanAction::Archived)));
        let completed = events
            .iter()
            .find(|event| event.kind == TaskEventKind::Completed)
            .unwrap();
        assert_eq!(completed.run_id, summary.run_id);
        assert_eq!(completed.task_name, "extract-images");
        assert_eq!(completed.progress.scanned_files, 1);
        assert_eq!(completed.progress.candidates, 1);
        assert_eq!(completed.progress.archived, 1);
        assert_eq!(completed.progress.new_objects, 1);
    }

    #[test]
    fn pre_cancelled_task_stops_before_archive_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("image.jpg"), synthetic_jpeg(64, 32)).unwrap();

        let cancel_token = CancelToken::new();
        cancel_token.cancel();
        let events = Arc::new(Mutex::new(Vec::<TaskEvent>::new()));
        let reporter = TaskReporter::new({
            let events = Arc::clone(&events);
            move |event| events.lock().unwrap().push(event)
        });

        let error = extract_images_with_task(
            ArchiveConfig {
                source_dir: source,
                archive_dir: archive.clone(),
                dry_run: false,
                dat_options: DatDecodeOptions::default(),
                explain_unsupported: false,
            },
            TaskOptions::new()
                .with_cancel_token(cancel_token)
                .with_reporter(reporter),
        )
        .unwrap_err();

        match error {
            ArchiverError::TaskCancelled { task_name } => {
                assert_eq!(task_name, "extract-images");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(!archive.exists());

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, TaskEventKind::Started);
        assert_eq!(events[1].kind, TaskEventKind::Cancelled);
    }

    #[test]
    fn task_runner_executes_image_extract_in_background() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();

        let direct_path = source.join("image.jpg");
        let direct_bytes = synthetic_jpeg(64, 32);
        std::fs::write(&direct_path, &direct_bytes).unwrap();

        let runner = TaskRunner::new();
        let handle = runner.spawn("extract-images", move |options| {
            extract_images_with_task(
                ArchiveConfig {
                    source_dir: source,
                    archive_dir: archive,
                    dry_run: false,
                    dat_options: DatDecodeOptions::default(),
                    explain_unsupported: false,
                },
                options,
            )
        });

        let snapshot = handle.join();
        assert_eq!(snapshot.status, TaskStatus::Completed);
        let summary = snapshot.result.unwrap();
        assert_eq!(summary.archived, 1);
        assert_eq!(summary.new_objects, 1);
        assert_eq!(std::fs::read(&direct_path).unwrap(), direct_bytes);

        let events = handle.drain_events();
        assert!(events
            .iter()
            .any(|event| event.kind == TaskEventKind::Started));
        assert!(events
            .iter()
            .any(|event| event.kind == TaskEventKind::Completed));
    }

    #[test]
    fn extracts_direct_and_xor_images_without_touching_source() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(source.join("attach/hash/2026-01/Img")).unwrap();

        let direct_path = source.join("image.jpg");
        let direct_bytes = synthetic_jpeg(64, 32);
        std::fs::write(&direct_path, &direct_bytes).unwrap();

        let dat_path = source.join("attach/hash/2026-01/Img/sample.dat");
        let decoded = synthetic_png(16, 8);
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
        assert_eq!(summary.already_archived, 0);
        assert_eq!(summary.reused_records, 0);
        assert_eq!(summary.decoded_dat, 1);
        assert_eq!(summary.metadata_backfilled, 0);
        assert_eq!(summary.new_objects, 2);
        assert_eq!(summary.existing_objects, 0);
        assert_eq!(std::fs::read(&direct_path).unwrap(), direct_bytes);
        assert_eq!(std::fs::read(&dat_path).unwrap(), encrypted);
        assert!(archive.join("index.sqlite").exists());
        assert!(summary.manifest_path.unwrap().exists());

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (original_filename, mime_type, width_px, height_px, duration_ms): (
            String,
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) = conn
            .query_row(
                r#"
                SELECT original_filename, mime_type, width_px, height_px, duration_ms
                FROM media_items
                WHERE source_relative_path = 'image.jpg'
                "#,
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(original_filename, "image.jpg");
        assert_eq!(mime_type, "image/jpeg");
        assert_eq!(width_px, Some(64));
        assert_eq!(height_px, Some(32));
        assert_eq!(duration_ms, None);

        let verify = verify_archive(&archive).unwrap();
        assert_eq!(verify.checked, 2);
        assert_eq!(verify.ok, 2);
        assert_eq!(verify.missing, 0);
        assert_eq!(verify.mismatched, 0);
    }

    #[cfg(unix)]
    #[test]
    fn unchanged_direct_media_reuses_index_without_reading_source() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();

        let direct_path = source.join("image.jpg");
        std::fs::write(&direct_path, synthetic_jpeg(64, 32)).unwrap();

        let first = extract_images(ArchiveConfig {
            source_dir: source.clone(),
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();
        assert_eq!(first.archived, 1);
        assert_eq!(first.already_archived, 0);
        assert_eq!(first.reused_records, 0);
        assert_eq!(first.new_objects, 1);
        assert_eq!(first.existing_objects, 0);

        let mut permissions = std::fs::metadata(&direct_path).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&direct_path, permissions).unwrap();

        let second = extract_images(ArchiveConfig {
            source_dir: source.clone(),
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        let mut permissions = std::fs::metadata(&direct_path).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&direct_path, permissions).unwrap();

        assert_eq!(second.archived, 0);
        assert_eq!(second.already_archived, 1);
        assert_eq!(second.reused_records, 1);
        assert_eq!(second.decoded_dat, 0);
        assert_eq!(second.metadata_backfilled, 0);
        assert_eq!(second.new_objects, 0);
        assert_eq!(second.existing_objects, 0);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (source_size_bytes, source_modified_ms): (Option<i64>, Option<i64>) = conn
            .query_row(
                r#"
                SELECT source_size_bytes, source_modified_ms
                FROM media_items
                WHERE source_relative_path = 'image.jpg'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(source_size_bytes.is_some());
        assert!(source_modified_ms.is_some());

        let manifest_path = second.manifest_path.unwrap();
        let manifest = std::fs::read_to_string(manifest_path).unwrap();
        let event = manifest
            .lines()
            .map(|line| serde_json::from_str::<ManifestEvent>(line).unwrap())
            .find(|event| event.source_relative_path == "image.jpg")
            .unwrap();
        assert_eq!(event.action, ScanAction::AlreadyArchived);
        assert!(event.source_size_bytes.is_some());
        assert!(event.source_modified_ms.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn unchanged_dat_image_reuses_index_without_reading_source() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(source.join("attach/hash/2026-01/Img")).unwrap();

        let dat_path = source.join("attach/hash/2026-01/Img/sample.dat");
        write_legacy_xor_dat(&dat_path, 16, 8);

        let first = extract_images(ArchiveConfig {
            source_dir: source.clone(),
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();
        assert_eq!(first.archived, 1);
        assert_eq!(first.already_archived, 0);
        assert_eq!(first.reused_records, 0);
        assert_eq!(first.decoded_dat, 1);
        assert_eq!(first.new_objects, 1);
        assert_eq!(first.existing_objects, 0);

        let mut permissions = std::fs::metadata(&dat_path).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(&dat_path, permissions).unwrap();

        let second = extract_images(ArchiveConfig {
            source_dir: source.clone(),
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        let mut permissions = std::fs::metadata(&dat_path).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&dat_path, permissions).unwrap();

        assert_eq!(second.archived, 0);
        assert_eq!(second.already_archived, 1);
        assert_eq!(second.reused_records, 1);
        assert_eq!(second.decoded_dat, 0);
        assert_eq!(second.metadata_backfilled, 0);
        assert_eq!(second.new_objects, 0);
        assert_eq!(second.existing_objects, 0);

        let manifest_path = second.manifest_path.unwrap();
        let manifest = std::fs::read_to_string(manifest_path).unwrap();
        let event = manifest
            .lines()
            .map(|line| serde_json::from_str::<ManifestEvent>(line).unwrap())
            .find(|event| event.source_relative_path == "attach/hash/2026-01/Img/sample.dat")
            .unwrap();
        assert_eq!(event.action, ScanAction::AlreadyArchived);
        assert_eq!(event.decoder.as_deref(), Some("legacy_xor"));
        assert_eq!(
            event.decode_fingerprint.as_deref(),
            Some(dat_decode_fingerprint(&DatDecodeOptions::default()).as_str())
        );
        assert_eq!(event.width_px, Some(16));
        assert_eq!(event.height_px, Some(8));
        assert!(event.source_size_bytes.is_some());
        assert!(event.source_modified_ms.is_some());
    }

    #[test]
    fn duplicate_direct_media_counts_existing_object() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();

        let image = synthetic_jpeg(64, 32);
        std::fs::write(source.join("one.jpg"), &image).unwrap();
        std::fs::write(source.join("two.jpg"), &image).unwrap();

        let summary = extract_images(ArchiveConfig {
            source_dir: source,
            archive_dir: archive,
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.candidates, 2);
        assert_eq!(summary.archived, 1);
        assert_eq!(summary.already_archived, 1);
        assert_eq!(summary.reused_records, 0);
        assert_eq!(summary.decoded_dat, 0);
        assert_eq!(summary.new_objects, 1);
        assert_eq!(summary.existing_objects, 1);
    }

    #[test]
    fn direct_media_reuse_counts_metadata_backfill() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();

        let direct_path = source.join("image.jpg");
        std::fs::write(&direct_path, synthetic_jpeg(64, 32)).unwrap();

        let first = extract_images(ArchiveConfig {
            source_dir: source.clone(),
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();
        assert_eq!(first.archived, 1);
        assert_eq!(first.new_objects, 1);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        conn.execute(
            r#"
            UPDATE media_items
            SET width_px = NULL,
                height_px = NULL
            WHERE source_relative_path = 'image.jpg'
            "#,
            [],
        )
        .unwrap();
        drop(conn);

        let second = extract_images(ArchiveConfig {
            source_dir: source,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(second.archived, 0);
        assert_eq!(second.already_archived, 1);
        assert_eq!(second.reused_records, 1);
        assert_eq!(second.metadata_backfilled, 1);
        assert_eq!(second.new_objects, 0);
        assert_eq!(second.existing_objects, 0);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (width_px, height_px): (Option<i64>, Option<i64>) = conn
            .query_row(
                r#"
                SELECT width_px, height_px
                FROM media_items
                WHERE source_relative_path = 'image.jpg'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(width_px, Some(64));
        assert_eq!(height_px, Some(32));
    }

    #[test]
    fn records_dat_source_kind_and_decoder_separately() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(source.join("attach/hash/2026-01/Img")).unwrap();

        let dat_path = source.join("attach/hash/2026-01/Img/sample.dat");
        let decoded = synthetic_png(16, 8);
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
        let (
            source_kind,
            decoder,
            decode_fingerprint,
            extension,
            original_filename,
            mime_type,
            width_px,
            height_px,
        ): DatIndexMetadataRow = conn
            .query_row(
                r#"
                SELECT source_kind, decoder, decode_fingerprint, extension, original_filename, mime_type, width_px, height_px
                FROM media_items
                WHERE source_relative_path = 'attach/hash/2026-01/Img/sample.dat'
                "#,
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(source_kind, "dat_image");
        assert_eq!(decoder.as_deref(), Some("legacy_xor"));
        assert_eq!(
            decode_fingerprint.as_deref(),
            Some(dat_decode_fingerprint(&DatDecodeOptions::default()).as_str())
        );
        assert_eq!(extension, "png");
        assert_eq!(original_filename, "sample.dat");
        assert_eq!(mime_type, "image/png");
        assert_eq!(width_px, Some(16));
        assert_eq!(height_px, Some(8));

        let manifest_path = summary.manifest_path.unwrap();
        let manifest = std::fs::read_to_string(manifest_path).unwrap();
        let event = manifest
            .lines()
            .map(|line| serde_json::from_str::<ManifestEvent>(line).unwrap())
            .find(|event| event.source_relative_path == "attach/hash/2026-01/Img/sample.dat")
            .unwrap();
        assert_eq!(event.source_kind, "dat_image");
        assert_eq!(event.decoder.as_deref(), Some("legacy_xor"));
        assert_eq!(
            event.decode_fingerprint.as_deref(),
            Some(dat_decode_fingerprint(&DatDecodeOptions::default()).as_str())
        );
        assert_eq!(event.original_filename.as_deref(), Some("sample.dat"));
        assert_eq!(event.mime_type.as_deref(), Some("image/png"));
        assert_eq!(event.width_px, Some(16));
        assert_eq!(event.height_px, Some(8));
        assert_eq!(event.duration_ms, None);
    }

    #[test]
    fn dat_reuse_requires_matching_decode_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("wechat-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(source.join("attach/hash/2026-01/Img")).unwrap();

        let dat_path = source.join("attach/hash/2026-01/Img/sample.dat");
        write_legacy_xor_dat(&dat_path, 16, 8);

        extract_images(ArchiveConfig {
            source_dir: source,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (source_path, source_size_bytes, source_modified_ms, decode_fingerprint): (
            String,
            i64,
            i64,
            String,
        ) = conn
            .query_row(
                r#"
                SELECT source_path, source_size_bytes, source_modified_ms, decode_fingerprint
                FROM media_items
                WHERE source_relative_path = 'attach/hash/2026-01/Img/sample.dat'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            decode_fingerprint,
            dat_decode_fingerprint(&DatDecodeOptions::default())
        );
        let reusable = reusable_decoded_media_record(
            &conn,
            &source_path,
            "dat_image",
            "image",
            source_size_bytes as u64,
            source_modified_ms,
            &decode_fingerprint,
        )
        .unwrap()
        .unwrap();
        assert_eq!(reusable.extension.as_deref(), Some("png"));
        assert_eq!(reusable.decoder.as_deref(), Some("legacy_xor"));

        let changed_key = DatDecodeOptions {
            image_aes_key: Some(vec![1; 16]),
            ..DatDecodeOptions::default()
        };
        assert!(
            reusable_decoded_media_record(
                &conn,
                &source_path,
                "dat_image",
                "image",
                source_size_bytes as u64,
                source_modified_ms,
                &dat_decode_fingerprint(&changed_key),
            )
            .unwrap()
            .is_none(),
            "换 AES key 后不能复用旧 .dat 解码结果"
        );

        let changed_wxgf = DatDecodeOptions {
            wxgf_mode: WxgfMode::Raw,
            ..DatDecodeOptions::default()
        };
        assert!(
            reusable_decoded_media_record(
                &conn,
                &source_path,
                "dat_image",
                "image",
                source_size_bytes as u64,
                source_modified_ms,
                &dat_decode_fingerprint(&changed_wxgf),
            )
            .unwrap()
            .is_none(),
            "换 wxgf mode 后不能复用旧 .dat 解码结果"
        );

        conn.execute("UPDATE media_items SET decode_fingerprint = NULL", [])
            .unwrap();
        assert!(
            reusable_decoded_media_record(
                &conn,
                &source_path,
                "dat_image",
                "image",
                source_size_bytes as u64,
                source_modified_ms,
                &decode_fingerprint,
            )
            .unwrap()
            .is_none(),
            "旧索引没有 decode_fingerprint 时不能复用 .dat 解码结果"
        );
    }

    #[test]
    fn dat_decode_fingerprint_does_not_store_raw_key_material() {
        let options = DatDecodeOptions {
            image_aes_key: Some(b"very-secret-key".to_vec()),
            ..DatDecodeOptions::default()
        };

        let fingerprint = dat_decode_fingerprint(&options);

        assert!(fingerprint.starts_with("dat-v1:"));
        assert!(!fingerprint.contains("very-secret-key"));
        assert_ne!(
            fingerprint,
            dat_decode_fingerprint(&DatDecodeOptions::default())
        );
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
        let video_bytes = synthetic_mp4(320, 180, 6_543);
        std::fs::write(&video_path, &video_bytes).unwrap();
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
        type DirectVideoRow = (
            String,
            String,
            String,
            String,
            String,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        );

        let (
            source_kind,
            media_type,
            extension,
            original_filename,
            mime_type,
            width_px,
            height_px,
            duration_ms,
        ): DirectVideoRow = conn
            .query_row(
                r#"
                SELECT source_kind, media_type, extension, original_filename, mime_type, width_px, height_px, duration_ms
                FROM media_items
                WHERE source_relative_path = 'clip.MP4'
                "#,
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(source_kind, "direct_video");
        assert_eq!(media_type, "video");
        assert_eq!(extension, "mp4");
        assert_eq!(original_filename, "clip.MP4");
        assert_eq!(mime_type, "video/mp4");
        assert_eq!(width_px, Some(320));
        assert_eq!(height_px, Some(180));
        assert_eq!(duration_ms, Some(6_543));

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
        assert_eq!(event.original_filename.as_deref(), Some("clip.MP4"));
        assert_eq!(event.mime_type.as_deref(), Some("video/mp4"));
        assert_eq!(event.width_px, Some(320));
        assert_eq!(event.height_px, Some(180));
        assert_eq!(event.duration_ms, Some(6_543));

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
        let (source_kind, media_type, extension, original_filename, mime_type): (
            String,
            String,
            String,
            String,
            String,
        ) = conn
            .query_row(
                r#"
                SELECT source_kind, media_type, extension, original_filename, mime_type
                FROM media_items
                WHERE source_relative_path = 'report.PDF'
                "#,
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(source_kind, "direct_file");
        assert_eq!(media_type, "file");
        assert_eq!(extension, "pdf");
        assert_eq!(original_filename, "report.PDF");
        assert_eq!(mime_type, "application/pdf");

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
        let (source_kind, media_type, extension, original_filename, mime_type): (
            String,
            String,
            String,
            String,
            String,
        ) = conn
            .query_row(
                r#"
                SELECT source_kind, media_type, extension, original_filename, mime_type
                FROM media_items
                WHERE source_relative_path = 'sample.SILK'
                "#,
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(source_kind, "direct_voice");
        assert_eq!(media_type, "voice");
        assert_eq!(extension, "silk");
        assert_eq!(original_filename, "sample.SILK");
        assert_eq!(mime_type, "audio/silk");

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
    fn extracts_direct_wav_duration() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("voice-source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();

        let voice_path = source.join("sample.wav");
        let voice_bytes = synthetic_wav(8_000, 1, 16, 1_250);
        std::fs::write(&voice_path, voice_bytes).unwrap();

        let summary = extract_voices(ArchiveConfig {
            source_dir: source,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (extension, mime_type, duration_ms): (String, String, Option<i64>) = conn
            .query_row(
                r#"
                SELECT extension, mime_type, duration_ms
                FROM media_items
                WHERE source_relative_path = 'sample.wav'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(extension, "wav");
        assert_eq!(mime_type, "audio/wav");
        assert_eq!(duration_ms, Some(1_250));
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

    fn synthetic_png(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        data.extend_from_slice(&13u32.to_be_bytes());
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data
    }

    fn write_legacy_xor_dat(path: &Path, width: u32, height: u32) {
        let decoded = synthetic_png(width, height);
        let encrypted: Vec<u8> = decoded.iter().map(|byte| byte ^ 0x88).collect();
        std::fs::write(path, encrypted).unwrap();
    }

    fn synthetic_jpeg(width: u16, height: u16) -> Vec<u8> {
        vec![
            0xff,
            0xd8,
            0xff,
            0xe0,
            0x00,
            0x04,
            0x00,
            0x00,
            0xff,
            0xc0,
            0x00,
            0x0b,
            0x08,
            (height >> 8) as u8,
            height as u8,
            (width >> 8) as u8,
            width as u8,
            0x01,
            0x01,
            0x11,
            0x00,
        ]
    }

    fn synthetic_mp4(width: u32, height: u32, duration_ms: u32) -> Vec<u8> {
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0, 0, 0, 0]);
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&1000u32.to_be_bytes());
        mvhd.extend_from_slice(&duration_ms.to_be_bytes());

        let mut tkhd = vec![0u8; 84];
        tkhd[1..4].copy_from_slice(&[0, 0, 7]);
        tkhd[76..80].copy_from_slice(&(width << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&(height << 16).to_be_bytes());

        let trak = mp4_box(*b"trak", &mp4_box(*b"tkhd", &tkhd));
        let moov_payload = [mp4_box(*b"mvhd", &mvhd), trak].concat();
        [mp4_box(*b"ftyp", b"isom"), mp4_box(*b"moov", &moov_payload)].concat()
    }

    fn mp4_box(name: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = 8 + payload.len() as u32;
        let mut data = Vec::new();
        data.extend_from_slice(&size.to_be_bytes());
        data.extend_from_slice(&name);
        data.extend_from_slice(payload);
        data
    }

    fn synthetic_wav(
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
        duration_ms: u32,
    ) -> Vec<u8> {
        let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
        let data_size = byte_rate * duration_ms / 1000;
        let mut data = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + data_size).to_le_bytes());
        data.extend_from_slice(b"WAVE");
        data.extend_from_slice(b"fmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&channels.to_le_bytes());
        data.extend_from_slice(&sample_rate.to_le_bytes());
        data.extend_from_slice(&byte_rate.to_le_bytes());
        data.extend_from_slice(&(channels * bits_per_sample / 8).to_le_bytes());
        data.extend_from_slice(&bits_per_sample.to_le_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&data_size.to_le_bytes());
        data.resize(data.len() + data_size as usize, 0);
        data
    }
}
