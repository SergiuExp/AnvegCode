//! Claude Code's session transcripts: where they live and what the app
//! reads from them (build plan, sections 5.2 and 5.3).
//!
//! The JSONL format is internal to Claude Code and changes between
//! versions, so the parser is defensive: it reads each line into a
//! `serde_json::Value`, skips anything it does not understand, and never
//! panics.
//!
//! Observed with Claude Code 2.1.330 (fixtures in `tests/fixtures/transcripts`):
//!
//! - a custom name, from `-n`/`--name` or `/rename`, is a
//!   `{"type":"custom-title","customTitle":…}` entry; it is written again
//!   from time to time, and the last one wins;
//! - user prompts are `{"type":"user", "message":{"content":…}}` entries;
//!   meta entries carry `"isMeta":true`, subagent entries
//!   `"isSidechain":true`;
//! - sessions from `claude -p` or the Agent SDK carry
//!   `"entrypoint":"sdk-cli"` / `"promptSource":"sdk"` on their user entries;
//!   interactive ones `"entrypoint":"cli"`;
//! - a sibling directory `<session-id>/` may hold extra files (for example
//!   `custom-title.json`).

use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::time::{parse_rfc3339_millis, system_time_millis};

/// The longest encoded directory name Claude Code uses as is.
pub const MAX_ENCODED_LEN: usize = 200;

/// How long a first prompt may be as a fallback title.
pub const FIRST_PROMPT_TITLE_CHARS: usize = 80;

/// Claude Code's directory name for a working directory: every character
/// that is not an ASCII letter or digit becomes `-`. A character outside
/// the Basic Multilingual Plane counts as two (Claude Code works on UTF-16
/// code units).
pub fn encode_cwd(cwd: &Path) -> String {
    let s = cwd.to_string_lossy();
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            for _ in 0..ch.len_utf16() {
                out.push('-');
            }
        }
    }
    out
}

/// `<config>/projects`.
pub fn projects_root(config_dir: &Path) -> PathBuf {
    config_dir.join("projects")
}

/// The transcript directory for `cwd`, if it can be determined.
///
/// For names up to 200 characters this is simply `projects/<encoded>`
/// (which may not exist yet). Longer names are truncated by Claude Code and
/// get a hash appended; rather than reimplementing the hash, look for
/// directories that start with the 200-character prefix and confirm one by
/// a `cwd` recorded in a transcript inside it.
pub fn transcript_dir(config_dir: &Path, cwd: &Path) -> Option<PathBuf> {
    let root = projects_root(config_dir);
    let encoded = encode_cwd(cwd);
    if encoded.len() <= MAX_ENCODED_LEN {
        return Some(root.join(encoded));
    }
    let prefix = &encoded[..MAX_ENCODED_LEN];
    let entries = std::fs::read_dir(&root).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    candidates.sort();
    let wanted = cwd.to_string_lossy();
    candidates
        .into_iter()
        .find(|dir| recorded_cwd(dir).as_deref() == Some(wanted.as_ref()))
}

/// The first `cwd` value found in any transcript in `dir`.
fn recorded_cwd(dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jsonl")
            && let Some(cwd) = first_cwd_in(&path)
        {
            return Some(cwd);
        }
    }
    None
}

fn first_cwd_in(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0; 256 * 1024];
    let n = file.read(&mut buf).ok()?;
    buf.truncate(n);
    for line in buf.split(|b| *b == b'\n') {
        if let Ok(v) = serde_json::from_slice::<Value>(line)
            && let Some(cwd) = v.get("cwd").and_then(Value::as_str)
        {
            return Some(cwd.to_owned());
        }
    }
    None
}

/// The session id for a transcript file name (`<id>.jsonl`).
pub fn session_id_of(path: &Path) -> Option<String> {
    if path.extension()? != "jsonl" {
        return None;
    }
    Some(path.file_stem()?.to_string_lossy().into_owned())
}

