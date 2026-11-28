//! The built-in icons a toolbar command can have, and the one picked by
//! itself from the command's name (Build → a hammer, Test → a flask).
//!
//! A command stores the icon's key (`ProjectCommand::icon`): `None` means
//! Automatic, [`NO_ICON`] means none. The app maps each key to its picture.

/// Shown with no icon at all.
pub const NO_ICON: &str = "none";

/// Shown when nothing in the name or command suggests another icon, and
/// for a key the app no longer knows.
pub const DEFAULT_ICON: &str = "run";

/// The icons, in the picker's order: (key, what it suggests).
pub const ICONS: &[(&str, &str)] = &[
    ("run", "Run"),
    ("debug", "Debug"),
    ("build", "Build"),
    ("test", "Test"),
    ("check", "Check, lint"),
    ("format", "Format"),
    ("clean", "Clean"),
    ("stop", "Stop"),
    ("restart", "Restart, sync"),
    ("watch", "Watch, preview"),
    ("deploy", "Deploy"),
    ("publish", "Publish, upload"),
    ("install", "Install, download"),
    ("package", "Package"),
    ("server", "Server"),
    ("web", "Open in browser"),
    ("database", "Database, migrate"),
    ("container", "Docker, containers"),
    ("terminal", "Shell"),
    ("git", "Git"),
    ("docs", "Docs"),
    ("generate", "Generate"),
    ("ai", "AI"),
    ("bench", "Benchmark"),
    ("profile", "Profile"),
    ("security", "Audit, security"),
    ("tools", "Tools, configure"),
    ("code", "Code"),
];

/// What the icon with `key` suggests, for its tooltip.
pub fn label(key: &str) -> Option<&'static str> {
    ICONS.iter().find(|(k, _)| *k == key).map(|(_, l)| *l)
}

/// Words that suggest an icon, most specific first: a word in the name or
/// command matches when it starts with one of these ("tests", "building").
const HINTS: &[(&str, &[&str])] = &[
    ("debug", &["debug", "lldb", "gdb", "dlv"]),
    ("test", &["test", "spec", "pytest", "jest", "vitest", "nextest", "e2e"]),
    ("bench", &["bench"]),
    ("profile", &["profil", "flamegraph"]),
    ("check", &["lint", "clippy", "check", "eslint", "typecheck", "tsc", "vet"]),
    ("format", &["fmt", "format", "prettier", "rustfmt", "gofmt"]),
    ("clean", &["clean", "clear", "purge"]),
    ("install", &["install", "setup", "bootstrap", "download", "fetch"]),
    ("stop", &["stop", "kill", "down", "halt"]),
    ("restart", &["restart", "reload", "sync", "refresh"]),
    ("deploy", &["deploy", "release", "ship"]),
    ("publish", &["publish", "upload", "push"]),
    ("container", &["docker", "compose", "podman", "container", "k8s", "kubectl"]),
    ("database", &["migrat", "db", "database", "sql", "psql", "seed"]),
    ("docs", &["doc", "docs", "readme"]),
    ("generate", &["gen", "codegen", "scaffold"]),
    ("security", &["audit", "security", "deny"]),
    ("package", &["package", "pack", "bundle", "dist", "archive"]),
    ("watch", &["watch", "preview", "storybook"]),
    ("server", &["serve", "server", "backend", "api"]),
    ("web", &["open", "browser", "web", "frontend"]),
    ("build", &["build", "compile", "make", "cmake", "assemble"]),
    ("git", &["git", "commit", "pull", "merge", "rebase"]),
    ("ai", &["claude", "ai", "llm"]),
    ("terminal", &["shell", "bash", "zsh", "repl", "console"]),
    ("tools", &["config", "tool", "tune"]),
    ("run", &["run", "start", "dev", "exec", "launch"]),
];

/// The icon picked by itself: from the name's words, else the command's,
/// else [`DEFAULT_ICON`].
pub fn guess(name: &str, command: &str) -> &'static str {
    guess_from(name)
        .or_else(|| guess_from(command))
        .unwrap_or(DEFAULT_ICON)
}

fn guess_from(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    // The first word that suggests anything wins ("cargo test --release"
    // is a test), checking each word against every icon's hints.
    words.iter().find_map(|word| {
        HINTS.iter().find_map(|(icon, hints)| {
            hints
                .iter()
                .any(|h| hint_matches(word, h))
                .then_some(*icon)
        })
    })
}

/// Short hints ("db", "ai", "gen", "vet") must be the whole word, longer
/// ones may start it ("tests", "building").
fn hint_matches(word: &str, hint: &str) -> bool {
    if hint.len() <= 3 {
        word == hint
    } else {
        word.starts_with(hint)
    }
}

/// The icon a command shows: its own choice, or the guess. `None` for no
/// icon.
pub fn resolve(icon: Option<&str>, name: &str, command: &str) -> Option<&'static str> {
    match icon {
        Some(NO_ICON) => None,
        Some(key) => Some(
            ICONS
                .iter()
                .find(|(k, _)| *k == key)
                .map_or(DEFAULT_ICON, |(k, _)| *k),
        ),
        None => Some(guess(name, command)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_from_the_name_then_the_command() {
        assert_eq!(guess("Build", "make"), "build");
        assert_eq!(guess("Run", "cargo run"), "run");
        assert_eq!(guess("Debug", "lldb target/debug/app"), "debug");
        assert_eq!(guess("Tests", "cargo nextest run"), "test");
        assert_eq!(guess("Lint", "cargo clippy"), "check");
        assert_eq!(guess("Deploy to staging", "./deploy.sh staging"), "deploy");
        assert_eq!(guess("Format", "cargo fmt"), "format");
        // Nothing in the name: the command decides, its first telling word.
        assert_eq!(guess("Go", "cargo test --release"), "test");
        assert_eq!(guess("Hello", "echo hello"), DEFAULT_ICON);
        assert_eq!(guess("", ""), DEFAULT_ICON);
        assert_eq!(guess("Up", "docker compose up"), "container");
        assert_eq!(guess("Migrate DB", "npm run migrate"), "database");
        assert_eq!(guess("Docs", "mkdocs serve"), "docs");
        // Short hints are whole words only: "dbg" is not "db", "gender"
        // is not "gen".
        assert_eq!(guess("Gender report", "echo"), DEFAULT_ICON);
        // "download" is an install, not a "down".
        assert_eq!(guess("Download models", ""), "install");
    }

    #[test]
    fn resolves_choices_none_and_unknown_keys() {
        assert_eq!(resolve(None, "Build", ""), Some("build"));
        assert_eq!(resolve(Some("rocket-ship"), "Build", ""), Some(DEFAULT_ICON));
        assert_eq!(resolve(Some("debug"), "Build", ""), Some("debug"));
        assert_eq!(resolve(Some(NO_ICON), "Build", ""), None);
        assert_eq!(label("test"), Some("Test"));
        // Every hint points at a known icon.
        for (icon, _) in HINTS {
            assert!(label(icon).is_some(), "{icon}");
        }
    }
}
