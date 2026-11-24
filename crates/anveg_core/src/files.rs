//! Files for the Files tab and the editor (next phases, 4.1–4.2): listing
//! folders lazily with ignore rules, loading and saving text, quick open,
//! and `path:line[:col]` links.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use ignore::WalkBuilder;

use crate::atomic;

/// Above this, a file opens read-only as plain text (4.2).
pub const LARGE_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// Above this many lines, a file opens read-only as plain text (4.2).
pub const LARGE_FILE_LINES: usize = 200_000;

/// One entry of a listed folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// Matched by an ignore file (only listed with "show ignored").
    pub ignored: bool,
}

/// List one folder (not recursive): folders first, then files, by name.
/// Respects `.gitignore` and `.ignore` (and the global git excludes) and
/// hides `.git`. With `show_ignored`, ignored entries are listed too and
/// marked.
pub fn list_dir(dir: &Path, show_ignored: bool) -> Result<Vec<DirEntry>> {
    let visible = walk_level(dir, true)?;
    let mut entries: Vec<DirEntry> = if show_ignored {
        let all = walk_level(dir, false)?;
        all.into_iter()
            .map(|mut e| {
                e.ignored = !visible.iter().any(|v| v.path == e.path);
                e
            })
            .collect()
    } else {
        visible
    };
    entries.retain(|e| e.name != ".git");
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

fn walk_level(dir: &Path, respect_ignores: bool) -> Result<Vec<DirEntry>> {
    if !dir.is_dir() {
        bail!("{} is not a folder", dir.display());
    }
    let mut out = Vec::new();
    let walker = WalkBuilder::new(dir)
        .max_depth(Some(1))
        .hidden(false)
        .git_ignore(respect_ignores)
        .git_global(respect_ignores)
        .git_exclude(respect_ignores)
        .ignore(respect_ignores)
        .parents(respect_ignores)
        // Honour .gitignore even outside a git repository.
        .require_git(false)
        .build();
    for entry in walker.flatten() {
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path().to_path_buf();
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir())
            || (entry.path_is_symlink() && path.is_dir());
        out.push(DirEntry {
            path,
            name,
            is_dir,
            ignored: false,
        });
    }
    Ok(out)
}

/// Every file under `roots` that is not ignored, as (root index, relative
/// path), for quick open. Stops after `limit` files.
pub fn index_files(roots: &[PathBuf], limit: usize) -> Vec<(usize, PathBuf)> {
    let mut out = Vec::new();
    for (i, root) in roots.iter().enumerate() {
        let walker = WalkBuilder::new(root)
            .hidden(false)
            .require_git(false)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        for entry in walker.flatten() {
            if out.len() >= limit {
                return out;
            }
            if entry.file_type().is_some_and(|t| t.is_file())
                && let Ok(rel) = entry.path().strip_prefix(root)
            {
                out.push((i, rel.to_path_buf()));
            }
        }
    }
    out
}

/// Fuzzy-match `query` against `candidates`; best first, at most `limit`.
/// Returns indices into `candidates`.
pub fn fuzzy_rank(query: &str, candidates: &[String], limit: usize) -> Vec<usize> {
    use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
    use nucleo_matcher::{Config, Matcher, Utf32Str};
    let query = query.trim();
    if query.is_empty() {
        return (0..candidates.len().min(limit)).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let haystack = Utf32Str::new(c, &mut buf);
            pattern.score(haystack, &mut matcher).map(|s| (s, i))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| candidates[a.1].len().cmp(&candidates[b.1].len()))
    });
    scored.into_iter().take(limit).map(|(_, i)| i).collect()
}

/// A file's line endings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::CrLf => "CRLF",
        }
    }
}

/// How a file is indented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Indent {
    Tabs,
    Spaces(usize),
}

impl Default for Indent {
    fn default() -> Self {
        Self::Spaces(4)
    }
}

/// Why a file opens read-only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadOnlyReason {
    Binary,
    NotUtf8,
    Large,
}

/// A file loaded for the editor.
#[derive(Clone, Debug)]
pub struct LoadedText {
    /// The text with `\n` line endings.
    pub text: String,
    pub line_ending: LineEnding,
    pub trailing_newline: bool,
    pub indent: Indent,
    pub read_only: Option<ReadOnlyReason>,
    pub len: u64,
    pub modified: Option<std::time::SystemTime>,
}

