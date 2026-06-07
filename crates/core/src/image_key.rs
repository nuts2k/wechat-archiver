use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Aes128;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::error::{ArchiverError, IoContext, Result};

const V2_MAGIC_FULL: &[u8; 6] = b"\x07\x08V2\x08\x07";
const MAX_TEMPLATES: usize = 3;
const MAX_TEMPLATE_FILES: usize = 64;

#[derive(Debug, Clone)]
pub struct DeriveImageKeyOptions {
    pub account_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageKeyDerivation {
    pub account_id: String,
    pub account_dir: PathBuf,
    pub attach_dir: PathBuf,
    pub method: ImageKeyMethod,
    pub uin: u32,
    pub wxid: String,
    pub image_aes_key: String,
    pub image_xor_key: String,
    pub image_xor_key_value: u8,
    pub templates_checked: usize,
    pub kvcomm_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageKeyMethod {
    Kvcomm,
    WxidSuffixSearch,
}

#[derive(Debug, Clone)]
struct DerivedKey {
    xor_key: u8,
    aes_key: String,
}

#[derive(Debug, Clone)]
struct WxidParts {
    full: String,
    normalized: String,
    suffix: String,
}

struct BuildResultArgs<'a> {
    account_dir: &'a Path,
    account_id: &'a str,
    attach_dir: &'a Path,
    method: ImageKeyMethod,
    uin: u32,
    wxid: String,
    derived: DerivedKey,
    templates_checked: usize,
    kvcomm_dir: Option<PathBuf>,
}

pub fn derive_image_key(options: DeriveImageKeyOptions) -> Result<ImageKeyDerivation> {
    let account_dir = canonical_existing_dir(&options.account_dir)?;
    let account_id = account_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string();
    let attach_dir = account_dir.join("msg").join("attach");
    if !attach_dir.is_dir() {
        return Err(ArchiverError::InvalidSource(attach_dir));
    }

    let templates = find_v2_template_ciphertexts(&attach_dir);
    if templates.is_empty() {
        return Err(ArchiverError::Other(format!(
            "no V2 .dat templates found under {}",
            attach_dir.display()
        )));
    }

    if let Some(result) = derive_via_kvcomm(&account_dir, &account_id, &attach_dir, &templates) {
        return Ok(result);
    }

    derive_via_wxid_suffix_search(&account_dir, &account_id, &attach_dir, &templates)
}

fn derive_via_kvcomm(
    account_dir: &Path,
    account_id: &str,
    attach_dir: &Path,
    templates: &[[u8; 16]],
) -> Option<ImageKeyDerivation> {
    let kvcomm_dir = find_existing_kvcomm_dir(account_dir)?;
    let codes = collect_kvcomm_codes(&kvcomm_dir);
    if codes.is_empty() {
        return None;
    }

    for wxid in wxid_candidates(account_id) {
        for code in &codes {
            let derived = derive_image_keys(*code, &wxid);
            if verify_aes_key_against_all(&derived.aes_key, templates) {
                return Some(build_result(BuildResultArgs {
                    account_dir,
                    account_id,
                    attach_dir,
                    method: ImageKeyMethod::Kvcomm,
                    uin: *code,
                    wxid,
                    derived,
                    templates_checked: templates.len(),
                    kvcomm_dir: Some(kvcomm_dir),
                }));
            }
        }
    }

    None
}

fn derive_via_wxid_suffix_search(
    account_dir: &Path,
    account_id: &str,
    attach_dir: &Path,
    templates: &[[u8; 16]],
) -> Result<ImageKeyDerivation> {
    let parts = extract_wxid_parts(account_id).ok_or_else(|| {
        ArchiverError::Other(format!(
            "account id does not contain a _<4 hex> suffix: {account_id}"
        ))
    })?;
    let (xor_key, votes, total) = derive_xor_key_from_v2_dat(attach_dir).ok_or_else(|| {
        ArchiverError::Other(format!(
            "not enough V2 .dat samples under {} to infer xor key",
            attach_dir.display()
        ))
    })?;
    if votes * 2 <= total {
        return Err(ArchiverError::Other(format!(
            "unable to infer xor key confidently: top_votes={votes}, total={total}"
        )));
    }

    let mut wxid_tries = vec![parts.normalized.clone()];
    if parts.full != parts.normalized {
        wxid_tries.push(parts.full);
    }

    for wxid in wxid_tries {
        if let Some((uin, aes_key)) = search_uin_by_suffix(xor_key, &parts.suffix, &wxid, templates)
        {
            return Ok(build_result(BuildResultArgs {
                account_dir,
                account_id,
                attach_dir,
                method: ImageKeyMethod::WxidSuffixSearch,
                uin,
                wxid,
                derived: DerivedKey { xor_key, aes_key },
                templates_checked: templates.len(),
                kvcomm_dir: None,
            }));
        }
    }

    Err(ArchiverError::Other(
        "no uin candidate passed AES template verification".to_string(),
    ))
}

fn build_result(args: BuildResultArgs<'_>) -> ImageKeyDerivation {
    ImageKeyDerivation {
        account_id: args.account_id.to_string(),
        account_dir: args.account_dir.to_path_buf(),
        attach_dir: args.attach_dir.to_path_buf(),
        method: args.method,
        uin: args.uin,
        wxid: args.wxid,
        image_aes_key: args.derived.aes_key,
        image_xor_key: format!("0x{:02x}", args.derived.xor_key),
        image_xor_key_value: args.derived.xor_key,
        templates_checked: args.templates_checked,
        kvcomm_dir: args.kvcomm_dir,
    }
}

fn derive_image_keys(code: u32, wxid: &str) -> DerivedKey {
    let input = format!("{code}{wxid}");
    let aes_key = format!("{:x}", md5::compute(input.as_bytes()))
        .chars()
        .take(16)
        .collect();
    DerivedKey {
        xor_key: (code & 0xff) as u8,
        aes_key,
    }
}

fn normalize_wxid(account_id: &str) -> String {
    let trimmed = account_id.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.to_ascii_lowercase().starts_with("wxid_") {
        let mut parts = trimmed.split('_');
        let Some(prefix) = parts.next() else {
            return trimmed.to_string();
        };
        let Some(segment) = parts.next() else {
            return trimmed.to_string();
        };
        return format!("{prefix}_{segment}");
    }

    trimmed
        .rsplit_once('_')
        .filter(|(_, suffix)| {
            suffix.len() == 4 && suffix.chars().all(|c| c.is_ascii_alphanumeric())
        })
        .map(|(base, _)| base.to_string())
        .unwrap_or_else(|| trimmed.to_string())
}

fn wxid_candidates(account_id: &str) -> Vec<String> {
    let raw = account_id.trim().to_string();
    if raw.is_empty() {
        return Vec::new();
    }

    let normalized = normalize_wxid(&raw);
    if normalized != raw && !normalized.is_empty() {
        vec![raw, normalized]
    } else {
        vec![raw]
    }
}

fn extract_wxid_parts(account_id: &str) -> Option<WxidParts> {
    let (base, suffix) = account_id.rsplit_once('_')?;
    if suffix.len() != 4 || !suffix.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }

