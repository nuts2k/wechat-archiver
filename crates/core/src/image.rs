use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Aes128;

use crate::config::{DatDecodeOptions, WxgfMode};
use crate::error::{IoContext, Result};

const V2_MAGIC_FULL: &[u8; 6] = b"\x07\x08V2\x08\x07";
const V1_MAGIC_FULL: &[u8; 6] = b"\x07\x08V1\x08\x07";
const WXGF_MAGIC: &[u8; 4] = b"wxgf";
const WXGF_MIN_HEVC_RATIO: f64 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ImageDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DatDecode {
    Decoded {
        bytes: Vec<u8>,
        extension: &'static str,
        decoder: &'static str,
    },
    Validated {
        extension: &'static str,
        decoder: &'static str,
    },
    Unsupported {
        reason: &'static str,
    },
}

pub(crate) fn direct_image_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("jpg"),
        "png" => Some("png"),
        "gif" => Some("gif"),
        "bmp" => Some("bmp"),
        "webp" => Some("webp"),
        "tif" | "tiff" => Some("tif"),
        "heic" => Some("heic"),
        "heif" => Some("heif"),
        _ => None,
    }
}

pub(crate) fn is_dat_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("dat"))
        .unwrap_or(false)
}

pub(crate) fn detect_image_dimensions_from_file(path: &Path) -> Option<ImageDimensions> {
    let bytes = std::fs::read(path).ok()?;
    detect_image_dimensions(&bytes)
}

pub(crate) fn detect_image_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    detect_png_dimensions(bytes)
        .or_else(|| detect_jpeg_dimensions(bytes))
        .or_else(|| detect_gif_dimensions(bytes))
        .or_else(|| detect_webp_dimensions(bytes))
        .or_else(|| detect_bmp_dimensions(bytes))
        .or_else(|| detect_tiff_dimensions(bytes))
}

pub(crate) fn decode_dat(path: &Path, options: &DatDecodeOptions) -> Result<DatDecode> {
    decode_dat_inner(path, options, false)
}

pub(crate) fn validate_dat(path: &Path, options: &DatDecodeOptions) -> Result<DatDecode> {
    decode_dat_inner(path, options, true)
}

fn decode_dat_inner(
    path: &Path,
    options: &DatDecodeOptions,
    validate_only: bool,
) -> Result<DatDecode> {
    let mut file = File::open(path).with_path(path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data).with_path(path)?;

    if data.len() < 4 {
        return Ok(DatDecode::Unsupported {
            reason: "dat_file_too_small",
        });
    }

    if data.starts_with(V1_MAGIC_FULL) {
        return decode_aes_dat(&data, b"cfcd208495d565ef", options, "v1_aes", validate_only);
    }

    if data.starts_with(V2_MAGIC_FULL) {
        let Some(key) = options.image_aes_key.as_deref() else {
            return Ok(DatDecode::Unsupported {
                reason: "v2_aes_key_missing",
            });
        };
        if key.len() < 16 {
            return Ok(DatDecode::Unsupported {
                reason: "v2_aes_key_too_short",
            });
        }
        return decode_aes_dat(&data, &key[..16], options, "v2_aes", validate_only);
    }

    let Some(key) = detect_xor_key(&data) else {
        return Ok(DatDecode::Unsupported {
            reason: "xor_key_not_detected",
        });
    };

    let decrypted: Vec<u8> = data.into_iter().map(|byte| byte ^ key).collect();
    Ok(decoded_image(
        decrypted,
        options,
        "legacy_xor",
        validate_only,
    ))
}

pub(crate) fn detect_image_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("jpg");
    }
    if bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47]) {
        return Some("png");
    }
    if bytes.starts_with(b"GIF") {
        return Some("gif");
    }
    if bytes.starts_with(b"BM") {
        return Some("bmp");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    if bytes.starts_with(&[0x49, 0x49, 0x2a, 0x00]) {
        return Some("tif");
    }
    None
}

