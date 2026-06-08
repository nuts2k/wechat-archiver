mod archive;
mod audio;
mod config;
mod error;
mod hash;
mod image;
mod image_key;
mod index;
mod lookup;
mod manifest;
mod media;
mod message_db;
mod report;
mod scanner;
mod status;
mod task;
mod task_store;
mod types;
mod verify;
mod video;
mod views;
mod wechat;

pub use config::{ArchiveConfig, DatDecodeOptions, ResolvedConfig, WxgfMode};
pub use error::{ArchiverError, Result};
pub use image_key::{derive_image_key, DeriveImageKeyOptions, ImageKeyDerivation, ImageKeyMethod};
pub use lookup::{lookup_index, IndexLookup, IndexLookupQuery, IndexLookupRecord};
pub use message_db::{
    count_message_db_media, extract_message_db_files, extract_message_db_files_with_task,
    extract_message_db_images, extract_message_db_images_with_task, extract_message_db_videos,
    extract_message_db_videos_with_task, extract_message_db_voices,
    extract_message_db_voices_with_task, inspect_message_db, MessageDbDirectoryStatus,
    MessageDbExtractConfig, MessageDbFileInspection, MessageDbFileRole, MessageDbFileStatus,
    MessageDbInspectConfig, MessageDbInspection, MessageDbInspectionStatus,
    MessageDbMediaCountConfig, MessageDbMediaCountSummary, MessageDbMediaTypeCount,
    MessageDbNextAction,
};
pub use report::{archive_report, ArchiveReport};
pub use scanner::{
    extract_files, extract_files_with_task, extract_images, extract_images_with_task,
    extract_videos, extract_videos_with_task, extract_voices, extract_voices_with_task,
};
pub use status::{archive_status, ArchiveStatus, StatusCount};
pub use task::{
    CancelToken, TaskEvent, TaskEventKind, TaskHandle, TaskMetadata, TaskOptions, TaskProgress,
    TaskReporter, TaskRunner, TaskSnapshot, TaskStatus,
};
pub use task_store::{
    PersistentTaskStatus, SqliteTaskStore, TaskCreate, TaskListQuery, TaskRecord, TaskStore,
};
pub use types::{
    ExtractSummary, ManifestEvent, ScanAction, UnsupportedExplanation, UnsupportedReasonSummary,
};
pub use verify::{verify_archive, IndexVerifyFailure, VerifyFailure, VerifySummary};
pub use views::{generate_views, ViewFailure, ViewLink, ViewsConfig, ViewsSummary};
pub use wechat::{discover_wechat, DiscoverOptions, WechatAccount, WechatDiscovery};
