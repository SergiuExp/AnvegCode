# Anveg Code

A lightweight, native macOS app for managing many [Claude Code](https://code.claude.com) sessions across projects. Each project gets its own window, with its sessions listed on the left. On the right is the real `claude` terminal UI, running in an embedded terminal (Ghostty's libghostty-vt).

Status: early development. See [`docs/PROGRESS.md`](docs/PROGRESS.md) for where things stand, and [`docs/build-plan.md`](docs/build-plan.md) for the plan.

## Platform

macOS (Apple silicon and Intel). Linux and Windows are deferred.

## Build

Requirements:

- **Rust 1.96 or newer** (`rustup update stable`).
- **Xcode Command Line Tools** (`xcode-select --install`). GPUI compiles its Metal shaders with them.
- **Network access on the first build.** On macOS, `ghostty-vt-sys` downloads the prebuilt libghostty-vt 1.0.1 XCFramework (about 16 MB) from `release.files.ghostty.org` and checks its SHA-256. **No Zig is needed on macOS.**
  - For an offline build, set `GHOSTTY_RELEASE_DIR` to a directory that already holds the release files. See the `ghostty-vt-sys` crate README for details.
  - Zig **0.17** is only needed to build libghostty-vt from source (`GHOSTTY_VT_FROM_SOURCE=1`, or on non-Apple hosts).

```bash
cargo run
```

Checks that every milestone must pass:

```bash
cargo build
cargo clippy --all-targets -- -D warnings
cargo test
cargo deny check licenses   # cargo install cargo-deny
```

Logs go to `~/Library/Logs/Anveg Code/anveg-code.log` and to stderr. `RUST_LOG=debug` gives more detail.

### Milestone 1 spike

`cargo run` opens one window with two tabs:

- **Claude** runs `claude` in the current directory.
- **Terminal** runs your login shell. It starts the first time you switch to it, with ⌘T.

Environment overrides for the spike:

- `ANVEG_CLAUDE=/path/to/claude`: the program the Claude tab runs.
- `ANVEG_SPIKE_DIR=/some/project`: the working directory.

Start it from a terminal, so it inherits your `PATH`. Login-environment capture arrives in M2.

## License

Apache-2.0. Third-party notices are in [`THIRD_PARTY.md`](THIRD_PARTY.md).
