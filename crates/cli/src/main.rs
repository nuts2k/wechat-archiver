use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use wechat_archiver_core::WxgfMode as CoreWxgfMode;
use wechat_archiver_core::{
    archive_status, discover_wechat, extract_images, extract_message_db_images, verify_archive,
    ArchiveConfig, ArchiveStatus, DatDecodeOptions, DiscoverOptions, ExtractSummary,
    MessageDbExtractConfig, VerifySummary, WechatDiscovery,
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

        /// 输出 JSON。
        #[arg(long)]
        json: bool,
    },

    /// 归档普通图片和可识别旧 XOR .dat 图片。
    ExtractImages {
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
    },

    /// 从已解密/普通 SQLite 微信消息库枚举图片消息并归档。
    ExtractDbImages {
        /// 单个微信账号目录，通常是 xwechat_files/<wxid>。
        #[arg(long)]
        account: PathBuf,

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
        Commands::Scan {
            source,
            archive,
            image_aes_key,
            image_xor_key,
            wxgf_mode,
            wxgf_ffmpeg_path,
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
            })?;
            print_extract_summary(&summary, json)?;
        }
        Commands::ExtractImages {
            source,
            archive,
            dry_run,
            image_aes_key,
            image_xor_key,
            wxgf_mode,
            wxgf_ffmpeg_path,
            json,
        } => {
            let summary = extract_images(ArchiveConfig {
                source_dir: source,
                archive_dir: archive,
                dry_run,
                dat_options: parse_dat_options(
                    image_aes_key,
                    &image_xor_key,
                    wxgf_mode,
                    wxgf_ffmpeg_path,
                )?,
            })?;
            print_extract_summary(&summary, json)?;
        }
        Commands::ExtractDbImages {
            account,
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
                archive_dir: archive,
                dry_run,
                dat_options: parse_dat_options(
                    image_aes_key,
                    &image_xor_key,
                    wxgf_mode,
                    wxgf_ffmpeg_path,
                )?,
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
    if let Some(path) = &summary.index_path {
        println!("index: {}", path.display());
    }
    if let Some(path) = &summary.manifest_path {
        println!("manifest: {}", path.display());
    }
    Ok(())
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
