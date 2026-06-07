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
