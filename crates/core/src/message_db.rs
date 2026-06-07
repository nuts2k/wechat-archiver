use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};

use crate::config::{create_archive_dirs, ArchiveConfig, DatDecodeOptions};
use crate::error::{ArchiverError, IoContext, Result};
use crate::index::{index_path, open_index};
use crate::manifest::ManifestWriter;
use crate::scanner::{apply_result, persist, process_dat_image, ScanOutcome};
use crate::types::{now_epoch_ms, ExtractSummary, ManifestEvent, ScanAction};

#[derive(Debug, Clone)]
pub struct MessageDbExtractConfig {
    pub account_dir: PathBuf,
    pub archive_dir: PathBuf,
    pub dry_run: bool,
    pub dat_options: DatDecodeOptions,
    pub explain_unsupported: bool,
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

pub fn extract_message_db_images(config: MessageDbExtractConfig) -> Result<ExtractSummary> {
    let resolved = ArchiveConfig {
        source_dir: config.account_dir,
        archive_dir: config.archive_dir,
        dry_run: config.dry_run,
        dat_options: config.dat_options,
        explain_unsupported: config.explain_unsupported,
    }
    .resolve()?;

    let message_dir = resolved.source_dir.join("db_storage").join("message");
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

    let attach_root = resolved.source_dir.join("msg").join("attach");
    if !attach_root.is_dir() {
        return Err(ArchiverError::Other(format!(
            "attach directory does not exist: {}",
            attach_root.display()
        )));
    }

    let resources = query_image_resources(&resource_db)?;
    let message_dbs = message_db_paths(&message_dir)?;
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
        let writer = ManifestWriter::create(&resolved.archive_dir, &run_id)?;
        summary.manifest_path = Some(writer.path().to_path_buf());
        conn = Some(opened);
        manifest = Some(writer);
    }

    for resource in resources {
        if !image_messages.contains(&resource.key) {
            continue;
        }

        summary.candidates += 1;
        let result = match find_dat_file(
            &attach_root,
            &resource.key.talker,
            &resource.file_md5,
            resource.key.create_time,
        ) {
            Some(dat_path) => process_dat_image(
                &dat_path,
                &resolved.source_dir,
                &resolved.archive_dir,
                &run_id,
                resolved.dry_run,
                &resolved.dat_options,
                conn.as_ref(),
                manifest.as_mut(),
            ),
            None => record_missing_dat(
                &resource,
                &resolved.source_dir,
                &run_id,
                conn.as_ref(),
                manifest.as_mut(),
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

fn record_missing_dat(
    resource: &ImageResource,
    account_dir: &Path,
    run_id: &str,
    conn: Option<&Connection>,
    manifest: Option<&mut ManifestWriter>,
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
            archive_dir: archive.clone(),
            dry_run: false,
            dat_options: DatDecodeOptions::default(),
            explain_unsupported: false,
        })
        .unwrap();

        assert_eq!(summary.scanned_files, 1);
        assert_eq!(summary.candidates, 1);
        assert_eq!(summary.failed, 1);

        let status = archive_status(&archive).unwrap();
        assert_eq!(status.total_records, 1);
        assert_eq!(status.failed_records, 1);
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
            params![local_id, create_time, (1_i64 << 32) + 3],
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
}