    Some(WxidParts {
        full: account_id.to_string(),
        normalized: base.to_string(),
        suffix: suffix.to_ascii_lowercase(),
    })
}

fn find_existing_kvcomm_dir(account_dir: &Path) -> Option<PathBuf> {
    derive_kvcomm_dir_candidates(account_dir)
        .into_iter()
        .find(|path| path.is_dir())
}

fn derive_kvcomm_dir_candidates(account_dir: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(xwechat_files_dir) = account_dir
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "xwechat_files"))
    {
        if let Some(documents_root) = xwechat_files_dir.parent() {
            candidates.push(documents_root.join("app_data/net/kvcomm"));
            candidates.push(documents_root.join("xwechat/net/kvcomm"));
            if let Some(container_root) = documents_root.parent() {
                candidates.push(
                    container_root.join(
                        "Library/Application Support/com.tencent.xinWeChat/xwechat/net/kvcomm",
                    ),
                );
                candidates.push(
                    container_root
                        .join("Library/Application Support/com.tencent.xinWeChat/net/kvcomm"),
                );
            }
        }
    }

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        candidates.push(
            home.join(
                "Library/Containers/com.tencent.xinWeChat/Data/Documents/app_data/net/kvcomm",
            ),
        );
    }

    dedupe_paths(candidates)
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut deduped = Vec::new();
    for path in paths {
        if !deduped.iter().any(|existing| existing == &path) {
            deduped.push(path);
        }
    }
    deduped
}

