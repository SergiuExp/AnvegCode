//! Publish to GitHub (a project → a new GitHub repository) and the checks
//! before it. Blocking: run on a background thread.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};

use crate::git::{Git, NetworkOp};
use crate::git_activity::{self, Entry};
use crate::github_api::{CreatedRepo, GitHub, NewRepo};
use crate::process::CancelToken;

/// What the project looks like before publishing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Precheck {
    /// The repository root (the folder itself when it is not a repository
    /// yet).
    pub repo: PathBuf,
    pub is_repo: bool,
    /// At least one commit.
    pub has_head: bool,
    /// The `origin` remote's URL, if there is one (publishing stops).
    pub origin: Option<String>,
    pub branch: Option<String>,
    /// The folder is inside another repository (whose root this is):
    /// publishing would publish that one, so it stops.
    pub inside: Option<PathBuf>,
}

pub fn precheck(git: &Git, dir: &Path) -> Precheck {
    let found = git.find_repo(dir);
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let inside = found.clone().filter(|r| canon(r) != canon(dir));
    let is_repo = found.is_some() && inside.is_none();
    let repo = if is_repo {
        found.unwrap_or_else(|| dir.to_path_buf())
    } else {
        dir.to_path_buf()
    };
    Precheck {
        inside,
        has_head: is_repo && git.has_head(&repo),
        origin: is_repo
            .then(|| git.remote_url(&repo, "origin").ok())
            .flatten(),
        branch: is_repo.then(|| git.current_branch(&repo)).flatten(),
        repo,
        is_repo,
    }
}

/// A name GitHub accepts, from a folder name: letters, digits, `-`, `_`
/// and `.`; anything else becomes `-`.
pub fn repo_name_from_folder(folder: &str) -> String {
    let name: String = folder
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '-' })
        .collect();
    let name = name.trim_matches('-').to_owned();
    if name.is_empty() { "project".to_owned() } else { name }
}

/// Why `name` cannot be a repository name, if it cannot.
pub fn check_repo_name(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("Enter a name.")
    } else if name.len() > 100 {
        Some("The name is too long (100 characters at most).")
    } else if !name.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) {
        Some("Use only letters, digits, “-”, “_” and “.”.")
    } else if name == "." || name == ".." {
        Some("That name is reserved.")
    } else {
        None
    }
}

/// Create the repository on GitHub, then: `git init` if needed, the first
/// commit if there is none (`commit_first`, which the user agreed to), add
/// `origin`, and push the current branch with `-u`. The push's activity
/// entry comes back as well, even when it fails.
pub fn publish(
    github: &GitHub,
    pre: &Precheck,
    new: &NewRepo,
    commit_first: bool,
    cancel: &CancelToken,
) -> (Result<CreatedRepo>, Option<Entry>) {
    let git = &github.git;
    if let Some(outer) = &pre.inside {
        return (
            Err(anyhow!(
                "This folder is inside another git repository ({}). Publish that repository, or move the folder out of it.",
                outer.display()
            )),
            None,
        );
    }
    if let Some(url) = &pre.origin {
        return (
            Err(anyhow!(
                "This project already has a remote named origin ({url}). Publishing stopped, so nothing changed."
            )),
            None,
        );
    }
    if !pre.has_head && !commit_first {
        return (Err(anyhow!("The project has no commits yet.")), None);
    }
    let created = match github.create_repo(new) {
        Ok(c) => c,
        Err(e) => return (Err(e), None),
    };
    let local = || -> Result<()> {
        if !pre.is_repo {
            git.init(&pre.repo)?;
        }
        if !pre.has_head {
            git.initial_commit(&pre.repo, "Initial commit").map_err(|e| {
                anyhow!("Could not make the first commit: {e:#}\n(Is git's user.name and user.email set? Is the folder empty?)")
            })?;
        }
        git.add_remote(&pre.repo, "origin", &created.clone_url)
    };
    if let Err(e) = local() {
        return (
            Err(anyhow!(
                "The repository was created on GitHub ({}), but setting up the project failed: {e:#}",
                created.html_url
            )),
            None,
        );
    }
    let (pushed, entry) = git_activity::run_logged(
        git,
        &pre.repo,
        &NetworkOp::Push {
            set_upstream: Some("origin".into()),
        },
        cancel,
        None,
    );
    match pushed {
        Ok(_) => (Ok(created), Some(entry)),
        Err(e) => (
            Err(anyhow!(
                "The repository was created on GitHub ({}) and added as origin, but the push failed: {e:#}\nPush again from the Git panel.",
                created.html_url
            )),
            Some(entry),
        ),
    }
}

/// Where a clone goes: `parent/name`, refused when it exists and is not
/// empty.
pub fn clone_target(parent: &Path, name: &str) -> Result<PathBuf> {
    let dest = parent.join(name);
    if dest.exists() {
        let empty = std::fs::read_dir(&dest).map(|mut d| d.next().is_none()).unwrap_or(false);
        if !empty {
            bail!("{} already exists and is not empty. Choose another folder.", dest.display());
        }
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{self, Cmd};

    fn sh(dir: &Path, script: &str) -> Result<()> {
        process::run(
            &Cmd::new("sh")
                .args(["-c", script])
                .cwd(dir)
                .var("GIT_AUTHOR_NAME", "T")
                .var("GIT_AUTHOR_EMAIL", "t@example.com")
                .var("GIT_COMMITTER_NAME", "T")
                .var("GIT_COMMITTER_EMAIL", "t@example.com"),
        )?
        .check()
        .map(drop)
    }

    #[test]
    fn prechecks() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().canonicalize()?;
        let git = Git::default();
        let p = precheck(&git, &dir);
        assert!(!p.is_repo && !p.has_head && p.origin.is_none());
        sh(&dir, "git init -q -b main")?;
        let p = precheck(&git, &dir);
        assert!(p.is_repo && !p.has_head);
        assert_eq!(p.branch.as_deref(), Some("main"));
        sh(&dir, "git config commit.gpgsign false && echo x > a && git add a && git commit -qm one && git remote add origin https://github.com/o/r.git")?;
        let p = precheck(&git, &dir);
        assert!(p.has_head);
        assert_eq!(p.origin.as_deref(), Some("https://github.com/o/r.git"));
        // A subfolder of a repository is not a repository of its own.
        std::fs::create_dir(dir.join("sub"))?;
        let p = precheck(&git, &dir.join("sub"));
        assert!(!p.is_repo);
        assert_eq!(p.inside.as_deref(), Some(dir.as_path()));
        Ok(())
    }

    #[test]
    fn names_and_targets() -> Result<()> {
        assert_eq!(repo_name_from_folder("My Project!"), "My-Project");
        assert_eq!(repo_name_from_folder("  "), "project");
        assert_eq!(check_repo_name("ok-name_1.0"), None);
        assert!(check_repo_name("bad name").is_some());
        assert!(check_repo_name("").is_some());
        let tmp = tempfile::tempdir()?;
        assert_eq!(clone_target(tmp.path(), "new")?, tmp.path().join("new"));
        std::fs::create_dir(tmp.path().join("empty"))?;
        assert!(clone_target(tmp.path(), "empty").is_ok());
        std::fs::create_dir(tmp.path().join("full"))?;
        std::fs::write(tmp.path().join("full/x"), "x")?;
        assert!(clone_target(tmp.path(), "full").is_err());
        Ok(())
    }
}
