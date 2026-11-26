//! Image files the app shows as pictures instead of text: which extensions,
//! their pixel size (for the viewer's info bar), and byte sizes in words.

use std::path::Path;

/// Larger files are not opened as images (decoding would take seconds and
/// a lot of memory); the viewer says so instead.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
    Svg,
    Webp,
    Bmp,
    Ico,
    Tiff,
    /// Netpbm: .pbm, .pgm, .ppm, .pnm.
    Pnm,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
            Self::Svg => "SVG",
            Self::Webp => "WebP",
            Self::Bmp => "BMP",
            Self::Ico => "ICO",
            Self::Tiff => "TIFF",
            Self::Pnm => "Netpbm",
        }
    }
}

/// The image kind of a file, by its extension (any letter case).
pub fn kind_for(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => Kind::Png,
        "jpg" | "jpeg" | "jfif" => Kind::Jpeg,
        "gif" => Kind::Gif,
        "svg" => Kind::Svg,
        "webp" => Kind::Webp,
        "bmp" => Kind::Bmp,
        "ico" => Kind::Ico,
        "tif" | "tiff" => Kind::Tiff,
        "pbm" | "pgm" | "ppm" | "pnm" => Kind::Pnm,
        _ => return None,
    })
}

/// Width and height in pixels, when the header says (for SVG: the
/// `width`/`height` attributes, else the `viewBox`).
pub fn dimensions(kind: Kind, bytes: &[u8]) -> Option<(u32, u32)> {
    if kind == Kind::Svg {
        return svg_dimensions(std::str::from_utf8(bytes).ok()?);
    }
    let size = imagesize::blob_size(bytes).ok()?;
    Some((u32::try_from(size.width).ok()?, u32::try_from(size.height).ok()?))
}

fn svg_dimensions(text: &str) -> Option<(u32, u32)> {
    let start = text.find("<svg")?;
    let rest = &text[start..];
    let tag = &rest[..rest.find('>')?];
    let attr = |name: &str| -> Option<&str> {
        // ` name="value"` or ` name='value'`, not part of a longer name.
        let mut from = 0;
        while let Some(i) = tag[from..].find(name) {
            let at = from + i;
            from = at + name.len();
            let before = tag[..at].chars().next_back();
            if !before.is_some_and(char::is_whitespace) {
                continue;
            }
            let after = tag[from..].trim_start().strip_prefix('=')?.trim_start();
            let quote = after.chars().next()?;
            if quote != '"' && quote != '\'' {
                return None;
            }
            let value = &after[1..];
            return Some(&value[..value.find(quote)?]);
        }
        None
    };
    // "120", "120px", "120.5": percentages and other units are skipped.
    let length = |v: &str| -> Option<u32> {
        let v = v.trim().trim_end_matches("px");
        let n: f64 = v.parse().ok()?;
        (n > 0.0 && n < 1.0e6).then(|| n.round() as u32)
    };
    if let (Some(w), Some(h)) = (attr("width").and_then(length), attr("height").and_then(length)) {
        return Some((w, h));
    }
    let view_box: Vec<f64> = attr("viewBox")?
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    match view_box[..] {
        [_, _, w, h] if w > 0.0 && h > 0.0 && w < 1.0e6 && h < 1.0e6 => {
            Some((w.round() as u32, h.round() as u32))
        }
        _ => None,
    }
}

/// "812 bytes", "14.2 KB", "3.1 MB".
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} bytes")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / KB / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_for(Path::new("a/logo.PNG")), Some(Kind::Png));
        assert_eq!(kind_for(Path::new("photo.jpeg")), Some(Kind::Jpeg));
        assert_eq!(kind_for(Path::new("icon.svg")), Some(Kind::Svg));
        assert_eq!(kind_for(Path::new("favicon.ico")), Some(Kind::Ico));
        assert_eq!(kind_for(Path::new("main.rs")), None);
        assert_eq!(kind_for(Path::new("Makefile")), None);
    }

    #[test]
    fn svg_sizes() {
        let d = |s: &str| dimensions(Kind::Svg, s.as_bytes());
        assert_eq!(d(r#"<svg xmlns="x" width="24" height="16px">"#), Some((24, 16)));
        assert_eq!(d(r#"<?xml?><svg viewBox="0 0 100 50" stroke-width="2">"#), Some((100, 50)));
        // stroke-width is not width; percentages fall back to the viewBox.
        assert_eq!(
            d(r#"<svg stroke-width='3' width='100%' height='100%' viewBox='0,0,32,32'>"#),
            Some((32, 32))
        );
        assert_eq!(d("<svg>"), None);
        assert_eq!(d("not svg"), None);
    }

    #[test]
    fn raster_sizes_and_words() {
        // A 1×1 PNG header (IHDR with width 3, height 2).
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&3u32.to_be_bytes());
        png.extend_from_slice(&2u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(dimensions(Kind::Png, &png), Some((3, 2)));
        assert_eq!(dimensions(Kind::Png, b"junk"), None);
        assert_eq!(human_size(812), "812 bytes");
        assert_eq!(human_size(14_540), "14.2 KB");
        assert_eq!(human_size(3_250_586), "3.1 MB");
    }
}
