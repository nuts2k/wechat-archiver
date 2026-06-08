use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use wechat_archiver_core::WxgfMode as CoreWxgfMode;
use wechat_archiver_core::{
    archive_report, archive_status, count_message_db_media, derive_image_key, discover_wechat,
    extract_files_with_task, extract_images_with_task, extract_message_db_files_with_task,
    extract_message_db_images_with_task, extract_message_db_videos_with_task,
    extract_message_db_voices_with_task, extract_videos_with_task, extract_voices_with_task,
    generate_views, inspect_message_db, lookup_index, verify_archive, ArchiveConfig, ArchiveReport,
    ArchiveStatus, DatDecodeOptions, DeriveImageKeyOptions, DiscoverOptions, ExtractSummary,
    ImageKeyDerivation, ImageKeyMethod, IndexLookup, IndexLookupQuery, MessageDbExtractConfig,
    MessageDbInspectConfig, MessageDbInspection, MessageDbMediaCountConfig,
    MessageDbMediaCountSummary, MessageDbMediaTypeCount, TaskOptions, TaskReporter, VerifySummary,
    ViewsConfig, ViewsSummary, WechatDiscovery,
};

#[derive(Debug, Parser)]
#[command(name = "wechat-archiver")]
#[command(about = "只读扫描微信本地媒体并归档到独立目录")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// 只读发现本机微信 4.x 账号目录和媒体目录。
    Discover {
        /// 微信 xwechat_files 根目录或单个账号目录。默认尝试 macOS 常见路径。
        #[arg(long)]
        root: Option<PathBuf>,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,
    },

    /// 只读派生 macOS 微信 4.x 图片 .dat AES/XOR key。
    DeriveImageKey {
        /// 单个微信账号目录，通常是 xwechat_files/<wxid>。
        #[arg(long)]
        account: PathBuf,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,
    },

    /// 只读诊断微信消息库是否可被当前 SQLite 路径读取。
    InspectDb {
        /// 单个微信账号目录，通常是 xwechat_files/<wxid>。
        #[arg(long)]
        account: PathBuf,

        /// 已解密/普通 SQLite 消息库目录；不传时使用 account/db_storage/message。
        #[arg(long)]
        message_db_dir: Option<PathBuf>,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,
    },

    /// 只读统计已解密/普通 SQLite 消息库中的媒体候选数量。
    CountDbMedia {
        /// 单个微信账号目录，通常是 xwechat_files/<wxid>。
        #[arg(long)]
        account: PathBuf,

        /// 已解密/普通 SQLite 消息库目录；不传时使用 account/db_storage/message。
        #[arg(long)]
        message_db_dir: Option<PathBuf>,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,
    },

    /// 只读扫描源目录，列出可归档图片数量；默认不写 archive。
    Scan {
        /// 微信本地数据目录或待扫描目录。
        #[arg(long)]
        source: PathBuf,

        /// 独立归档目录。即使 scan 默认 dry-run，也用于路径安全检查。
        #[arg(long)]
        archive: PathBuf,

        /// V2 图片 .dat AES key，显式提供才会尝试解码 V2。
        #[arg(long)]
        image_aes_key: Option<String>,

        /// V1/V2 图片 .dat 尾段 XOR key，默认 0x88。
        #[arg(long, default_value = "0x88")]
        image_xor_key: String,

        /// wxgf 私有图片处理模式：jpg 调用 ffmpeg 输出首帧 JPG，raw 归档原始 wxgf，off 关闭。
        #[arg(long, value_enum, default_value = "jpg")]
        wxgf_mode: WxgfMode,

        /// ffmpeg 可执行文件路径；不传时使用 PATH 中的 ffmpeg。
        #[arg(long)]
        wxgf_ffmpeg_path: Option<PathBuf>,

        /// 输出 unsupported 原因计数和样例路径。
        #[arg(long)]
        explain_unsupported: bool,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,

        /// 将任务进度事件以 JSONL 输出到 stderr。
        #[arg(long)]
        jsonl_progress: bool,
    },

    /// 统一媒体抽取入口；当前已接入 image、video、file、voice。
    Extract {
        /// 媒体类型，支持逗号分隔；多类型会按给定顺序逐个运行并输出聚合结果。
        #[arg(long = "type", value_enum, value_delimiter = ',', required = true)]
        media_types: Vec<MediaType>,

        #[command(flatten)]
        args: ImageExtractArgs,
    },

    /// 归档普通图片和可识别旧 XOR .dat 图片。
    ExtractImages {
        #[command(flatten)]
        args: ImageExtractArgs,
    },

    /// 从已解密/普通 SQLite 微信消息库枚举图片消息并归档。
    ExtractDbImages {
        /// 单个微信账号目录，通常是 xwechat_files/<wxid>。
        #[arg(long)]
        account: PathBuf,

        /// 已解密/普通 SQLite 消息库目录；媒体文件仍从 account/msg 读取。
        #[arg(long)]
        message_db_dir: Option<PathBuf>,

        /// 独立归档目录，不能位于 account 内部，也不能包含 account。
        #[arg(long)]
        archive: PathBuf,

        /// 只读枚举和解码，不写入 archive。
        #[arg(long)]
        dry_run: bool,

        /// V2 图片 .dat AES key，显式提供才会尝试解码 V2。
        #[arg(long)]
        image_aes_key: Option<String>,

        /// V1/V2 图片 .dat 尾段 XOR key，默认 0x88。
        #[arg(long, default_value = "0x88")]
        image_xor_key: String,

        /// wxgf 私有图片处理模式：jpg 调用 ffmpeg 输出首帧 JPG，raw 归档原始 wxgf，off 关闭。
        #[arg(long, value_enum, default_value = "jpg")]
        wxgf_mode: WxgfMode,

        /// ffmpeg 可执行文件路径；不传时使用 PATH 中的 ffmpeg。
        #[arg(long)]
        wxgf_ffmpeg_path: Option<PathBuf>,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,

        /// 将任务进度事件以 JSONL 输出到 stderr。
        #[arg(long)]
        jsonl_progress: bool,
    },

    /// 从已解密/普通 SQLite 微信消息库枚举视频消息并归档。
    ExtractDbVideos {
        /// 单个微信账号目录，通常是 xwechat_files/<wxid>。
        #[arg(long)]
        account: PathBuf,

        /// 已解密/普通 SQLite 消息库目录；媒体文件仍从 account/msg 读取。
        #[arg(long)]
        message_db_dir: Option<PathBuf>,

        /// 独立归档目录，不能位于 account 内部，也不能包含 account。
        #[arg(long)]
        archive: PathBuf,

        /// 只读枚举和定位，不写入 archive。
        #[arg(long)]
        dry_run: bool,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,

        /// 将任务进度事件以 JSONL 输出到 stderr。
        #[arg(long)]
        jsonl_progress: bool,
    },

    /// 从已解密/普通 SQLite 微信消息库枚举文件附件并归档。
    ExtractDbFiles {
        /// 单个微信账号目录，通常是 xwechat_files/<wxid>。
        #[arg(long)]
        account: PathBuf,

        /// 已解密/普通 SQLite 消息库目录；媒体文件仍从 account/msg 读取。
        #[arg(long)]
        message_db_dir: Option<PathBuf>,

        /// 独立归档目录，不能位于 account 内部，也不能包含 account。
        #[arg(long)]
        archive: PathBuf,

        /// 只读枚举和定位，不写入 archive。
        #[arg(long)]
        dry_run: bool,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,

        /// 将任务进度事件以 JSONL 输出到 stderr。
        #[arg(long)]
        jsonl_progress: bool,
    },

    /// 从已解密/普通 SQLite 微信消息库枚举语音 BLOB 并归档。
    ExtractDbVoices {
        /// 单个微信账号目录，通常是 xwechat_files/<wxid>。
        #[arg(long)]
        account: PathBuf,

        /// 已解密/普通 SQLite 消息库目录；语音 BLOB 仍从该目录下 media_*.db 只读读取。
        #[arg(long)]
        message_db_dir: Option<PathBuf>,

        /// 独立归档目录，不能位于 account 内部，也不能包含 account。
        #[arg(long)]
        archive: PathBuf,

        /// 只读枚举和 hash，不写入 archive。
        #[arg(long)]
        dry_run: bool,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,

        /// 将任务进度事件以 JSONL 输出到 stderr。
        #[arg(long)]
        jsonl_progress: bool,
    },

    /// 查看归档索引统计。
    Status {
        /// 独立归档目录。
        #[arg(long)]
        archive: PathBuf,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,
    },

    /// 按 sha256 或源路径反查归档索引记录。
    Lookup(LookupArgs),

    /// 导出归档索引报告。
    Report {
        /// 独立归档目录。
        #[arg(long)]
        archive: PathBuf,

        /// 输出格式。
        #[arg(long, value_enum, default_value = "json")]
        format: ReportFormat,
    },

    /// 生成归档目录内的可浏览 views 视图；默认 dry-run。
    Views(ViewsArgs),

    /// 校验已归档对象是否仍与索引 sha256 一致。
    Verify {
        /// 独立归档目录。
        #[arg(long)]
        archive: PathBuf,

        /// 输出 JSON。
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("lookup_key")
        .required(true)
        .args(["sha256", "source_path"])
))]
struct LookupArgs {
    /// 独立归档目录。
    #[arg(long)]
    archive: PathBuf,