fn collect_kvcomm_codes(kvcomm_dir: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(kvcomm_dir) else {
        return Vec::new();
    };

    let mut codes = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(rest) = name
            .strip_prefix("key_")
            .filter(|_| name.to_ascii_lowercase().ends_with(".statistic"))
        else {
            continue;
        };
        let Some((code, _)) = rest.split_once('_') else {
            continue;
        };
        let Ok(code) = code.parse::<u32>() else {
            continue;
        };
        if code > 0 {
            codes.push(code);
        }
    }
    codes.sort_unstable();
    codes.dedup();
    codes
}

fn find_v2_template_ciphertexts(attach_dir: &Path) -> Vec<[u8; 16]> {
    scan_template_ciphertexts(attach_dir, "_t.dat")
        .into_iter()
        .chain(scan_template_ciphertexts(attach_dir, ".dat"))
        .fold(Vec::new(), |mut out, template| {
            if out.len() < MAX_TEMPLATES && !out.contains(&template) {
                out.push(template);
            }
            out
        })
}

fn scan_template_ciphertexts(attach_dir: &Path, suffix: &str) -> Vec<[u8; 16]> {
    let mut out = Vec::new();
    let mut examined = 0usize;
    for entry in WalkDir::new(attach_dir).follow_links(false) {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(name) = entry.path().file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(suffix) {
            continue;
        }

        examined += 1;
        let Ok(mut file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        let mut head = [0u8; 0x20];
        let Ok(read) = std::io::Read::read(&mut file, &mut head) else {
            continue;
        };
        if read >= 0x1f && head.starts_with(V2_MAGIC_FULL) {
            let mut template = [0u8; 16];
            template.copy_from_slice(&head[0x0f..0x1f]);
            if !out.contains(&template) {
                out.push(template);
                if out.len() >= MAX_TEMPLATES {
                    break;
                }
            }
        }
        if examined >= MAX_TEMPLATE_FILES && !out.is_empty() {
            break;
        }
    }
    out
}

fn verify_aes_key_against_all(aes_key: &str, templates: &[[u8; 16]]) -> bool {
    !templates.is_empty()
        && templates
            .iter()
            .all(|template| verify_aes_key(aes_key, template))
}

fn verify_aes_key(aes_key: &str, template: &[u8; 16]) -> bool {
    let key = aes_key.as_bytes();
    if key.len() < 16 {
        return false;
    }

    let Ok(cipher) = Aes128::new_from_slice(&key[..16]) else {
        return false;
    };
    let mut block = aes::Block::clone_from_slice(template);
    cipher.decrypt_block(&mut block);
    decrypted_template_looks_like_image(&block)
}

fn decrypted_template_looks_like_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xd8, 0xff])
        || bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47])
        || bytes.starts_with(b"GIF")
        || bytes.starts_with(b"RIFF")
        || bytes.starts_with(b"wxgf")
}