/// What the app knows about one session from its transcript.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TranscriptMeta {
    /// Set by `-n`/`--name` or `/rename`.
    pub custom_title: Option<String>,
    /// A title or summary Claude Code generated, if any.
    pub generated_title: Option<String>,
    /// The first real user prompt, whitespace collapsed.
    pub first_prompt: Option<String>,
    /// Oldest timestamp seen (Unix ms).
    pub first_activity: Option<i64>,
    /// Newest timestamp seen (Unix ms).
    pub last_activity: Option<i64>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    /// `Some(true)` for `claude -p` / Agent SDK sessions, `Some(false)` for
    /// interactive ones, `None` until a user entry says which.
    pub non_interactive: Option<bool>,
    /// Whether any real user prompt was seen.
    pub has_prompt: bool,
}

impl TranscriptMeta {
    /// Apply one parsed JSONL entry.
    pub fn apply(&mut self, entry: &Value) {
        let Some(kind) = entry.get("type").and_then(Value::as_str) else {
            return;
        };
        if let Some(ts) = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_millis)
        {
            self.first_activity = Some(self.first_activity.map_or(ts, |t| t.min(ts)));
            self.last_activity = Some(self.last_activity.map_or(ts, |t| t.max(ts)));
        }
        if self.cwd.is_none()
            && let Some(cwd) = entry.get("cwd").and_then(Value::as_str)
        {
            self.cwd = Some(cwd.to_owned());
        }
        if let Some(branch) = entry
            .get("gitBranch")
            .and_then(Value::as_str)
            .filter(|b| !b.is_empty() && *b != "HEAD")
        {
            self.git_branch = Some(branch.to_owned());
        }
        match kind {
            "custom-title" => {
                if let Some(title) = non_empty_str(entry, "customTitle") {
                    self.custom_title = Some(title);
                }
            }
            "summary" => {
                if let Some(title) = non_empty_str(entry, "summary") {
                    self.generated_title = Some(title);
                }
            }
            "ai-title" => {
                if let Some(title) =
                    non_empty_str(entry, "aiTitle").or_else(|| non_empty_str(entry, "title"))
                {
                    self.generated_title = Some(title);
                }
            }
            "user" => self.apply_user(entry),
            _ => {}
        }
    }

    fn apply_user(&mut self, entry: &Value) {
        let is_true = |key: &str| entry.get(key).and_then(Value::as_bool).unwrap_or(false);
        if is_true("isSidechain") {
            return;
        }
        if self.non_interactive.is_none() {
            let entrypoint = entry.get("entrypoint").and_then(Value::as_str);
            let source = entry.get("promptSource").and_then(Value::as_str);
            if entrypoint.is_some() || source.is_some() {
                let sdk = entrypoint.is_some_and(|e| e.starts_with("sdk")) || source == Some("sdk");
                self.non_interactive = Some(sdk);
            }
        }
        if is_true("isMeta") || self.has_prompt {
            return;
        }
        if let Some(text) = entry.get("message").and_then(prompt_text) {
            self.first_prompt = Some(text);
            self.has_prompt = true;
        }
    }

    /// The best title from the transcript alone: custom name, then
    /// generated title, then the first prompt (truncated).
    pub fn title(&self) -> Option<String> {
        self.custom_title
            .clone()
            .or_else(|| self.generated_title.clone())
            .or_else(|| self.first_prompt.as_deref().map(truncate_title))
    }
}

fn non_empty_str(entry: &Value, key: &str) -> Option<String> {
    entry
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The text of a user message, or `None` for tool results, slash-command
/// wrappers and other non-prompts.
fn prompt_text(message: &Value) -> Option<String> {
    let content = message.get("content")?;
    let raw = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(Value::as_str) {
                            parts.push(t.to_owned());
                        }
                    }
                    // A tool result is not a prompt.
                    Some("tool_result") => return None,
                    _ => {}
                }
            }
            parts.join(" ")
        }
        _ => return None,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() || is_wrapper(trimmed) {
        return None;
    }
    let collapsed: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(collapsed)
}