    /// 按归档对象 sha256 查询所有来源。
    #[arg(long)]
    sha256: Option<String>,

    /// 按微信源文件完整路径查询当前索引状态。
    #[arg(long)]
    source_path: Option<PathBuf>,

    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("views_mode")
        .args(["dry_run", "write"])
        .multiple(false)
))]
struct ViewsArgs {
    /// 独立归档目录。
    #[arg(long)]
    archive: PathBuf,

    /// 只输出将要创建的视图计划，不写入 views；默认行为。
    #[arg(long)]
    dry_run: bool,

    /// 写入 archive/views 下的相对软链接视图。
    #[arg(long)]
    write: bool,

    /// 输出 JSON。
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Args)]
struct ImageExtractArgs {
    /// 微信本地数据目录或待扫描目录。
    #[arg(long)]
    source: PathBuf,

    /// 独立归档目录，不能位于 source 内部，也不能包含 source。
    #[arg(long)]
    archive: PathBuf,

    /// 只扫描和解码，不写入 archive。
    #[arg(long)]
    dry_run: bool,

    /// V2 图片 .dat AES key，显式提供才会尝试解码 V2。
    #[arg(long)]
    image_aes_key: Option<String>,

    /// V1/V2 图片 .dat 尾段 XOR key，默认 0x88。
    #[arg(long, default_value = "0x88")]
    image_xor_key: String,

    /// wxgf 私有图片处理模式：jpg 调用 ffmpeg 输出首帧 JPG，raw 归档原始 wxgf，off 关闭。
    #[arg(long, value_enum, default_value = "jpg")]
    wxgf_mode: WxgfMode,

    /// ffmpeg 可执行文件路径；不传时使用 PATH 中的 ffmpeg。
    #[arg(long)]
    wxgf_ffmpeg_path: Option<PathBuf>,

    /// 输出 JSON。
    #[arg(long)]
    json: bool,