fn derive_xor_key_from_v2_dat(attach_dir: &Path) -> Option<(u8, usize, usize)> {
    let mut votes = Vec::new();
    for entry in WalkDir::new(attach_dir).follow_links(false) {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_file()
            || entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_none_or(|ext| !ext.eq_ignore_ascii_case("dat"))
        {
            continue;
        }
        let Ok(mut file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if metadata.len() < 0x20 {
            continue;
        }
        let mut head = [0u8; 6];
        if std::io::Read::read_exact(&mut file, &mut head).is_err() || head != *V2_MAGIC_FULL {
            continue;
        }
        if std::io::Seek::seek(&mut file, std::io::SeekFrom::End(-1)).is_err() {
            continue;
        }
        let mut last = [0u8; 1];
        if std::io::Read::read_exact(&mut file, &mut last).is_err() {
            continue;
        }
        votes.push(last[0] ^ 0xd9);
        if votes.len() >= 10 {
            break;
        }
    }

    if votes.len() < 3 {
        return None;
    }

    let mut counts = HashMap::<u8, usize>::new();
    for vote in &votes {
        *counts.entry(*vote).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(key, count)| (key, count, votes.len()))
}

fn search_uin_by_suffix(
    xor_key: u8,
    suffix: &str,
    wxid: &str,
    templates: &[[u8; 16]],
) -> Option<(u32, String)> {
    let suffix_bytes = suffix_bytes(suffix)?;
    let workers = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .max(1);
    let total = 1usize << 24;
    let chunk = total.div_ceil(workers);
    let templates = Arc::new(templates.to_vec());
    let wxid = Arc::new(wxid.to_string());
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();

    thread::scope(|scope| {
        for worker in 0..workers {
            let start = worker * chunk;
            let end = ((worker + 1) * chunk).min(total);
            if start >= end {
                continue;
            }
            let templates = Arc::clone(&templates);
            let wxid = Arc::clone(&wxid);
            let stop = Arc::clone(&stop);
            let sender = sender.clone();
            scope.spawn(move || {
                for i in start..end {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let uin = ((i as u32) << 8) | u32::from(xor_key);
                    let uin_string = uin.to_string();
                    let digest = md5::compute(uin_string.as_bytes());
                    if digest.0[..2] != suffix_bytes {
                        continue;
                    }

                    let aes_key = derive_image_keys(uin, &wxid).aes_key;
                    if verify_aes_key_against_all(&aes_key, &templates) {
                        stop.store(true, Ordering::Relaxed);
                        let _ = sender.send((uin, aes_key));
                        return;
                    }
                }
            });
        }
        drop(sender);
        receiver.recv().ok()
    })
}

fn suffix_bytes(suffix: &str) -> Option<[u8; 2]> {
    if suffix.len() != 4 {
        return None;
    }
    let parsed = u16::from_str_radix(suffix, 16).ok()?;
    Some(parsed.to_be_bytes())
}

fn canonical_existing_dir(path: &Path) -> Result<PathBuf> {
    let normalized = path.canonicalize().with_path(path)?;
    if !normalized.is_dir() {
        return Err(ArchiverError::InvalidSource(normalized));
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncrypt;

    #[test]
    fn normalizes_wxid_candidates() {
        assert_eq!(normalize_wxid("nuts2k_0868"), "nuts2k");
        assert_eq!(
            normalize_wxid("wxid_3784487844911_14bf"),
            "wxid_3784487844911"
        );
        assert_eq!(normalize_wxid("plain"), "plain");
    }

    #[test]
    fn collects_kvcomm_codes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("key_123_456.statistic"), b"").unwrap();
        std::fs::write(tmp.path().join("key_reportnow_123.statistic"), b"").unwrap();
        std::fs::write(tmp.path().join("key_123_789.statistic"), b"").unwrap();
        std::fs::write(tmp.path().join("key_0_789.statistic"), b"").unwrap();

        assert_eq!(collect_kvcomm_codes(tmp.path()), vec![123]);
    }

    #[test]
    fn derives_key_via_kvcomm_and_templates() {
        let tmp = tempfile::tempdir().unwrap();
        let documents = tmp.path().join("Documents");
        let account = documents.join("xwechat_files").join("tester_abcd");
        let attach = account.join("msg/attach/hash/2026-01/Img");
        let kvcomm = documents.join("app_data/net/kvcomm");
        std::fs::create_dir_all(&attach).unwrap();
        std::fs::create_dir_all(&kvcomm).unwrap();

        let uin = 123_456_789u32;
        let key = derive_image_keys(uin, "tester").aes_key;
        std::fs::write(kvcomm.join(format!("key_{uin}_456_1_input.statistic")), b"").unwrap();
        std::fs::write(
            attach.join("a_t.dat"),
            synthetic_v2_template_dat(b"\xff\xd8\xffsynthetic", key.as_bytes()),
        )
        .unwrap();

        let result = derive_image_key(DeriveImageKeyOptions {
            account_dir: account,
        })
        .unwrap();

        assert_eq!(result.method, ImageKeyMethod::Kvcomm);
        assert_eq!(result.uin, uin);
        assert_eq!(result.wxid, "tester");
        assert_eq!(result.image_aes_key, key);
        assert_eq!(result.image_xor_key_value, (uin & 0xff) as u8);
    }

    #[test]
    fn suffix_bytes_parse_hex() {
        assert_eq!(suffix_bytes("14bf"), Some([0x14, 0xbf]));
        assert_eq!(suffix_bytes("zzzz"), None);
    }

    fn synthetic_v2_template_dat(plain: &[u8], key: &[u8]) -> Vec<u8> {
        let encrypted = encrypt_first_block(plain, key);
        let mut data = Vec::new();
        data.extend_from_slice(V2_MAGIC_FULL);
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.push(0);
        data.extend_from_slice(&encrypted);
        data
    }

    fn encrypt_first_block(plain: &[u8], key: &[u8]) -> [u8; 16] {
        let cipher = Aes128::new_from_slice(&key[..16]).unwrap();
        let mut block = [0u8; 16];
        let len = plain.len().min(16);
        block[..len].copy_from_slice(&plain[..len]);
        let aes_block = aes::Block::from_mut_slice(&mut block);
        cipher.encrypt_block(aes_block);
        block
    }
}
