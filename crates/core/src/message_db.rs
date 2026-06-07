use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;

use crate::config::{create_archive_dirs, ArchiveConfig, DatDecodeOptions};
use crate::error::{ArchiverError, IoContext, Result};
use crate::index::{index_path, open_index};
use crate::manifest::ManifestWriter;
use crate::media::{direct_file_extension, direct_video_extension};
use crate::scanner::{
    apply_result, persist, process_dat_image_with_message_source,
    process_direct_media_with_message_source, MessageSource, ScanOutcome,
};
use crate::types::{now_epoch_ms, ExtractSummary, ManifestEvent, ScanAction};

#[derive(Debug, Clone)]
pub struct MessageDbExtractConfig {
    pub account_dir: PathBuf,
    pub message_db_dir: Option<PathBuf>,
    pub archive_dir: PathBuf,
    pub dry_run: bool,
    pub dat_options: DatDecodeOptions,
    pub explain_unsupported: bool,
}

#[derive(Debug, Clone)]
pub struct MessageDbInspectConfig {
    pub account_dir: PathBuf,
    pub message_db_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MessageDbInspection {
    pub account_dir: PathBuf,
    pub message_db_dir: PathBuf,
    pub message_db_dir_overridden: bool,
    pub status: MessageDbInspectionStatus,
    pub directory_status: MessageDbDirectoryStatus,
    pub resource_db: MessageDbFileInspection,
    pub message_dbs: Vec<MessageDbFileInspection>,
    pub total_message_dbs: usize,
    pub readable_message_dbs: usize,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDbInspectionStatus {
    Ready,
    Missing,
    EncryptedOrNotSqlite,
    Unsupported,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDbDirectoryStatus {
    Ready,
    Missing,
    NotDirectory,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MessageDbFileInspection {
    pub path: PathBuf,
    pub role: MessageDbFileRole,
    pub status: MessageDbFileStatus,
    pub sqlite_header: bool,
    pub table_count: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDbFileRole {
    MessageResource,
    Message,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageDbFileStatus {
    ReadableSqlite,
    Missing,
    NotFile,
    EncryptedOrNotSqlite,
    UnsupportedSqlite,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct MessageKey {
    talker: String,
    local_id: i64,
    create_time: i64,
}

#[derive(Debug, Clone)]
struct ImageResource {
    key: MessageKey,
    file_md5: String,
}

#[derive(Debug, Clone)]
struct VideoResource {
    key: MessageKey,
    file_md5: String,
}

#[derive(Debug, Clone)]
struct FileResource {
    key: MessageKey,
    file_name: String,
}

pub fn extract_message_db_images(config: MessageDbExtractConfig) -> Result<ExtractSummary> {
    let resolved = ArchiveConfig {
        source_dir: config.account_dir.clone(),
        archive_dir: config.archive_dir,
        dry_run: config.dry_run,
        dat_options: config.dat_options,
        explain_unsupported: config.explain_unsupported,
    }
    .resolve()?;

    let db_source = resolve_message_db_source(&resolved.source_dir, config.message_db_dir)?;

    let attach_root = resolved.source_dir.join("msg").join("attach");
    if !attach_root.is_dir() {
        return Err(ArchiverError::Other(format!(
            "attach directory does not exist: {}",
            attach_root.display()
        )));
    }

    let resources = query_image_resources(&db_source.resource_db)?;
    let message_dbs = message_db_paths(&db_source.message_dir)?;
    let (image_messages, scanned_rows) = query_image_message_keys(&message_dbs, &resources)?;

    let run_id = format!("{}", now_epoch_ms());
    let mut summary = ExtractSummary::new(
        run_id.clone(),
        resolved.source_dir.clone(),
        resolved.archive_dir.clone(),
        resolved.dry_run,
    );
    summary.scanned_files = scanned_rows;
    if resolved.explain_unsupported {
        summary.enable_unsupported_explanation();
    }

    let mut conn = None;
    let mut manifest = None;
    if !resolved.dry_run {
        create_archive_dirs(&resolved.archive_dir)?;
        let opened = open_index(&resolved.archive_dir)?;
        summary.index_path = Some(index_path(&resolved.archive_dir));
        let writer = ManifestWriter::create(&resolved.archive_dir, &run_id, "extract-db-images")?;
        summary.manifest_path = Some(writer.path().to_path_buf());
        conn = Some(opened);
        manifest = Some(writer);
    }

    for resource in resources {
        if !image_messages.contains(&resource.key) {
            continue;
        }

        summary.candidates += 1;
        let message_source = message_source_from_key(&resource.key);
        let result = match find_dat_file(
            &attach_root,
            &resource.key.talker,
            &resource.file_md5,
            resource.key.create_time,
        ) {
            Some(dat_path) => process_dat_image_with_message_source(
                &dat_path,
                &resolved.source_dir,
                &resolved.archive_dir,
                &run_id,
                "message_db_image",
                resolved.dry_run,
                &resolved.dat_options,
                conn.as_ref(),
                manifest.as_mut(),
                Some(&message_source),
            ),
            None => record_missing_dat(
                &resource,
                &resolved.source_dir,
                &run_id,
                conn.as_ref(),
                manifest.as_mut(),
                &message_source,
            ),
        };
        apply_result(&mut summary, result)?;
    }

    if let Some(writer) = manifest.as_mut() {
        writer.flush()?;
    }
    summary.finish_unsupported_explanation();

    Ok(summary)
}

pub fn extract_message_db_videos(config: MessageDbExtractConfig) -> Result<ExtractSummary> {
    let resolved = ArchiveConfig {
        source_dir: config.account_dir.clone(),
        archive_dir: config.archive_dir,
        dry_run: config.dry_run,
        dat_options: config.dat_options,
        explain_unsupported: config.explain_unsupported,
    }
    .resolve()?;

    let db_source = resolve_message_db_source(&resolved.source_dir, config.message_db_dir)?;

    let video_root = resolved.source_dir.join("msg").join("video");
    let resources = query_video_resources(&db_source.resource_db)?;
    let message_dbs = message_db_paths(&db_source.message_dir)?;
    let resource_keys = resources
        .iter()
        .map(|resource| resource.key.clone())
        .collect::<Vec<_>>();
    let (video_messages, scanned_rows) =
        query_message_keys_for_type(&message_dbs, &resource_keys, 43)?;

    let run_id = format!("{}", now_epoch_ms());
    let mut summary = ExtractSummary::new(
        run_id.clone(),
        resolved.source_dir.clone(),
        resolved.archive_dir.clone(),
        resolved.dry_run,
    );
    summary.scanned_files = scanned_rows;
    if resolved.explain_unsupported {
        summary.enable_unsupported_explanation();
    }

    let mut conn = None;
    let mut manifest = None;
    if !resolved.dry_run {
        create_archive_dirs(&resolved.archive_dir)?;
        let opened = open_index(&resolved.archive_dir)?;
        summary.index_path = Some(index_path(&resolved.archive_dir));
        let writer = ManifestWriter::create(&resolved.archive_dir, &run_id, "extract-db-videos")?;
        summary.manifest_path = Some(writer.path().to_path_buf());
        conn = Some(opened);
        manifest = Some(writer);
    }

    for resource in resources {
        if !video_messages.contains(&resource.key) {
            continue;
        }

        summary.candidates += 1;
        let message_source = message_source_from_key(&resource.key);
        let result =
            match find_video_file(&video_root, &resource.file_md5, resource.key.create_time) {
                Some(video_path) => {
                    let extension = direct_video_extension(&video_path).unwrap_or("mp4");
                    process_direct_media_with_message_source(
                        &video_path,
                        extension,
                        &resolved.source_dir,
                        &resolved.archive_dir,
                        &run_id,
                        resolved.dry_run,
                        conn.as_ref(),
                        manifest.as_mut(),
                        "message_db_video",
                        "video",
                        Some(&message_source),
                    )
                }
                None => record_missing_video(
                    &resource,
                    &resolved.source_dir,
                    &run_id,
                    conn.as_ref(),
                    manifest.as_mut(),
                    &message_source,
                ),
            };
        apply_result(&mut summary, result)?;
    }

    if let Some(writer) = manifest.as_mut() {
        writer.flush()?;
    }
    summary.finish_unsupported_explanation();

    Ok(summary)
}

pub fn extract_message_db_files(config: MessageDbExtractConfig) -> Result<ExtractSummary> {
    let resolved = ArchiveConfig {
        source_dir: config.account_dir.clone(),
        archive_dir: config.archive_dir,
        dry_run: config.dry_run,
        dat_options: config.dat_options,
        explain_unsupported: config.explain_unsupported,
    }
    .resolve()?;

    let db_source = resolve_message_db_source(&resolved.source_dir, config.message_db_dir)?;

    let file_root = resolved.source_dir.join("msg").join("file");
    let resources = query_file_resources(&db_source.resource_db)?;
    let message_dbs = message_db_paths(&db_source.message_dir)?;
    let resource_keys = resources
        .iter()
        .map(|resource| resource.key.clone())
        .collect::<Vec<_>>();
    let (file_messages, scanned_rows) =
        query_message_keys_for_type(&message_dbs, &resource_keys, 49)?;

    let run_id = format!("{}", now_epoch_ms());
    let mut summary = ExtractSummary::new(
        run_id.clone(),
        resolved.source_dir.clone(),
        resolved.archive_dir.clone(),
        resolved.dry_run,
    );
    summary.scanned_files = scanned_rows;
    if resolved.explain_unsupported {
        summary.enable_unsupported_explanation();
    }

    let mut conn = None;
    let mut manifest = None;
    if !resolved.dry_run {
        create_archive_dirs(&resolved.archive_dir)?;
        let opened = open_index(&resolved.archive_dir)?;
        summary.index_path = Some(index_path(&resolved.archive_dir));
        let writer = ManifestWriter::create(&resolved.archive_dir, &run_id, "extract-db-files")?;
        summary.manifest_path = Some(writer.path().to_path_buf());
        conn = Some(opened);
        manifest = Some(writer);
    }

    for resource in resources {
        if !file_messages.contains(&resource.key) {
            continue;
        }

        summary.candidates += 1;
        let message_source = message_source_from_key(&resource.key);
        let result =
            match find_file_attachment(&file_root, &resource.file_name, resource.key.create_time) {
                Some(file_path) => {
                    let Some(extension) = direct_file_extension(&file_path) else {
                        continue;
                    };
                    process_direct_media_with_message_source(
                        &file_path,
                        &extension,
                        &resolved.source_dir,
                        &resolved.archive_dir,
                        &run_id,
                        resolved.dry_run,
                        conn.as_ref(),
                        manifest.as_mut(),
                        "message_db_file",
                        "file",
                        Some(&message_source),
                    )
                }
                None => record_missing_file(
                    &resource,
                    &resolved.source_dir,
                    &run_id,
                    conn.as_ref(),
                    manifest.as_mut(),
                    &message_source,
                ),
            };
        apply_result(&mut summary, result)?;
    }

    if let Some(writer) = manifest.as_mut() {
        writer.flush()?;
    }
    summary.finish_unsupported_explanation();

    Ok(summary)
}

pub fn inspect_message_db(config: MessageDbInspectConfig) -> Result<MessageDbInspection> {
    let account_dir = normalize_existing_dir_for_message_db(&config.account_dir)?;
    let message_db_dir_overridden = config.message_db_dir.is_some();
    let message_db_dir = resolve_message_db_dir_for_inspect(&account_dir, config.message_db_dir)?;
    Ok(inspect_message_db_dir(
        account_dir,
        message_db_dir,
        message_db_dir_overridden,
    ))
}

#[derive(Debug, Clone)]
struct MessageDbSource {
    message_dir: PathBuf,
    resource_db: PathBuf,
}

fn resolve_message_db_source(
    account_dir: &Path,
    message_db_dir: Option<PathBuf>,
) -> Result<MessageDbSource> {
    let message_dir = resolve_message_db_dir_for_extract(account_dir, message_db_dir)?;
    if !message_dir.is_dir() {
        return Err(ArchiverError::Other(format!(
            "message directory does not exist: {}",
            message_dir.display()
        )));
    }

    let resource_db = message_dir.join("message_resource.db");
    if !resource_db.is_file() {
        return Err(ArchiverError::Other(format!(
            "message_resource.db does not exist: {}",
            resource_db.display()
        )));
    }

    Ok(MessageDbSource {
        message_dir,
        resource_db,
    })
}

fn resolve_message_db_dir_for_extract(
    account_dir: &Path,
    message_db_dir: Option<PathBuf>,
) -> Result<PathBuf> {
    match message_db_dir {
        Some(path) => normalize_existing_dir_for_message_db(&path),
        None => Ok(account_dir.join("db_storage").join("message")),
    }
}

fn resolve_message_db_dir_for_inspect(
    account_dir: &Path,
    message_db_dir: Option<PathBuf>,
) -> Result<PathBuf> {
    match message_db_dir {
        Some(path) => {
            let abs = absolutize_message_db_path(&path)?;
            if abs.exists() {
                abs.canonicalize().with_path(&abs)
            } else {
                Ok(abs)
            }
        }
        None => Ok(account_dir.join("db_storage").join("message")),
    }
}

fn normalize_existing_dir_for_message_db(path: &Path) -> Result<PathBuf> {
    let abs = absolutize_message_db_path(path)?;
    let normalized = abs.canonicalize().with_path(&abs)?;
    if !normalized.is_dir() {
        return Err(ArchiverError::InvalidSource(normalized));
    }
    Ok(normalized)
}

fn absolutize_message_db_path(path: &Path) -> Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| ArchiverError::Io {
                path: PathBuf::from("."),
                source,
            })?
            .join(path)
    };
    Ok(abs)
}

fn inspect_message_db_dir(
    account_dir: PathBuf,
    message_db_dir: PathBuf,
    message_db_dir_overridden: bool,
) -> MessageDbInspection {
    let directory_status = if message_db_dir.is_dir() {
        MessageDbDirectoryStatus::Ready
    } else if message_db_dir.exists() {
        MessageDbDirectoryStatus::NotDirectory
    } else {
        MessageDbDirectoryStatus::Missing
    };

    let resource_db = inspect_db_file(
        message_db_dir.join("message_resource.db"),
        MessageDbFileRole::MessageResource,
    );
    let message_dbs = if directory_status == MessageDbDirectoryStatus::Ready {
        message_db_paths(&message_db_dir)
            .unwrap_or_default()
            .into_iter()
            .map(|path| inspect_db_file(path, MessageDbFileRole::Message))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let total_message_dbs = message_dbs.len();
    let readable_message_dbs = message_dbs
        .iter()
        .filter(|db| db.status == MessageDbFileStatus::ReadableSqlite)
        .count();
    let status = summarize_inspection_status(directory_status, &resource_db, &message_dbs);

    MessageDbInspection {
        account_dir,
        message_db_dir,
        message_db_dir_overridden,
        status,
        directory_status,
        resource_db,
        message_dbs,
        total_message_dbs,
        readable_message_dbs,
    }
}

fn summarize_inspection_status(
    directory_status: MessageDbDirectoryStatus,
    resource_db: &MessageDbFileInspection,
    message_dbs: &[MessageDbFileInspection],
) -> MessageDbInspectionStatus {
    if directory_status != MessageDbDirectoryStatus::Ready {
        return MessageDbInspectionStatus::Missing;
    }
    if resource_db.status == MessageDbFileStatus::Missing || message_dbs.is_empty() {
        return MessageDbInspectionStatus::Missing;
    }
    if resource_db.status == MessageDbFileStatus::ReadableSqlite
        && message_dbs
            .iter()
            .any(|db| db.status == MessageDbFileStatus::ReadableSqlite)
    {
        return MessageDbInspectionStatus::Ready;
    }
    if resource_db.status == MessageDbFileStatus::EncryptedOrNotSqlite
        || message_dbs
            .iter()
            .any(|db| db.status == MessageDbFileStatus::EncryptedOrNotSqlite)
    {
        return MessageDbInspectionStatus::EncryptedOrNotSqlite;
    }
    if resource_db.status == MessageDbFileStatus::UnsupportedSqlite
        || message_dbs
            .iter()
            .any(|db| db.status == MessageDbFileStatus::UnsupportedSqlite)
    {
        return MessageDbInspectionStatus::Unsupported;
    }
    MessageDbInspectionStatus::Error
}

fn inspect_db_file(path: PathBuf, role: MessageDbFileRole) -> MessageDbFileInspection {
    if !path.exists() {
        return MessageDbFileInspection {
            path,
            role,
            status: MessageDbFileStatus::Missing,
            sqlite_header: false,
            table_count: None,
            error: None,
        };
    }
    if !path.is_file() {
        return MessageDbFileInspection {
            path,
            role,
            status: MessageDbFileStatus::NotFile,
            sqlite_header: false,
            table_count: None,
            error: Some("path is not a file".to_string()),
        };
    }

    let sqlite_header = has_sqlite_header(&path).unwrap_or(false);
    match inspect_sqlite_file(&path, role) {
        Ok((table_count, has_expected_schema)) => {
            let status = if has_expected_schema {
                MessageDbFileStatus::ReadableSqlite
            } else {
                MessageDbFileStatus::UnsupportedSqlite
            };
            MessageDbFileInspection {
                path,
                role,
                status,
                sqlite_header,
                table_count: Some(table_count),
                error: None,
            }
        }
        Err(error) => {
            let status = classify_db_error(&error, sqlite_header);
            MessageDbFileInspection {
                path,
                role,
                status,
                sqlite_header,
                table_count: None,
                error: Some(error.to_string()),
            }
        }
    }
}

fn has_sqlite_header(path: &Path) -> std::io::Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut header = [0_u8; 16];
    let read = file.read(&mut header)?;
    Ok(read == header.len() && &header == b"SQLite format 3\0")
}

fn inspect_sqlite_file(path: &Path, role: MessageDbFileRole) -> Result<(u64, bool)> {
    let conn = open_readonly_db(path)?;
    let count = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'",
        [],
        |row| row.get::<_, u64>(0),
    )?;
    let has_expected_schema = match role {
        MessageDbFileRole::MessageResource => {
            table_exists(&conn, "ChatName2Id")? && table_exists(&conn, "MessageResourceInfo")?
        }
        MessageDbFileRole::Message => has_message_table(&conn)?,
    };
    Ok((count, has_expected_schema))
}

fn has_message_table(conn: &Connection) -> Result<bool> {
    let exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name LIKE 'Msg_%')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(exists)
}

fn classify_db_error(error: &ArchiverError, sqlite_header: bool) -> MessageDbFileStatus {
    match error {
        ArchiverError::Sqlite(sqlite_error) => match sqlite_error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::NotADatabase) => MessageDbFileStatus::EncryptedOrNotSqlite,
            Some(rusqlite::ErrorCode::DatabaseCorrupt) => MessageDbFileStatus::EncryptedOrNotSqlite,
            Some(_) if sqlite_header => MessageDbFileStatus::UnsupportedSqlite,
            Some(_) => MessageDbFileStatus::Error,
            None => MessageDbFileStatus::Error,
        },
        _ => MessageDbFileStatus::Error,
    }
}

