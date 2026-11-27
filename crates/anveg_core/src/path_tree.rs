//! Relative file paths as a folder tree, flattened into rows to draw (the
//! Git tab's changes in Tree view).
//!
//! - Folders come before files, each sorted by name (case-insensitive).
//! - A chain of folders with one folder and no files in each is one row
//!   (`src/app/views`), so deep paths don't waste rows.
//! - Each folder row lists every file below it, for its checkbox.

use std::collections::BTreeMap;

/// One row of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeItem {
    Dir {
        /// The folder's path from the repository root, without a trailing
        /// `/` (`src/app/views`). Unique among the rows: the key for
        /// collapsing it.
        path: String,
        /// What the row shows: the folder's name, or the names of a chain
        /// joined with `/` (`app/views`).
        label: String,
        depth: usize,
        /// Every file below it, as indices into the paths given.
        files: Vec<usize>,
    },
    File {
        /// Index into the paths given.
        index: usize,
        /// The file's name.
        name: String,
        depth: usize,
    },
}

impl TreeItem {
    pub fn depth(&self) -> usize {
        match self {
            Self::Dir { depth, .. } | Self::File { depth, .. } => *depth,
        }
    }
}

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<(String, usize)>,
}

impl Node {
    fn all_files(&self, out: &mut Vec<usize>) {
        for d in self.dirs.values() {
            d.all_files(out);
        }
        out.extend(self.files.iter().map(|f| f.1));
    }
}

/// The rows for `paths` (relative, `/`-separated; a trailing `/` is
/// ignored). Below a folder for which `collapsed(path)` is true, nothing
/// is listed.
pub fn flatten(paths: &[&str], collapsed: &dyn Fn(&str) -> bool) -> Vec<TreeItem> {
    let mut root = Node::default();
    for (index, path) in paths.iter().enumerate() {
        let path = path.trim_end_matches('/');
        let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let Some(name) = parts.pop() else {
            continue;
        };
        let mut node = &mut root;
        for part in parts {
            node = node.dirs.entry(part.to_owned()).or_default();
        }
        node.files.push((name.to_owned(), index));
    }
    let mut rows = Vec::new();
    emit(&root, "", 0, collapsed, &mut rows);
    rows
}

fn emit(node: &Node, prefix: &str, depth: usize, collapsed: &dyn Fn(&str) -> bool, rows: &mut Vec<TreeItem>) {
    let mut dirs: Vec<(&String, &Node)> = node.dirs.iter().collect();
    dirs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.0.cmp(b.0)));
    for (name, mut child) in dirs {
        let mut label = name.clone();
        // One folder and no files: fold it into this row.
        while child.files.is_empty() && child.dirs.len() == 1 {
            let Some((next_name, next)) = child.dirs.iter().next() else {
                break;
            };
            label = format!("{label}/{next_name}");
            child = next;
        }
        let path = if prefix.is_empty() { label.clone() } else { format!("{prefix}/{label}") };
        let mut files = Vec::new();
        child.all_files(&mut files);
        files.sort_unstable();
        let closed = collapsed(&path);
        rows.push(TreeItem::Dir {
            path: path.clone(),
            label,
            depth,
            files,
        });
        if !closed {
            emit(child, &path, depth + 1, collapsed, rows);
        }
    }
    let mut files = node.files.clone();
    files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.0.cmp(&b.0)));
    for (name, index) in files {
        rows.push(TreeItem::File { index, name, depth });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(rows: &[TreeItem]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                TreeItem::Dir { label, depth, files, .. } => {
                    format!("{}{label}/ {files:?}", "  ".repeat(*depth))
                }
                TreeItem::File { name, depth, index } => format!("{}{name} #{index}", "  ".repeat(*depth)),
            })
            .collect()
    }

    #[test]
    fn folders_first_chains_folded() {
        let paths = [
            "README.md",
            "src/main.rs",
            "src/ui/view.rs",
            "src/ui/Button.rs",
            "deep/a/b/c.txt",
            "Cargo.toml",
            "vendor/",
        ];
        let rows = flatten(&paths, &|_| false);
        assert_eq!(
            show(&rows),
            [
                "deep/a/b/ [4]",
                "  c.txt #4",
                "src/ [1, 2, 3]",
                "  ui/ [2, 3]",
                "    Button.rs #3",
                "    view.rs #2",
                "  main.rs #1",
                "Cargo.toml #5",
                "README.md #0",
                "vendor #6",
            ]
        );
        let TreeItem::Dir { path, .. } = &rows[3] else {
            panic!("not a folder");
        };
        assert_eq!(path, "src/ui");
    }

    #[test]
    fn collapsed_folders_hide_their_rows() {
        let paths = ["src/main.rs", "src/ui/view.rs", "top.rs"];
        let rows = flatten(&paths, &|p| p == "src/ui");
        assert_eq!(show(&rows), ["src/ [0, 1]", "  ui/ [1]", "  main.rs #0", "top.rs #2"]);
        let rows = flatten(&paths, &|p| p == "src");
        assert_eq!(show(&rows), ["src/ [0, 1]", "top.rs #2"]);
    }
}
