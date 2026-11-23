# Progress

Newest entries at the bottom. Each milestone lists what was done, the decisions taken, known issues, and a manual test checklist.

---

## Pinned versions

| Component | Version | Source |
|---|---|---|
| Rust toolchain | 1.96+ (developed with 1.97.0) | rustup |
| GPUI | `gpui-pre` **=0.3.10** (snapshot of Zed's `crates/gpui`, Apache-2.0) | crates.io, pinned by gpui-kit 0.8.1 |
| gpui-kit (Longbridge) | **=0.8.1**, `default-features = false`, `features = ["assets"]` | crates.io |
| gpui-base | 0.8.1 (via gpui-kit) | crates.io |
| gpui-cn | **=0.9.0**, `default-features = false`, `features = ["ghostty"]` | crates.io |
| libghostty-vt | 1.0.1, Ghostty commit `108cacb17ed8990840cd14abfce43d8ed8503ca1` (`ghostty-vt` / `ghostty-vt-sys` 0.9.0+ghostty-vt.1.0.1) | prebuilt XCFramework on macOS, SHA-256-checked |
| Zig | 0.17 (source builds only; not needed on macOS) | |

There are no git dependencies: everything comes from crates.io with exact `=` pins, and `Cargo.lock` is committed.

---

## M0 — Skeleton (2026-11-23)

**Done**
- Cargo workspace: `anveg_core` (no GPUI), `anveg_term`, and `anveg_app` (binary `anveg-code`).
- `deny.toml` with the section 3.2 allow-list. It checks only the macOS targets, the only ones that ship.
- Logging with `tracing` to `~/Library/Logs/Anveg Code/anveg-code.log` and stderr. GPUI's `log` records are bridged in. Startup logs `first window opened elapsed_ms=…` to measure cold start.
- `anveg_core::paths`: the app's directories (section 11), with tests.
- A GPUI window with a transparent title bar (gpui-cn `TitleBar`), using gpui-cn's theme. It follows the system light or dark mode.

**Decisions**
1. **Component library: gpui-base + gpui-cn, not Longbridge's gpui-component.** *(Confirmed by the owner on 2026-11-23.)*
   - Longbridge's library is now `gpui-kit` (gpui-base + gpui-component + assets). gpui-cn's terminal requires gpui-kit **0.8.x**, so gpui-kit is pinned at 0.8.1, not the latest 0.11.
   - gpui-component and gpui-cn each install their own theme and their own `Root` plugin (tooltip overlay, window surface). Running both would mean two competing themes in one window.
   - gpui-cn is a complete styled library on top of gpui-base: sidebar, dialogs, menus and context menus, tabs, inputs, a command palette, and folder and file pickers. It is also where the terminal lives.
   - So the app uses gpui-base (behavior) + gpui-cn (styling), and gpui-component is turned off. This also removes an MPL/0BSD dependency (`enum-iterator`) and makes the binary smaller.
   - Cost: the plan's later code editor (Phases 4–7) was going to come from gpui-component. Revisit then; gpui-kit can re-enable `component` if the theme clash can be managed.
2. **Linux removed from the plan** (owner's request). It now sits in section 12 (Deferred) of `docs/build-plan.md`, next to Windows. The code still compiles on Linux, because the cloud containers that run `cargo build/clippy/test` are Linux. That is a development convenience, not a supported target.

---

## M1 — Terminal spike (2026-11-23) — ⏸ waiting for the owner's checklist

**Choice: option 1, gpui-cn with the `ghostty` feature.** It compiled against our pinned GPUI on the first try.

**Done**
- `anveg_term::TerminalView` wraps the engine. The rest of the app uses only `TerminalView`, `TerminalSettings`, `TerminalViewEvent`, `RunState` and `ExitInfo`, never gpui-cn's terminal types, so the engine can be replaced later (for example, `alacritty_terminal` for Windows).
- `anveg_core::launcher::LaunchSpec`: an engine-independent description of what to run (program, args, cwd, env, login).
- The spike window has two tabs:
  - **Claude** runs `claude` in the current directory. Override with `ANVEG_CLAUDE` and `ANVEG_SPIKE_DIR`.
  - **Terminal** runs the login shell and starts lazily the first time it is shown.
  - ⌘T toggles between them. When the program exits, a dimmed "Session ended / Exit code N" overlay appears, with a start-again button.
- Hidden terminals keep parsing output but publish no frames (`set_visible(false)`), so they are never drawn.
- **Option as Meta** (`TerminalSettings::option_as_meta`, default on): Option+letter, Option+Enter and Option+Backspace send ESC + key. The engine on its own would send the composed character (Option+a → "å"). Unit-tested.
- **Key bindings, ⌘ only:** copy, paste, select all, scroll (⌘↑ / ⌘↓ to top and bottom, ⌘PgUp / ⌘PgDn), font zoom (⌘= ⌘- ⌘0). gpui-cn's defaults also took Shift+↑/↓, Shift+PgUp/PgDn and Shift+Home/End; we drop those so the keys reach Claude Code.
- Smoke-tested in the Linux container under Xvfb with software rendering:
  - the window opens (first window 642 ms, debug build, software GPU);
  - typing reaches the PTY;
  - 24-bit color renders;
  - `exit 3` shows the overlay with "Exit code 3";
  - ⌘T starts the shell tab.

**How the plan's requirements map onto gpui-cn's engine**

| Plan | gpui-cn engine | Note |
|---|---|---|
| `TerminalEngine` trait (feed/resize/snapshot/encode) | Not needed as a trait. gpui-cn's `Engine` and `Terminal` element do all of it; our seam is the `TerminalView` API instead. | The engine owns the PTY (`portable-pty`), parsing, and drawing. |
| `pty_host.rs` in `anveg_core` | The PTY is owned by gpui-cn's `LocalPty`. `anveg_core` keeps the pure parts: `LaunchSpec` now, the claude command-line builder in M5. | Keeps the core GUI-free. |
| `TERM=xterm-256color` | `TERM=xterm-ghostty`, with Ghostty's terminfo shipped, plus `TERM_PROGRAM=ghostty`, `COLORTERM=truecolor`, and `LANG=en_US.UTF-8` if unset | More accurate than xterm-256color. Claude Code knows Ghostty, including Shift+Enter via the kitty keyboard protocol. |
| Stop: SIGHUP → up to 3 s → SIGKILL | SIGHUP → 250 ms → SIGTERM → 250 ms → SIGKILL, to every process in the PTY's session | Shorter than 3 s. Claude Code writes its transcript continuously, so this should be safe. Revisit in M5 if a resumed session ever loses its last message. |
| PTY resize debounce ~16 ms | Handled by the engine | |
| Idle CPU | Output is event-driven (no polling), hidden terminals publish no frames, cursor blink is off by default | gpui-cn also "parks" terminals idle for 60 s (frees the parser's memory). |

**Known issues / open questions**
- **Licenses (resolved 2026-11-23):** the owner approved adding MPL-2.0 and CDLA-Permissive-2.0 to the allow-list for these crates (details in `THIRD_PARTY.md`):
  - MPL-2.0: `nucleo-matcher` (inside gpui-cn, ships), `option-ext` (via `dirs`/`directories`, ships), `cbindgen` (build tool only).
  - CDLA-Permissive-2.0: `webpki-root-certs` (build tool only).
  - None of these can be avoided while gpui-cn is used. `option-ext` arrives through `ghostty-vt` → `dirs` even without our own `directories` dependency.
- The release download of libghostty-vt needs `release.files.ghostty.org` on the first build.
- The overlay's start-again button starts a **new** `claude` (no `--resume` yet: sessions arrive in M5).
- On quit, terminals are stopped from `on_app_quit`. GPUI waits only a short time for quit handlers, so very slow programs may receive SIGKILL. Proper shutdown ordering is M8.

**Manual checklist for the owner (macOS)** — this is the M1 gate (plan 5.5). From a terminal, in a project folder:

```bash
cd ~/some/project && cargo run --release --manifest-path /path/to/AnvegCode/Cargo.toml
```

Then check each item:

- [ ] The window opens, follows light or dark mode, and has native traffic lights over a transparent title bar. Claude starts in the Claude tab.
- [ ] 24-bit color: Claude's UI colors look right. In the Terminal tab: `printf "\e[38;2;255;100;0mtruecolor\e[0m\n"`.
- [ ] Alternate screen: `vim` or `less` in the Terminal tab, then quit; the previous screen comes back.
- [ ] Unicode and emoji widths: `echo "日本語 ✓ 🎉 é"` lines up, and Claude's box drawing is clean.
- [ ] Synchronized output: ask Claude for a long answer; no flicker or tearing while it streams.
- [ ] Bracketed paste: copy 500 lines (`seq 500 | pbcopy`), then ⌘V into Claude's prompt. It arrives as one paste ("[Pasted text …]").
- [ ] Enter submits. **Shift+Enter** inserts a newline. **Option+Enter** inserts a newline.
- [ ] These reach Claude: Esc, Esc Esc (rewind menu), Ctrl+C, Ctrl+R, **Shift+Tab** (cycles permission mode), arrow keys, Tab.
- [ ] **Ctrl+V** reaches Claude: copy an image (⌘⇧⌃4 screenshot to clipboard), then Ctrl+V in Claude's prompt attaches it.
- [ ] Option as Meta: in the Terminal tab (bash or zsh), type a few words, then Option+B / Option+F jump by word and Option+Backspace deletes a word.
- [ ] Mouse wheel scrolls Claude's view. In the Terminal tab, `seq 3000`, then wheel up: scrollback works. ⌘↑ / ⌘↓ jump to top and bottom.
- [ ] Select text with the mouse, ⌘C, then paste elsewhere: correct text.
- [ ] Resize the window quickly: content reflows without garbage. Claude redraws to the new width.
- [ ] ⌘-click a URL (e.g. `echo https://example.com`) opens the browser.
- [ ] ⌘T toggles Claude and Terminal. Switching is instant, and the hidden one keeps its state.
- [ ] `/exit` in Claude shows "Session ended". The button starts a new `claude`.
- [ ] ⌘Q quits, and no `claude` or shell processes are left (`pgrep -fl claude`).
- [ ] Feel and performance: typing latency, idle CPU (`top -pid $(pgrep anveg-code)`, ≈0% while idle), memory (`ps -o rss= -p $(pgrep anveg-code)`).
- [ ] Log file exists: `~/Library/Logs/Anveg Code/anveg-code.log` contains `first window opened elapsed_ms=…`. Note the number from a `--release` build.