fn message_source_from_key(key: &MessageKey) -> MessageSource {
    MessageSource {
        talker: Some(key.talker.clone()),
        sender: None,
        local_id: Some(key.local_id),
        create_time: Some(key.create_time),
    }
}

fn query_image_resources(resource_db: &Path) -> Result<Vec<ImageResource>> {
    let conn = open_readonly_db(resource_db)?;
    let has_detail_packed_info = table_has_column(&conn, "MessageResourceDetail", "packed_info")?;
    let sql = if has_detail_packed_info {
        r#"
        SELECT c.user_name,
               i.message_local_id,
               i.message_create_time,
               i.message_local_type,
               i.packed_info,
               d.packed_info
        FROM MessageResourceInfo i
        JOIN ChatName2Id c ON c.rowid = i.chat_id
        LEFT JOIN MessageResourceDetail d ON d.message_id = i.message_id
        WHERE c.user_name IS NOT NULL
          AND (i.message_local_type = 3 OR i.message_local_type % 4294967296 = 3)
        ORDER BY c.user_name, i.message_create_time, i.message_local_id, i.rowid
        "#
    } else {
        r#"
        SELECT c.user_name,
               i.message_local_id,
               i.message_create_time,
               i.message_local_type,
               i.packed_info,
               NULL
        FROM MessageResourceInfo i
        JOIN ChatName2Id c ON c.rowid = i.chat_id
        WHERE c.user_name IS NOT NULL
          AND (i.message_local_type = 3 OR i.message_local_type % 4294967296 = 3)
        ORDER BY c.user_name, i.message_create_time, i.message_local_id, i.rowid
        "#
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        let talker: String = row.get(0)?;
        let local_id: i64 = row.get(1)?;
        let create_time: i64 = row.get(2)?;
        let message_blob: Option<Vec<u8>> = row.get(4)?;
        let detail_blob: Option<Vec<u8>> = row.get(5)?;
        let file_md5 = message_blob
            .as_deref()
            .and_then(extract_md5_from_packed_info)
            .or_else(|| {
                detail_blob
                    .as_deref()
                    .and_then(extract_md5_from_packed_info)
            });

        Ok(file_md5.map(|file_md5| ImageResource {
            key: MessageKey {
                talker,
                local_id,
                create_time,
            },
            file_md5,
        }))
    })?;

    let mut seen = HashSet::<(MessageKey, String)>::new();
    let mut resources = Vec::new();
    for row in rows {
        let Some(resource) = row? else {
            continue;
        };
        if !is_md5_hex(&resource.file_md5) {
            continue;
        }
        if seen.insert((resource.key.clone(), resource.file_md5.clone())) {
            resources.push(resource);
        }
    }
    Ok(resources)
}

