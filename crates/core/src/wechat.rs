use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::error::{ArchiverError, IoContext, Result};
use crate::image::is_dat_file;

#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    pub root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WechatDiscovery {
    pub searched_roots: Vec<PathBuf>,
    pub accounts: Vec<WechatAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WechatAccount {
    pub account_id: String,
    pub account_dir: PathBuf,
    pub db_storage_dir: Option<PathBuf>,
    pub attach_dir: Option<PathBuf>,
    pub recommended_source_dir: PathBuf,
    pub image_dat_count: u64,
    pub has_v4_layout: bool,
}

pub fn discover_wechat(options: DiscoverOptions) -> Result<WechatDiscovery> {
    let searched_roots = match options.root {
        Some(root) => vec![canonical_existing_dir(&root)?],
        None => default_wechat_roots()
            .into_iter()
            .filter(|path| path.is_dir())
            .filter_map(|path| path.canonicalize().ok())
            .collect(),
    };

    let mut by_account_dir = BTreeMap::<PathBuf, WechatAccount>::new();
    for root in &searched_roots {
        for account in discover_accounts_under(root)? {
            by_account_dir
                .entry(account.account_dir.clone())
                .or_insert(account);
        }
    }

    Ok(WechatDiscovery {
        searched_roots,
        accounts: by_account_dir.into_values().collect(),
    })
}

fn discover_accounts_under(root: &Path) -> Result<Vec<WechatAccount>> {
    let mut accounts = Vec::new();

    if looks_like_account_dir(root) {
        accounts.push(build_account(root)?);
        return Ok(accounts);
    }

    for entry in std::fs::read_dir(root).with_path(root)? {
        let entry = entry.with_path(root)?;
        let path = entry.path();
        if path.is_dir() && looks_like_account_dir(&path) {
            accounts.push(build_account(&path)?);
        }
    }

    Ok(accounts)
}

fn looks_like_account_dir(path: &Path) -> bool {
    path.join("db_storage").is_dir() || path.join("msg").join("attach").is_dir()
}

fn build_account(account_dir: &Path) -> Result<WechatAccount> {
    let account_dir = canonical_existing_dir(account_dir)?;
    let account_id = account_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string();
    let db_storage = account_dir.join("db_storage");
    let attach_dir = account_dir.join("msg").join("attach");
    let db_storage_dir = db_storage.is_dir().then_some(db_storage);
    let attach_dir = attach_dir.is_dir().then_some(attach_dir);
    let recommended_source_dir = attach_dir.clone().unwrap_or_else(|| account_dir.clone());
    let has_v4_layout = db_storage_dir.is_some() && attach_dir.is_some();
    let image_dat_count = attach_dir
        .as_deref()
        .map(count_image_dat_files)
        .unwrap_or(0);

    Ok(WechatAccount {
        account_id,
        account_dir,
        db_storage_dir,
        attach_dir,
        recommended_source_dir,
        image_dat_count,
        has_v4_layout,
    })
}

fn count_image_dat_files(attach_dir: &Path) -> u64 {
    let mut count = 0u64;
    for entry in WalkDir::new(attach_dir).follow_links(false) {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_file() || !is_dat_file(entry.path()) {
            continue;
        }
        if entry
            .path()
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .map(|name| name.eq_ignore_ascii_case("Img"))
            .unwrap_or(false)
        {
            count += 1;
        }
    }
    count
}

fn canonical_existing_dir(path: &Path) -> Result<PathBuf> {
    let normalized = path.canonicalize().with_path(path)?;
    if !normalized.is_dir() {
        return Err(ArchiverError::InvalidSource(normalized));
    }
    Ok(normalized)
}

fn default_wechat_roots() -> Vec<PathBuf> {
    let Some(home) = env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };

    vec![
        home.join("Library/Containers/com.tencent.xinWeChat/Data/Documents/xwechat_files"),
        home.join("Documents/xwechat_files"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_v4_account_layout_without_writing_source() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("xwechat_files").join("wxid_example");
        std::fs::create_dir_all(account.join("db_storage/message")).unwrap();
        std::fs::create_dir_all(account.join("msg/attach/hash/2026-01/Img")).unwrap();
        std::fs::write(account.join("msg/attach/hash/2026-01/Img/a.dat"), b"dat").unwrap();
        std::fs::write(
            account.join("msg/attach/hash/2026-01/not-image.dat"),
            b"dat",
        )
        .unwrap();

        let discovery = discover_wechat(DiscoverOptions {
            root: Some(tmp.path().join("xwechat_files")),
        })
        .unwrap();

        assert_eq!(discovery.accounts.len(), 1);
        let account = &discovery.accounts[0];
        assert_eq!(account.account_id, "wxid_example");
        assert_eq!(account.image_dat_count, 1);
        assert!(account.has_v4_layout);
        assert!(account.recommended_source_dir.ends_with("msg/attach"));
    }

    #[test]
    fn accepts_account_dir_as_root() {
        let tmp = tempfile::tempdir().unwrap();
        let account = tmp.path().join("wxid_example");
        std::fs::create_dir_all(account.join("db_storage")).unwrap();

        let discovery = discover_wechat(DiscoverOptions {
            root: Some(account.clone()),
        })
        .unwrap();

        assert_eq!(discovery.accounts.len(), 1);
        assert_eq!(
            discovery.accounts[0].account_dir,
            account.canonicalize().unwrap()
        );
    }
}