    /// 将任务进度事件以 JSONL 输出到 stderr。
    #[arg(long)]
    jsonl_progress: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MediaType {
    Image,
    Video,
    File,
    Voice,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum WxgfMode {
    Off,
    Raw,
    Jpg,
    Mp4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ReportFormat {
    Json,
    Csv,
}

impl From<WxgfMode> for CoreWxgfMode {
    fn from(value: WxgfMode) -> Self {
        match value {
            WxgfMode::Off => CoreWxgfMode::Off,
            WxgfMode::Raw => CoreWxgfMode::Raw,
            WxgfMode::Jpg => CoreWxgfMode::Jpg,
            WxgfMode::Mp4 => CoreWxgfMode::Mp4,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Discover { root, json } => {
            let discovery = discover_wechat(DiscoverOptions { root })?;
            print_discovery(&discovery, json)?;
        }
        Commands::DeriveImageKey { account, json } => {
            let derivation = derive_image_key(DeriveImageKeyOptions {
                account_dir: account,
            })?;
            print_image_key_derivation(&derivation, json)?;
        }
        Commands::InspectDb {
            account,
            message_db_dir,
            json,
        } => {
            let inspection = inspect_message_db(MessageDbInspectConfig {
                account_dir: account,
                message_db_dir,
            })?;
            print_message_db_inspection(&inspection, json)?;
        }
        Commands::CountDbMedia {
            account,
            message_db_dir,
            json,
        } => {
            let summary = count_message_db_media(MessageDbMediaCountConfig {
                account_dir: account,
                message_db_dir,
            })?;
            print_message_db_media_count(&summary, json)?;
        }
        Commands::Scan {
            source,
            archive,
            image_aes_key,
            image_xor_key,
            wxgf_mode,
            wxgf_ffmpeg_path,
            explain_unsupported,
            json,
            jsonl_progress,
        } => {
            let summary = extract_images_with_task(
                ArchiveConfig {
                    source_dir: source,
                    archive_dir: archive,
                    dry_run: true,
                    dat_options: parse_dat_options(
                        image_aes_key,
                        &image_xor_key,
                        wxgf_mode,
                        wxgf_ffmpeg_path,
                    )?,
                    explain_unsupported,
                },
                task_options(jsonl_progress),
            )?;
            print_extract_summary(&summary, json)?;
        }
        Commands::Extract { media_types, args } => {
            ensure_supported_extract_types(&media_types)?;
            let json = args.json;
            if media_types.len() == 1 {
                let summary = run_extract(media_types[0], args)?;
                print_extract_summary(&summary, json)?;
            } else {
                let summary = run_extract_many(&media_types, args)?;
                print_aggregate_extract_summary(&summary, json)?;
            }
        }
        Commands::ExtractImages { args } => {
            let json = args.json;
            let summary = run_image_extract(args)?;
            print_extract_summary(&summary, json)?;
        }
        Commands::ExtractDbImages {
            account,
            message_db_dir,
            archive,
            dry_run,
            image_aes_key,
            image_xor_key,
            wxgf_mode,
            wxgf_ffmpeg_path,
            json,
            jsonl_progress,
        } => {
            let summary = extract_message_db_images_with_task(
                MessageDbExtractConfig {
                    account_dir: account,
                    message_db_dir,
                    archive_dir: archive,
                    dry_run,
                    dat_options: parse_dat_options(
                        image_aes_key,
                        &image_xor_key,
                        wxgf_mode,
                        wxgf_ffmpeg_path,
                    )?,
                    explain_unsupported: false,
                },
                task_options(jsonl_progress),
            )?;
            print_extract_summary(&summary, json)?;
        }
        Commands::ExtractDbVideos {
            account,
            message_db_dir,
            archive,
            dry_run,
            json,
            jsonl_progress,
        } => {
            let summary = extract_message_db_videos_with_task(
                MessageDbExtractConfig {
                    account_dir: account,
                    message_db_dir,
                    archive_dir: archive,
                    dry_run,
                    dat_options: DatDecodeOptions::default(),
                    explain_unsupported: false,
                },
                task_options(jsonl_progress),
            )?;
            print_extract_summary(&summary, json)?;
        }
        Commands::ExtractDbFiles {
            account,
            message_db_dir,
            archive,
            dry_run,
            json,
            jsonl_progress,
        } => {
            let summary = extract_message_db_files_with_task(
                MessageDbExtractConfig {
                    account_dir: account,
                    message_db_dir,
                    archive_dir: archive,
                    dry_run,
                    dat_options: DatDecodeOptions::default(),
                    explain_unsupported: false,
                },
                task_options(jsonl_progress),
            )?;
            print_extract_summary(&summary, json)?;
        }
        Commands::ExtractDbVoices {
            account,
            message_db_dir,
            archive,
            dry_run,
            json,
            jsonl_progress,
        } => {
            let summary = extract_message_db_voices_with_task(
                MessageDbExtractConfig {
                    account_dir: account,
                    message_db_dir,
                    archive_dir: archive,
                    dry_run,
                    dat_options: DatDecodeOptions::default(),
                    explain_unsupported: false,
                },
                task_options(jsonl_progress),
            )?;
            print_extract_summary(&summary, json)?;
        }
        Commands::Status { archive, json } => {
            let status = archive_status(&archive)?;
            print_status(&status, json)?;
        }
        Commands::Lookup(args) => {
            let json = args.json;
            let lookup = run_lookup(args)?;
            print_index_lookup(&lookup, json)?;
        }
        Commands::Report { archive, format } => {
            let report = archive_report(&archive)?;
            print_archive_report(&report, format)?;
        }
        Commands::Views(args) => {
            let dry_run = args.dry_run || !args.write;
            let summary = generate_views(ViewsConfig {
                archive_dir: args.archive,
                dry_run,
            })?;
            print_views_summary(&summary, args.json)?;
            if summary.skipped_records > 0 || summary.failed_links > 0 {
                std::process::exit(2);
            }
        }
        Commands::Verify { archive, json } => {
            let summary = verify_archive(&archive)?;
            print_verify_summary(&summary, json)?;
            if summary.missing > 0
                || summary.unreadable > 0
                || summary.mismatched > 0
                || summary.index_failed > 0
            {
                std::process::exit(2);
            }
        }
    }

    Ok(())
}

fn run_lookup(args: LookupArgs) -> Result<IndexLookup> {
    let query = match (args.sha256, args.source_path) {
        (Some(sha256), None) => IndexLookupQuery::Sha256(sha256),
        (None, Some(source_path)) => {
            IndexLookupQuery::SourcePath(source_path.to_string_lossy().to_string())
        }
        _ => unreachable!("clap ArgGroup ensures exactly one lookup key"),
    };
    Ok(lookup_index(&args.archive, query)?)
}

fn ensure_supported_extract_types(media_types: &[MediaType]) -> Result<()> {
    anyhow::ensure!(
        !media_types.is_empty(),
        "extract --type requires at least one media type"
    );
    let unsupported = media_types
        .iter()
        .copied()
        .filter(|media_type| {
            !matches!(
                media_type,
                MediaType::Image | MediaType::Video | MediaType::File | MediaType::Voice
            )
        })
        .map(media_type_name)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        unsupported.is_empty(),
        "extract --type currently supports image, video, file and voice; unsupported types: {}",
        unsupported.join(",")
    );
    Ok(())
}

fn media_type_name(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Image => "image",
        MediaType::Video => "video",
        MediaType::File => "file",
        MediaType::Voice => "voice",
    }
}

fn run_image_extract(args: ImageExtractArgs) -> Result<ExtractSummary> {
    let jsonl_progress = args.jsonl_progress;
    Ok(extract_images_with_task(
        image_archive_config_from_args(args)?,
        task_options(jsonl_progress),
    )?)
}

fn run_extract(media_type: MediaType, args: ImageExtractArgs) -> Result<ExtractSummary> {
    let jsonl_progress = args.jsonl_progress;
    match media_type {
        MediaType::Image => run_image_extract(args),
        MediaType::Video => Ok(extract_videos_with_task(
            direct_media_archive_config_from_args(args),
            task_options(jsonl_progress),
        )?),
        MediaType::File => Ok(extract_files_with_task(
            direct_media_archive_config_from_args(args),
            task_options(jsonl_progress),
        )?),
        MediaType::Voice => Ok(extract_voices_with_task(
            direct_media_archive_config_from_args(args),
            task_options(jsonl_progress),
        )?),
    }
}

fn run_extract_many(
    media_types: &[MediaType],
    args: ImageExtractArgs,
) -> Result<AggregateExtractSummary> {
    let source_dir = args.source.clone();
    let archive_dir = args.archive.clone();
    let dry_run = args.dry_run;
    let mut items = Vec::new();

    for media_type in media_types {
        let summary = run_extract(*media_type, args.clone())?;
        items.push(MediaTypeExtractSummary {
            media_type: media_type_name(*media_type).to_string(),
            summary,
        });
    }

    Ok(AggregateExtractSummary::new(
        source_dir,
        archive_dir,
        dry_run,
        items,
    ))
}

fn image_archive_config_from_args(args: ImageExtractArgs) -> Result<ArchiveConfig> {
    Ok(ArchiveConfig {
        source_dir: args.source,
        archive_dir: args.archive,
        dry_run: args.dry_run,
        dat_options: parse_dat_options(
            args.image_aes_key,
            &args.image_xor_key,
            args.wxgf_mode,
            args.wxgf_ffmpeg_path,
        )?,
        explain_unsupported: false,
    })
}

fn task_options(jsonl_progress: bool) -> TaskOptions {
    if !jsonl_progress {
        return TaskOptions::default();
    }

    TaskOptions::new().with_reporter(TaskReporter::new(|event| {
        match serde_json::to_string(&event) {
            Ok(line) => eprintln!("{line}"),
            Err(error) => eprintln!(
                "{{\"kind\":\"progress_serialization_failed\",\"error\":{}}}",
                serde_json::to_string(&error.to_string()).unwrap_or_else(|_| "\"unknown\"".into())
            ),
        }
    }))
}

#[derive(Debug, Clone, Serialize)]
struct AggregateExtractSummary {
    source_dir: PathBuf,
    archive_dir: PathBuf,
    dry_run: bool,
    media_types: Vec<String>,
    scanned_files: u64,
    candidates: u64,
    would_archive: u64,
    archived: u64,
    already_archived: u64,
    reused_records: u64,
    decoded_dat: u64,
    metadata_backfilled: u64,
    new_objects: u64,
    existing_objects: u64,
    unsupported: u64,
    failed: u64,
    summaries: Vec<MediaTypeExtractSummary>,
}

impl AggregateExtractSummary {
    fn new(
        source_dir: PathBuf,
        archive_dir: PathBuf,
        dry_run: bool,
        summaries: Vec<MediaTypeExtractSummary>,
    ) -> Self {
        Self {
            source_dir,
            archive_dir,
            dry_run,
            media_types: summaries
                .iter()
                .map(|summary| summary.media_type.clone())
                .collect(),
            scanned_files: summaries
                .iter()
                .map(|summary| summary.summary.scanned_files)
                .sum(),
            candidates: summaries
                .iter()
                .map(|summary| summary.summary.candidates)
                .sum(),
            would_archive: summaries
                .iter()
                .map(|summary| summary.summary.would_archive)
                .sum(),
            archived: summaries
                .iter()
                .map(|summary| summary.summary.archived)
                .sum(),
            already_archived: summaries
                .iter()
                .map(|summary| summary.summary.already_archived)
                .sum(),
            reused_records: summaries
                .iter()
                .map(|summary| summary.summary.reused_records)
                .sum(),
            decoded_dat: summaries
                .iter()
                .map(|summary| summary.summary.decoded_dat)
                .sum(),
            metadata_backfilled: summaries
                .iter()
                .map(|summary| summary.summary.metadata_backfilled)
                .sum(),
            new_objects: summaries
                .iter()
                .map(|summary| summary.summary.new_objects)
                .sum(),
            existing_objects: summaries
                .iter()
                .map(|summary| summary.summary.existing_objects)
                .sum(),
            unsupported: summaries
                .iter()
                .map(|summary| summary.summary.unsupported)
                .sum(),
            failed: summaries.iter().map(|summary| summary.summary.failed).sum(),
            summaries,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct MediaTypeExtractSummary {
    media_type: String,
    summary: ExtractSummary,
}

fn direct_media_archive_config_from_args(args: ImageExtractArgs) -> ArchiveConfig {
    ArchiveConfig {
        source_dir: args.source,
        archive_dir: args.archive,
        dry_run: args.dry_run,
        dat_options: DatDecodeOptions::default(),
        explain_unsupported: false,
    }
}

fn print_image_key_derivation(result: &ImageKeyDerivation, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
        return Ok(());
    }

    println!("account_id: {}", result.account_id);
    println!("account_dir: {}", result.account_dir.display());
    println!("attach_dir: {}", result.attach_dir.display());
    println!("method: {}", image_key_method_name(result.method));
    println!("uin: {}", result.uin);
    println!("wxid: {}", result.wxid);
    println!("image_aes_key: {}", result.image_aes_key);
    println!("image_xor_key: {}", result.image_xor_key);
    println!("image_xor_key_value: {}", result.image_xor_key_value);
    println!("templates_checked: {}", result.templates_checked);
    if let Some(path) = &result.kvcomm_dir {
        println!("kvcomm_dir: {}", path.display());
    }
    println!(
        "next_extract_args: --image-aes-key \"{}\" --image-xor-key {}",
        result.image_aes_key, result.image_xor_key
    );
    println!("note: 结果只打印到终端，不会保存，也不会写入微信目录。");
    Ok(())
}

fn image_key_method_name(method: ImageKeyMethod) -> &'static str {
    match method {
        ImageKeyMethod::Kvcomm => "kvcomm",
        ImageKeyMethod::WxidSuffixSearch => "wxid_suffix_search",
    }
}

fn parse_dat_options(
    image_aes_key: Option<String>,
    image_xor_key: &str,
    wxgf_mode: WxgfMode,
    wxgf_ffmpeg_path: Option<PathBuf>,
) -> Result<DatDecodeOptions> {
    Ok(DatDecodeOptions {
        image_aes_key: image_aes_key.as_deref().map(parse_aes_key).transpose()?,
        image_xor_key: parse_u8_key(image_xor_key)
            .with_context(|| format!("invalid --image-xor-key: {image_xor_key}"))?,
        wxgf_mode: wxgf_mode.into(),
        wxgf_ffmpeg_path,
    })
}

fn parse_aes_key(value: &str) -> Result<Vec<u8>> {
    if let Some(hex) = value.strip_prefix("hex:") {
        let key = hex::decode(hex).context("invalid hex: AES key")?;
        anyhow::ensure!(
            key.len() >= 16,
            "--image-aes-key must contain at least 16 bytes"
        );
        return Ok(key);
    }

    let key = value.as_bytes().to_vec();
    anyhow::ensure!(
        key.len() >= 16,
        "--image-aes-key must contain at least 16 bytes"
    );
    Ok(key)
}

fn parse_u8_key(value: &str) -> Result<u8> {
    let parsed = if let Some(hex) = value.strip_prefix("0x") {
        u8::from_str_radix(hex, 16)?
    } else {
        value.parse::<u8>()?
    };
    Ok(parsed)
}

fn print_discovery(discovery: &WechatDiscovery, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(discovery)?);
        return Ok(());
    }

    println!("searched_roots:");
    for root in &discovery.searched_roots {
        println!("  {}", root.display());
    }
    println!("accounts: {}", discovery.accounts.len());
    for account in &discovery.accounts {
        println!("account_id: {}", account.account_id);
        println!("  account_dir: {}", account.account_dir.display());
        if let Some(path) = &account.db_storage_dir {
            println!("  db_storage: {}", path.display());
        }
        if let Some(path) = &account.attach_dir {
            println!("  attach: {}", path.display());
        }
        println!(
            "  recommended_source: {}",
            account.recommended_source_dir.display()
        );
        println!("  image_dat_count: {}", account.image_dat_count);
        println!("  has_v4_layout: {}", account.has_v4_layout);
    }
    Ok(())
}

fn print_extract_summary(summary: &ExtractSummary, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(summary)?);
        return Ok(());
    }

    println!("run_id: {}", summary.run_id);
    println!("source: {}", summary.source_dir.display());
    println!("archive: {}", summary.archive_dir.display());
    println!("dry_run: {}", summary.dry_run);
    println!("scanned_files: {}", summary.scanned_files);
    println!("candidates: {}", summary.candidates);
    println!("would_archive: {}", summary.would_archive);
    println!("archived: {}", summary.archived);
    println!("already_archived: {}", summary.already_archived);
    println!("reused_records: {}", summary.reused_records);
    println!("decoded_dat: {}", summary.decoded_dat);
    println!("metadata_backfilled: {}", summary.metadata_backfilled);
    println!("new_objects: {}", summary.new_objects);
    println!("existing_objects: {}", summary.existing_objects);
    println!("unsupported: {}", summary.unsupported);
    println!("failed: {}", summary.failed);
    if let Some(explanation) = &summary.unsupported_explanation {
        println!("unsupported_reasons:");
        for reason in &explanation.reasons {
            println!("  {}: {} - {}", reason.reason, reason.count, reason.message);
            for sample in &reason.samples {
                println!("    sample: {sample}");
            }
        }
    }
    if let Some(path) = &summary.index_path {
        println!("index: {}", path.display());
    }
    if let Some(path) = &summary.manifest_path {
        println!("manifest: {}", path.display());
    }
    Ok(())
}