fn query_image_message_keys(
    message_dbs: &[PathBuf],
    resources: &[ImageResource],
) -> Result<(BTreeSet<MessageKey>, u64)> {
    let talkers = resources
        .iter()
        .map(|resource| resource.key.talker.as_str())
        .collect::<BTreeSet<_>>();
    let mut keys = BTreeSet::new();
    let mut scanned_rows = 0u64;

    for db_path in message_dbs {
        let conn = open_readonly_db(db_path)?;
        for talker in &talkers {
            let table_name = message_table_name(talker);
            if !table_exists(&conn, &table_name)? {
                continue;
            }

            let sql = format!(
                r#"
                SELECT local_id, create_time
                FROM {}
                WHERE local_type = 3 OR local_type % 4294967296 = 3
                "#,
                quote_identifier(&table_name)
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([], |row| {
                Ok(MessageKey {
                    talker: (*talker).to_string(),
                    local_id: row.get(0)?,
                    create_time: row.get(1)?,
                })
            })?;

            for row in rows {
                keys.insert(row?);
                scanned_rows += 1;
            }
        }
    }

    Ok((keys, scanned_rows))
}

fn query_message_keys_for_type(
    message_dbs: &[PathBuf],
    resource_keys: &[MessageKey],
    local_type: i64,
) -> Result<(BTreeSet<MessageKey>, u64)> {
    let talkers = resource_keys
        .iter()
        .map(|key| key.talker.as_str())
        .collect::<BTreeSet<_>>();
    let mut keys = BTreeSet::new();
    let mut scanned_rows = 0u64;

    for db_path in message_dbs {
        let conn = open_readonly_db(db_path)?;
        for talker in &talkers {
            let table_name = message_table_name(talker);
            if !table_exists(&conn, &table_name)? {
                continue;
            }

            let sql = format!(
                r#"
                SELECT local_id, create_time
                FROM {}
                WHERE local_type = ?1 OR local_type % 4294967296 = ?1
                "#,
                quote_identifier(&table_name)
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map([local_type], |row| {
                Ok(MessageKey {
                    talker: (*talker).to_string(),
                    local_id: row.get(0)?,
                    create_time: row.get(1)?,
                })
            })?;

            for row in rows {
                keys.insert(row?);
                scanned_rows += 1;
            }
        }
    }

    Ok((keys, scanned_rows))
}

fn query_video_resources(resource_db: &Path) -> Result<Vec<VideoResource>> {
    let conn = open_readonly_db(resource_db)?;
    let has_detail_type = table_has_column(&conn, "MessageResourceDetail", "type")?;
    let has_detail_packed_info = table_has_column(&conn, "MessageResourceDetail", "packed_info")?;
    let sql = match (has_detail_type, has_detail_packed_info) {
        (true, true) => {
            r#"
            SELECT c.user_name,
                   i.message_local_id,
                   i.message_create_time,
                   i.message_local_type,
                   i.packed_info,
                   d.type,
                   d.packed_info
            FROM MessageResourceInfo i
            JOIN ChatName2Id c ON c.rowid = i.chat_id
            LEFT JOIN MessageResourceDetail d ON d.message_id = i.message_id
            WHERE c.user_name IS NOT NULL
              AND (i.message_local_type = 43 OR i.message_local_type % 4294967296 = 43)
              AND (d.type IS NULL OR d.type % 65536 = 2)
            ORDER BY c.user_name, i.message_create_time, i.message_local_id, i.rowid
            "#
        }
        (true, false) => {
            r#"
            SELECT c.user_name,
                   i.message_local_id,
                   i.message_create_time,
                   i.message_local_type,
                   i.packed_info,
                   d.type,
                   NULL
            FROM MessageResourceInfo i
            JOIN ChatName2Id c ON c.rowid = i.chat_id
            LEFT JOIN MessageResourceDetail d ON d.message_id = i.message_id
            WHERE c.user_name IS NOT NULL
              AND (i.message_local_type = 43 OR i.message_local_type % 4294967296 = 43)
              AND (d.type IS NULL OR d.type % 65536 = 2)
            ORDER BY c.user_name, i.message_create_time, i.message_local_id, i.rowid
            "#
        }
        (false, true) => {
            r#"
            SELECT c.user_name,
                   i.message_local_id,
                   i.message_create_time,
                   i.message_local_type,
                   i.packed_info,
                   NULL,
                   d.packed_info
            FROM MessageResourceInfo i
            JOIN ChatName2Id c ON c.rowid = i.chat_id
            LEFT JOIN MessageResourceDetail d ON d.message_id = i.message_id
            WHERE c.user_name IS NOT NULL
              AND (i.message_local_type = 43 OR i.message_local_type % 4294967296 = 43)
            ORDER BY c.user_name, i.message_create_time, i.message_local_id, i.rowid
            "#
        }
        (false, false) => {
            r#"
            SELECT c.user_name,
                   i.message_local_id,
                   i.message_create_time,
                   i.message_local_type,
                   i.packed_info,
                   NULL,
                   NULL
            FROM MessageResourceInfo i
            JOIN ChatName2Id c ON c.rowid = i.chat_id
            WHERE c.user_name IS NOT NULL
              AND (i.message_local_type = 43 OR i.message_local_type % 4294967296 = 43)
            ORDER BY c.user_name, i.message_create_time, i.message_local_id, i.rowid
            "#
        }
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        let talker: String = row.get(0)?;
        let local_id: i64 = row.get(1)?;
        let create_time: i64 = row.get(2)?;
        let message_blob: Option<Vec<u8>> = row.get(4)?;
        let detail_blob: Option<Vec<u8>> = row.get(6)?;
        let file_md5 = message_blob
            .as_deref()
            .and_then(extract_md5_from_packed_info)
            .or_else(|| {
                detail_blob
                    .as_deref()
                    .and_then(extract_md5_from_packed_info)
            });

        Ok(file_md5.map(|file_md5| VideoResource {
            key: MessageKey {
                talker,
                local_id,
                create_time,
            },
            file_md5,
        }))
    })?;

    let mut seen = HashSet::<(MessageKey, String)>::new();
    let mut resources = Vec::new();
    for row in rows {
        let Some(resource) = row? else {
            continue;
        };
        if !is_md5_hex(&resource.file_md5) {
            continue;
        }
        if seen.insert((resource.key.clone(), resource.file_md5.clone())) {
            resources.push(resource);
        }
    }
    Ok(resources)
}

fn query_file_resources(resource_db: &Path) -> Result<Vec<FileResource>> {
    let conn = open_readonly_db(resource_db)?;
    let has_detail_packed_info = table_has_column(&conn, "MessageResourceDetail", "packed_info")?;
    let sql = if has_detail_packed_info {
        r#"
        SELECT c.user_name,
               i.message_local_id,
               i.message_create_time,
               i.message_local_type,
               i.packed_info,
               d.packed_info
        FROM MessageResourceInfo i
        JOIN ChatName2Id c ON c.rowid = i.chat_id
        LEFT JOIN MessageResourceDetail d ON d.message_id = i.message_id
        WHERE c.user_name IS NOT NULL
          AND (i.message_local_type = 49 OR i.message_local_type % 4294967296 = 49)
        ORDER BY c.user_name, i.message_create_time, i.message_local_id, i.rowid
        "#
    } else {
        r#"
        SELECT c.user_name,
               i.message_local_id,
               i.message_create_time,
               i.message_local_type,
               i.packed_info,
               NULL
        FROM MessageResourceInfo i
        JOIN ChatName2Id c ON c.rowid = i.chat_id
        WHERE c.user_name IS NOT NULL
          AND (i.message_local_type = 49 OR i.message_local_type % 4294967296 = 49)
        ORDER BY c.user_name, i.message_create_time, i.message_local_id, i.rowid
        "#
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |row| {
        let talker: String = row.get(0)?;
        let local_id: i64 = row.get(1)?;
        let create_time: i64 = row.get(2)?;
        let message_blob: Option<Vec<u8>> = row.get(4)?;
        let detail_blob: Option<Vec<u8>> = row.get(5)?;
        let strings = message_blob
            .as_deref()
            .into_iter()
            .chain(detail_blob.as_deref())
            .flat_map(extract_printable_strings)
            .collect::<Vec<_>>();
        let file_name = strings
            .into_iter()
            .find(|value| plausible_file_attachment_name(value));

        Ok(file_name.map(|file_name| FileResource {
            key: MessageKey {
                talker,
                local_id,
                create_time,
            },
            file_name,
        }))
    })?;

    let mut seen = HashSet::<(MessageKey, String)>::new();
    let mut resources = Vec::new();
    for row in rows {
        let Some(resource) = row? else {
            continue;
        };
        if seen.insert((resource.key.clone(), resource.file_name.clone())) {
            resources.push(resource);
        }
    }
    Ok(resources)
}

fn record_missing_dat(
    resource: &ImageResource,
    account_dir: &Path,
    run_id: &str,
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
    message_source: &MessageSource,
) -> Result<ScanOutcome> {
    let source_relative_path = format!(
        "db_storage/message:talker={}:local_id={}:create_time={}:md5={}",
        resource.key.talker, resource.key.local_id, resource.key.create_time, resource.file_md5
    );
    let source_path = account_dir
        .join(&source_relative_path)
        .to_string_lossy()
        .to_string();
    let event = ManifestEvent {
        event: "media_item".to_string(),
        run_id: run_id.to_string(),
        timestamp_epoch_ms: now_epoch_ms(),
        source_path,
        source_relative_path,
        source_kind: "message_db_image".to_string(),
        media_type: "image".to_string(),
        message_talker: message_source.talker.clone(),
        message_sender: message_source.sender.clone(),
        message_local_id: message_source.local_id,
        message_create_time: message_source.create_time,
        decoder: None,
        action: ScanAction::Failed,
        archive_path: None,
        sha256: None,
        size_bytes: None,
        extension: None,
        decrypt_status: "source_missing".to_string(),
        verify_status: "failed".to_string(),
        error: Some("local_dat_not_found".to_string()),
    };
    persist(conn, manifest, &event)?;
    Ok(ScanOutcome::new(ScanAction::Failed))
}

fn record_missing_video(
    resource: &VideoResource,
    account_dir: &Path,
    run_id: &str,
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
    message_source: &MessageSource,
) -> Result<ScanOutcome> {
    let source_relative_path = format!(
        "db_storage/message:talker={}:local_id={}:create_time={}:video_md5={}",
        resource.key.talker, resource.key.local_id, resource.key.create_time, resource.file_md5
    );
    let source_path = account_dir
        .join(&source_relative_path)
        .to_string_lossy()
        .to_string();
    let event = ManifestEvent {
        event: "media_item".to_string(),
        run_id: run_id.to_string(),
        timestamp_epoch_ms: now_epoch_ms(),
        source_path,
        source_relative_path,
        source_kind: "message_db_video".to_string(),
        media_type: "video".to_string(),
        message_talker: message_source.talker.clone(),
        message_sender: message_source.sender.clone(),
        message_local_id: message_source.local_id,
        message_create_time: message_source.create_time,
        decoder: None,
        action: ScanAction::Failed,
        archive_path: None,
        sha256: None,
        size_bytes: None,
        extension: Some("mp4".to_string()),
        decrypt_status: "source_missing".to_string(),
        verify_status: "failed".to_string(),
        error: Some("local_video_not_found".to_string()),
    };
    persist(conn, manifest, &event)?;
    Ok(ScanOutcome::new(ScanAction::Failed))
}

fn record_missing_file(
    resource: &FileResource,
    account_dir: &Path,
    run_id: &str,
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
    message_source: &MessageSource,
) -> Result<ScanOutcome> {
    let source_relative_path = format!(
        "db_storage/message:talker={}:local_id={}:create_time={}:file_name={}",
        resource.key.talker, resource.key.local_id, resource.key.create_time, resource.file_name
    );
    let source_path = account_dir
        .join(&source_relative_path)
        .to_string_lossy()
        .to_string();
    let event = ManifestEvent {
        event: "media_item".to_string(),
        run_id: run_id.to_string(),
        timestamp_epoch_ms: now_epoch_ms(),
        source_path,
        source_relative_path,
        source_kind: "message_db_file".to_string(),
        media_type: "file".to_string(),
        message_talker: message_source.talker.clone(),
        message_sender: message_source.sender.clone(),
        message_local_id: message_source.local_id,
        message_create_time: message_source.create_time,
        decoder: None,
        action: ScanAction::Failed,
        archive_path: None,
        sha256: None,
        size_bytes: None,
        extension: direct_file_extension(Path::new(&resource.file_name)),
        decrypt_status: "source_missing".to_string(),
        verify_status: "failed".to_string(),
        error: Some("local_file_not_found".to_string()),
    };
    persist(conn, manifest, &event)?;
    Ok(ScanOutcome::new(ScanAction::Failed))
}

fn find_video_file(video_root: &Path, file_md5: &str, create_time: i64) -> Option<PathBuf> {
    for month in month_candidates(create_time) {
        let video_dir = video_root.join(month);
        if let Some(path) = pick_video_file(&video_dir, file_md5) {
            return Some(path);
        }
    }

    let mut month_dirs = fs::read_dir(video_root)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    month_dirs.sort();

    for month_dir in month_dirs {
        if let Some(path) = pick_video_file(&month_dir, file_md5) {
            return Some(path);
        }
    }
    None
}

fn pick_video_file(video_dir: &Path, file_md5: &str) -> Option<PathBuf> {
    if !video_dir.is_dir() {
        return None;
    }

    for suffix in [".mp4", ".mov", ".m4v"] {
        let path = video_dir.join(format!("{file_md5}{suffix}"));
        if path.is_file() {
            return Some(path);
        }
    }

    let mut candidates = fs::read_dir(video_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            let Some(extension) = direct_video_extension(path) else {
                return false;
            };
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(|stem| stem.eq_ignore_ascii_case(file_md5) && !extension.is_empty())
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.into_iter().next()
}

fn find_file_attachment(file_root: &Path, file_name: &str, create_time: i64) -> Option<PathBuf> {
    if !safe_leaf_name(file_name) {
        return None;
    }

    for month in month_candidates(create_time) {
        let path = file_root.join(month).join(file_name);
        if path.is_file() {
            return Some(path);
        }
    }

    let mut month_dirs = fs::read_dir(file_root)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    month_dirs.sort();

    for month_dir in month_dirs {
        let path = month_dir.join(file_name);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn message_db_paths(message_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(message_dir).with_path(message_dir)? {
        let entry = entry.with_path(message_dir)?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if file_name == "message_resource.db" {
            continue;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("db"))
            .unwrap_or(false)
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn open_readonly_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.pragma_update(None, "query_only", "ON")?;
    Ok(conn)
}

fn table_exists(conn: &Connection, table_name: &str) -> Result<bool> {
    let exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![table_name],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(exists)
}

fn table_has_column(conn: &Connection, table_name: &str, column_name: &str) -> Result<bool> {
    if !table_exists(conn, table_name)? {
        return Ok(false);
    }

    let sql = format!("PRAGMA table_info({})", quote_identifier(table_name));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row?.eq_ignore_ascii_case(column_name) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn message_table_name(talker: &str) -> String {
    format!("Msg_{}", talker_hash(talker))
}

fn talker_hash(talker: &str) -> String {
    format!("{:x}", md5::compute(talker.as_bytes()))
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn find_dat_file(
    attach_root: &Path,
    talker: &str,
    file_md5: &str,
    create_time: i64,
) -> Option<PathBuf> {
    let chat_dir = attach_root.join(talker_hash(talker));
    if !chat_dir.is_dir() {
        return None;
    }

    for month in month_candidates(create_time) {
        let img_dir = chat_dir.join(month).join("Img");
        if let Some(path) = pick_best_dat(&img_dir, file_md5) {
            return Some(path);
        }
    }

    let mut month_dirs = fs::read_dir(&chat_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    month_dirs.sort();

    for month_dir in month_dirs {
        let img_dir = month_dir.join("Img");
        if let Some(path) = pick_best_dat(&img_dir, file_md5) {
            return Some(path);
        }
    }
    None
}

fn pick_best_dat(img_dir: &Path, file_md5: &str) -> Option<PathBuf> {
    if !img_dir.is_dir() {
        return None;
    }

    for suffix in [".dat", "_h.dat", "_W.dat", "_w.dat", "_t.dat"] {
        let path = img_dir.join(format!("{file_md5}{suffix}"));
        if path.is_file() {
            return Some(path);
        }
    }

    let mut candidates = fs::read_dir(img_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| {
                    let lower = name.to_ascii_lowercase();
                    lower.starts_with(file_md5) && lower.ends_with(".dat")
                })
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let rank = if name == format!("{file_md5}.dat") {
            0
        } else if name.contains("_h.") {
            1
        } else if name.contains("_w.") {
            2
        } else if name.contains("_t.") {
            4
        } else {
            3
        };
        let reverse_size = fs::metadata(path)
            .map(|metadata| u64::MAX - metadata.len())
            .unwrap_or(u64::MAX);
        (rank, reverse_size, name)
    });
    candidates.into_iter().next()
}

fn month_candidates(unix_ts: i64) -> Vec<String> {
    let Some((year, month)) = unix_month_utc(unix_ts) else {
        return Vec::new();
    };

    [-1, 0, 1]
        .into_iter()
        .map(|offset| add_month(year, month, offset))
        .map(|(year, month)| format!("{year:04}-{month:02}"))
        .collect()
}

fn add_month(year: i32, month: u32, offset: i32) -> (i32, u32) {
    let zero_based = year * 12 + month as i32 - 1 + offset;
    let next_year = zero_based.div_euclid(12);
    let next_month = zero_based.rem_euclid(12) as u32 + 1;
    (next_year, next_month)
}

fn unix_month_utc(unix_ts: i64) -> Option<(i32, u32)> {
    let days = unix_ts.div_euclid(86_400);
    let (year, month, _day) = civil_from_days(days)?;
    Some((year, month))
}

fn civil_from_days(days_since_epoch: i64) -> Option<(i32, u32, u32)> {
    let shifted = days_since_epoch.checked_add(719_468)?;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    }
    .div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524
        - day_of_era / 146_096)
        .div_euclid(365);
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2).div_euclid(153);
    let day = day_of_year - (153 * month_prime + 2).div_euclid(5) + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }

    let year = i32::try_from(year).ok()?;
    let month = u32::try_from(month).ok()?;
    let day = u32::try_from(day).ok()?;
    Some((year, month, day))
}

fn extract_md5_from_packed_info(blob: &[u8]) -> Option<String> {
    const MARKER: &[u8; 4] = &[0x12, 0x22, 0x0a, 0x20];

    if let Some(pos) = blob
        .windows(MARKER.len())
        .position(|window| window == MARKER)
    {
        let start = pos + MARKER.len();
        if start + 32 <= blob.len() {
            let candidate = &blob[start..start + 32];
            if let Ok(value) = std::str::from_utf8(candidate) {
                if is_md5_hex(value) {
                    return Some(value.to_ascii_lowercase());
                }
            }
        }
    }

    if blob.len() < 32 {
        return None;
    }
    for start in 0..=blob.len() - 32 {
        let candidate = &blob[start..start + 32];
        if let Ok(value) = std::str::from_utf8(candidate) {
            if is_md5_hex(value) {
                return Some(value.to_ascii_lowercase());
            }
        }
    }
    None
}

fn extract_printable_strings(blob: &[u8]) -> Vec<String> {
    let mut strings = Vec::new();
    let mut start = None;
    for (index, byte) in blob.iter().copied().enumerate() {
        let readable = byte == b' ' || byte >= 0x21 && byte != 0x7f;
        match (start, readable) {
            (None, true) => start = Some(index),
            (Some(begin), false) => {
                push_printable_string(blob, begin, index, &mut strings);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        push_printable_string(blob, begin, blob.len(), &mut strings);
    }

    let mut seen = HashSet::new();
    strings
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn push_printable_string(blob: &[u8], begin: usize, end: usize, strings: &mut Vec<String>) {
    if end.saturating_sub(begin) < 2 || end.saturating_sub(begin) > 512 {
        return;
    }
    let Ok(value) = std::str::from_utf8(&blob[begin..end]) else {
        return;
    };
    let value = value.trim();
    if !value.is_empty() {
        strings.push(value.to_string());
    }
}

fn safe_leaf_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    if name.contains('\0') || name.contains('/') || name.contains('\\') {
        return false;
    }
    Path::new(name)
        .file_name()
        .and_then(|leaf| leaf.to_str())
        .map(|leaf| leaf == name)
        .unwrap_or(false)
}

fn plausible_file_attachment_name(name: &str) -> bool {
    if !safe_leaf_name(name) || is_md5_hex(name) {
        return false;
    }
    direct_file_extension(Path::new(name)).is_some()
}

fn is_md5_hex(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::sha256_file;
    use crate::status::archive_status;
    use crate::verify::verify_archive;
    use rusqlite::Connection;

    #[test]
    fn extracts_message_db_images_without_touching_source() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        let archive = tmp.path().join("archive");
        let talker = "room@example";
        let local_id = 7;
        let create_time = 1_779_472_800;
        let file_md5 = "fe8776339cd67e6023d7e47b97b073a0";
        create_fixture_account(&account, talker, local_id, create_time, file_md5, true);

        let dat_path = account
            .join("msg")
            .join("attach")
            .join(talker_hash(talker))
            .join("2026-05")
            .join("Img")
            .join(format!("{file_md5}.dat"));
        let db_path = account
            .join("db_storage")
            .join("message")
            .join("message_0.db");
        let dat_hash_before = sha256_file(&dat_path).unwrap();
        let db_hash_before = sha256_file(&db_path).unwrap();

        let summary = extract_message_db_images(MessageDbExtractConfig {
            account_dir: account.clone(),
            message_db_dir: None,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);
        assert_eq!(sha256_file(&dat_path).unwrap(), dat_hash_before);
        assert_eq!(sha256_file(&db_path).unwrap(), db_hash_before);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (stored_talker, stored_sender, stored_local_id, stored_create_time): (
            String,
            Option<String>,
            i64,
            i64,
        ) = conn
            .query_row(
                r#"
                SELECT message_talker, message_sender, message_local_id, message_create_time
                FROM media_items
                WHERE source_kind = 'message_db_image'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(stored_talker, talker);
        assert_eq!(stored_sender, None);
        assert_eq!(stored_local_id, local_id);
        assert_eq!(stored_create_time, create_time);

        let manifest_path = summary.manifest_path.clone().unwrap();
        let manifest = fs::read_to_string(manifest_path).unwrap();
        let event = manifest
            .lines()
            .map(|line| serde_json::from_str::<ManifestEvent>(line).unwrap())
            .find(|event| event.source_kind == "message_db_image")
            .unwrap();
        assert_eq!(event.message_talker.as_deref(), Some(talker));
        assert_eq!(event.message_sender, None);
        assert_eq!(event.message_local_id, Some(local_id));
        assert_eq!(event.message_create_time, Some(create_time));

        let verify = verify_archive(&archive).unwrap();
        assert_eq!(verify.checked, 1);
        assert_eq!(verify.ok, 1);

        let status = archive_status(&archive).unwrap();
        assert_eq!(status.archived_records, 1);
        assert_eq!(status.failed_records, 0);
    }

    #[test]
    fn dry_run_reads_message_db_but_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        let archive = tmp.path().join("archive");
        create_fixture_account(
            &account,
            "wxid_friend",
            9,
            1_779_472_800,
            "00112233445566778899aabbccddeeff",
            true,
        );

        let summary = extract_message_db_images(MessageDbExtractConfig {
            account_dir: account,
            message_db_dir: None,
            archive_dir: archive.clone(),
            dry_run: true,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.would_archive, 1);
        assert!(!archive.exists());
    }

    #[test]
    fn missing_local_dat_is_recorded_as_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        let archive = tmp.path().join("archive");
        create_fixture_account(
            &account,
            "wxid_friend",
            11,
            1_779_472_800,
            "abcdefabcdefabcdefabcdefabcdefab",
            false,
        );

        let summary = extract_message_db_images(MessageDbExtractConfig {
            account_dir: account,
            message_db_dir: None,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.failed, 1);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (message_talker, message_local_id, message_create_time): (String, i64, i64) = conn
            .query_row(
                r#"
                SELECT message_talker, message_local_id, message_create_time
                FROM media_items
                WHERE verify_status = 'failed'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(message_talker, "wxid_friend");
        assert_eq!(message_local_id, 11);
        assert_eq!(message_create_time, 1_779_472_800);

        let status = archive_status(&archive).unwrap();
        assert_eq!(status.total_records, 1);
        assert_eq!(status.failed_records, 1);
    }

    #[test]
    fn extracts_message_db_videos_with_message_source_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        let archive = tmp.path().join("archive");
        let talker = "room@example";
        let local_id = 21;
        let create_time = 1_779_472_800;
        let file_md5 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        create_video_fixture_account(&account, talker, local_id, create_time, file_md5, true);

        let video_path = account
            .join("msg")
            .join("video")
            .join("2026-05")
            .join(format!("{file_md5}.mp4"));
        let db_path = account
            .join("db_storage")
            .join("message")
            .join("message_0.db");
        let video_hash_before = sha256_file(&video_path).unwrap();
        let db_hash_before = sha256_file(&db_path).unwrap();

        let summary = extract_message_db_videos(MessageDbExtractConfig {
            account_dir: account.clone(),
            message_db_dir: None,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);
        assert_eq!(sha256_file(&video_path).unwrap(), video_hash_before);
        assert_eq!(sha256_file(&db_path).unwrap(), db_hash_before);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (source_kind, media_type, message_talker, message_local_id, message_create_time): (
            String,
            String,
            String,
            i64,
            i64,
        ) = conn
            .query_row(
                r#"
                SELECT source_kind, media_type, message_talker, message_local_id, message_create_time
                FROM media_items
                WHERE source_kind = 'message_db_video'
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
        assert_eq!(source_kind, "message_db_video");
        assert_eq!(media_type, "video");
        assert_eq!(message_talker, talker);
        assert_eq!(message_local_id, local_id);
        assert_eq!(message_create_time, create_time);

        let verify = verify_archive(&archive).unwrap();
        assert_eq!(verify.checked, 1);
        assert_eq!(verify.ok, 1);
    }

    #[test]
    fn missing_local_video_is_recorded_as_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        let archive = tmp.path().join("archive");
        create_video_fixture_account(
            &account,
            "wxid_friend",
            22,
            1_779_472_800,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            false,
        );

        let summary = extract_message_db_videos(MessageDbExtractConfig {
            account_dir: account,
            message_db_dir: None,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.failed, 1);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (source_kind, error, message_local_id): (String, String, i64) = conn
            .query_row(
                r#"
                SELECT source_kind, error, message_local_id
                FROM media_items
                WHERE verify_status = 'failed'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(source_kind, "message_db_video");
        assert_eq!(error, "local_video_not_found");
        assert_eq!(message_local_id, 22);
    }

    #[test]
    fn extracts_message_db_files_with_message_source_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        let archive = tmp.path().join("archive");
        let talker = "room@example";
        let local_id = 31;
        let create_time = 1_779_472_800;
        let file_name = "项目报告.pdf";
        create_file_fixture_account(&account, talker, local_id, create_time, file_name, true);

        let file_path = account
            .join("msg")
            .join("file")
            .join("2026-05")
            .join(file_name);
        let db_path = account
            .join("db_storage")
            .join("message")
            .join("message_0.db");
        let file_hash_before = sha256_file(&file_path).unwrap();
        let db_hash_before = sha256_file(&db_path).unwrap();

        let summary = extract_message_db_files(MessageDbExtractConfig {
            account_dir: account.clone(),
            message_db_dir: None,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);
        assert_eq!(sha256_file(&file_path).unwrap(), file_hash_before);
        assert_eq!(sha256_file(&db_path).unwrap(), db_hash_before);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (
            source_kind,
            media_type,
            extension,
            message_talker,
            message_local_id,
            message_create_time,
        ): (String, String, String, String, i64, i64) = conn
            .query_row(
                r#"
                SELECT source_kind, media_type, extension, message_talker, message_local_id, message_create_time
                FROM media_items
                WHERE source_kind = 'message_db_file'
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
                    ))
                },
            )
            .unwrap();
        assert_eq!(source_kind, "message_db_file");
        assert_eq!(media_type, "file");
        assert_eq!(extension, "pdf");
        assert_eq!(message_talker, talker);
        assert_eq!(message_local_id, local_id);
        assert_eq!(message_create_time, create_time);

        let verify = verify_archive(&archive).unwrap();
        assert_eq!(verify.checked, 1);
        assert_eq!(verify.ok, 1);
    }

    #[test]
    fn missing_local_file_is_recorded_as_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        let archive = tmp.path().join("archive");
        create_file_fixture_account(
            &account,
            "wxid_friend",
            32,
            1_779_472_800,
            "missing.docx",
            false,
        );

        let summary = extract_message_db_files(MessageDbExtractConfig {
            account_dir: account,
            message_db_dir: None,
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.failed, 1);

        let conn = Connection::open(archive.join("index.sqlite")).unwrap();
        let (source_kind, error, extension, message_local_id): (String, String, String, i64) = conn
            .query_row(
                r#"
                SELECT source_kind, error, extension, message_local_id
                FROM media_items
                WHERE verify_status = 'failed'
                "#,
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(source_kind, "message_db_file");
        assert_eq!(error, "local_file_not_found");
        assert_eq!(extension, "docx");
        assert_eq!(message_local_id, 32);
    }

    #[test]
    fn inspect_message_db_reports_ready_for_plain_sqlite_fixture() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        create_video_fixture_account(
            &account,
            "room@example",
            41,
            1_779_472_800,
            "cccccccccccccccccccccccccccccccc",
            true,
        );

        let inspection = inspect_message_db(MessageDbInspectConfig {
            account_dir: account.clone(),
            message_db_dir: None,
        })
        .unwrap();

        assert_eq!(inspection.account_dir, account.canonicalize().unwrap());
        assert_eq!(inspection.status, MessageDbInspectionStatus::Ready);
        assert_eq!(inspection.directory_status, MessageDbDirectoryStatus::Ready);
        assert!(!inspection.message_db_dir_overridden);
        assert_eq!(
            inspection.resource_db.status,
            MessageDbFileStatus::ReadableSqlite
        );
        assert!(inspection.resource_db.sqlite_header);
        assert_eq!(inspection.total_message_dbs, 1);
        assert_eq!(inspection.readable_message_dbs, 1);
        assert_eq!(
            inspection.message_dbs[0].status,
            MessageDbFileStatus::ReadableSqlite
        );
    }

    #[test]
    fn inspect_message_db_reports_encrypted_or_not_sqlite() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        let message_dir = account.join("db_storage").join("message");
        fs::create_dir_all(&message_dir).unwrap();
        fs::write(message_dir.join("message_resource.db"), b"not sqlite").unwrap();
        fs::write(message_dir.join("message_0.db"), b"not sqlite").unwrap();

        let inspection = inspect_message_db(MessageDbInspectConfig {
            account_dir: account,
            message_db_dir: None,
        })
        .unwrap();

        assert_eq!(
            inspection.status,
            MessageDbInspectionStatus::EncryptedOrNotSqlite
        );
        assert_eq!(
            inspection.resource_db.status,
            MessageDbFileStatus::EncryptedOrNotSqlite
        );
        assert!(!inspection.resource_db.sqlite_header);
        assert_eq!(inspection.total_message_dbs, 1);
        assert_eq!(inspection.readable_message_dbs, 0);
        assert_eq!(
            inspection.message_dbs[0].status,
            MessageDbFileStatus::EncryptedOrNotSqlite
        );
    }

    #[test]
    fn extract_message_db_videos_can_use_external_message_db_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        let archive = tmp.path().join("archive");
        let decrypted_message_dir = tmp.path().join("decrypted-message");
        let talker = "room@example";
        let local_id = 42;
        let create_time = 1_779_472_800;
        let file_md5 = "dddddddddddddddddddddddddddddddd";
        create_video_fixture_account(&account, talker, local_id, create_time, file_md5, true);
        copy_dir(
            &account.join("db_storage").join("message"),
            &decrypted_message_dir,
        );
        fs::remove_dir_all(account.join("db_storage")).unwrap();

        let video_path = account
            .join("msg")
            .join("video")
            .join("2026-05")
            .join(format!("{file_md5}.mp4"));
        let video_hash_before = sha256_file(&video_path).unwrap();

        let summary = extract_message_db_videos(MessageDbExtractConfig {
            account_dir: account.clone(),
            message_db_dir: Some(decrypted_message_dir.clone()),
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.archived, 1);
        assert_eq!(sha256_file(&video_path).unwrap(), video_hash_before);

        let inspection = inspect_message_db(MessageDbInspectConfig {
            account_dir: account,
            message_db_dir: Some(decrypted_message_dir),
        })
        .unwrap();
        assert!(inspection.message_db_dir_overridden);
        assert_eq!(inspection.status, MessageDbInspectionStatus::Ready);

        let verify = verify_archive(&archive).unwrap();
        assert_eq!(verify.checked, 1);
        assert_eq!(verify.ok, 1);
    }

    #[test]
    fn file_attachment_names_must_be_safe_leaf_names() {
        assert!(plausible_file_attachment_name("report.pdf"));
        assert!(plausible_file_attachment_name("2026 final.xlsx"));
        assert!(plausible_file_attachment_name("项目报告.pdf"));
        assert!(!plausible_file_attachment_name(""));
        assert!(!plausible_file_attachment_name("../report.pdf"));
        assert!(!plausible_file_attachment_name("a/report.pdf"));
        assert!(!plausible_file_attachment_name("a\\report.pdf"));
        assert!(!plausible_file_attachment_name("no_extension"));
        assert!(!plausible_file_attachment_name(
            "00112233445566778899aabbccddeeff"
        ));
    }

    #[test]
    fn extracts_md5_from_marker_and_fallback() {
        let mut marker_blob = vec![0x12, 0x22, 0x0a, 0x20];
        marker_blob.extend_from_slice(b"DEADBEEFCAFEBABE1234567890ABCDEF");
        assert_eq!(
            extract_md5_from_packed_info(&marker_blob),
            Some("deadbeefcafebabe1234567890abcdef".to_string())
        );

        let mut fallback_blob = b"prefix".to_vec();
        fallback_blob.extend_from_slice(b"00112233445566778899aabbccddeeff");
        assert_eq!(
            extract_md5_from_packed_info(&fallback_blob),
            Some("00112233445566778899aabbccddeeff".to_string())
        );
    }

    fn create_fixture_account(
        account: &Path,
        talker: &str,
        local_id: i64,
        create_time: i64,
        file_md5: &str,
        create_dat: bool,
    ) {
        let message_dir = account.join("db_storage").join("message");
        fs::create_dir_all(&message_dir).unwrap();
        create_message_db(
            &message_dir.join("message_0.db"),
            talker,
            local_id,
            create_time,
        );
        create_resource_db(
            &message_dir.join("message_resource.db"),
            talker,
            local_id,
            create_time,
            file_md5,
        );

        let img_dir = account
            .join("msg")
            .join("attach")
            .join(talker_hash(talker))
            .join("2026-05")
            .join("Img");
        fs::create_dir_all(&img_dir).unwrap();
        if create_dat {
            let decoded = b"\x89PNG\r\nsynthetic-message-db-png";
            let encrypted = decoded.iter().map(|byte| byte ^ 0x88).collect::<Vec<_>>();
            fs::write(img_dir.join(format!("{file_md5}.dat")), encrypted).unwrap();
        }
    }

    fn create_message_db(path: &Path, talker: &str, local_id: i64, create_time: i64) {
        create_message_db_with_type(path, talker, local_id, create_time, 3);
    }

    fn create_message_db_with_type(
        path: &Path,
        talker: &str,
        local_id: i64,
        create_time: i64,
        local_type: i64,
    ) {
        let conn = Connection::open(path).unwrap();
        let table_name = message_table_name(talker);
        conn.execute(
            &format!(
                "CREATE TABLE {} (local_id INTEGER, create_time INTEGER, local_type INTEGER)",
                quote_identifier(&table_name)
            ),
            [],
        )
        .unwrap();
        conn.execute(
            &format!(
                "INSERT INTO {} (local_id, create_time, local_type) VALUES (?1, ?2, ?3)",
                quote_identifier(&table_name)
            ),
            params![local_id, create_time, (1_i64 << 32) + local_type],
        )
        .unwrap();
    }

    fn create_resource_db(
        path: &Path,
        talker: &str,
        local_id: i64,
        create_time: i64,
        file_md5: &str,
    ) {
        let conn = Connection::open(path).unwrap();
        conn.execute("CREATE TABLE ChatName2Id (user_name TEXT)", [])
            .unwrap();
        conn.execute(
            "CREATE TABLE MessageResourceInfo (
                message_id INTEGER,
                chat_id INTEGER,
                message_local_id INTEGER,
                message_create_time INTEGER,
                message_local_type INTEGER,
                packed_info BLOB
            )",
            [],
        )
        .unwrap();
        if talker == "wxid_friend" {
            conn.execute(
                "CREATE TABLE MessageResourceDetail (
                    message_id INTEGER,
                    size INTEGER
                )",
                [],
            )
            .unwrap();
        } else {
            conn.execute(
                "CREATE TABLE MessageResourceDetail (
                    message_id INTEGER,
                    packed_info BLOB
                )",
                [],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO ChatName2Id(rowid, user_name) VALUES (?1, ?2)",
            params![1, talker],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO MessageResourceInfo (
                message_id,
                chat_id,
                message_local_id,
                message_create_time,
                message_local_type,
                packed_info
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                10,
                1,
                local_id,
                create_time,
                (1_i64 << 32) + 3,
                packed_md5(file_md5)
            ],
        )
        .unwrap();
        if talker == "wxid_friend" {
            conn.execute(
                "INSERT INTO MessageResourceDetail(message_id, size) VALUES (?1, ?2)",
                params![10, 1024],
            )
            .unwrap();
        } else {
            conn.execute(
                "INSERT INTO MessageResourceDetail(message_id, packed_info) VALUES (?1, ?2)",
                params![10, packed_md5(file_md5)],
            )
            .unwrap();
        }
    }

    fn packed_md5(file_md5: &str) -> Vec<u8> {
        let mut blob = vec![0x12, 0x22, 0x0a, 0x20];
        blob.extend_from_slice(file_md5.as_bytes());
        blob
    }

    fn create_video_fixture_account(
        account: &Path,
        talker: &str,
        local_id: i64,
        create_time: i64,
        file_md5: &str,
        create_video: bool,
    ) {
        let message_dir = account.join("db_storage").join("message");
        fs::create_dir_all(&message_dir).unwrap();
        create_message_db_with_type(
            &message_dir.join("message_0.db"),
            talker,
            local_id,
            create_time,
            43,
        );
        create_video_resource_db(
            &message_dir.join("message_resource.db"),
            talker,
            local_id,
            create_time,
            file_md5,
        );

        let video_dir = account.join("msg").join("video").join("2026-05");
        fs::create_dir_all(&video_dir).unwrap();
        if create_video {
            fs::write(
                video_dir.join(format!("{file_md5}.mp4")),
                b"synthetic-video",
            )
            .unwrap();
        }
    }

    fn create_video_resource_db(
        path: &Path,
        talker: &str,
        local_id: i64,
        create_time: i64,
        file_md5: &str,
    ) {
        let conn = Connection::open(path).unwrap();
        conn.execute("CREATE TABLE ChatName2Id (user_name TEXT)", [])
            .unwrap();
        conn.execute(
            "CREATE TABLE MessageResourceInfo (
                message_id INTEGER,
                chat_id INTEGER,
                message_local_id INTEGER,
                message_create_time INTEGER,
                message_local_type INTEGER,
                packed_info BLOB
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE MessageResourceDetail (
                message_id INTEGER,
                type INTEGER,
                packed_info BLOB
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ChatName2Id(rowid, user_name) VALUES (?1, ?2)",
            params![1, talker],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO MessageResourceInfo (
                message_id,
                chat_id,
                message_local_id,
                message_create_time,
                message_local_type,
                packed_info
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                20,
                1,
                local_id,
                create_time,
                (1_i64 << 32) + 43,
                Vec::<u8>::new()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO MessageResourceDetail(message_id, type, packed_info) VALUES (?1, ?2, ?3)",
            params![20, 2, packed_md5(file_md5)],
        )
        .unwrap();
    }

    fn create_file_fixture_account(
        account: &Path,
        talker: &str,
        local_id: i64,
        create_time: i64,
        file_name: &str,
        create_file: bool,
    ) {
        let message_dir = account.join("db_storage").join("message");
        fs::create_dir_all(&message_dir).unwrap();
        create_message_db_with_type(
            &message_dir.join("message_0.db"),
            talker,
            local_id,
            create_time,
            49,
        );
        create_file_resource_db(
            &message_dir.join("message_resource.db"),
            talker,
            local_id,
            create_time,
            file_name,
        );

        let file_dir = account.join("msg").join("file").join("2026-05");
        fs::create_dir_all(&file_dir).unwrap();
        if create_file {
            fs::write(file_dir.join(file_name), b"synthetic-file-attachment").unwrap();
        }
    }

    fn create_file_resource_db(
        path: &Path,
        talker: &str,
        local_id: i64,
        create_time: i64,
        file_name: &str,
    ) {
        let conn = Connection::open(path).unwrap();
        conn.execute("CREATE TABLE ChatName2Id (user_name TEXT)", [])
            .unwrap();
        conn.execute(
            "CREATE TABLE MessageResourceInfo (
                message_id INTEGER,
                chat_id INTEGER,
                message_local_id INTEGER,
                message_create_time INTEGER,
                message_local_type INTEGER,
                packed_info BLOB
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "CREATE TABLE MessageResourceDetail (
                message_id INTEGER,
                packed_info BLOB
            )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ChatName2Id(rowid, user_name) VALUES (?1, ?2)",
            params![1, talker],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO MessageResourceInfo (
                message_id,
                chat_id,
                message_local_id,
                message_create_time,
                message_local_type,
                packed_info
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                30,
                1,
                local_id,
                create_time,
                (1_i64 << 32) + 49,
                packed_file_name(file_name)
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO MessageResourceDetail(message_id, packed_info) VALUES (?1, ?2)",
            params![30, packed_file_name(file_name)],
        )
        .unwrap();
    }

    fn packed_file_name(file_name: &str) -> Vec<u8> {
        let mut blob = b"prefix\0".to_vec();
        blob.extend_from_slice(b"00112233445566778899aabbccddeeff");
        blob.push(0);
        blob.extend_from_slice(file_name.as_bytes());
        blob.push(0);
        blob.extend_from_slice(b"suffix");
        blob
    }

    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let target = to.join(entry.file_name());
            if path.is_dir() {
                copy_dir(&path, &target);
            } else {
                fs::copy(&path, &target).unwrap();
            }
        }
    }
}
