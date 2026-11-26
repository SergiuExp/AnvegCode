# Anveg Code

A lightweight, native macOS app for managing many [Claude Code](https://code.claude.com) sessions across projects.

- Each **project** (a main folder plus optional extra folders) opens in its own window.
- The **left side** lists the project's Claude Code sessions: live status, names, filter, and worktree groups.
- The **right side** runs the real `claude` terminal UI in an embedded terminal (Ghostty's libghostty-vt). Next to it are tabs for ordinary shells, and an optional split view.
- A **Notes** panel (toolbar on the right edge, ⌘⌥N) keeps plain-text notes per project in tabs. A note can be sent to the selected session or start a new session, after which it moves to the Archive tab. Notes are stored in the app's data folder, not in the project.
- A **session bar** above each Claude session: the model and effort (click to change them), how full the context is next to a Compact button, Stop, Rewind, Clear, a ⋯ menu of less used commands (/context, /usage, /btw…), and buttons of your own that send a command or message. Buttons are only usable when the session can take them.
- In **full screen**, a row of **window tabs** across the top switches between project windows (as in IntelliJ IDEA), with the close and exit-full-screen window buttons; macOS's slide-down title bar is hidden so nothing covers the app.
- A short, low **bass-riff chime when Claude finishes** a turn (Settings: off, only when you are not looking, or always).
- **Toolbar commands** per project (build, run, test…): buttons in the top bar and optional ⌘ shortcuts that run a shell command in a terminal tab of their own (or a new tab each time).
- The sidebar also has **Files** (a lazy file tree and ⌘P quick open), **Git** (status, side-by-side diffs where the working file can be edited and each change reverted or staged on its own, commit, branches, remotes, history, stash, and an activity log of what each push sent and each pull or fetch received) and **GitHub** (pull requests and CI, through GitHub's API; sign in with your browser or a token) tabs. **Publish to GitHub** turns a project into a new GitHub repository, and **Open from GitHub** clones one and opens it. Files, diffs, pull requests and CI runs open as tabs on the right; files open in a code editor with syntax highlighting and language servers.

The app does not reimplement Claude Code's interface. It starts, resumes, forks, renames, stops, and deletes sessions around it, and can type file and line mentions into Claude's prompt (never pressing Enter). The session bar's buttons type their command and press Enter, as you would. See [`docs/build-plan.md`](docs/build-plan.md) and [`docs/next-phases.md`](docs/next-phases.md) for the design, [`docs/PROGRESS.md`](docs/PROGRESS.md) for status, decisions, and manual test checklists, and [`docs/TODO.md`](docs/TODO.md) for what is postponed (Phase 7: per-session change review).

## Platform

macOS 12+ (Apple silicon and Intel). Linux and Windows are deferred. The code also compiles on Linux, but only so the cloud containers it is developed in can build and test it.

## Requirements

- **Rust 1.98.1**. `rust-toolchain.toml` pins it, so `rustup` installs it automatically. GPUI's macOS backend needs Rust 1.98 or newer.
- **Xcode Command Line Tools** (`xcode-select --install`): GPUI compiles its Metal shaders with them.
- **Network access on the first build.** `ghostty-vt-sys` downloads the prebuilt libghostty-vt 1.0.1 XCFramework (about 16 MB) from `release.files.ghostty.org` and verifies its SHA-256. **No Zig is needed on macOS.**
  - For an offline build, set `GHOSTTY_RELEASE_DIR` to a directory holding the release files (see the `ghostty-vt-sys` crate's README).
  - Zig **0.17** is needed only to build libghostty-vt from source (`GHOSTTY_VT_FROM_SOURCE=1`, or on Linux).
- **Claude Code** installed (`claude` on your login shell's `PATH`, or set its path in Settings).
- Optional: **`git`** (the Git tab), and **language servers** such as `rust-analyzer`, `typescript-language-server`, `pyright-langserver` or `gopls` (Settings ▸ Language servers). All are found on your login shell's `PATH` and run as separate processes.

## Build and run

Build the whole workspace once, so the `anveg-hook` helper sits next to the app binary. Live session status needs it.

```bash
cargo build --release
```

```bash
./target/release/anveg-code
```

For development, `scripts/run-dev.sh` builds the debug app and the helper and runs it, with its own settings (see [Files](#files)).

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
- Debug builds only: `ANVEG_GITHUB_API=<url>` points the GitHub client at a fake API server, and `ANVEG_PICK_FOLDER=<dir>` answers the folder picker of Open from GitHub (Xvfb has no native dialog).
- `scripts/fake-lsp`: a tiny language server (python3) for trying diagnostics, completion, hover and formatting; set it as a server command in Settings.
- `scripts/measure-idle.sh [seconds]`: idle CPU and memory of a running Anveg Code.
- **App icon:** `assets/icon/AppIcon.svg` is the source; `assets/icon/AppIcon.icns` is what the bundle uses. After editing the SVG, render it with `node scripts/render-icon.js assets/icon/AppIcon.svg <dir>` (needs Playwright's Chromium) and pack it with `scripts/make-icns.py <dir> assets/icon/AppIcon.icns`.

## GitHub

The app talks to GitHub's API directly (the `gh` CLI is not needed). Sign in from the GitHub tab or Settings ▸ GitHub:

- **Sign in with GitHub** shows a code to enter at github.com/login/device. It needs an OAuth App's client ID (with Device Flow enabled) in Settings ▸ GitHub, the `ANVEG_GITHUB_CLIENT_ID` environment variable, or baked in at build time (`ANVEG_GITHUB_CLIENT_ID=… cargo build --release`).
- Or paste a **personal access token** (classic, with the `repo`, `read:org` and `workflow` scopes; or a fine-grained one).

The token is kept in the macOS Keychain, never in a file or the log. git uses it for https remotes on github.com (through a `GIT_ASKPASS` helper that answers only github.com prompts), so pushing, pulling and cloning need no separate login. Sign Out removes it.

## Files

- **Settings, projects, window state, and the session cache:** `~/Library/Application Support/Anveg Code/`
- **Log:** `~/Library/Logs/Anveg Code/anveg-code.log`. Set `RUST_LOG=debug` for more detail.
- **Debug builds** (`cargo run`) use `Anveg Code Dev` in place of `Anveg Code` in both paths above, so debugging never touches the installed app's settings; their window titles end in "(Dev)". Set `ANVEG_HOME=<dir>` to keep everything in `<dir>/support` and `<dir>/logs` instead (e.g. a throwaway sandbox), with any build.
- **Claude Code's own files** (`~/.claude`, or the directory `claude auth status` reports) are only read. The one exception: Delete moves a session's transcript to the Trash.

## License

Apache-2.0. Third-party notices are in [`THIRD_PARTY.md`](THIRD_PARTY.md).
