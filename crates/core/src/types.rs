use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScanAction {
    Archived,
    AlreadyArchived,
    Unsupported,
    Failed,
    WouldArchive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEvent {
    pub event: String,
    pub run_id: String,
    pub timestamp_epoch_ms: u128,
    pub source_path: String,
    pub source_relative_path: String,
    pub source_kind: String,
    pub media_type: String,
    pub original_filename: Option<String>,
    pub mime_type: Option<String>,
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
    pub duration_ms: Option<u64>,
    pub message_talker: Option<String>,
    pub message_sender: Option<String>,
    pub message_local_id: Option<i64>,
    pub message_create_time: Option<i64>,
    pub decoder: Option<String>,
    pub decode_fingerprint: Option<String>,
    pub action: ScanAction,
    pub archive_path: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub source_size_bytes: Option<u64>,
    pub source_modified_ms: Option<i64>,
    pub extension: Option<String>,
    pub decrypt_status: String,
    pub verify_status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractSummary {
    pub run_id: String,
    pub source_dir: PathBuf,
    pub archive_dir: PathBuf,
    pub dry_run: bool,
    pub scanned_files: u64,
    pub candidates: u64,
    pub would_archive: u64,
    pub archived: u64,
    pub already_archived: u64,
    pub unsupported: u64,
    pub failed: u64,
    pub manifest_path: Option<PathBuf>,
    pub index_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported_explanation: Option<UnsupportedExplanation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UnsupportedExplanation {
    pub reasons: Vec<UnsupportedReasonSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsupportedReasonSummary {
    pub reason: String,
    pub message: String,
    pub count: u64,
    pub samples: Vec<String>,
}

impl ExtractSummary {
    pub(crate) fn new(
        run_id: String,
        source_dir: PathBuf,
        archive_dir: PathBuf,
        dry_run: bool,
    ) -> Self {
        Self {
            run_id,
            source_dir,
            archive_dir,
            dry_run,
            scanned_files: 0,
            candidates: 0,
            would_archive: 0,
            archived: 0,
            already_archived: 0,
            unsupported: 0,
            failed: 0,
            manifest_path: None,
            index_path: None,
            unsupported_explanation: None,
        }
    }

    pub(crate) fn enable_unsupported_explanation(&mut self) {
        self.unsupported_explanation = Some(UnsupportedExplanation::default());
    }

    pub(crate) fn record_unsupported(&mut self, reason: String, sample: Option<String>) {
        let Some(explanation) = self.unsupported_explanation.as_mut() else {
            return;
        };
        let entry = explanation
            .reasons
            .iter_mut()
            .find(|entry| entry.reason == reason);
        let entry = match entry {
            Some(entry) => entry,
            None => {
                let message = unsupported_reason_message(&reason).to_string();
                explanation.reasons.push(UnsupportedReasonSummary {
                    reason,
                    message,
                    count: 0,
                    samples: Vec::new(),
                });
                explanation
                    .reasons
                    .last_mut()
                    .expect("inserted reason must exist")
            }
        };
        entry.count += 1;
        if let Some(sample) = sample {
            if entry.samples.len() < 3 {
                entry.samples.push(sample);
            }
        }
    }

    pub(crate) fn finish_unsupported_explanation(&mut self) {
        let Some(explanation) = self.unsupported_explanation.as_mut() else {
            return;
        };
        explanation.reasons.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then(left.reason.cmp(&right.reason))
        });
    }
}

pub fn unsupported_reason_message(reason: &str) -> &'static str {
    match reason {
        "dat_file_too_small" => ".dat 文件过小，无法识别为图片数据。",
        "v2_aes_key_missing" => "V2 AES .dat 需要显式传入 --image-aes-key。",
        "v2_aes_key_too_short" => "--image-aes-key 至少需要 16 字节。",
        "xor_key_not_detected" => "旧 XOR .dat 未能从文件头自动识别 XOR key。",
        "aes_dat_file_too_small" => "AES .dat 文件过小，缺少必要分段信息。",
        "aes_dat_invalid_segments" => "AES .dat 分段长度异常，无法安全解码。",
        "aes_dat_decrypt_failed" => "AES 解密失败，通常是 key 不匹配或文件损坏。",
        "decrypted_magic_not_image" => "解密后文件头不是已知图片或 wxgf 格式。",
        "wxgf_mode_off" => "当前 --wxgf-mode=off，wxgf 私有图片未处理。",
        "wxgf_hevc_partition_not_found" => "wxgf 中未找到可用 HEVC 分片。",
        "wxgf_ffmpeg_not_found" => {
            "未找到 ffmpeg；请安装 ffmpeg、传入 --wxgf-ffmpeg-path，或改用 --wxgf-mode raw/off。"
        }
        "wxgf_ffmpeg_probe_failed" => {
            "ffmpeg 可执行文件探测失败，请检查路径、权限或可执行文件是否损坏。"
        }
        "wxgf_ffmpeg_spawn_failed" => "ffmpeg 启动失败，请检查路径和执行权限。",
        "wxgf_ffmpeg_stdin_unavailable" => "ffmpeg stdin 管道不可用，无法传入 HEVC 数据。",
        "wxgf_ffmpeg_write_failed" => "向 ffmpeg 写入 HEVC 数据失败。",
        "wxgf_ffmpeg_wait_failed" => "等待 ffmpeg 执行结果失败。",
        "wxgf_ffmpeg_exit_failed" => "ffmpeg 转码进程返回失败状态。",
        "wxgf_ffmpeg_output_empty" => "ffmpeg 执行成功但没有输出数据。",
        "wxgf_ffmpeg_output_not_jpeg" => "ffmpeg 输出不是 JPEG 数据，无法作为 wxgf_jpg 归档。",
        "wxgf_ffmpeg_output_not_mp4" => "ffmpeg 输出不是 MP4 数据，无法作为 wxgf_mp4 归档。",
        "wxgf_invalid_mode" => "内部错误：wxgf 转码收到不支持的模式。",
        _ => "未支持的媒体格式或解码路径。",
    }
}

pub(crate) fn now_epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