/// Load `path` for editing. Binary and non-UTF-8 files come back read-only
/// with a lossy text; large files come back read-only too.
pub fn load_text(path: &Path) -> Result<LoadedText> {
    let meta = std::fs::metadata(path).with_context(|| format!("{}", path.display()))?;
    if meta.is_dir() {
        bail!("{} is a folder", path.display());
    }
    let len = meta.len();
    let modified = meta.modified().ok();
    let bytes = std::fs::read(path).with_context(|| format!("{}", path.display()))?;
    let binary = looks_binary(&bytes);
    let (raw, mut read_only) = match String::from_utf8(bytes) {
        Ok(s) if !binary => (s, None),
        Ok(s) => (s, Some(ReadOnlyReason::Binary)),
        Err(e) => {
            let reason = if binary {
                ReadOnlyReason::Binary
            } else {
                ReadOnlyReason::NotUtf8
            };
            (String::from_utf8_lossy(e.as_bytes()).into_owned(), Some(reason))
        }
    };
    let line_ending = detect_line_ending(&raw);
    let trailing_newline = raw.ends_with('\n');
    let text = if line_ending == LineEnding::CrLf {
        raw.replace("\r\n", "\n")
    } else {
        raw
    };
    if read_only.is_none() && (len > LARGE_FILE_BYTES || text.lines().count() > LARGE_FILE_LINES)
    {
        read_only = Some(ReadOnlyReason::Large);
    }
    let indent = detect_indent(&text);
    Ok(LoadedText {
        text,
        line_ending,
        trailing_newline,
        indent,
        read_only,
        len,
        modified,
    })
}

/// A NUL byte in the first 8 KB means binary (git's rule).
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// CRLF if most line breaks are CRLF.
pub fn detect_line_ending(text: &str) -> LineEnding {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count();
    if crlf > 0 && crlf * 2 >= lf {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    }
}

/// Tabs if most indented lines start with a tab; otherwise the most common
/// step between space indents (2, 4 or 8), default 4.
pub fn detect_indent(text: &str) -> Indent {
    let mut tabs = 0usize;
    let mut spaces = 0usize;
    let mut steps = [0usize; 9];
    let mut prev = 0usize;
    for line in text.lines().take(2000) {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            tabs += 1;
            continue;
        }
        let n = line.len() - line.trim_start_matches(' ').len();
        if n > 0 {
            spaces += 1;
        }
        let diff = n.abs_diff(prev);
        if (2..=8).contains(&diff) {
            steps[diff] += 1;
        }
        prev = n;
    }
    if tabs > spaces {
        return Indent::Tabs;
    }
    let best = [2usize, 4, 8]
        .into_iter()
        .max_by_key(|&s| (steps[s], s == 4))
        .unwrap_or(4);
    if steps[best] == 0 {
        Indent::Spaces(4)
    } else {
        Indent::Spaces(best)
    }
}

/// The bytes to write for `text` (with `\n` endings), restoring the file's
/// line endings and its trailing newline (or lack of one).
pub fn encode_text(text: &str, line_ending: LineEnding, trailing_newline: bool) -> Vec<u8> {
    let mut body = text.to_owned();
    if trailing_newline && !body.ends_with('\n') && !body.is_empty() {
        body.push('\n');
    }
    if line_ending == LineEnding::CrLf {
        body = body.replace('\n', "\r\n");
    }
    body.into_bytes()
}

/// Save `text` atomically.
pub fn save_text(
    path: &Path,
    text: &str,
    line_ending: LineEnding,
    trailing_newline: bool,
) -> Result<()> {
    atomic::write_bytes(path, &encode_text(text, line_ending, trailing_newline))
}

/// A `path:line[:col]` reference found in terminal output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathLink {
    pub path: String,
    /// One-based.
    pub line: Option<u32>,
    /// One-based.
    pub column: Option<u32>,
}

fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '+' | '@' | '\\')
}

/// The `path[:line[:col]]` around character index `at` in `line_text`, if
/// the text there looks like a path (it has a `/` or a file extension).
pub fn path_link_at(line_text: &str, at: usize) -> Option<PathLink> {
    let chars: Vec<char> = line_text.chars().collect();
    if at >= chars.len() {
        return None;
    }
    let ok = |c: char| is_path_char(c) || c == ':' || c.is_ascii_digit();
    if !ok(chars[at]) {
        return None;
    }
    let mut start = at;
    while start > 0 && ok(chars[start - 1]) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < chars.len() && ok(chars[end]) {
        end += 1;
    }
    let token: String = chars[start..end].iter().collect();
    parse_path_link(&token)
}