fn decode_aes_dat(
    data: &[u8],
    aes_key: &[u8],
    options: &DatDecodeOptions,
    decoder: &'static str,
    validate_only: bool,
) -> Result<DatDecode> {
    if data.len() < 15 {
        return Ok(DatDecode::Unsupported {
            reason: "aes_dat_file_too_small",
        });
    }

    let aes_size = u32::from_le_bytes([data[6], data[7], data[8], data[9]]) as usize;
    let xor_size = u32::from_le_bytes([data[10], data[11], data[12], data[13]]) as usize;
    let aligned_aes_size = aligned_aes_block_size(aes_size);
    let aes_start = 15usize;
    let aes_end = aes_start.saturating_add(aligned_aes_size);
    if aes_end > data.len() || xor_size > data.len().saturating_sub(aes_end) {
        return Ok(DatDecode::Unsupported {
            reason: "aes_dat_invalid_segments",
        });
    }

    let Some(mut decrypted) = decrypt_aes_128_ecb_pkcs7(&data[aes_start..aes_end], aes_key) else {
        return Ok(DatDecode::Unsupported {
            reason: "aes_dat_decrypt_failed",
        });
    };

    let raw_end = data.len() - xor_size;
    if aes_end < raw_end {
        decrypted.extend_from_slice(&data[aes_end..raw_end]);
    }
    if xor_size > 0 {
        decrypted.extend(
            data[raw_end..]
                .iter()
                .map(|byte| byte ^ options.image_xor_key),
        );
    }

    Ok(decoded_image(decrypted, options, decoder, validate_only))
}

fn decoded_image(
    decrypted: Vec<u8>,
    options: &DatDecodeOptions,
    decoder: &'static str,
    validate_only: bool,
) -> DatDecode {
    if let Some(extension) = detect_image_format(&decrypted) {
        if validate_only {
            return DatDecode::Validated { extension, decoder };
        }
        return DatDecode::Decoded {
            bytes: decrypted,
            extension,
            decoder,
        };
    }

    if decrypted.starts_with(WXGF_MAGIC) {
        return decode_wxgf(decrypted, options, validate_only);
    }

    DatDecode::Unsupported {
        reason: "decrypted_magic_not_image",
    }
}

fn decode_wxgf(decrypted: Vec<u8>, options: &DatDecodeOptions, validate_only: bool) -> DatDecode {
    match options.wxgf_mode {
        WxgfMode::Off => DatDecode::Unsupported {
            reason: "wxgf_mode_off",
        },
        WxgfMode::Raw => {
            if validate_only {
                DatDecode::Validated {
                    extension: "wxgf",
                    decoder: "wxgf_raw",
                }
            } else {
                DatDecode::Decoded {
                    bytes: decrypted,
                    extension: "wxgf",
                    decoder: "wxgf_raw",
                }
            }
        }
        WxgfMode::Jpg => {
            let Some(hevc) = find_wxgf_hevc_partition(&decrypted) else {
                return DatDecode::Unsupported {
                    reason: "wxgf_hevc_partition_not_found",
                };
            };
            if validate_only {
                return match probe_ffmpeg(options) {
                    Ok(()) => DatDecode::Validated {
                        extension: "jpg",
                        decoder: "wxgf_jpg",
                    },
                    Err(reason) => DatDecode::Unsupported { reason },
                };
            }
            match transcode_wxgf_hevc(hevc, options, WxgfMode::Jpg) {
                Ok(bytes) if bytes.starts_with(&[0xff, 0xd8, 0xff]) => DatDecode::Decoded {
                    bytes,
                    extension: "jpg",
                    decoder: "wxgf_jpg",
                },
                Ok(_) => DatDecode::Unsupported {
                    reason: "wxgf_ffmpeg_output_not_jpeg",
                },
                Err(reason) => DatDecode::Unsupported { reason },
            }
        }
        WxgfMode::Mp4 => {
            let Some(hevc) = find_wxgf_hevc_partition(&decrypted) else {
                return DatDecode::Unsupported {
                    reason: "wxgf_hevc_partition_not_found",
                };
            };
            if validate_only {
                return match probe_ffmpeg(options) {
                    Ok(()) => DatDecode::Validated {
                        extension: "mp4",
                        decoder: "wxgf_mp4",
                    },
                    Err(reason) => DatDecode::Unsupported { reason },
                };
            }
            match transcode_wxgf_hevc(hevc, options, WxgfMode::Mp4) {
                Ok(bytes) if looks_like_mp4(&bytes) => DatDecode::Decoded {
                    bytes,
                    extension: "mp4",
                    decoder: "wxgf_mp4",
                },
                Ok(_) => DatDecode::Unsupported {
                    reason: "wxgf_ffmpeg_output_not_mp4",
                },
                Err(reason) => DatDecode::Unsupported { reason },
            }
        }
    }
}

