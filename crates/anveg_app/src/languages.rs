//! Which language a file is, for highlighting and for picking a language
//! server (next phases, 4.2). The built-in list covers the bundled grammars;
//! Settings ▸ Editor can add more (`EditorPrefs::file_types`).

use std::collections::BTreeMap;
use std::path::Path;

/// The highlighter language name for `path` ("plaintext" when unknown).
pub fn language_for(path: &Path, extra: &BTreeMap<String, String>) -> &'static str {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if let Some(lang) = extra.get(&name).or_else(|| extra.get(&ext)) {
        return intern(lang);
    }
    match name.as_str() {
        "cargo.lock" | "pipfile" | "poetry.lock" => return "toml",
        "dockerfile" | "makefile" => return "bash",
        ".bashrc" | ".zshrc" | ".profile" | ".bash_profile" | ".zprofile" => return "bash",
        _ => {}
    }
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "py" | "pyi" => "python",
        "go" => "go",
        "json" | "jsonc" | "json5" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" | "mdx" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "sh" | "bash" | "zsh" => "bash",
        "swift" => "swift",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "mm" => "cpp",
        "diff" | "patch" => "diff",
        _ => "plaintext",
    }
}

/// Map a user-configured name onto a known one (so we can return
/// `&'static str`); unknown names are plain text.
fn intern(name: &str) -> &'static str {
    const KNOWN: [&str; 17] = [
        "rust",
        "typescript",
        "tsx",
        "javascript",
        "python",
        "go",
        "json",
        "toml",
        "yaml",
        "markdown",
        "html",
        "css",
        "bash",
        "swift",
        "c",
        "cpp",
        "diff",
    ];
    let lower = name.to_ascii_lowercase();
    KNOWN
        .iter()
        .find(|k| **k == lower)
        .copied()
        .unwrap_or("plaintext")
}

/// The language whose server handles `language` files (TSX goes to the
/// TypeScript server).
pub fn lsp_language(language: &str) -> &str {
    match language {
        "tsx" => "typescript",
        other => other,
    }
}

/// What to install for a missing server (the hint in the editor).
pub fn install_hint(language: &str) -> Option<&'static str> {
    Some(match language {
        "rust" => "rustup component add rust-analyzer",
        "typescript" | "javascript" | "tsx" => {
            "npm install -g typescript typescript-language-server"
        }
        "python" => "npm install -g pyright",
        "go" => "go install golang.org/x/tools/gopls@latest",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        let none = BTreeMap::new();
        assert_eq!(language_for(Path::new("a/b.rs"), &none), "rust");
        assert_eq!(language_for(Path::new("x.TSX"), &none), "tsx");
        assert_eq!(language_for(Path::new("Cargo.lock"), &none), "toml");
        assert_eq!(language_for(Path::new("notes.txt"), &none), "plaintext");
        let extra = BTreeMap::from([("tpl".to_owned(), "HTML".to_owned())]);
        assert_eq!(language_for(Path::new("x.tpl"), &extra), "html");
        assert_eq!(lsp_language("tsx"), "typescript");
        assert!(install_hint("go").is_some());
    }
}
