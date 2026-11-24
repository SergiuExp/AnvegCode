# Anveg Code

A lightweight, native macOS app for managing many [Claude Code](https://code.claude.com) sessions across projects.

- Each **project** (a main folder plus optional extra folders) opens in its own window.
- The **left side** lists the project's Claude Code sessions: live status, names, filter, and worktree groups.
- The **right side** runs the real `claude` terminal UI in an embedded terminal (Ghostty's libghostty-vt). Next to it are tabs for ordinary shells, and an optional split view.
- A **Notes** panel (toolbar on the right edge, ⌘⌥N) keeps plain-text notes per project in tabs. A note can be sent to the selected session or start a new session, after which it moves to the Archive tab. Notes are stored in the app's data folder, not in the project.
- **Toolbar commands** per project (build, run, test…): buttons in the top bar and optional ⌘ shortcuts that run a shell command in a terminal tab of their own (or a new tab each time).
- The sidebar also has **Files** (a lazy file tree and ⌘P quick open), **Git** (status, diffs, hunk staging, commit, branches, remotes, history, stash) and **GitHub** (pull requests and CI through `gh`) tabs. Files, diffs, pull requests and CI runs open as tabs on the right; files open in a code editor with syntax highlighting and language servers.

The app does not reimplement Claude Code's interface. It starts, resumes, forks, renames, stops, and deletes sessions around it, and can type file and line mentions into Claude's prompt (never pressing Enter). See [`docs/build-plan.md`](docs/build-plan.md) and [`docs/next-phases.md`](docs/next-phases.md) for the design, [`docs/PROGRESS.md`](docs/PROGRESS.md) for status, decisions, and manual test checklists, and [`docs/TODO.md`](docs/TODO.md) for what is postponed (Phase 7: per-session change review).

## Platform

macOS 12+ (Apple silicon and Intel). Linux and Windows are deferred. The code also compiles on Linux, but only so the cloud containers it is developed in can build and test it.

## Requirements

- **Rust 1.98.1**. `rust-toolchain.toml` pins it, so `rustup` installs it automatically. GPUI's macOS backend needs Rust 1.98 or newer.
- **Xcode Command Line Tools** (`xcode-select --install`): GPUI compiles its Metal shaders with them.
- **Network access on the first build.** `ghostty-vt-sys` downloads the prebuilt libghostty-vt 1.0.1 XCFramework (about 16 MB) from `release.files.ghostty.org` and verifies its SHA-256. **No Zig is needed on macOS.**
  - For an offline build, set `GHOSTTY_RELEASE_DIR` to a directory holding the release files (see the `ghostty-vt-sys` crate's README).
  - Zig **0.17** is needed only to build libghostty-vt from source (`GHOSTTY_VT_FROM_SOURCE=1`, or on Linux).
- **Claude Code** installed (`claude` on your login shell's `PATH`, or set its path in Settings).
- Optional: **`git`** (the Git tab), the **GitHub CLI `gh`**, signed in (the GitHub tab), and **language servers** such as `rust-analyzer`, `typescript-language-server`, `pyright-langserver` or `gopls` (Settings ▸ Language servers). All are found on your login shell's `PATH` and run as separate processes.

## Build and run

Build the whole workspace once, so the `anveg-hook` helper sits next to the app binary. Live session status needs it.

```bash
cargo build --release
```

```bash
./target/release/anveg-code
```

### The app bundle

```bash
scripts/bundle-macos.sh
```

This builds `dist/Anveg Code.app` with the helper inside, and signs it ad hoc for local use. Options:

- `UNIVERSAL=1 scripts/bundle-macos.sh` builds for both Apple silicon and Intel.
- `BUNDLE_ID=…` overrides the placeholder identifier `dev.anveg.code`.

Notifications only work from the bundled app; under `cargo run` they are written to the log.

### Install in /Applications

```bash
scripts/install-macos.sh --open
```

This builds the bundle (as above) and installs it as `/Applications/Anveg Code.app`, replacing an older copy. If that copy is running, it is asked to quit first; it may ask whether to stop running sessions. Leave out `--open` to install without launching. Other options:

- `DEST=~/Applications scripts/install-macos.sh` installs for your user only.
- `UNIVERSAL=1` and `BUNDLE_ID=…` work as for `bundle-macos.sh`.

Your settings, projects and notes stay where they are, in `~/Library/Application Support/Anveg Code/`.

## Checks

Every milestone must pass these:

```bash
cargo build
```

```bash
cargo clippy --all-targets -- -D warnings
```

```bash
cargo test
```

```bash
cargo deny check licenses
```

From a Linux machine, `scripts/check-macos.sh` type-checks the macOS build, including the macOS-only code, without linking. It compiles the tree-sitter grammars' C code with `zig cc` when there is no Apple toolchain.

## Tools

- `scripts/fake-claude`: a stand-in for `claude` used by the tests and for trying the UI without model calls. Put it on `PATH` as `claude`, or set it in Settings ▸ Claude ▸ Path.
- `scripts/fake-gh`: a stand-in for the GitHub CLI with fixed pull requests, checks and runs. `FAKE_GH_LOG=<file>` records every call.
- `scripts/fake-lsp`: a tiny language server (python3) for trying diagnostics, completion, hover and formatting; set it as a server command in Settings.
- `scripts/measure-idle.sh [seconds]`: idle CPU and memory of a running Anveg Code.
- **App icon:** `assets/icon/AppIcon.svg` is the source; `assets/icon/AppIcon.icns` is what the bundle uses. After editing the SVG, render it with `node scripts/render-icon.js assets/icon/AppIcon.svg <dir>` (needs Playwright's Chromium) and pack it with `scripts/make-icns.py <dir> assets/icon/AppIcon.icns`.

## Files

- **Settings, projects, window state, and the session cache:** `~/Library/Application Support/Anveg Code/`
- **Log:** `~/Library/Logs/Anveg Code/anveg-code.log`. Set `RUST_LOG=debug` for more detail.
- **Claude Code's own files** (`~/.claude`, or the directory `claude auth status` reports) are only read. The one exception: Delete moves a session's transcript to the Trash.

## License

Apache-2.0. Third-party notices are in [`THIRD_PARTY.md`](THIRD_PARTY.md).