fn print_aggregate_extract_summary(summary: &AggregateExtractSummary, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(summary)?);
        return Ok(());
    }

    println!("source: {}", summary.source_dir.display());
    println!("archive: {}", summary.archive_dir.display());
    println!("dry_run: {}", summary.dry_run);
    println!("media_types: {}", summary.media_types.join(","));
    println!("scanned_files: {}", summary.scanned_files);
    println!("candidates: {}", summary.candidates);
    println!("would_archive: {}", summary.would_archive);
    println!("archived: {}", summary.archived);
    println!("already_archived: {}", summary.already_archived);
    println!("reused_records: {}", summary.reused_records);
    println!("decoded_dat: {}", summary.decoded_dat);
    println!("metadata_backfilled: {}", summary.metadata_backfilled);
    println!("new_objects: {}", summary.new_objects);
    println!("existing_objects: {}", summary.existing_objects);
    println!("unsupported: {}", summary.unsupported);
    println!("failed: {}", summary.failed);
    println!("summaries:");
    for item in &summary.summaries {
        println!("  media_type: {}", item.media_type);
        println!("    run_id: {}", item.summary.run_id);
        println!("    scanned_files: {}", item.summary.scanned_files);
        println!("    candidates: {}", item.summary.candidates);
        println!("    would_archive: {}", item.summary.would_archive);
        println!("    archived: {}", item.summary.archived);
        println!("    already_archived: {}", item.summary.already_archived);
        println!("    reused_records: {}", item.summary.reused_records);
        println!("    decoded_dat: {}", item.summary.decoded_dat);
        println!(
            "    metadata_backfilled: {}",
            item.summary.metadata_backfilled
        );
        println!("    new_objects: {}", item.summary.new_objects);
        println!("    existing_objects: {}", item.summary.existing_objects);
        println!("    unsupported: {}", item.summary.unsupported);
        println!("    failed: {}", item.summary.failed);
        if let Some(path) = &item.summary.index_path {
            println!("    index: {}", path.display());
        }
        if let Some(path) = &item.summary.manifest_path {
            println!("    manifest: {}", path.display());
        }
    }
    Ok(())
}