/// Parse `path[:line[:col]]`, trimming punctuation around it.
pub fn parse_path_link(token: &str) -> Option<PathLink> {
    let token = token
        .trim_start_matches([':', '(', '\'', '"'])
        .trim_end_matches([':', '.', ',', ')', '\'', '"']);
    let mut parts = token.split(':');
    let path = parts.next()?.trim_end_matches('.');
    let line = parts.next().and_then(|s| s.parse::<u32>().ok());
    let column = line.and(parts.next().and_then(|s| s.parse::<u32>().ok()));
    if path.is_empty() || path.contains("://") {
        return None;
    }
    let looks_like_path = path.contains('/')
        || Path::new(path)
            .extension()
            .is_some_and(|e| !e.is_empty() && e.len() <= 10);
    if !looks_like_path {
        return None;
    }
    Some(PathLink {
        path: path.to_owned(),
        line: line.filter(|&l| l > 0),
        column: column.filter(|&c| c > 0),
    })
}

/// Resolve a link path against the roots (and `~`). Returns an existing file.
pub fn resolve_link(link: &PathLink, roots: &[PathBuf], home: Option<&Path>) -> Option<PathBuf> {
    let raw = &link.path;
    let candidate = if let Some(rest) = raw.strip_prefix("~/") {
        home.map(|h| h.join(rest))
    } else {
        None
    };
    let path = Path::new(raw);
    let mut tries: Vec<PathBuf> = candidate.into_iter().collect();
    if path.is_absolute() {
        tries.push(path.to_path_buf());
    } else {
        tries.extend(roots.iter().map(|r| r.join(path)));
    }
    tries.into_iter().find(|p| p.is_file())
}

/// `path` relative to the first root that contains it.
pub fn relative_to_roots(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    roots
        .iter()
        .find_map(|r| path.strip_prefix(r).ok().map(Path::to_path_buf))
}

/// Check a new file or folder name: one path component, no separators.
pub fn validate_name(name: &str) -> std::result::Result<&str, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a name.");
    }
    if name.contains('/') || name.contains('\0') {
        return Err("A name can't contain “/”.");
    }
    let mut comps = Path::new(name).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(_)), None) => Ok(name),
        _ => Err("That name isn't allowed."),
    }
}

/// Create an empty file (fails if it exists).
pub fn create_file(path: &Path) -> Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    Ok(())
}

/// Rename within the same folder (fails if the target exists).
pub fn rename(from: &Path, new_name: &str) -> Result<PathBuf> {
    let to = from
        .parent()
        .map(|p| p.join(new_name))
        .context("no parent folder")?;
    if to.exists() {
        bail!("{} already exists", to.display());
    }
    std::fs::rename(from, &to).with_context(|| format!("could not rename {}", from.display()))?;
    Ok(to)
}