/// Text Claude Code writes into user entries that the user did not type.
fn is_wrapper(text: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "<command-name>",
        "<command-message>",
        "<command-args>",
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<local-command-caveat>",
        "<system-reminder>",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
        "<user-memory-input>",
        "Caveat: The messages below",
        "[Request interrupted",
    ];
    PREFIXES.iter().any(|p| text.starts_with(p))
}

/// Shorten a prompt for use as a title.
pub fn truncate_title(text: &str) -> String {
    if text.chars().count() <= FIRST_PROMPT_TITLE_CHARS {
        return text.to_owned();
    }
    let cut: String = text.chars().take(FIRST_PROMPT_TITLE_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

/// Per-file parse state, kept in the sessions cache so unchanged files are
/// never re-read and changed ones are read only from where parsing stopped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CachedTranscript {
    pub size: u64,
    /// Modification time, Unix ms.
    pub mtime: i64,
    /// Bytes parsed so far: always just after a newline, so a half-written
    /// last line is read again next time.
    pub parsed_offset: u64,
    pub meta: TranscriptMeta,
}

/// Bring `cache` up to date with the file at `path`. Returns whether the
/// metadata may have changed.
pub fn update_cached(path: &Path, cache: &mut CachedTranscript) -> std::io::Result<bool> {
    let metadata = std::fs::metadata(path)?;
    let size = metadata.len();
    let mtime = metadata.modified().map(system_time_millis).unwrap_or(0);
    if size == cache.size && mtime == cache.mtime && cache.parsed_offset <= size {
        return Ok(false);
    }
    if size < cache.parsed_offset {
        // Rewritten (shorter): start over.
        *cache = CachedTranscript::default();
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(cache.parsed_offset))?;
    let mut buf = Vec::with_capacity((size - cache.parsed_offset) as usize);
    file.read_to_end(&mut buf)?;
    let consumed = parse_complete_lines(&buf, &mut cache.meta);
    cache.parsed_offset += consumed as u64;
    cache.size = size;
    cache.mtime = mtime;
    if cache.meta.last_activity.is_none() {
        // No timestamps at all: fall back to the file's modification time.
        cache.meta.last_activity = Some(mtime);
    }
    Ok(true)
}

/// Apply every complete line in `buf`. Returns the number of bytes up to
/// and including the last newline.
pub fn parse_complete_lines(buf: &[u8], meta: &mut TranscriptMeta) -> usize {
    let Some(last_newline) = buf.iter().rposition(|b| *b == b'\n') else {
        return 0;
    };
    for line in buf[..last_newline].split(|b| *b == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if let Ok(entry) = serde_json::from_slice::<Value>(line) {
            meta.apply(&entry);
        }
    }
    last_newline + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/transcripts")
            .join(name)
    }

    fn parse_fixture(name: &str) -> TranscriptMeta {
        let mut cache = CachedTranscript::default();
        let changed = update_cached(&fixture(name), &mut cache);
        assert!(matches!(changed, Ok(true)), "{name}: {changed:?}");
        cache.meta
    }

    #[test]
    fn encodes_like_claude_code() {
        assert_eq!(
            encode_cwd(Path::new("/home/user/AnvegCode")),
            "-home-user-AnvegCode"
        );
        assert_eq!(
            encode_cwd(Path::new("/tmp/claude-0/fx/proj")),
            "-tmp-claude-0-fx-proj"
        );
        assert_eq!(
            encode_cwd(Path::new("/tmp/claude-0/fx/proj/.claude/worktrees/wt1")),
            "-tmp-claude-0-fx-proj--claude-worktrees-wt1"
        );
        assert_eq!(
            encode_cwd(Path::new("/Users/a/my app_v2")),
            "-Users-a-my-app-v2"
        );
        assert_eq!(encode_cwd(Path::new("/x/é")), "-x--");
        assert_eq!(encode_cwd(Path::new("/x/🎉")), "-x---");
    }

    #[test]
    fn short_paths_map_directly() {
        let dir = transcript_dir(Path::new("/cfg"), Path::new("/Users/a/p"));
        assert_eq!(dir, Some(PathBuf::from("/cfg/projects/-Users-a-p")));
    }

    #[test]
    fn long_paths_are_found_by_prefix_and_cwd() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let cfg = tmp.path();
        let long = format!("/Users/a/{}", "very-long-folder-name/".repeat(12));
        let long = Path::new(long.trim_end_matches('/'));
        let encoded = encode_cwd(long);
        assert!(encoded.len() > MAX_ENCODED_LEN);
        let prefix = &encoded[..MAX_ENCODED_LEN];
        // Two candidates with the same prefix; only one records our cwd.
        let other = projects_root(cfg).join(format!("{prefix}-aaaa"));
        let ours = projects_root(cfg).join(format!("{prefix}-bbbb"));
        std::fs::create_dir_all(&other)?;
        std::fs::create_dir_all(&ours)?;
        std::fs::write(
            other.join("x.jsonl"),
            "{\"type\":\"user\",\"cwd\":\"/somewhere/else\"}\n",
        )?;
        std::fs::write(
            ours.join("y.jsonl"),
            format!(
                "{{\"type\":\"mode\"}}\n{{\"type\":\"user\",\"cwd\":{:?}}}\n",
                long.to_string_lossy()
            ),
        )?;
        assert_eq!(transcript_dir(cfg, long), Some(ours));
        assert_eq!(
            transcript_dir(cfg, Path::new(&format!("{}x", long.display()))),
            None
        );
        Ok(())
    }

    #[test]
    fn named_fixture() {
        let meta = parse_fixture("named.jsonl");
        assert_eq!(meta.custom_title.as_deref(), Some("Named with flag"));
        assert_eq!(
            meta.first_prompt.as_deref(),
            Some("Reply with exactly: named answer")
        );
        assert_eq!(meta.non_interactive, Some(false));
        assert_eq!(meta.cwd.as_deref(), Some("/Users/test/proj"));
        assert_eq!(meta.title().as_deref(), Some("Named with flag"));
        assert!(meta.last_activity.is_some());
        assert!(meta.first_activity <= meta.last_activity);
    }

    #[test]
    fn renamed_fixture() {
        let meta = parse_fixture("renamed.jsonl");
        assert_eq!(meta.custom_title.as_deref(), Some("Renamed by slash"));
        // The /rename wrapper is not the first prompt.
        assert_eq!(
            meta.first_prompt.as_deref(),
            Some("Reply with exactly: first prompt answer")
        );
    }

    #[test]
    fn plain_fixture_uses_a_truncated_first_prompt() {
        let meta = parse_fixture("plain.jsonl");
        assert_eq!(meta.custom_title, None);
        let title = meta.title().unwrap_or_default();
        assert!(title.starts_with("Please reply with the single word: plain."));
        assert_eq!(title.chars().count(), FIRST_PROMPT_TITLE_CHARS);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn print_mode_fixture_is_non_interactive() {
        let meta = parse_fixture("print_mode.jsonl");
        assert_eq!(meta.non_interactive, Some(true));
        assert_eq!(
            meta.first_prompt.as_deref(),
            Some("Reply with exactly: print mode")
        );
    }

    #[test]
    fn garbage_and_unknown_entries_are_skipped() {
        let mut meta = TranscriptMeta::default();
        let buf = b"not json at all\n{\"type\":\"never-seen\",\"x\":1}\n[1,2]\n\n{\"type\":\"user\",\"message\":{\"content\":\"hello  there\"},\"timestamp\":\"2026-01-01T00:00:00Z\"}\n";
        let consumed = parse_complete_lines(buf, &mut meta);
        assert_eq!(consumed, buf.len());
        assert_eq!(meta.first_prompt.as_deref(), Some("hello there"));
        assert!(meta.last_activity.is_some());
    }

    #[test]
    fn meta_sidechain_tool_results_and_wrappers_are_not_prompts() {
        let mut meta = TranscriptMeta::default();
        let lines = [
            r#"{"type":"user","isMeta":true,"message":{"content":"Caveat: x"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"subagent task"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/model</command-name>"}}"#,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"real prompt"}]}}"#,
            r#"{"type":"user","message":{"content":"second prompt"}}"#,
        ];
        let buf = lines.join("\n") + "\n";
        parse_complete_lines(buf.as_bytes(), &mut meta);
        assert_eq!(meta.first_prompt.as_deref(), Some("real prompt"));
    }

    #[test]
    fn generated_titles_and_precedence() {
        let mut meta = TranscriptMeta::default();
        let buf = concat!(
            r#"{"type":"user","message":{"content":"the prompt"}}"#,
            "\n",
            r#"{"type":"summary","summary":"A generated summary"}"#,
            "\n",
        );
        parse_complete_lines(buf.as_bytes(), &mut meta);
        assert_eq!(meta.title().as_deref(), Some("A generated summary"));
        parse_complete_lines(
            b"{\"type\":\"custom-title\",\"customTitle\":\"Mine\"}\n",
            &mut meta,
        );
        assert_eq!(meta.title().as_deref(), Some("Mine"));
        // A later custom title replaces an earlier one.
        parse_complete_lines(
            b"{\"type\":\"custom-title\",\"customTitle\":\"Newer\"}\n",
            &mut meta,
        );
        assert_eq!(meta.title().as_deref(), Some("Newer"));
    }

    #[test]
    fn incremental_parsing_handles_half_lines_and_rewrites() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("s.jsonl");
        let mut file = File::create(&path)?;
        write!(
            file,
            "{{\"type\":\"user\",\"message\":{{\"content\":\"one\"}}}}\n{{\"type\":\"custom-ti"
        )?;
        file.flush()?;
        let mut cache = CachedTranscript::default();
        assert!(update_cached(&path, &mut cache)?);
        assert_eq!(cache.meta.first_prompt.as_deref(), Some("one"));
        assert_eq!(cache.meta.custom_title, None);
        let first_offset = cache.parsed_offset;

        // Finish the half-written line.
        writeln!(file, "tle\",\"customTitle\":\"Done\"}}")?;
        file.flush()?;
        assert!(update_cached(&path, &mut cache)?);
        assert_eq!(cache.meta.custom_title.as_deref(), Some("Done"));
        assert!(cache.parsed_offset > first_offset);

        // Unchanged: nothing to do.
        assert!(!update_cached(&path, &mut cache)?);

        // Rewritten shorter: parsed again from the start.
        std::fs::write(
            &path,
            "{\"type\":\"user\",\"message\":{\"content\":\"fresh\"}}\n",
        )?;
        assert!(update_cached(&path, &mut cache)?);
        assert_eq!(cache.meta.first_prompt.as_deref(), Some("fresh"));
        assert_eq!(cache.meta.custom_title, None);
        Ok(())
    }

    #[test]
    fn mtime_is_the_fallback_activity() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("s.jsonl");
        std::fs::write(&path, "{\"type\":\"mode\"}\n")?;
        let mut cache = CachedTranscript::default();
        update_cached(&path, &mut cache)?;
        assert_eq!(cache.meta.last_activity, Some(cache.mtime));
        Ok(())
    }

    #[test]
    fn session_ids_from_file_names() {
        assert_eq!(
            session_id_of(Path::new("/x/abc-123.jsonl")).as_deref(),
            Some("abc-123")
        );
        assert_eq!(session_id_of(Path::new("/x/abc.json")), None);
    }
}