fn print_message_db_inspection(inspection: &MessageDbInspection, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(inspection)?);
        return Ok(());
    }

    println!("account: {}", inspection.account_dir.display());
    println!("message_db_dir: {}", inspection.message_db_dir.display());
    println!(
        "message_db_dir_overridden: {}",
        inspection.message_db_dir_overridden
    );
    println!("status: {:?}", inspection.status);
    println!("directory_status: {:?}", inspection.directory_status);
    println!("message: {}", inspection.message);
    println!("next_action: {:?}", inspection.next_action);
    print_message_db_file("resource_db", &inspection.resource_db);
    println!("message_dbs: {}", inspection.total_message_dbs);
    println!("readable_message_dbs: {}", inspection.readable_message_dbs);
    for db in &inspection.message_dbs {
        print_message_db_file("message_db", db);
    }
    if inspection.status != wechat_archiver_core::MessageDbInspectionStatus::Ready {
        println!("note: 当前只支持普通 SQLite 或用户提供的已解密消息库目录。");
    }
    Ok(())
}

fn print_message_db_media_count(summary: &MessageDbMediaCountSummary, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(summary)?);
        return Ok(());
    }

    println!("account: {}", summary.account_dir.display());
    println!("message_db_dir: {}", summary.message_db_dir.display());
    println!(
        "message_db_dir_overridden: {}",
        summary.message_db_dir_overridden
    );
    print_media_type_count("image", summary.image);
    print_media_type_count("video", summary.video);
    print_media_type_count("file", summary.file);
    print_media_type_count("voice", summary.voice);
    println!("note: 该命令只读消息库，不读取、复制或 hash account/msg 下的媒体文件。");
    println!("note: voice 会只读统计 media_*.db/VoiceInfo 候选，但不会读取或复制微信媒体目录。");
    Ok(())
}

