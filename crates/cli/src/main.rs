use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use wechat_archiver_core::WxgfMode as CoreWxgfMode;
use wechat_archiver_core::{
    archive_status, count_message_db_media, derive_image_key, discover_wechat, extract_files,
    extract_images, extract_message_db_files, extract_message_db_images, extract_message_db_videos,
    extract_videos, extract_voices, inspect_message_db, verify_archive, ArchiveConfig,
    ArchiveStatus, DatDecodeOptions, DeriveImageKeyOptions, DiscoverOptions, ExtractSummary,
    ImageKeyDerivation, ImageKeyMethod, MessageDbExtractConfig, MessageDbInspectConfig,
    MessageDbInspection, MessageDbMediaCountConfig, MessageDbMediaCountSummary,
    MessageDbMediaTypeCount, VerifySummary, WechatDiscovery,
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
    },

    /// 统一媒体抽取入口；当前已接入 image、video、file、voice。
    Extract {
        /// 媒体类型，支持逗号分隔；当前每次只执行一种类型。
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
        } => {
            let summary = extract_images(ArchiveConfig {
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
            })?;
            print_extract_summary(&summary, json)?;
        }
        Commands::Extract { media_types, args } => {
            ensure_supported_extract_types(&media_types)?;
            let json = args.json;
            let summary = run_extract(media_types[0], args)?;
            print_extract_summary(&summary, json)?;
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
        } => {
            let summary = extract_message_db_images(MessageDbExtractConfig {
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
            })?;
            print_extract_summary(&summary, json)?;
        }
        Commands::ExtractDbVideos {
            account,
            message_db_dir,
            archive,
            dry_run,
            json,
        } => {
            let summary = extract_message_db_videos(MessageDbExtractConfig {
                account_dir: account,
                message_db_dir,
                archive_dir: archive,
                dry_run,
                dat_options: DatDecodeOptions::default(),
                explain_unsupported: false,
            })?;
            print_extract_summary(&summary, json)?;
        }
        Commands::ExtractDbFiles {
            account,
            message_db_dir,
            archive,
            dry_run,
            json,
        } => {
            let summary = extract_message_db_files(MessageDbExtractConfig {
                account_dir: account,
                message_db_dir,
                archive_dir: archive,
                dry_run,
                dat_options: DatDecodeOptions::default(),
                explain_unsupported: false,
            })?;
            print_extract_summary(&summary, json)?;
        }
        Commands::Status { archive, json } => {
            let status = archive_status(&archive)?;
            print_status(&status, json)?;
        }
        Commands::Verify { archive, json } => {
            let summary = verify_archive(&archive)?;
            print_verify_summary(&summary, json)?;
            if summary.missing > 0 || summary.mismatched > 0 {
                std::process::exit(2);
            }
        }
    }

    Ok(())
}

fn ensure_supported_extract_types(media_types: &[MediaType]) -> Result<()> {
    anyhow::ensure!(
        media_types.len() == 1,
        "extract --type currently accepts one media type per run; got: {}",
        media_types
            .iter()
            .copied()
            .map(media_type_name)
            .collect::<Vec<_>>()
            .join(",")
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
    Ok(extract_images(image_archive_config_from_args(args)?)?)
}

fn run_extract(media_type: MediaType, args: ImageExtractArgs) -> Result<ExtractSummary> {
    match media_type {
        MediaType::Image => run_image_extract(args),
        MediaType::Video => Ok(extract_videos(direct_media_archive_config_from_args(args))?),
        MediaType::File => Ok(extract_files(direct_media_archive_config_from_args(args))?),
        MediaType::Voice => Ok(extract_voices(direct_media_archive_config_from_args(args))?),
    }
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
    println!("note: voice 当前只统计消息表 local_type=34 行数，暂不解析语音资源 BLOB。");
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
    Ok(())
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
    println!("mismatched: {}", summary.mismatched);
    for failure in &summary.failures {
        println!(
            "failure: {} expected={} actual={:?} error={}",
            failure.archive_path, failure.expected_sha256, failure.actual_sha256, failure.error
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
        ])
        .unwrap();

        match cli.command {
            Commands::Extract { media_types, args } => {
                assert_eq!(media_types, vec![MediaType::Image]);
                assert_eq!(args.source, PathBuf::from("/tmp/wechat-source"));
                assert_eq!(args.archive, PathBuf::from("/tmp/wechat-archive"));
                assert!(args.dry_run);
                assert!(args.json);
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
        ])
        .unwrap();

        match cli.command {
            Commands::ExtractDbVideos {
                account,
                message_db_dir,
                archive,
                dry_run,
                json,
            } => {
                assert_eq!(account, PathBuf::from("/tmp/xwechat_files/wxid"));
                assert_eq!(
                    message_db_dir,
                    Some(PathBuf::from("/tmp/decrypted-message"))
                );
                assert_eq!(archive, PathBuf::from("/tmp/wechat-archive"));
                assert!(dry_run);
                assert!(json);
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
            } => {
                assert_eq!(account, PathBuf::from("/tmp/xwechat_files/wxid"));
                assert_eq!(message_db_dir, None);
                assert_eq!(archive, PathBuf::from("/tmp/wechat-archive"));
                assert!(dry_run);
                assert!(json);
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
    fn rejects_multi_type_extract_until_summary_model_exists() {
        let error = ensure_supported_extract_types(&[MediaType::Image, MediaType::Video])
            .expect_err("multi-type extraction is not wired yet");

        assert!(error
            .to_string()
            .contains("currently accepts one media type per run; got: image,video"));
    }
}
