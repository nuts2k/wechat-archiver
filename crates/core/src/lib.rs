mod archive;
mod config;
mod error;
mod hash;
mod image;
mod image_key;
mod index;
mod manifest;
mod media;
mod message_db;
mod scanner;
mod status;
mod types;
mod verify;
mod wechat;

pub use config::{ArchiveConfig, DatDecodeOptions, ResolvedConfig, WxgfMode};
pub use error::{ArchiverError, Result};
pub use image_key::{derive_image_key, DeriveImageKeyOptions, ImageKeyDerivation, ImageKeyMethod};
pub use message_db::{
    count_message_db_media, extract_message_db_files, extract_message_db_images,
    extract_message_db_videos, extract_message_db_voices, inspect_message_db,
    MessageDbDirectoryStatus, MessageDbExtractConfig, MessageDbFileInspection, MessageDbFileRole,
    MessageDbFileStatus, MessageDbInspectConfig, MessageDbInspection, MessageDbInspectionStatus,
    MessageDbMediaCountConfig, MessageDbMediaCountSummary, MessageDbMediaTypeCount,
    MessageDbNextAction,
};
pub use scanner::{extract_files, extract_images, extract_videos, extract_voices};
pub use status::{archive_status, ArchiveStatus};
pub use types::{
    ExtractSummary, ManifestEvent, ScanAction, UnsupportedExplanation, UnsupportedReasonSummary,
};
pub use verify::{verify_archive, VerifySummary};
pub use wechat::{discover_wechat, DiscoverOptions, WechatAccount, WechatDiscovery};