fn print_media_type_count(label: &str, count: MessageDbMediaTypeCount) {
    println!(
        "{label}: resource_candidates={} message_rows={} matched_messages={}",
        count.resource_candidates, count.message_rows, count.matched_messages
    );
}

fn print_message_db_file(label: &str, file: &wechat_archiver_core::MessageDbFileInspection) {
    println!(
        "{label}: {} status={:?} sqlite_header={} tables={:?}",
        file.path.display(),
        file.status,
        file.sqlite_header,
        file.table_count
    );
    if let Some(error) = &file.error {
        println!("{label}_error: {error}");
    }
}

fn print_status(status: &ArchiveStatus, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(status)?);
        return Ok(());
    }

    println!("archive: {}", status.archive_dir.display());
    println!("index: {}", status.index_path.display());
    println!("total_records: {}", status.total_records);
    println!("archived_records: {}", status.archived_records);
    println!("unsupported_records: {}", status.unsupported_records);
    println!("failed_records: {}", status.failed_records);
    println!("unique_objects: {}", status.unique_objects);
    println!("unique_bytes: {}", status.unique_bytes);
    print_status_counts("media_type_counts", &status.media_type_counts);
    print_status_counts("source_kind_counts", &status.source_kind_counts);
    print_status_counts("decrypt_status_counts", &status.decrypt_status_counts);
    print_status_counts("verify_status_counts", &status.verify_status_counts);
    Ok(())
}

fn print_status_counts(label: &str, counts: &[wechat_archiver_core::StatusCount]) {
    println!("{label}:");
    for count in counts {
        println!("  {}: {}", count.value, count.count);
    }
}

fn print_index_lookup(lookup: &IndexLookup, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(lookup)?);
        return Ok(());
    }

    println!("archive: {}", lookup.archive_dir.display());
    println!("index: {}", lookup.index_path.display());
    match &lookup.query {
        IndexLookupQuery::Sha256(sha256) => println!("query_sha256: {sha256}"),
        IndexLookupQuery::SourcePath(source_path) => println!("query_source_path: {source_path}"),
    }
    println!("matched_records: {}", lookup.matched_records);
    for record in &lookup.records {
        println!("record: id={}", record.id);
        println!("  source_path: {}", record.source_path);
        println!("  source_relative_path: {}", record.source_relative_path);
        println!("  source_kind: {}", record.source_kind);
        println!("  media_type: {}", record.media_type);
        println!(
            "  original_filename: {}",
            optional_string(&record.original_filename)
        );
        println!("  mime_type: {}", optional_string(&record.mime_type));
        println!("  width_px: {}", optional_u32(record.width_px));
        println!("  height_px: {}", optional_u32(record.height_px));
        println!("  duration_ms: {}", optional_u64(record.duration_ms));
        println!("  archive_path: {}", optional_string(&record.archive_path));
        println!("  sha256: {}", optional_string(&record.sha256));
        println!("  size_bytes: {}", optional_u64(record.size_bytes));
        println!(
            "  source_size_bytes: {}",
            optional_u64(record.source_size_bytes)
        );
        println!(
            "  source_modified_ms: {}",
            optional_i64(record.source_modified_ms)
        );
        println!("  extension: {}", optional_string(&record.extension));
        println!("  decoder: {}", optional_string(&record.decoder));
        println!(
            "  decode_fingerprint: {}",
            optional_string(&record.decode_fingerprint)
        );
        println!("  decrypt_status: {}", record.decrypt_status);
        println!("  verify_status: {}", record.verify_status);
        println!(
            "  message_talker: {}",
            optional_string(&record.message_talker)
        );
        println!(
            "  message_sender: {}",
            optional_string(&record.message_sender)
        );
        println!(
            "  message_local_id: {}",
            optional_i64(record.message_local_id)
        );
        println!(
            "  message_create_time: {}",
            optional_i64(record.message_create_time)
        );
        println!("  error: {}", optional_string(&record.error));
        println!("  created_at_ms: {}", record.created_at_ms);
        println!("  updated_at_ms: {}", record.updated_at_ms);
    }
    Ok(())
}

fn print_archive_report(report: &ArchiveReport, format: ReportFormat) -> Result<()> {
    match format {
        ReportFormat::Json => {
            println!("{}", serde_json::to_string_pretty(report)?);
        }
        ReportFormat::Csv => print_archive_report_csv(report),
    }
    Ok(())
}

fn print_archive_report_csv(report: &ArchiveReport) {
    print_csv_row(&[
        "id",
        "source_path",
        "source_relative_path",
        "source_kind",
        "media_type",
        "original_filename",
        "mime_type",
        "width_px",
        "height_px",
        "duration_ms",
        "message_talker",
        "message_sender",
        "message_local_id",
        "message_create_time",
        "decoder",
        "decode_fingerprint",
        "archive_path",
        "sha256",
        "size_bytes",
        "source_size_bytes",
        "source_modified_ms",
        "extension",
        "decrypt_status",
        "verify_status",
        "error",
        "created_at_ms",
        "updated_at_ms",
    ]);
    for record in &report.records {
        let id = record.id.to_string();
        let message_local_id = optional_i64_csv(record.message_local_id);
        let message_create_time = optional_i64_csv(record.message_create_time);
        let width_px = optional_u32_csv(record.width_px);
        let height_px = optional_u32_csv(record.height_px);
        let duration_ms = optional_u64_csv(record.duration_ms);
        let size_bytes = optional_u64_csv(record.size_bytes);
        let source_size_bytes = optional_u64_csv(record.source_size_bytes);
        let source_modified_ms = optional_i64_csv(record.source_modified_ms);
        let created_at_ms = record.created_at_ms.to_string();
        let updated_at_ms = record.updated_at_ms.to_string();
        print_csv_row(&[
            &id,
            &record.source_path,
            &record.source_relative_path,
            &record.source_kind,
            &record.media_type,
            optional_string_csv(&record.original_filename),
            optional_string_csv(&record.mime_type),
            &width_px,
            &height_px,
            &duration_ms,
            optional_string_csv(&record.message_talker),
            optional_string_csv(&record.message_sender),
            &message_local_id,
            &message_create_time,
            optional_string_csv(&record.decoder),
            optional_string_csv(&record.decode_fingerprint),
            optional_string_csv(&record.archive_path),
            optional_string_csv(&record.sha256),
            &size_bytes,
            &source_size_bytes,
            &source_modified_ms,
            optional_string_csv(&record.extension),
            &record.decrypt_status,
            &record.verify_status,
            optional_string_csv(&record.error),
            &created_at_ms,
            &updated_at_ms,
        ]);
    }
}

