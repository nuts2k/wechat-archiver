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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DatDecode {
    Decoded {
        bytes: Vec<u8>,
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

pub(crate) fn decode_dat(path: &Path, options: &DatDecodeOptions) -> Result<DatDecode> {
    let mut file = File::open(path).with_path(path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data).with_path(path)?;

    if data.len() < 4 {
        return Ok(DatDecode::Unsupported {
            reason: "dat_file_too_small",
        });
    }

    if data.starts_with(V1_MAGIC_FULL) {
        return decode_aes_dat(&data, b"cfcd208495d565ef", options, "v1_aes");
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
        return decode_aes_dat(&data, &key[..16], options, "v2_aes");
    }

    let Some(key) = detect_xor_key(&data) else {
        return Ok(DatDecode::Unsupported {
            reason: "xor_key_not_detected",
        });
    };

    let decrypted: Vec<u8> = data.into_iter().map(|byte| byte ^ key).collect();
    Ok(decoded_image(decrypted, options, "legacy_xor"))
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

    Ok(decoded_image(decrypted, options, decoder))
}

fn decoded_image(
    decrypted: Vec<u8>,
    options: &DatDecodeOptions,
    decoder: &'static str,
) -> DatDecode {
    if let Some(extension) = detect_image_format(&decrypted) {
        return DatDecode::Decoded {
            bytes: decrypted,
            extension,
            decoder,
        };
    }

    if decrypted.starts_with(WXGF_MAGIC) {
        return decode_wxgf(decrypted, options);
    }

    DatDecode::Unsupported {
        reason: "decrypted_magic_not_image",
    }
}

fn decode_wxgf(decrypted: Vec<u8>, options: &DatDecodeOptions) -> DatDecode {
    match options.wxgf_mode {
        WxgfMode::Off => DatDecode::Unsupported {
            reason: "wxgf_mode_off",
        },
        WxgfMode::Raw => DatDecode::Decoded {
            bytes: decrypted,
            extension: "wxgf",
            decoder: "wxgf_raw",
        },
        WxgfMode::Jpg => {
            let Some(hevc) = find_wxgf_hevc_partition(&decrypted) else {
                return DatDecode::Unsupported {
                    reason: "wxgf_hevc_partition_not_found",
                };
            };
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
    let mut command = if let Some(path) = &options.wxgf_ffmpeg_path {
        Command::new(path)
    } else {
        Command::new("ffmpeg")
    };
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
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "wxgf_ffmpeg_not_found"
            } else {
                "wxgf_ffmpeg_failed"
            }
        })?;

    let mut stdin = child.stdin.take().ok_or("wxgf_ffmpeg_failed")?;
    if stdin.write_all(hevc).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return Err("wxgf_ffmpeg_failed");
    }
    drop(stdin);

    let output = child.wait_with_output().map_err(|_| "wxgf_ffmpeg_failed")?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err("wxgf_ffmpeg_failed");
    }
    Ok(output.stdout)
}

fn looks_like_mp4(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[4..8] == b"ftyp"
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