fn find_wxgf_hevc_partition(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 15 || !data.starts_with(WXGF_MAGIC) {
        return None;
    }

    let header_len = data[4] as usize;
    if header_len >= data.len() {
        return None;
    }

    for pattern in [&[0x00, 0x00, 0x00, 0x01][..], &[0x00, 0x00, 0x01][..]] {
        let mut search_start = header_len;
        while search_start < data.len() {
            let Some(relative_index) = find_subslice(&data[search_start..], pattern) else {
                break;
            };
            let index = search_start + relative_index;
            if index >= 4 {
                let length = u32::from_be_bytes([
                    data[index - 4],
                    data[index - 3],
                    data[index - 2],
                    data[index - 1],
                ]) as usize;
                if length > 0
                    && index
                        .checked_add(length)
                        .is_some_and(|end| end <= data.len())
                    && (length as f64 / data.len() as f64) >= WXGF_MIN_HEVC_RATIO
                {
                    return Some(&data[index..index + length]);
                }
            }
            search_start = index + pattern.len();
        }
    }

    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn transcode_wxgf_hevc(
    hevc: &[u8],
    options: &DatDecodeOptions,
    mode: WxgfMode,
) -> std::result::Result<Vec<u8>, &'static str> {
    let mut command = ffmpeg_command(options);
    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("hevc")
        .arg("-i")
        .arg("-");

    match mode {
        WxgfMode::Jpg => {
            command
                .arg("-vframes")
                .arg("1")
                .arg("-c:v")
                .arg("mjpeg")
                .arg("-q:v")
                .arg("4")
                .arg("-f")
                .arg("image2")
                .arg("-");
        }
        WxgfMode::Mp4 => {
            command
                .arg("-c:v")
                .arg("copy")
                .arg("-movflags")
                .arg("frag_keyframe+empty_moov")
                .arg("-f")
                .arg("mp4")
                .arg("-");
        }
        WxgfMode::Off | WxgfMode::Raw => return Err("wxgf_invalid_mode"),
    }

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(map_ffmpeg_spawn_error)?;

    let mut stdin = child.stdin.take().ok_or("wxgf_ffmpeg_stdin_unavailable")?;
    if stdin.write_all(hevc).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("wxgf_ffmpeg_write_failed");
    }
    drop(stdin);

    let output = child
        .wait_with_output()
        .map_err(|_| "wxgf_ffmpeg_wait_failed")?;
    if !output.status.success() {
        return Err("wxgf_ffmpeg_exit_failed");
    }
    if output.stdout.is_empty() {
        return Err("wxgf_ffmpeg_output_empty");
    }
    Ok(output.stdout)
}

fn probe_ffmpeg(options: &DatDecodeOptions) -> std::result::Result<(), &'static str> {
    let status = ffmpeg_command(options)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(map_ffmpeg_spawn_error)?;
    if status.success() {
        Ok(())
    } else {
        Err("wxgf_ffmpeg_probe_failed")
    }
}

fn ffmpeg_command(options: &DatDecodeOptions) -> Command {
    if let Some(path) = &options.wxgf_ffmpeg_path {
        Command::new(path)
    } else {
        Command::new("ffmpeg")
    }
}

fn map_ffmpeg_spawn_error(error: std::io::Error) -> &'static str {
    if error.kind() == std::io::ErrorKind::NotFound {
        "wxgf_ffmpeg_not_found"
    } else {
        "wxgf_ffmpeg_spawn_failed"
    }
}

fn looks_like_mp4(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[4..8] == b"ftyp"
}