/// A unified diff (git style) from `old` to `new` for `path`, empty when
/// they are equal. Used for "Compare" (disk against the unsaved buffer).
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    if old == new {
        return String::new();
    }
    let diff = similar::TextDiff::from_lines(old, new);
    let body = diff
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string();
    format!("diff --git a/{path} b/{path}\n{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_diffs() {
        assert_eq!(unified_diff("a\n", "a\n", "x"), "");
        let d = unified_diff("a\nb\n", "a\nc\n", "x.rs");
        assert!(d.starts_with("diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n@@"));
        assert!(d.contains("-b\n+c\n"));
    }

    #[test]
    fn lists_with_ignore_rules() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let d = tmp.path();
        std::fs::create_dir(d.join(".git"))?;
        std::fs::create_dir(d.join("src"))?;
        std::fs::create_dir(d.join("target"))?;
        std::fs::write(d.join(".gitignore"), "target/\n*.log\n")?;
        std::fs::write(d.join("b.rs"), "")?;
        std::fs::write(d.join("A.md"), "")?;
        std::fs::write(d.join("x.log"), "")?;
        std::fs::write(d.join(".ignore"), "secret.txt\n")?;
        std::fs::write(d.join("secret.txt"), "")?;
        let names: Vec<String> = list_dir(d, false)?.into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["src", ".gitignore", ".ignore", "A.md", "b.rs"]);
        let all = list_dir(d, true)?;
        let ignored: Vec<&str> = all
            .iter()
            .filter(|e| e.ignored)
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(ignored, ["target", "secret.txt", "x.log"]);
        assert!(!all.iter().any(|e| e.name == ".git"));
        Ok(())
    }

    #[test]
    fn index_and_fuzzy() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let d = tmp.path();
        std::fs::create_dir_all(d.join("src/ui"))?;
        std::fs::write(d.join(".gitignore"), "out/\n")?;
        std::fs::create_dir(d.join("out"))?;
        std::fs::write(d.join("out/gen.rs"), "")?;
        std::fs::write(d.join("src/main.rs"), "")?;
        std::fs::write(d.join("src/ui/sidebar.rs"), "")?;
        let files = index_files(&[d.to_path_buf()], 100);
        let names: Vec<String> = files
            .iter()
            .map(|(_, p)| p.to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"src/ui/sidebar.rs".to_owned()));
        assert!(!names.iter().any(|n| n.starts_with("out/")));
        let ranked = fuzzy_rank("sidbr", &names, 5);
        assert_eq!(names[ranked[0]], "src/ui/sidebar.rs");
        assert_eq!(fuzzy_rank("", &names, 2).len(), 2);
        Ok(())
    }

    #[test]
    fn text_round_trip_keeps_endings() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let p = tmp.path().join("f.txt");
        std::fs::write(&p, "a\r\n\tb\r\nc")?;
        let t = load_text(&p)?;
        assert_eq!(t.text, "a\n\tb\nc");
        assert_eq!(t.line_ending, LineEnding::CrLf);
        assert!(!t.trailing_newline);
        assert_eq!(t.indent, Indent::Tabs);
        assert!(t.read_only.is_none());
        save_text(&p, "a\n\tb\nc\nd", t.line_ending, t.trailing_newline)?;
        assert_eq!(std::fs::read(&p)?, b"a\r\n\tb\r\nc\r\nd");

        std::fs::write(&p, "x\n")?;
        let t = load_text(&p)?;
        save_text(&p, "x\ny", t.line_ending, t.trailing_newline)?;
        assert_eq!(std::fs::read(&p)?, b"x\ny\n");
        Ok(())
    }

    #[test]
    fn binary_and_non_utf8_are_read_only() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let bin = tmp.path().join("b");
        std::fs::write(&bin, [0u8, 1, 2])?;
        assert_eq!(load_text(&bin)?.read_only, Some(ReadOnlyReason::Binary));
        let latin = tmp.path().join("l");
        std::fs::write(&latin, [b'c', b'a', b'f', 0xe9])?;
        assert_eq!(load_text(&latin)?.read_only, Some(ReadOnlyReason::NotUtf8));
        Ok(())
    }

    #[test]
    fn indent_detection() {
        assert_eq!(detect_indent("a\n  b\n    c\n  d\n"), Indent::Spaces(2));
        assert_eq!(detect_indent("a\n    b\n        c\n"), Indent::Spaces(4));
        assert_eq!(detect_indent("x\ny\n"), Indent::Spaces(4));
    }

    #[test]
    fn path_links() {
        let l = path_link_at("error at src/main.rs:12:5: oops", 12);
        assert_eq!(
            l,
            Some(PathLink {
                path: "src/main.rs".into(),
                line: Some(12),
                column: Some(5)
            })
        );
        let l = path_link_at("see README.md.", 6);
        assert_eq!(l.map(|l| (l.path, l.line)), Some(("README.md".into(), None)));
        assert_eq!(path_link_at("plain words here", 3), None);
        assert_eq!(parse_path_link("https://x.com/a.rs"), None);
        assert_eq!(
            parse_path_link("(./a/b.ts:7)").map(|l| (l.path, l.line)),
            Some(("./a/b.ts".into(), Some(7)))
        );
    }

    #[test]
    fn resolving_links() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        std::fs::create_dir(tmp.path().join("src"))?;
        std::fs::write(tmp.path().join("src/a.rs"), "")?;
        let link = parse_path_link("src/a.rs:3").ok_or_else(|| anyhow::anyhow!("no link"))?;
        let roots = [tmp.path().join("other"), tmp.path().to_path_buf()];
        assert_eq!(
            resolve_link(&link, &roots, None),
            Some(tmp.path().join("src/a.rs"))
        );
        Ok(())
    }

    #[test]
    fn names() {
        assert!(validate_name("a.rs").is_ok());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name(" ").is_err());
    }
}
