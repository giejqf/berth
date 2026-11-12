//! File helpers for attachments and artifacts.

/// Largest file a user can attach to a prompt.
pub const MAX_ATTACHMENT_BYTES: u64 = 50 * 1024 * 1024;

/// MIME type from a file name's extension (`application/octet-stream` if unknown).
#[must_use]
pub fn mime_for(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "mov" => "video/quicktime",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "pdf" => "application/pdf",
        "html" | "htm" => "text/html",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// Images the app renders inline and Claude accepts as image blocks.
#[must_use]
pub fn is_inline_image(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

#[must_use]
pub fn is_video(mime: &str) -> bool {
    mime.starts_with("video/")
}

/// Where a file opens when clicked, for a file the app doesn't draw itself
/// (it shows images in the review pane).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opener {
    /// Quick Look: read-only for every type, so nothing opens in an editor
    /// (§1.1).
    QuickLook,
    /// The Mac's own app for it: a video plays in the user's player
    /// (QuickTime Player, or whichever they chose), with its controls.
    DefaultApp,
}

/// A video plays in the user's player; anything else opens in Quick Look.
#[must_use]
pub fn opener(mime: &str) -> Opener {
    if is_video(mime) {
        Opener::DefaultApp
    } else {
        Opener::QuickLook
    }
}

/// Files worth a Quick Look thumbnail: a recording's first frame, a PDF's or
/// page's first page, images the app cannot draw itself. Plain text is not
/// (its thumbnail is unreadable at tile size); it gets an icon.
#[must_use]
pub fn wants_thumbnail(mime: &str) -> bool {
    is_video(mime)
        || matches!(mime, "application/pdf" | "text/html")
        || (mime.starts_with("image/") && !is_inline_image(mime))
}

/// A file name safe to use on disk and in a shell argument: the last path
/// component, with anything but `[A-Za-z0-9._-]` replaced by `_`.
#[must_use]
pub fn safe_file_name(name: &str) -> String {
    let base = name.rsplit('/').next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_start_matches('.');
    if trimmed.is_empty() {
        "file".to_owned()
    } else {
        trimmed.chars().take(120).collect()
    }
}

/// "12 KB", "3.4 MB".
#[must_use]
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / KB / KB)
    } else {
        format!("{:.1} GB", b / KB / KB / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn videos_open_in_the_users_player_the_rest_in_quick_look() {
        for name in ["demo.mp4", "Demo.M4V", "screen.mov", "clip.webm"] {
            assert_eq!(opener(mime_for(name)), Opener::DefaultApp, "{name}");
        }
        for name in [
            "report.pdf",
            "page.html",
            "notes.md",
            "photo.heic",
            "data.bin",
        ] {
            assert_eq!(opener(mime_for(name)), Opener::QuickLook, "{name}");
        }
    }

    #[test]
    fn mime_types_by_extension() {
        assert_eq!(mime_for("Shot.PNG"), "image/png");
        assert_eq!(mime_for("rec.mov"), "video/quicktime");
        assert_eq!(mime_for("report.html"), "text/html");
        assert_eq!(mime_for("noext"), "application/octet-stream");
        assert!(is_inline_image("image/webp"));
        assert!(!is_inline_image("image/heic"));
        assert!(is_video("video/mp4"));
        assert!(wants_thumbnail("video/quicktime"));
        assert!(wants_thumbnail("image/heic"));
        assert!(wants_thumbnail("text/html"));
        assert!(!wants_thumbnail("image/png"));
        assert!(!wants_thumbnail("text/plain"));
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(
            safe_file_name("/Users/me/My Shot (1).png"),
            "My_Shot__1_.png"
        );
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name(".hidden"), "hidden");
        assert_eq!(safe_file_name("$(rm -rf).txt"), "__rm_-rf_.txt");
        assert_eq!(safe_file_name(""), "file");
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(12_288), "12 KB");
        assert_eq!(human_size(3_565_158), "3.4 MB");
    }
}
