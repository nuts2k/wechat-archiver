use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VideoMetadata {
    pub width_px: Option<u32>,
    pub height_px: Option<u32>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Default)]
struct VideoMetadataState {
    width_px: Option<u32>,
    height_px: Option<u32>,
    duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct BoxHeader {
    name: [u8; 4],
    header_size: u64,
    end: u64,
}

const MAX_PARSE_DEPTH: u8 = 8;

pub(crate) fn detect_video_metadata_from_file(path: &Path) -> Option<VideoMetadata> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut state = VideoMetadataState::default();
    parse_box_range(&mut file, 0, len, 0, &mut state).ok()?;
    if state.width_px.is_some() || state.height_px.is_some() || state.duration_ms.is_some() {
        Some(VideoMetadata {
            width_px: state.width_px,
            height_px: state.height_px,
            duration_ms: state.duration_ms,
        })
    } else {
        None
    }
}

fn parse_box_range(
    file: &mut File,
    start: u64,
    end: u64,
    depth: u8,
    state: &mut VideoMetadataState,
) -> std::io::Result<()> {
    if depth > MAX_PARSE_DEPTH {
        return Ok(());
    }

    let mut offset = start;
    while offset.checked_add(8).is_some_and(|header| header <= end) {
        file.seek(SeekFrom::Start(offset))?;
        let Some(header) = read_box_header(file, offset, end)? else {
            return Ok(());
        };
        let payload_start = offset + header.header_size;
        if payload_start > header.end {
            return Ok(());
        }

        match &header.name {
            b"mvhd" => read_mvhd(file, payload_start, header.end, state)?,
            b"tkhd" => read_tkhd(file, payload_start, header.end, state)?,
            name if is_container_box(name) => {
                parse_box_range(file, payload_start, header.end, depth + 1, state)?;
            }
            _ => {}
        }

        if state.duration_ms.is_some() && state.width_px.is_some() && state.height_px.is_some() {
            return Ok(());
        }

        if header.end <= offset {
            return Ok(());
        }
        offset = header.end;
    }

    Ok(())
}

fn read_box_header(
    file: &mut File,
    offset: u64,
    range_end: u64,
) -> std::io::Result<Option<BoxHeader>> {
    let mut header = [0u8; 8];
    file.read_exact(&mut header)?;
    let size32 = u32::from_be_bytes(header[0..4].try_into().expect("slice length"));
    let name = header[4..8].try_into().expect("slice length");

    let (size, header_size) = match size32 {
        0 => (range_end.saturating_sub(offset), 8),
        1 => {
            let mut largesize = [0u8; 8];
            file.read_exact(&mut largesize)?;
            (u64::from_be_bytes(largesize), 16)
        }
        value => (value as u64, 8),
    };

    if size < header_size {
        return Ok(None);
    }
    let Some(end) = offset.checked_add(size) else {
        return Ok(None);
    };
    if end > range_end {
        return Ok(None);
    }

    Ok(Some(BoxHeader {
        name,
        header_size,
        end,
    }))
}

fn read_mvhd(
    file: &mut File,
    payload_start: u64,
    payload_end: u64,
    state: &mut VideoMetadataState,
) -> std::io::Result<()> {
    if state.duration_ms.is_some() {
        return Ok(());
    }

    let payload_len = payload_end.saturating_sub(payload_start);
    let read_len = payload_len.min(32) as usize;
    if read_len < 20 {
        return Ok(());
    }

    file.seek(SeekFrom::Start(payload_start))?;
    let mut payload = vec![0u8; read_len];
    file.read_exact(&mut payload)?;

    let version = payload[0];
    let (timescale, duration) = match version {
        0 if payload.len() >= 20 => (
            u32::from_be_bytes(payload[12..16].try_into().expect("slice length")) as u64,
            u32::from_be_bytes(payload[16..20].try_into().expect("slice length")) as u64,
        ),
        1 if payload.len() >= 32 => (
            u32::from_be_bytes(payload[20..24].try_into().expect("slice length")) as u64,
            u64::from_be_bytes(payload[24..32].try_into().expect("slice length")),
        ),
        _ => return Ok(()),
    };

    if timescale == 0 || duration == 0 {
        return Ok(());
    }
    state.duration_ms = duration.checked_mul(1000).map(|value| value / timescale);
    Ok(())
}

fn read_tkhd(
    file: &mut File,
    payload_start: u64,
    payload_end: u64,
    state: &mut VideoMetadataState,
) -> std::io::Result<()> {
    if state.width_px.is_some() && state.height_px.is_some() {
        return Ok(());
    }

    if payload_end.saturating_sub(payload_start) < 8 {
        return Ok(());
    }

    file.seek(SeekFrom::Start(payload_end - 8))?;
    let mut dimensions = [0u8; 8];
    file.read_exact(&mut dimensions)?;
    let width_fixed = u32::from_be_bytes(dimensions[0..4].try_into().expect("slice length"));
    let height_fixed = u32::from_be_bytes(dimensions[4..8].try_into().expect("slice length"));
    let width = fixed_16_16_to_u32(width_fixed);
    let height = fixed_16_16_to_u32(height_fixed);
    if width == 0 || height == 0 {
        return Ok(());
    }

    state.width_px = Some(width);
    state.height_px = Some(height);
    Ok(())
}

fn fixed_16_16_to_u32(value: u32) -> u32 {
    let integer = value >> 16;
    let fraction = value & 0xffff;
    integer + u32::from(fraction >= 0x8000)
}

fn is_container_box(name: &[u8; 4]) -> bool {
    matches!(name, b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_mp4_duration_and_dimensions() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sample.mp4");
        std::fs::write(&path, synthetic_mp4(1920, 1080, 12_345)).unwrap();

        assert_eq!(
            detect_video_metadata_from_file(&path),
            Some(VideoMetadata {
                width_px: Some(1920),
                height_px: Some(1080),
                duration_ms: Some(12_345),
            })
        );
    }

    #[test]
    fn ignores_non_video_boxes_without_failing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sample.mp4");
        std::fs::write(&path, box_with_payload(*b"ftyp", b"isom")).unwrap();

        assert_eq!(detect_video_metadata_from_file(&path), None);
    }

    fn synthetic_mp4(width: u32, height: u32, duration_ms: u32) -> Vec<u8> {
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&[0, 0, 0, 0]);
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&1000u32.to_be_bytes());
        mvhd.extend_from_slice(&duration_ms.to_be_bytes());

        let mut tkhd = vec![0u8; 84];
        tkhd[0] = 0;
        tkhd[1..4].copy_from_slice(&[0, 0, 7]);
        let width_fixed = width << 16;
        let height_fixed = height << 16;
        tkhd[76..80].copy_from_slice(&width_fixed.to_be_bytes());
        tkhd[80..84].copy_from_slice(&height_fixed.to_be_bytes());

        let trak = box_with_payload(*b"trak", &box_with_payload(*b"tkhd", &tkhd));
        let moov_payload = [box_with_payload(*b"mvhd", &mvhd), trak].concat();
        [
            box_with_payload(*b"ftyp", b"isom"),
            box_with_payload(*b"moov", &moov_payload),
        ]
        .concat()
    }

    fn box_with_payload(name: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = 8 + payload.len() as u32;
        let mut data = Vec::new();
        data.extend_from_slice(&size.to_be_bytes());
        data.extend_from_slice(&name);
        data.extend_from_slice(payload);
        data
    }
}