fn detect_png_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || &bytes[12..16] != b"IHDR" {
        return None;
    }
    dimensions(
        u32::from_be_bytes(bytes[16..20].try_into().ok()?),
        u32::from_be_bytes(bytes[20..24].try_into().ok()?),
    )
}

fn detect_gif_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 10 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return None;
    }
    dimensions(
        u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32,
        u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32,
    )
}

fn detect_bmp_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 26 || !bytes.starts_with(b"BM") {
        return None;
    }
    let width = i32::from_le_bytes(bytes[18..22].try_into().ok()?);
    let height = i32::from_le_bytes(bytes[22..26].try_into().ok()?);
    if width <= 0 || height == 0 {
        return None;
    }
    dimensions(width as u32, height.unsigned_abs())
}

fn detect_webp_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 20 || !bytes.starts_with(b"RIFF") || &bytes[8..12] != b"WEBP" {
        return None;
    }

    let mut offset = 12usize;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let fourcc = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().ok()?) as usize;
        let payload_start = offset + 8;
        let payload_end = payload_start.checked_add(size)?;
        if payload_end > bytes.len() {
            return None;
        }
        let payload = &bytes[payload_start..payload_end];
        match fourcc {
            b"VP8X" if payload.len() >= 10 => {
                return dimensions(
                    read_u24_le(&payload[4..7])?.checked_add(1)?,
                    read_u24_le(&payload[7..10])?.checked_add(1)?,
                );
            }
            b"VP8 " if payload.len() >= 10 && payload[3..6] == [0x9d, 0x01, 0x2a] => {
                let width = u16::from_le_bytes(payload[6..8].try_into().ok()?) & 0x3fff;
                let height = u16::from_le_bytes(payload[8..10].try_into().ok()?) & 0x3fff;
                return dimensions(width as u32, height as u32);
            }
            b"VP8L" if payload.len() >= 5 && payload[0] == 0x2f => {
                let b1 = payload[1] as u32;
                let b2 = payload[2] as u32;
                let b3 = payload[3] as u32;
                let b4 = payload[4] as u32;
                let width = ((b2 & 0x3f) << 8) | b1;
                let height = ((b4 & 0x0f) << 10) | (b3 << 2) | ((b2 & 0xc0) >> 6);
                return dimensions(width + 1, height + 1);
            }
            _ => {}
        }

        let padded_size = size + (size % 2);
        offset = payload_start.checked_add(padded_size)?;
    }

    None
}

fn detect_jpeg_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 4 || !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }

    let mut offset = 2usize;
    while offset < bytes.len() {
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        if offset >= bytes.len() {
            return None;
        }
        let marker = bytes[offset];
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            return None;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if offset.checked_add(2).is_none_or(|end| end > bytes.len()) {
            return None;
        }
        let segment_len = u16::from_be_bytes(bytes[offset..offset + 2].try_into().ok()?) as usize;
        if segment_len < 2 {
            return None;
        }
        let segment_start = offset + 2;
        let segment_end = offset.checked_add(segment_len)?;
        if segment_end > bytes.len() {
            return None;
        }
        if is_jpeg_sof_marker(marker) {
            if segment_start
                .checked_add(5)
                .is_none_or(|end| end > segment_end)
            {
                return None;
            }
            let height = u16::from_be_bytes(
                bytes[segment_start + 1..segment_start + 3]
                    .try_into()
                    .ok()?,
            );
            let width = u16::from_be_bytes(
                bytes[segment_start + 3..segment_start + 5]
                    .try_into()
                    .ok()?,
            );
            return dimensions(width as u32, height as u32);
        }
        offset = segment_end;
    }

    None
}

