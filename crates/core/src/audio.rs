use std::path::Path;

use crate::video::detect_video_metadata_from_file;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioMetadata {
    pub duration_ms: Option<u64>,
}

pub(crate) fn audio_duration_supported_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "wav" | "mp3" | "m4a" | "aac"
    )
}

pub(crate) fn detect_audio_metadata_from_file(
    path: &Path,
    extension: &str,
) -> Option<AudioMetadata> {
    match extension.to_ascii_lowercase().as_str() {
        "m4a" => detect_video_metadata_from_file(path).and_then(|metadata| {
            metadata.duration_ms.map(|duration_ms| AudioMetadata {
                duration_ms: Some(duration_ms),
            })
        }),
        "wav" | "mp3" | "aac" => {
            let bytes = std::fs::read(path).ok()?;
            detect_audio_metadata(&bytes, extension)
        }
        _ => None,
    }
}

pub(crate) fn detect_audio_metadata(bytes: &[u8], extension: &str) -> Option<AudioMetadata> {
    let duration_ms = match extension.to_ascii_lowercase().as_str() {
        "wav" => detect_wav_duration_ms(bytes),
        "mp3" => detect_mp3_duration_ms(bytes),
        "aac" => detect_adts_aac_duration_ms(bytes),
        _ => None,
    }?;
    Some(AudioMetadata {
        duration_ms: Some(duration_ms),
    })
}

fn detect_wav_duration_ms(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 12 || !bytes.starts_with(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return None;
    }

    let mut offset = 12usize;
    let mut byte_rate = None;
    let mut data_size = None;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let chunk_id = bytes.get(offset..offset + 4)?;
        let chunk_size =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().ok()?) as usize;
        let payload_start = offset + 8;
        let payload_end = payload_start.checked_add(chunk_size)?;
        if payload_end > bytes.len() {
            return None;
        }

        match chunk_id {
            b"fmt " if chunk_size >= 16 => {
                byte_rate = Some(u32::from_le_bytes(
                    bytes[payload_start + 8..payload_start + 12]
                        .try_into()
                        .ok()?,
                ) as u64);
            }
            b"data" => data_size = Some(chunk_size as u64),
            _ => {}
        }

        if byte_rate.is_some() && data_size.is_some() {
            break;
        }
        offset = payload_start.checked_add(chunk_size + (chunk_size % 2))?;
    }

    let byte_rate = byte_rate?;
    if byte_rate == 0 {
        return None;
    }
    data_size?.checked_mul(1000).map(|value| value / byte_rate)
}

fn detect_mp3_duration_ms(bytes: &[u8]) -> Option<u64> {
    let start = mp3_audio_start(bytes)?;
    let header_offset = find_mp3_frame_header(bytes, start)?;
    let bitrate_bps = mp3_frame_bitrate_bps(&bytes[header_offset..header_offset + 4])?;
    let tag_bytes = if bytes.len() >= 128 && bytes[bytes.len() - 128..].starts_with(b"TAG") {
        128
    } else {
        0
    };
    let audio_bytes = bytes
        .len()
        .checked_sub(header_offset)?
        .checked_sub(tag_bytes)? as u64;
    audio_bytes
        .checked_mul(8)?
        .checked_mul(1000)
        .map(|bits_ms| bits_ms / bitrate_bps)
}

fn mp3_audio_start(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 10 || !bytes.starts_with(b"ID3") {
        return Some(0);
    }
    let size = synchsafe_u32(&bytes[6..10])? as usize;
    let footer_size = if bytes[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + size + footer_size)
}

fn find_mp3_frame_header(bytes: &[u8], start: usize) -> Option<usize> {
    let search_end = bytes.len().min(start.saturating_add(128 * 1024));
    (start..search_end.saturating_sub(3)).find(|offset| {
        bytes[*offset] == 0xff
            && bytes[*offset + 1] & 0xe0 == 0xe0
            && mp3_frame_bitrate_bps(&bytes[*offset..*offset + 4]).is_some()
    })
}

fn mp3_frame_bitrate_bps(header: &[u8]) -> Option<u64> {
    if header.len() < 4 || header[0] != 0xff || header[1] & 0xe0 != 0xe0 {
        return None;
    }
    let version_id = (header[1] >> 3) & 0x03;
    let layer = (header[1] >> 1) & 0x03;
    let bitrate_index = (header[2] >> 4) & 0x0f;
    if version_id == 1 || layer != 1 || bitrate_index == 0 || bitrate_index == 15 {
        return None;
    }
    let kbps = if version_id == 3 {
        MPEG1_LAYER3_BITRATES[bitrate_index as usize]
    } else {
        MPEG2_LAYER3_BITRATES[bitrate_index as usize]
    };
    (kbps > 0).then_some(kbps as u64 * 1000)
}