fn print_csv_row(fields: &[&str]) {
    let row = fields
        .iter()
        .map(|field| csv_field(field))
        .collect::<Vec<_>>()
        .join(",");
    println!("{row}");
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn optional_string_csv(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("")
}

fn optional_u64_csv(value: Option<u64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn optional_u32_csv(value: Option<u32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn optional_i64_csv(value: Option<i64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn print_views_summary(summary: &ViewsSummary, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(summary)?);
        return Ok(());
    }

    println!("archive: {}", summary.archive_dir.display());
    println!("views: {}", summary.views_dir.display());
    println!("dry_run: {}", summary.dry_run);
    println!("scanned_records: {}", summary.scanned_records);
    println!("planned_links: {}", summary.planned_links);
    println!("created_links: {}", summary.created_links);
    println!("existing_links: {}", summary.existing_links);
    println!("skipped_records: {}", summary.skipped_records);
    println!("failed_links: {}", summary.failed_links);
    for link in &summary.links {
        println!(
            "link: {} -> {} ({})",
            link.view_path.display(),
            link.link_target.display(),
            link.view_kind
        );
    }
    for failure in &summary.failures {
        println!(
            "failure: id={:?} source_path={:?} archive_path={:?} view_path={:?} error={}",
            failure.media_item_id,
            failure.source_path,
            failure.archive_path,
            failure.view_path,
            failure.error
        );
    }
    Ok(())
}

fn optional_string(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("-")
}

fn optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn optional_u32(value: Option<u32>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn optional_i64(value: Option<i64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn print_verify_summary(summary: &VerifySummary, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(summary)?);
        return Ok(());
    }

    println!("archive: {}", summary.archive_dir.display());
    println!("checked: {}", summary.checked);
    println!("ok: {}", summary.ok);
    println!("missing: {}", summary.missing);
    println!("unreadable: {}", summary.unreadable);
    println!("mismatched: {}", summary.mismatched);
    println!("index_checked: {}", summary.index_checked);
    println!("index_ok: {}", summary.index_ok);
    println!("index_failed: {}", summary.index_failed);
    for failure in &summary.failures {
        println!(
            "failure: {} expected={} actual={:?} error={}",
            failure.archive_path, failure.expected_sha256, failure.actual_sha256, failure.error
        );
    }
    for failure in &summary.index_failures {
        println!(
            "index_failure: id={} source_path={} archive_path={:?} sha256={:?} error={}",
            failure.media_item_id,
            failure.source_path,
            failure.archive_path,
            failure.sha256,
            failure.error
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_unified_extract_image_command() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "extract",
            "--type",
            "image",
            "--source",
            "/tmp/wechat-source",
            "--archive",
            "/tmp/wechat-archive",
            "--dry-run",
            "--json",
            "--jsonl-progress",
        ])
        .unwrap();

        match cli.command {
            Commands::Extract { media_types, args } => {
                assert_eq!(media_types, vec![MediaType::Image]);
                assert_eq!(args.source, PathBuf::from("/tmp/wechat-source"));
                assert_eq!(args.archive, PathBuf::from("/tmp/wechat-archive"));
                assert!(args.dry_run);
                assert!(args.json);
                assert!(args.jsonl_progress);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_comma_separated_extract_types() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "extract",
            "--type",
            "image,video,voice",
            "--source",
            "/tmp/wechat-source",
            "--archive",
            "/tmp/wechat-archive",
        ])
        .unwrap();

        match cli.command {
            Commands::Extract { media_types, .. } => {
                assert_eq!(
                    media_types,
                    vec![MediaType::Image, MediaType::Video, MediaType::Voice]
                );
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_inspect_db_command() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "inspect-db",
            "--account",
            "/tmp/xwechat_files/wxid",
            "--message-db-dir",
            "/tmp/decrypted-message",
            "--json",
        ])
        .unwrap();

        match cli.command {
            Commands::InspectDb {
                account,
                message_db_dir,
                json,
            } => {
                assert_eq!(account, PathBuf::from("/tmp/xwechat_files/wxid"));
                assert_eq!(
                    message_db_dir,
                    Some(PathBuf::from("/tmp/decrypted-message"))
                );
                assert!(json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_count_db_media_command() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "count-db-media",
            "--account",
            "/tmp/xwechat_files/wxid",
            "--message-db-dir",
            "/tmp/decrypted-message",
            "--json",
        ])
        .unwrap();

        match cli.command {
            Commands::CountDbMedia {
                account,
                message_db_dir,
                json,
            } => {
                assert_eq!(account, PathBuf::from("/tmp/xwechat_files/wxid"));
                assert_eq!(
                    message_db_dir,
                    Some(PathBuf::from("/tmp/decrypted-message"))
                );
                assert!(json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_lookup_by_sha256_command() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "lookup",
            "--archive",
            "/tmp/wechat-archive",
            "--sha256",
            "abc123",
            "--json",
        ])
        .unwrap();

        match cli.command {
            Commands::Lookup(args) => {
                assert_eq!(args.archive, PathBuf::from("/tmp/wechat-archive"));
                assert_eq!(args.sha256.as_deref(), Some("abc123"));
                assert_eq!(args.source_path, None);
                assert!(args.json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_lookup_by_source_path_command() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "lookup",
            "--archive",
            "/tmp/wechat-archive",
            "--source-path",
            "/tmp/source/image.jpg",
        ])
        .unwrap();

        match cli.command {
            Commands::Lookup(args) => {
                assert_eq!(args.archive, PathBuf::from("/tmp/wechat-archive"));
                assert_eq!(args.sha256, None);
                assert_eq!(
                    args.source_path,
                    Some(PathBuf::from("/tmp/source/image.jpg"))
                );
                assert!(!args.json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn rejects_lookup_with_both_query_keys() {
        let result = Cli::try_parse_from([
            "wechat-archiver",
            "lookup",
            "--archive",
            "/tmp/wechat-archive",
            "--sha256",
            "abc123",
            "--source-path",
            "/tmp/source/image.jpg",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn parses_report_csv_command() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "report",
            "--archive",
            "/tmp/wechat-archive",
            "--format",
            "csv",
        ])
        .unwrap();

        match cli.command {
            Commands::Report { archive, format } => {
                assert_eq!(archive, PathBuf::from("/tmp/wechat-archive"));
                assert_eq!(format, ReportFormat::Csv);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn csv_field_escapes_commas_quotes_and_newlines() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_field("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn parses_views_dry_run_command() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "views",
            "--archive",
            "/tmp/wechat-archive",
            "--dry-run",
            "--json",
        ])
        .unwrap();

        match cli.command {
            Commands::Views(args) => {
                assert_eq!(args.archive, PathBuf::from("/tmp/wechat-archive"));
                assert!(args.dry_run);
                assert!(!args.write);
                assert!(args.json);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn rejects_views_with_dry_run_and_write() {
        let result = Cli::try_parse_from([
            "wechat-archiver",
            "views",
            "--archive",
            "/tmp/wechat-archive",
            "--dry-run",
            "--write",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn parses_extract_db_videos_command() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "extract-db-videos",
            "--account",
            "/tmp/xwechat_files/wxid",
            "--message-db-dir",
            "/tmp/decrypted-message",
            "--archive",
            "/tmp/wechat-archive",
            "--dry-run",
            "--json",
            "--jsonl-progress",
        ])
        .unwrap();

        match cli.command {
            Commands::ExtractDbVideos {
                account,
                message_db_dir,
                archive,
                dry_run,
                json,
                jsonl_progress,
            } => {
                assert_eq!(account, PathBuf::from("/tmp/xwechat_files/wxid"));
                assert_eq!(
                    message_db_dir,
                    Some(PathBuf::from("/tmp/decrypted-message"))
                );
                assert_eq!(archive, PathBuf::from("/tmp/wechat-archive"));
                assert!(dry_run);
                assert!(json);
                assert!(jsonl_progress);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_extract_db_files_command() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "extract-db-files",
            "--account",
            "/tmp/xwechat_files/wxid",
            "--archive",
            "/tmp/wechat-archive",
            "--dry-run",
            "--json",
        ])
        .unwrap();

        match cli.command {
            Commands::ExtractDbFiles {
                account,
                message_db_dir,
                archive,
                dry_run,
                json,
                jsonl_progress,
            } => {
                assert_eq!(account, PathBuf::from("/tmp/xwechat_files/wxid"));
                assert_eq!(message_db_dir, None);
                assert_eq!(archive, PathBuf::from("/tmp/wechat-archive"));
                assert!(dry_run);
                assert!(json);
                assert!(!jsonl_progress);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_extract_db_voices_command() {
        let cli = Cli::try_parse_from([
            "wechat-archiver",
            "extract-db-voices",
            "--account",
            "/tmp/xwechat_files/wxid",
            "--message-db-dir",
            "/tmp/decrypted-message",
            "--archive",
            "/tmp/wechat-archive",
            "--dry-run",
            "--json",
        ])
        .unwrap();

        match cli.command {
            Commands::ExtractDbVoices {
                account,
                message_db_dir,
                archive,
                dry_run,
                json,
                jsonl_progress,
            } => {
                assert_eq!(account, PathBuf::from("/tmp/xwechat_files/wxid"));
                assert_eq!(
                    message_db_dir,
                    Some(PathBuf::from("/tmp/decrypted-message"))
                );
                assert_eq!(archive, PathBuf::from("/tmp/wechat-archive"));
                assert!(dry_run);
                assert!(json);
                assert!(!jsonl_progress);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn accepts_video_extract_type() {
        ensure_supported_extract_types(&[MediaType::Video]).unwrap();
    }

    #[test]
    fn accepts_file_extract_type() {
        ensure_supported_extract_types(&[MediaType::File]).unwrap();
    }

    #[test]
    fn accepts_voice_extract_type() {
        ensure_supported_extract_types(&[MediaType::Voice]).unwrap();
    }

    #[test]
    fn accepts_multi_type_extract() {
        ensure_supported_extract_types(&[MediaType::Image, MediaType::Video]).unwrap();
    }

    #[test]
    fn aggregates_multi_type_extract_summaries() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("image.jpg"), b"\xff\xd8\xffimage\xff\xd9").unwrap();
        std::fs::write(source.join("video.mp4"), b"video").unwrap();
        std::fs::write(source.join("document.pdf"), b"file").unwrap();
        std::fs::write(source.join("voice.silk"), b"\x02voice").unwrap();

        let summary = run_extract_many(
            &[
                MediaType::Image,
                MediaType::Video,
                MediaType::File,
                MediaType::Voice,
            ],
            ImageExtractArgs {
                source,
                archive: archive.clone(),
                dry_run: true,
                image_aes_key: None,
                image_xor_key: "0x88".to_string(),
                wxgf_mode: WxgfMode::Jpg,
                wxgf_ffmpeg_path: None,
                json: true,
                jsonl_progress: false,
            },
        )
        .unwrap();

        assert_eq!(summary.media_types, vec!["image", "video", "file", "voice"]);
        assert_eq!(summary.summaries.len(), 4);
        assert_eq!(summary.summaries[0].summary.candidates, 1);
        assert_eq!(summary.summaries[1].summary.candidates, 1);
        assert_eq!(summary.summaries[2].summary.candidates, 4);
        assert_eq!(summary.summaries[3].summary.candidates, 1);
        assert_eq!(summary.candidates, 7);
        assert_eq!(summary.would_archive, 7);
        assert_eq!(summary.archived, 0);
        assert_eq!(summary.already_archived, 0);
        assert_eq!(summary.reused_records, 0);
        assert_eq!(summary.decoded_dat, 0);
        assert_eq!(summary.metadata_backfilled, 0);
        assert_eq!(summary.new_objects, 0);
        assert_eq!(summary.existing_objects, 0);
        assert!(!archive.exists());
    }
}
