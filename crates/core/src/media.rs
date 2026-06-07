use std::path::Path;

pub(crate) fn direct_video_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "mp4" => Some("mp4"),
        "mov" => Some("mov"),
        "m4v" => Some("m4v"),
        _ => None,
    }
}

pub(crate) fn direct_file_extension(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if extension.is_empty() {
        None
    } else {
        Some(extension)
    }
}

pub(crate) fn direct_voice_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "silk" => Some("silk"),
        "slk" => Some("slk"),
        "amr" => Some("amr"),
        "mp3" => Some("mp3"),
        "m4a" => Some("m4a"),
        "aac" => Some("aac"),
        "wav" => Some("wav"),
        "ogg" => Some("ogg"),
        "opus" => Some("opus"),
        _ => None,
    }
}

pub(crate) fn mime_type_for_extension(extension: &str) -> Option<&'static str> {
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "bmp" => Some("image/bmp"),
        "webp" => Some("image/webp"),
        "tif" | "tiff" => Some("image/tiff"),
        "heic" => Some("image/heic"),
        "heif" => Some("image/heif"),
        "mp4" => Some("video/mp4"),
        "mov" => Some("video/quicktime"),
        "m4v" => Some("video/x-m4v"),
        "silk" | "slk" => Some("audio/silk"),
        "amr" => Some("audio/amr"),
        "mp3" => Some("audio/mpeg"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "wav" => Some("audio/wav"),
        "ogg" => Some("audio/ogg"),
        "opus" => Some("audio/opus"),
        "pdf" => Some("application/pdf"),
        "txt" => Some("text/plain"),
        "csv" => Some("text/csv"),
        "json" => Some("application/json"),
        "zip" => Some("application/zip"),
        "doc" => Some("application/msword"),
        "docx" => Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
        "xls" => Some("application/vnd.ms-excel"),
        "xlsx" => Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        "ppt" => Some("application/vnd.ms-powerpoint"),
        "pptx" => Some("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
        _ => None,
    }
}
