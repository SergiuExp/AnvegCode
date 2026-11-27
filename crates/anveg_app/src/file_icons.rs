//! Icons for the Files tree: a filled folder for folders (closed or open),
//! so they stand apart from files at a glance, and an outline icon by file
//! type for files (code, text, config, image…).
//!
//! The two folder glyphs are drawn here (simple shapes, filled); the file
//! icons are Lucide's (`app_assets` bundles them).

use gpui_kit::assets::IconName;
use gpui_kit::{Hsla, hsla};

/// The folder icons' colour: a soft blue, a little darker on light themes.
pub fn folder_color(dark: bool) -> Hsla {
    if dark {
        hsla(0.58, 0.45, 0.66, 1.0)
    } else {
        hsla(0.58, 0.55, 0.52, 1.0)
    }
}

/// A closed folder, filled.
pub const FOLDER: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#000" d="M2.5 6.5A2.5 2.5 0 0 1 5 4h3.7a2 2 0 0 1 1.45.62L12 6.5h7a2.5 2.5 0 0 1 2.5 2.5v8.5A2.5 2.5 0 0 1 19 20H5a2.5 2.5 0 0 1-2.5-2.5z"/></svg>"##;

/// An open folder: the back in a lighter shade, the front tipped forward.
pub const FOLDER_OPEN: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#000" fill-opacity="0.55" d="M2.5 6.5A2.5 2.5 0 0 1 5 4h3.7a2 2 0 0 1 1.45.62L12 6.5h6a2.5 2.5 0 0 1 2.5 2.5v1H8.3a2.6 2.6 0 0 0-2.4 1.6L2.5 18.5z"/><path fill="#000" d="M6.9 12.3A1.6 1.6 0 0 1 8.4 11.3h13.1a1 1 0 0 1 .93 1.38l-2.5 6.1A2 2 0 0 1 18.1 20H3.6a.8.8 0 0 1-.74-1.1z"/></svg>"##;

/// The icon for a file, by its name.
pub fn file_icon(name: &str) -> IconName {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "bun.lockb" | "gemfile.lock"
        | "poetry.lock" | "composer.lock" | "flake.lock" => return IconName::FileLock,
        "dockerfile" | "makefile" | "justfile" | "procfile" | "gemfile" | "rakefile" => return IconName::FileCog,
        "license" | "licence" | "copying" | "notice" | "authors" | "changelog" | "readme" => {
            return IconName::FileText;
        }
        _ => {}
    }
    let Some((stem, ext)) = lower.rsplit_once('.') else {
        return IconName::File;
    };
    // `.gitignore`, `.env`: settings files.
    if stem.is_empty() {
        return IconName::FileCog;
    }
    match ext {
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go" | "swift" | "c" | "h" | "cc" | "cpp"
        | "cxx" | "hpp" | "m" | "mm" | "java" | "kt" | "kts" | "rb" | "php" | "cs" | "html" | "htm" | "css"
        | "scss" | "sass" | "less" | "vue" | "svelte" | "lua" | "dart" | "zig" | "ex" | "exs" | "hs" | "scala"
        | "sql" | "graphql" | "gql" | "proto" | "wasm" | "elm" | "clj" | "erl" | "ml" | "nim" | "r" | "jl" => {
            IconName::FileCode
        }
        "json" | "jsonc" | "json5" | "jsonl" | "ndjson" => IconName::FileBraces,
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "config" | "env" | "plist" | "xml" | "editorconfig"
        | "properties" => IconName::FileCog,
        "lock" => IconName::FileLock,
        "md" | "markdown" | "mdx" | "txt" | "rst" | "adoc" | "org" | "rtf" | "pdf" | "doc" | "docx" | "tex" => {
            IconName::FileText
        }
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "icns" | "bmp" | "tif" | "tiff" | "heic"
        | "avif" => IconName::FileImage,
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" | "command" => IconName::FileTerminal,
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "dmg" | "pkg" | "jar" => {
            IconName::FileArchive
        }
        "csv" | "tsv" | "xls" | "xlsx" | "numbers" => IconName::FileSpreadsheet,
        "diff" | "patch" => IconName::FileDiff,
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" | "aiff" => IconName::FileMusic,
        "mp4" | "mov" | "mkv" | "webm" | "avi" | "m4v" => IconName::FileVideoCamera,
        "ttf" | "otf" | "woff" | "woff2" => IconName::FileType,
        "log" => IconName::FileText,
        _ => IconName::File,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_by_file_type() {
        assert_eq!(file_icon("main.rs"), IconName::FileCode);
        assert_eq!(file_icon("App.TSX"), IconName::FileCode);
        assert_eq!(file_icon("package.json"), IconName::FileBraces);
        assert_eq!(file_icon("package-lock.json"), IconName::FileLock);
        assert_eq!(file_icon("Cargo.lock"), IconName::FileLock);
        assert_eq!(file_icon("Cargo.toml"), IconName::FileCog);
        assert_eq!(file_icon(".gitignore"), IconName::FileCog);
        assert_eq!(file_icon("Dockerfile"), IconName::FileCog);
        assert_eq!(file_icon("README.md"), IconName::FileText);
        assert_eq!(file_icon("LICENSE"), IconName::FileText);
        assert_eq!(file_icon("logo.png"), IconName::FileImage);
        assert_eq!(file_icon("build.sh"), IconName::FileTerminal);
        assert_eq!(file_icon("notes"), IconName::File);
        assert_eq!(file_icon("data.bin"), IconName::File);
    }
}