fn detect_tiff_dimensions(bytes: &[u8]) -> Option<ImageDimensions> {
    if bytes.len() < 8 {
        return None;
    }
    let little_endian = match &bytes[0..4] {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let ifd_offset = read_u32(bytes, 4, little_endian)? as usize;
    if ifd_offset
        .checked_add(2)
        .is_none_or(|end| end > bytes.len())
    {
        return None;
    }
    let entry_count = read_u16(bytes, ifd_offset, little_endian)? as usize;
    let mut width = None;
    let mut height = None;
    for index in 0..entry_count {
        let entry_offset = ifd_offset
            .checked_add(2)?
            .checked_add(index.checked_mul(12)?)?;
        if entry_offset
            .checked_add(12)
            .is_none_or(|end| end > bytes.len())
        {
            return None;
        }
        let tag = read_u16(bytes, entry_offset, little_endian)?;
        if tag != 256 && tag != 257 {
            continue;
        }
        let value_type = read_u16(bytes, entry_offset + 2, little_endian)?;
        let count = read_u32(bytes, entry_offset + 4, little_endian)?;
        if count != 1 {
            continue;
        }
        let value = match value_type {
            3 => read_u16(bytes, entry_offset + 8, little_endian)? as u32,
            4 => read_u32(bytes, entry_offset + 8, little_endian)?,
            _ => continue,
        };
        if tag == 256 {
            width = Some(value);
        } else {
            height = Some(value);
        }
    }
    dimensions(width?, height?)
}

fn is_jpeg_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
    )
}

fn dimensions(width_px: u32, height_px: u32) -> Option<ImageDimensions> {
    if width_px == 0 || height_px == 0 {
        None
    } else {
        Some(ImageDimensions {
            width_px,
            height_px,
        })
    }
}

fn read_u24_le(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 3 {
        return None;
    }
    Some(bytes[0] as u32 | ((bytes[1] as u32) << 8) | ((bytes[2] as u32) << 16))
}

