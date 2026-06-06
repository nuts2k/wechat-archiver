use std::fs::File;
use std::io::Read;
use std::path::Path;

use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Aes128;

use crate::config::DatDecodeOptions;
use crate::error::{IoContext, Result};

const V2_MAGIC_FULL: &[u8; 6] = b"\x07\x08V2\x08\x07";
const V1_MAGIC_FULL: &[u8; 6] = b"\x07\x08V1\x08\x07";

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
        return decode_aes_dat(&data, b"cfcd208495d565ef", options.image_xor_key, "v1_aes");
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
        return decode_aes_dat(&data, &key[..16], options.image_xor_key, "v2_aes");
    }

    let Some(key) = detect_xor_key(&data) else {
        return Ok(DatDecode::Unsupported {
            reason: "xor_key_not_detected",
        });
    };

    let decrypted: Vec<u8> = data.into_iter().map(|byte| byte ^ key).collect();
    let Some(extension) = detect_image_format(&decrypted) else {
        return Ok(DatDecode::Unsupported {
            reason: "decrypted_magic_not_image",
        });
    };

    Ok(DatDecode::Decoded {
        bytes: decrypted,
        extension,
        decoder: "legacy_xor",
    })
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
    xor_key: u8,
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
        decrypted.extend(data[raw_end..].iter().map(|byte| byte ^ xor_key));
    }

    let Some(extension) = detect_image_format(&decrypted) else {
        return Ok(DatDecode::Unsupported {
            reason: "decrypted_magic_not_image",
        });
    };

    Ok(DatDecode::Decoded {
        bytes: decrypted,
        extension,
        decoder,
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
