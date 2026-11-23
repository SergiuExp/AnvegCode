# Anveg Code

A lightweight, native macOS app for managing many [Claude Code](https://code.claude.com) sessions across projects.

- Each **project** (a main folder plus optional extra folders) opens in its own window.
- The **left side** lists the project's Claude Code sessions: live status, names, filter, and worktree groups.
- The **right side** runs the real `claude` terminal UI in an embedded terminal (Ghostty's libghostty-vt). Next to it are tabs for ordinary shells, and an optional split view.

The app does not reimplement Claude Code's interface. It starts, resumes, forks, renames, stops, and deletes sessions around it. See [`docs/build-plan.md`](docs/build-plan.md) for the design and [`docs/PROGRESS.md`](docs/PROGRESS.md) for status, decisions, and manual test checklists.

## Platform

macOS 12+ (Apple silicon and Intel). Linux and Windows are deferred. The code also compiles on Linux, but only so the cloud containers it is developed in can build and test it.

## Requirements

- **Rust 1.98.1**. `rust-toolchain.toml` pins it, so `rustup` installs it automatically. GPUI's macOS backend needs Rust 1.98 or newer.
- **Xcode Command Line Tools** (`xcode-select --install`): GPUI compiles its Metal shaders with them.
- **Network access on the first build.** `ghostty-vt-sys` downloads the prebuilt libghostty-vt 1.0.1 XCFramework (about 16 MB) from `release.files.ghostty.org` and verifies its SHA-256. **No Zig is needed on macOS.**
  - For an offline build, set `GHOSTTY_RELEASE_DIR` to a directory holding the release files (see the `ghostty-vt-sys` crate's README).
  - Zig **0.17** is needed only to build libghostty-vt from source (`GHOSTTY_VT_FROM_SOURCE=1`, or on Linux).
- **Claude Code** installed (`claude` on your login shell's `PATH`, or set its path in Settings).

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

From a Linux machine, `scripts/check-macos.sh` type-checks the macOS build, including the macOS-only code, without linking.

## Tools

- `scripts/fake-claude`: a stand-in for `claude` used by the tests and for trying the UI without model calls. Put it on `PATH` as `claude`, or set it in Settings ▸ Claude ▸ Path.
- `scripts/measure-idle.sh [seconds]`: idle CPU and memory of a running Anveg Code.

## Files

- **Settings, projects, window state, and the session cache:** `~/Library/Application Support/Anveg Code/`
- **Log:** `~/Library/Logs/Anveg Code/anveg-code.log`. Set `RUST_LOG=debug` for more detail.
- **Claude Code's own files** (`~/.claude`, or the directory `claude auth status` reports) are only read. The one exception: Delete moves a session's transcript to the Trash.

## License

Apache-2.0. Third-party notices are in [`THIRD_PARTY.md`](THIRD_PARTY.md).