fn read_u16(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u16> {
    let end = offset.checked_add(2)?;
    let data = bytes.get(offset..end)?;
    Some(if little_endian {
        u16::from_le_bytes(data.try_into().ok()?)
    } else {
        u16::from_be_bytes(data.try_into().ok()?)
    })
}

fn read_u32(bytes: &[u8], offset: usize, little_endian: bool) -> Option<u32> {
    let end = offset.checked_add(4)?;
    let data = bytes.get(offset..end)?;
    Some(if little_endian {
        u32::from_le_bytes(data.try_into().ok()?)
    } else {
        u32::from_be_bytes(data.try_into().ok()?)
    })
}

fn aligned_aes_block_size(aes_size: usize) -> usize {
    if aes_size.is_multiple_of(16) {
        aes_size + 16
    } else {
        aes_size + (16 - aes_size % 16)
    }
}

fn decrypt_aes_128_ecb_pkcs7(data: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    if key.len() < 16 || data.is_empty() || !data.len().is_multiple_of(16) {
        return None;
    }

    let cipher = Aes128::new_from_slice(&key[..16]).ok()?;
    let mut output = Vec::with_capacity(data.len());
    for chunk in data.chunks_exact(16) {
        let mut block = aes::Block::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        output.extend_from_slice(&block);
    }

    pkcs7_unpad(output)
}

fn pkcs7_unpad(mut data: Vec<u8>) -> Option<Vec<u8>> {
    let padding = *data.last()? as usize;
    if padding == 0 || padding > 16 || padding > data.len() {
        return None;
    }
    if !data[data.len() - padding..]
        .iter()
        .all(|byte| *byte as usize == padding)
    {
        return None;
    }
    data.truncate(data.len() - padding);
    Some(data)
}

fn detect_xor_key(data: &[u8]) -> Option<u8> {
    let magics: &[(&str, &[u8])] = &[
        ("png", &[0x89, 0x50, 0x4e, 0x47]),
        ("gif", &[0x47, 0x49, 0x46, 0x38]),
        ("tif", &[0x49, 0x49, 0x2a, 0x00]),
        ("webp", &[0x52, 0x49, 0x46, 0x46]),
        ("wxgf", &[0x77, 0x78, 0x67, 0x66]),
        ("jpg", &[0xff, 0xd8, 0xff]),
    ];

    for (_fmt, magic) in magics {
        if data.len() < magic.len() {
            continue;
        }
        let key = data[0] ^ magic[0];
        if magic
            .iter()
            .enumerate()
            .all(|(index, expected)| (data[index] ^ key) == *expected)
        {
            return Some(key);
        }
    }

    if data.len() >= 14 {
        let key = data[0] ^ b'B';
        if data[1] ^ key == b'M' {
            return Some(key);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncrypt;

    #[test]
    fn detects_common_image_formats() {
        assert_eq!(detect_image_format(&[0xff, 0xd8, 0xff, 0x00]), Some("jpg"));
        assert_eq!(detect_image_format(b"\x89PNG\r\n"), Some("png"));
        assert_eq!(detect_image_format(b"GIF89a"), Some("gif"));
        assert_eq!(detect_image_format(b"BMxxxx"), Some("bmp"));
    }

    #[test]
    fn detects_image_dimensions_from_headers() {
        assert_eq!(
            detect_image_dimensions(&synthetic_png(640, 480)),
            Some(ImageDimensions {
                width_px: 640,
                height_px: 480,
            })
        );
        assert_eq!(
            detect_image_dimensions(&synthetic_jpeg(320, 240)),
            Some(ImageDimensions {
                width_px: 320,
                height_px: 240,
            })
        );
        assert_eq!(
            detect_image_dimensions(&synthetic_gif(111, 222)),
            Some(ImageDimensions {
                width_px: 111,
                height_px: 222,
            })
        );
        assert_eq!(
            detect_image_dimensions(&synthetic_bmp(333, 444)),
            Some(ImageDimensions {
                width_px: 333,
                height_px: 444,
            })
        );
        assert_eq!(
            detect_image_dimensions(&synthetic_webp_vp8x(555, 666)),
            Some(ImageDimensions {
                width_px: 555,
                height_px: 666,
            })
        );
        assert_eq!(
            detect_image_dimensions(&synthetic_tiff(777, 888)),
            Some(ImageDimensions {
                width_px: 777,
                height_px: 888,
            })
        );
    }

    #[test]
    fn decodes_synthetic_xor_dat() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sample.dat");
        let original = b"\xff\xd8\xffsynthetic-jpeg\xff\xd9";
        let encrypted: Vec<u8> = original.iter().map(|byte| byte ^ 0x88).collect();
        std::fs::write(&path, encrypted).unwrap();

        let decoded = decode_dat(&path, &DatDecodeOptions::default()).unwrap();
        match decoded {
            DatDecode::Decoded {
                bytes,
                extension,
                decoder,
            } => {
                assert_eq!(bytes, original);
                assert_eq!(extension, "jpg");
                assert_eq!(decoder, "legacy_xor");
            }
            other => panic!("unexpected decode result: {other:?}"),
        }
    }

    #[test]
    fn decodes_synthetic_v1_aes_dat() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sample.dat");
        let original = b"\xff\xd8\xffsynthetic-v1-jpeg\xff\xd9";
        std::fs::write(
            &path,
            synthetic_aes_dat(V1_MAGIC_FULL, original, b"cfcd208495d565ef"),
        )
        .unwrap();

        let decoded = decode_dat(&path, &DatDecodeOptions::default()).unwrap();
        match decoded {
            DatDecode::Decoded {
                bytes,
                extension,
                decoder,
            } => {
                assert_eq!(bytes, original);
                assert_eq!(extension, "jpg");
                assert_eq!(decoder, "v1_aes");
            }
            other => panic!("unexpected decode result: {other:?}"),
        }
    }

    #[test]
    fn decodes_synthetic_v2_aes_dat_with_explicit_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sample.dat");
        let key = b"0123456789abcdef";
        let original = b"\x89PNG\r\nsynthetic-v2-png";
        std::fs::write(&path, synthetic_aes_dat(V2_MAGIC_FULL, original, key)).unwrap();

        let decoded = decode_dat(
            &path,
            &DatDecodeOptions {
                image_aes_key: Some(key.to_vec()),
                image_xor_key: 0x88,
                wxgf_mode: WxgfMode::Off,
                wxgf_ffmpeg_path: None,
            },
        )
        .unwrap();
        match decoded {
            DatDecode::Decoded {
                bytes,
                extension,
                decoder,
            } => {
                assert_eq!(bytes, original);
                assert_eq!(extension, "png");
                assert_eq!(decoder, "v2_aes");
            }
            other => panic!("unexpected decode result: {other:?}"),
        }
    }

    #[test]
    fn skips_v2_aes_dat_without_explicit_key() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sample.dat");
        std::fs::write(&path, [V2_MAGIC_FULL.as_slice(), &[0; 16]].concat()).unwrap();

        assert_eq!(
            decode_dat(&path, &DatDecodeOptions::default()).unwrap(),
            DatDecode::Unsupported {
                reason: "v2_aes_key_missing"
            }
        );
    }

    #[test]
    fn finds_wxgf_hevc_partition() {
        let hevc = synthetic_hevc();
        let wxgf = synthetic_wxgf(hevc);

        assert_eq!(find_wxgf_hevc_partition(&wxgf), Some(hevc));
    }

    #[test]
    fn decodes_wxgf_raw_when_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sample.dat");
        let key = b"0123456789abcdef";
        let wxgf = synthetic_wxgf(synthetic_hevc());
        std::fs::write(&path, synthetic_aes_dat(V2_MAGIC_FULL, &wxgf, key)).unwrap();

        let decoded = decode_dat(&path, &dat_options(key, WxgfMode::Raw, None)).unwrap();

        match decoded {
            DatDecode::Decoded {
                bytes,
                extension,
                decoder,
            } => {
                assert_eq!(bytes, wxgf);
                assert_eq!(extension, "wxgf");
                assert_eq!(decoder, "wxgf_raw");
            }
            other => panic!("unexpected decode result: {other:?}"),
        }
    }

    #[test]
    fn transcodes_wxgf_to_jpg_with_ffmpeg_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dat_path = tmp.path().join("sample.dat");
        let ffmpeg_path = tmp.path().join("fake-ffmpeg");
        let key = b"0123456789abcdef";
        let wxgf = synthetic_wxgf(synthetic_hevc());
        std::fs::write(&dat_path, synthetic_aes_dat(V2_MAGIC_FULL, &wxgf, key)).unwrap();
        write_fake_ffmpeg(&ffmpeg_path, "\\377\\330\\377synthetic-jpeg");

        let decoded = decode_dat(
            &dat_path,
            &dat_options(key, WxgfMode::Jpg, Some(ffmpeg_path)),
        )
        .unwrap();

        match decoded {
            DatDecode::Decoded {
                bytes,
                extension,
                decoder,
            } => {
                assert!(bytes.starts_with(&[0xff, 0xd8, 0xff]));
                assert_eq!(extension, "jpg");
                assert_eq!(decoder, "wxgf_jpg");
            }
            other => panic!("unexpected decode result: {other:?}"),
        }
    }

    #[test]
    fn validates_wxgf_jpg_by_probing_ffmpeg_without_transcoding() {
        let tmp = tempfile::tempdir().unwrap();
        let dat_path = tmp.path().join("sample.dat");
        let ffmpeg_path = tmp.path().join("probe-only-ffmpeg");
        let key = b"0123456789abcdef";
        let wxgf = synthetic_wxgf(synthetic_hevc());
        std::fs::write(&dat_path, synthetic_aes_dat(V2_MAGIC_FULL, &wxgf, key)).unwrap();
        write_probe_only_ffmpeg(&ffmpeg_path);

        let validated = validate_dat(
            &dat_path,
            &dat_options(key, WxgfMode::Jpg, Some(ffmpeg_path.clone())),
        )
        .unwrap();

        assert_eq!(
            validated,
            DatDecode::Validated {
                extension: "jpg",
                decoder: "wxgf_jpg",
            }
        );

        let decoded = decode_dat(
            &dat_path,
            &dat_options(key, WxgfMode::Jpg, Some(ffmpeg_path)),
        )
        .unwrap();
        assert_eq!(
            decoded,
            DatDecode::Unsupported {
                reason: "wxgf_ffmpeg_exit_failed"
            }
        );
    }

    fn synthetic_aes_dat(magic: &[u8; 6], plain: &[u8], key: &[u8]) -> Vec<u8> {
        let encrypted = encrypt_aes_128_ecb_pkcs7(plain, key);
        let mut data = Vec::new();
        data.extend_from_slice(magic);
        data.extend_from_slice(&(plain.len() as u32).to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.push(0);
        data.extend_from_slice(&encrypted);
        data
    }

    fn synthetic_hevc() -> &'static [u8] {
        b"\x00\x00\x00\x01\x40\x01synthetic-hevc-sample-with-enough-bytes-for-ratio"
    }

    fn synthetic_wxgf(hevc: &[u8]) -> Vec<u8> {
        let header_len = 18usize;
        let mut data = vec![0u8; header_len];
        data[0..4].copy_from_slice(WXGF_MAGIC);
        data[4] = header_len as u8;
        data.extend_from_slice(&(hevc.len() as u32).to_be_bytes());
        data.extend_from_slice(hevc);
        data
    }

    fn dat_options(
        key: &[u8],
        wxgf_mode: WxgfMode,
        wxgf_ffmpeg_path: Option<std::path::PathBuf>,
    ) -> DatDecodeOptions {
        DatDecodeOptions {
            image_aes_key: Some(key.to_vec()),
            image_xor_key: 0x88,
            wxgf_mode,
            wxgf_ffmpeg_path,
        }
    }

    fn write_fake_ffmpeg(path: &Path, output: &str) {
        std::fs::write(
            path,
            format!("#!/bin/sh\ncat >/dev/null\nprintf '{output}'\n"),
        )
        .unwrap();
        make_executable(path);
    }

    fn write_probe_only_ffmpeg(path: &Path) {
        std::fs::write(
            path,
            "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then\n  printf 'ffmpeg fake\\n'\n  exit 0\nfi\nexit 42\n",
        )
        .unwrap();
        make_executable(path);
    }

    fn synthetic_png(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        data.extend_from_slice(&13u32.to_be_bytes());
        data.extend_from_slice(b"IHDR");
        data.extend_from_slice(&width.to_be_bytes());
        data.extend_from_slice(&height.to_be_bytes());
        data
    }

    fn synthetic_jpeg(width: u16, height: u16) -> Vec<u8> {
        vec![
            0xff,
            0xd8,
            0xff,
            0xe0,
            0x00,
            0x04,
            0x00,
            0x00,
            0xff,
            0xc0,
            0x00,
            0x0b,
            0x08,
            (height >> 8) as u8,
            height as u8,
            (width >> 8) as u8,
            width as u8,
            0x01,
            0x01,
            0x11,
            0x00,
        ]
    }

    fn synthetic_gif(width: u16, height: u16) -> Vec<u8> {
        let mut data = Vec::from(&b"GIF89a"[..]);
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data
    }

    fn synthetic_bmp(width: i32, height: i32) -> Vec<u8> {
        let mut data = vec![0u8; 26];
        data[0..2].copy_from_slice(b"BM");
        data[18..22].copy_from_slice(&width.to_le_bytes());
        data[22..26].copy_from_slice(&height.to_le_bytes());
        data
    }

    fn synthetic_webp_vp8x(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"RIFF");
        data.extend_from_slice(&18u32.to_le_bytes());
        data.extend_from_slice(b"WEBP");
        data.extend_from_slice(b"VP8X");
        data.extend_from_slice(&10u32.to_le_bytes());
        data.extend_from_slice(&[0, 0, 0, 0]);
        data.extend_from_slice(&u24_le(width - 1));
        data.extend_from_slice(&u24_le(height - 1));
        data
    }

    fn synthetic_tiff(width: u32, height: u32) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"II*\0");
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&256u16.to_le_bytes());
        data.extend_from_slice(&4u16.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&257u16.to_le_bytes());
        data.extend_from_slice(&4u16.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data
    }

    fn u24_le(value: u32) -> [u8; 3] {
        [
            (value & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            ((value >> 16) & 0xff) as u8,
        ]
    }

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
        }
    }

    fn encrypt_aes_128_ecb_pkcs7(plain: &[u8], key: &[u8]) -> Vec<u8> {
        let cipher = Aes128::new_from_slice(&key[..16]).unwrap();
        let mut data = plain.to_vec();
        let padding = 16 - (data.len() % 16);
        data.extend(std::iter::repeat_n(padding as u8, padding));
        for chunk in data.chunks_exact_mut(16) {
            let block = aes::Block::from_mut_slice(chunk);
            cipher.encrypt_block(block);
        }
        data
    }
}