fn detect_adts_aac_duration_ms(bytes: &[u8]) -> Option<u64> {
    let mut offset = 0usize;
    let mut frames = 0u64;
    let mut sample_rate = None;
    while offset.checked_add(7).is_some_and(|end| end <= bytes.len()) {
        if bytes[offset] != 0xff || bytes[offset + 1] & 0xf0 != 0xf0 {
            if frames == 0 {
                offset += 1;
                continue;
            }
            break;
        }
        let sample_rate_index = (bytes[offset + 2] >> 2) & 0x0f;
        let frame_sample_rate = AAC_SAMPLE_RATES.get(sample_rate_index as usize).copied()?;
        if frame_sample_rate == 0 {
            return None;
        }
        sample_rate.get_or_insert(frame_sample_rate as u64);
        let frame_len = (((bytes[offset + 3] & 0x03) as usize) << 11)
            | ((bytes[offset + 4] as usize) << 3)
            | (((bytes[offset + 5] & 0xe0) as usize) >> 5);
        if frame_len < 7
            || offset
                .checked_add(frame_len)
                .is_none_or(|end| end > bytes.len())
        {
            return None;
        }
        frames += 1;
        offset += frame_len;
    }

    let sample_rate = sample_rate?;
    (frames > 0).then(|| frames * 1024 * 1000 / sample_rate)
}

fn synchsafe_u32(bytes: &[u8]) -> Option<u32> {
    if bytes.len() != 4 || bytes.iter().any(|byte| byte & 0x80 != 0) {
        return None;
    }
    Some(
        ((bytes[0] as u32) << 21)
            | ((bytes[1] as u32) << 14)
            | ((bytes[2] as u32) << 7)
            | bytes[3] as u32,
    )
}

const MPEG1_LAYER3_BITRATES: [u16; 16] = [
    0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
];
const MPEG2_LAYER3_BITRATES: [u16; 16] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
];
const AAC_SAMPLE_RATES: [u32; 16] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350, 0, 0, 0,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_wav_duration() {
        let wav = synthetic_wav(44_100, 2, 16, 2_000);

        assert_eq!(
            detect_audio_metadata(&wav, "wav"),
            Some(AudioMetadata {
                duration_ms: Some(2_000),
            })
        );
    }

    #[test]
    fn detects_mp3_cbr_duration() {
        let mp3 = synthetic_mp3(128_000, 1_000);

        assert_eq!(
            detect_audio_metadata(&mp3, "mp3"),
            Some(AudioMetadata {
                duration_ms: Some(1_000),
            })
        );
    }

    #[test]
    fn detects_adts_aac_duration() {
        let aac = synthetic_adts_aac(44_100, 44);

        assert_eq!(
            detect_audio_metadata(&aac, "aac"),
            Some(AudioMetadata {
                duration_ms: Some(44 * 1024 * 1000 / 44_100),
            })
        );
    }

    fn synthetic_wav(
        sample_rate: u32,
        channels: u16,
        bits_per_sample: u16,
        duration_ms: u32,
    ) -> Vec<u8> {
        let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
        let data_size = byte_rate * duration_ms / 1000;
        let mut data = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&(36 + data_size).to_le_bytes());
        data.extend_from_slice(b"WAVE");
        data.extend_from_slice(b"fmt ");
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&channels.to_le_bytes());
        data.extend_from_slice(&sample_rate.to_le_bytes());
        data.extend_from_slice(&byte_rate.to_le_bytes());
        data.extend_from_slice(&(channels * bits_per_sample / 8).to_le_bytes());
        data.extend_from_slice(&bits_per_sample.to_le_bytes());
        data.extend_from_slice(b"data");
        data.extend_from_slice(&data_size.to_le_bytes());
        data.resize(data.len() + data_size as usize, 0);
        data
    }

    fn synthetic_mp3(bitrate_bps: u32, duration_ms: u32) -> Vec<u8> {
        assert_eq!(bitrate_bps, 128_000);
        let audio_bytes = bitrate_bps as usize * duration_ms as usize / 8 / 1000;
        let mut data = Vec::new();
        data.extend_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
        data.resize(audio_bytes, 0);
        data
    }

    fn synthetic_adts_aac(sample_rate: u32, frames: u16) -> Vec<u8> {
        assert_eq!(sample_rate, 44_100);
        let frame_len = 7usize;
        let mut data = Vec::new();
        for _ in 0..frames {
            data.extend_from_slice(&adts_header(frame_len));
        }
        data
    }

    fn adts_header(frame_len: usize) -> [u8; 7] {
        [
            0xff,
            0xf1,
            0x50,
            0x80 | (((frame_len >> 11) & 0x03) as u8),
            ((frame_len >> 3) & 0xff) as u8,
            (((frame_len & 0x07) << 5) as u8) | 0x1f,
            0xfc,
        ]
    }
}
