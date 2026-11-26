# Progress

Newest entries at the bottom. Each milestone lists what was done, the decisions taken, known issues, and a manual test checklist.

---

## Pinned versions

| Component | Version | Source |
|---|---|---|
| Rust toolchain | **1.98.1** (pinned in `rust-toolchain.toml`; GPUI's macOS backend needs ≥ 1.98) | rustup |
| GPUI | `gpui-pre` **=0.3.10** (snapshot of Zed's `crates/gpui`, Apache-2.0) | crates.io, pinned by gpui-kit 0.8.1 |
| gpui-kit (Longbridge) | **=0.8.1**, `default-features = false`, `features = ["assets", "component", "tree-sitter-*"]` (Phase 4: gpui-component's editor, diff view, Markdown and 16 grammars) | crates.io |
| gpui-component | 0.8.1 (via gpui-kit, from Phase 4) | crates.io |
| lsp-types | **=0.97.0** (the version gpui-base's editor uses) | crates.io |
| gpui-base | 0.8.1 (via gpui-kit) | crates.io |
| gpui-cn | **=0.9.0**, `default-features = false`, `features = ["ghostty"]` | crates.io |
| libghostty-vt | 1.0.1, Ghostty commit `108cacb17ed8990840cd14abfce43d8ed8503ca1` (`ghostty-vt` / `ghostty-vt-sys` 0.9.0+ghostty-vt.1.0.1) | prebuilt XCFramework on macOS, SHA-256-checked |
| Zig | 0.17 (source builds only; not needed on macOS) | |

There are no git dependencies: everything comes from crates.io with exact `=` pins, and `Cargo.lock` is committed.

Other notable crates: `ignore` 0.4 (ignore rules in the Files tab and quick open), `nucleo-matcher` 0.3 (fuzzy matching), `similar` 2.7 (Compare), `notify` 8 + `notify-debouncer-full` 0.7 (file watching), `trash` 5, `sysinfo` 0.37 (memory display), and `objc2` 0.6 / `objc2-app-kit` / `objc2-user-notifications` 0.3 (Dock badge, notifications; macOS only).

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
   - **Revisited in M19 (Phase 4):** gpui-component is now on, for its editor, diff view and Markdown renderer only. gpui-cn still styles the app; gpui-component's theme is kept in step with gpui-cn's (`component_theme.rs`), and gpui-cn keeps ownership of the gpui-base theme. The owner approved the one new license this needs (0BSD, `enum-iterator`).
2. **Linux removed from the plan** (owner's request). It now sits in section 12 (Deferred) of `docs/build-plan.md`, next to Windows. The code still compiles on Linux, because the cloud containers that run `cargo build/clippy/test` are Linux. That is a development convenience, not a supported target.

---

## M1 — Terminal spike (2026-11-23)

> The owner asked to continue through every phase without stopping at this gate. The checklist below has not been run on a Mac yet. It now applies to the full app: the spike window was replaced by the real project window in M3.

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

**Manual checklist for the owner (macOS)** — the terminal gate (plan 5.5). Build with `cargo build --release`, start `./target/release/anveg-code`, open a project (⌘O), and start a session (⌘N). Then check each item:

- [ ] The window follows light or dark mode, and has native traffic lights over a transparent title bar. The terminal palette switches live with it.
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
- [ ] `/exit` in Claude shows "Session ended". **Resume** continues the same session.
- [ ] ⌘Q quits, and no `claude` or shell processes are left (`pgrep -fl claude`).
- [ ] Feel and performance: typing latency, idle CPU (`top -pid $(pgrep anveg-code)`, ≈0% while idle), memory (`ps -o rss= -p $(pgrep anveg-code)`).
- [ ] Log file exists: `~/Library/Logs/Anveg Code/anveg-code.log` contains `first window opened elapsed_ms=…`. Note the number from a `--release` build.

---

## M2 — Core config (2026-11-23)

**Done** (all in `anveg_core`, unit-tested)
- `atomic.rs`: JSON files are written to a temp file and renamed over the original. A file that can't be parsed is moved aside as `*.json.corrupt`, and the app starts with defaults.
- `config.rs`: `Settings` (every setting in 6.8 plus the Phase 2 ones), `ProjectStore` (`projects.json`; opening a folder canonicalizes it, so `/tmp/x` and `/private/tmp/x` are one project), and `AppState` (`state.json`: windows, bounds, selection, tab, sidebar width, running sessions, terminal tabs, split). Missing or unknown fields are tolerated.
- `claude_env.rs`:
  - Login-shell environment capture: `$SHELL -l -i -c`, with a marker before `env -0`, a 5 s timeout, then a retry without `-i`, then the app's own environment.
  - Finding `claude`: the Settings path, else `PATH`, else common install locations.
  - `claude --version`, and the config directory from `claude auth status` → `configDirectory` (verified with 2.1.330), else `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
  - `claude agents --json` parsing (shape verified with 2.1.330).
- `time.rs`: RFC 3339 parsing, "5 min ago", and UTC formatting, with no date crate.

**Finding:** when Anveg Code is started from inside a Claude Code terminal, `claude` inherits `CLAUDE_CODE_CHILD_SESSION` and similar markers, and then **turns transcript saving off** (observed with 2.1.330). The app removes these markers from every child's environment (`INHERITED_CLAUDE_MARKERS`).

---

## M3 — Projects and windows (2026-11-23)

**Done**
- **Welcome window** (when no project window is open): Claude Code's version, or install help when it is missing; **Open Folder…**; recent projects (click to open, × to remove).
- **⌘O** opens a folder picker; a folder that is already a project focuses its window. **File ▸ Open Recent** has the 15 most recent projects plus **Clear Menu**.
- **One window per project**, titled with the project's name.
- **Project Settings** (gear icon in the sidebar header, or File ▸ Project Settings…): rename the project, see the main folder, add or remove extra folders, with a note that folder changes apply to sessions started afterwards.
- **Restore on launch** from `state.json`: bounds, selected session, tab, sidebar width, number of terminal tabs, split. Saved 500 ms after changes and on quit.
- A missing main folder shows a banner with **Locate…** and **Remove Project**.

**Manual checks**
- [ ] First launch shows the welcome window. **Open Folder…** opens a project window titled with the folder's name.
- [ ] Open the same folder again (⌘O, or a symlinked path): the existing window comes to the front.
- [ ] Gear icon → rename the project. The title changes, and so does File ▸ Open Recent.
- [ ] Add an extra folder; start a session; `ps -o args= -p $(pgrep -n claude)` shows `--add-dir <folder>`.
- [ ] Move or resize the window, quit (⌘Q), and relaunch: same place and size, same selected session, same tab.
- [ ] Rename the project's folder in Finder, then relaunch: the banner appears. **Locate…** fixes it; **Remove Project** closes the window and drops the project from Recent.
- [ ] Close the last project window: the welcome window appears.

---

## M4 — Session index (2026-11-23)

**Fixtures.** Four real sessions were recorded with Claude Code 2.1.330 (haiku, in a throwaway `CLAUDE_CONFIG_DIR`), then sanitized into `tests/fixtures/transcripts/`: a session named with `--name`, one renamed with `/rename`, a plain one, and a `claude -p` one. What they show:
- A custom name (from `--name` or `/rename`) is a `{"type":"custom-title","customTitle":…}` entry. It is written again from time to time, and the last one wins.
- No generated-title entry appeared in these short sessions. The parser still accepts `{"type":"summary"}` and `{"type":"ai-title"}` entries if a version writes them.
- **Non-interactive marker:** user entries from `claude -p` and the SDK carry `"entrypoint":"sdk-cli"` / `"promptSource":"sdk"`; interactive ones carry `"cli"`. So `-p` sessions are hidden by default, with a toggle in Settings.
- A sibling directory `<session-id>/` exists for interactive sessions (for example `custom-title.json`). **Delete** moves it to the Trash too.

**Done**
- `transcripts.rs`:
  - Path encoding (each non-alphanumeric UTF-16 unit becomes `-`). Names over 200 characters are found by prefix and confirmed by a recorded `cwd`.
  - A defensive incremental parser. It re-reads a half-written last line, starts over when a file was rewritten shorter, skips garbage and unknown entries, and skips meta, sidechain, tool-result and slash-command wrapper entries when looking for the first prompt.
  - Display-name precedence: alias → custom name → generated title → first prompt (truncated to 80 characters) → "New session".
- `session_index.rs`: the per-project list with aliases and a parse cache in `sessions/<project-id>.json`, worktree groups, and filtering.
- `watcher.rs`: non-recursive watches on the transcript directories, `projects/` (to see a new directory appear) and `.claude/worktrees`, debounced by 200 ms. **Read access events are ignored.** On Linux, reading transcripts during a refresh triggered another refresh, an endless loop at 15–18% CPU, found while measuring idle CPU in M9. FSEvents doesn't report reads, but the filter keeps it safe everywhere.
- Sidebar:
  - virtualized rows (`uniform_list`), newest activity first, with name, relative time, a status dot, and a multi-line tooltip (id, status, created, last activity, branch, worktree, memory);
  - a filter field (⌘F) matching name and first prompt;
  - an empty state with **New Session**;
  - a footer noting Claude Code's 30-day `cleanupPeriodDays`.

**Manual checks**
- [ ] Open a project you have used with Claude Code. The list matches `claude --resume`'s picker for that folder: same sessions and the same names (except sessions renamed in Anveg Code), newest first.
- [ ] `claude -p "hi"` in that folder: the session appears only after Settings ▸ Claude ▸ **Show non-interactive sessions** is turned on.
- [ ] In a separate Terminal.app window, run `claude` in the same folder and send a prompt: the row appears within about a second, without polling.
- [ ] ⌘F, then type part of a name or of a first prompt: the list filters, and clearing the field restores it.
- [ ] A project with hundreds of sessions scrolls smoothly.

---

## M5 — Running sessions (2026-11-23)

**Done**
- **New** (⌘N, or `+`): an optional name; runs `claude --session-id <uuid> [--name …] [--add-dir …] [--settings <hooks>] [default flags]` in the main folder. The row appears at once, before the transcript exists. A new session that exits before writing anything disappears from the list.
- **Resume** by clicking a row: `claude --resume <id> --add-dir … --settings …`. The extra folders and the hooks are passed again every time, because Claude Code doesn't restore them.
- **Duplicate guard:** before resuming, `claude agents --json` runs once. If the session is open elsewhere, a dialog offers **Open a Copy (Fork)** or Cancel. If `claude agents` fails, the check is skipped.
- **Several sessions at once:** switching is instant; hidden terminals keep parsing but are not drawn.
- **When `claude` exits:** the last screen stays, dimmed, with **Session ended** (and the exit code when non-zero) and **Resume**. A selected session that isn't running shows "This session is not running" with **Resume**.
- **Shortcuts:** ⌘1–⌘9 open the nth session; ⌘⇧] / ⌘⇧[ step through them.
- **Stopping** (context menu, Session ▸ Stop): SIGHUP, then SIGTERM, then SIGKILL to the session's processes (gpui-cn's engine, 250 ms between steps).
- **Verified in the container** with `scripts/fake-claude` under Xvfb:
  - new sessions show their name and appear at the top;
  - resume passes the expected arguments;
  - typing reaches the session without clicking it;
  - the list updates live as transcripts change.

**Bugs found and fixed while testing**
- A resumed session was started hidden and never shown: visibility and focus were not re-applied after the asynchronous duplicate check.
- Focus on a terminal that isn't rendered yet was dropped. Focus is now applied again on the next frame.
- With nothing focused, the window's shortcuts didn't work. Focus now falls back to the session list.

**Manual checks**
- [ ] ⌘N, type a name, press Enter: Claude starts with that name (`/status` in Claude shows it), and the row is at the top.
- [ ] Click an older session: it resumes, with its history visible in Claude.
- [ ] Start two sessions and switch between them a few times: no restart, and each keeps its screen.
- [ ] `/exit` in one: **Session ended** appears with **Resume**; Resume continues the same conversation.
- [ ] Open a session in Terminal.app (`claude --resume <id>`), then click it in Anveg Code: the "open somewhere else" dialog appears. **Open a Copy (Fork)** starts a fork named "Fork of …".
- [ ] ⌘1 / ⌘2 / ⌘⇧] select sessions in list order.
- [ ] Session ▸ Stop: the dot becomes hollow, and `pgrep -fl claude` no longer shows it.

---

## M6 — Terminal tab (2026-11-23)

**Done.** A **Terminal** tab runs the login shell (`-l`) in the main folder, with the captured environment. It starts the first time it is shown. ⌘T toggles between Claude and the last terminal. When the shell exits, **Process exited** appears with **Restart**.

**Manual checks**
- [ ] ⌘T: a shell starts in the project folder (`pwd`), with your usual prompt and `PATH`.
- [ ] `exit`: **Process exited** appears; **Restart** gives a fresh shell.
- [ ] ⌘T twice returns to Claude, and the shell keeps its state.

---

## M7 — Rename, delete, context menu, multi-select (2026-11-23)

**Done**
- **Rename…** (context menu, Session menu): validated (non-empty, one line, at most 100 characters) and stored as an app alias at once. If the session is running, `/rename <name>` is sent, followed by Enter 150 ms later. With hooks working, this waits until the session is **Idle**. The dialog explains the limitation about unsent prompt text.
- **Delete…** (context menu, Session menu, ⌫ in the list): a confirmation that lists the names. Running sessions are stopped, then `<id>.jsonl` and the `<id>/` folder move to the Trash on a background thread. Errors appear as a toast, and a file that is already gone is not an error.
- **Context menu:** Open, Open in Split View, Rename…, Fork Session, Stop (only when running), Copy Session ID(s), Show Transcript in Finder, Delete…. Row tooltips are hidden while the menu is open.
- **Multi-select:** ⌘-click toggles and ⇧-click selects a range. Stop, Delete and Copy IDs apply to the whole selection.
- **Verified with fake-claude:**
  - rename stored the alias, and `/rename Renamed in app` reached the session, which recorded a `custom-title`;
  - delete moved the transcript to the (freedesktop) Trash.

**Manual checks**
- [ ] Right-click a running session → Rename…, type, Enter: the row updates at once. Claude shows "Session renamed to: …" (or does so after its current turn finishes).
- [ ] Rename a session that isn't running: the row updates; resume it later and the name is still there.
- [ ] ⌘-click three rows → right-click → Delete 3 Sessions…: the confirmation lists all three. After Move to Trash, they are in the Finder Trash (`<id>.jsonl` and an `<id>` folder).
- [ ] Select a row in the list and press ⌫: the delete confirmation appears.
- [ ] Copy Session ID, then paste: the UUID. Show Transcript in Finder reveals the `.jsonl` file.

---

## M8 — Close/quit, Settings, menus, resume on launch (2026-11-23)

**Done**
- **Closing a window** (⌘W, or the red button) with running Claude sessions asks first: *Stop N running sessions?*, listing the names, with **Stop and Close** / Cancel and a **Don't show this again** switch (which turns off Settings ▸ General ▸ Confirm…).
- **⌘Q** asks the same, as **Stop and Quit**. Dock ▸ Quit and logout or shutdown never ask: GPUI can't cancel a platform quit, which is what the plan wants for logout.
- **Quit order:** save `state.json`, including the running sessions; stop every child; exit.
- **Settings (⌘,)**: every setting in 6.8 plus Notifications, idle shutdown minutes and memory display. Saved at once. Font and Option-as-Meta changes apply to open terminals too; scrollback and default flags apply to new ones.
- **Menus** as in 6.9, plus Window ▸ Minimize/Zoom.
- **Resume on launch** (off by default) resumes the recorded sessions after the windows appear.

**Bugs found and fixed while testing**
- ⌘W skipped the confirmation.
- ⌘Q didn't show the dialog: GPUI runs global actions while the window is busy, so quitting is now deferred.
- A re-entrancy panic when the menus were rebuilt during a model update.
- gpui-base's dialog turns Enter into `Confirm` and simply closed the dialog; Enter is now routed to the dialog's own action.

**Verified with fake-claude:** the close dialog appears; Stop and Quit leaves no child processes; `state.json` records the running session; with resume-on-launch on, the session comes back by itself after relaunch.

**Manual checks**
- [ ] ⌘W with a running session: the dialog lists it. Cancel keeps the window; **Stop and Close** closes it and the `claude` process is gone.
- [ ] Tick **Don't show this again**, then close another window: no dialog. Settings ▸ General shows the setting off; turn it back on.
- [ ] ⌘Q with sessions in two windows: one dialog lists all of them.
- [ ] Dock icon → Quit: no dialog, and no `claude` processes are left.
- [ ] Settings ▸ General ▸ Resume sessions…: on; quit with two sessions running; relaunch: both come back running.
- [ ] Settings ▸ Claude ▸ Default extra flags `--permission-mode plan`: new sessions start in plan mode. An invalid value (an unclosed quote) shows an error.
- [ ] Settings ▸ Terminal ▸ Font size 15: open terminals change size.

---

## M9 — Hardening and packaging (2026-11-23)

**Done**
- `scripts/bundle-macos.sh` builds `dist/Anveg Code.app`:
  - `Contents/MacOS/anveg-code` with the `anveg-hook` helper next to it, plus an `Info.plist`;
  - an ad-hoc `codesign`;
  - `UNIVERSAL=1` builds a universal binary with `lipo`.
  - **The bundle identifier is the placeholder `dev.anveg.code`. Please confirm the real one** (set `BUNDLE_ID=`).
- `scripts/measure-idle.sh [seconds]` prints idle CPU and RSS. `scripts/check-macos.sh` type-checks the macOS build from Linux.
- `rust-toolchain.toml` pins Rust 1.98.1: GPUI's macOS backend fails to compile on 1.97 (`Option::map_or_default`). The cross-check caught this; a Mac build on 1.97 would have failed.

**Measurements in the Linux container** (Xvfb, software rendering through Mesa llvmpipe, release build). Not representative of macOS/Metal; please re-measure there.

| Target | Result here | Note |
|---|---|---|
| Cold start to first window | **137–169 ms** (release) | Target: well under 1 s. |
| Idle CPU, 1 window, no sessions | **0.45%** (20 s) | Target: about 0%. Before the watcher fix: 15–18%. |
| Idle CPU, 1 window, 2 running sessions + 1 shell | **0.47%** (30 s) | Software rendering; expect lower on Metal. |
| App memory (RSS) | **171–176 MB** | llvmpipe (software GPU) dominates: the bare welcome window is already 160 MB here, so the app's own share is about 15 MB. Measure on macOS for the 100 MB target. |

**Manual checks**
- [ ] `scripts/bundle-macos.sh`, then `open "dist/Anveg Code.app"`: the app starts from Finder or the Dock with your login `PATH` (`claude` is found).
- [ ] With 3 project windows and 5 idle sessions: `scripts/measure-idle.sh 60` reports about 0% CPU and an app RSS under ~100 MB.
- [ ] `~/Library/Logs/Anveg Code/anveg-code.log` → `first window opened elapsed_ms=…` well under 1000 for the bundled app.

---

## M10 — Hooks spike (2026-11-23)

**Test** (Claude Code 2.1.330, a throwaway `CLAUDE_CONFIG_DIR`):
1. A `UserPromptSubmit` and a `Stop` hook went into the config's `settings.json`; they append to a file.
2. A session ran with `--settings ours.json`, which defines hooks for SessionStart, UserPromptSubmit, PreToolUse, Notification, Stop and SessionEnd.

**Result: merged, not replaced.** Both the user's hooks and ours fired. So the `--settings` approach ships, and the user's `~/.claude/settings.json` is never touched.

**Observed hook input:** every event has `session_id`, `transcript_path`, `cwd` and `hook_event_name`. `SessionStart` adds `source` (`startup`, `resume`, `clear`, `compact`, `fork`) and `model`; `SessionEnd` adds `reason`; `Notification` adds `notification_type`.

**Other findings**
- `--fork-session` accepts our own `--session-id`, so the app knows a fork's id up front.
- `--worktree <name>` also accepts `--session-id`. Those sessions are stored under `<encoded main>--claude-worktrees-<name>/`.

---

## M11 — Status (2026-11-23)

**Done**
- `anveg-hook` (`crates/anveg_hook`, std + serde_json only): reads the hook JSON from stdin and sends one line to `$ANVEG_HOOK_SOCKET`. It prints nothing and always exits 0; a run takes about 2 ms.
- The app writes `hooks-settings.json` (every event in 7.1, with the Notification matcher `permission_prompt|idle_prompt|elicitation_dialog`). It passes `--settings` and `ANVEG_HOOK_SOCKET` to every `claude`, plus `ANVEG_TERMINAL_ID`.
- **`/clear` follows the session.** `/clear` starts a new session id in the same process; `ANVEG_TERMINAL_ID` lets the app move the running session to the new id.
- The hook server listens on a Unix socket in `Application Support/Anveg Code/run/hooks.sock`. If that path is longer than a socket allows, it uses a temp-dir socket.
- **Status rules** from 7.1 (unit-tested). The row dot is blue for Working, yellow for Needs input, gray for Idle, and hollow when stopped. The sidebar header shows "N need input". Queued `/rename`s are sent when a session becomes Idle.
- **Verified:** SessionStart, UserPromptSubmit and Stop events arrived from fake-claude through the real `anveg-hook`. The integration test `anveg_hook/tests/round_trip.rs` covers the binary and the socket.
- If `anveg-hook` isn't found next to the app (for example under `cargo run` without a full `cargo build`), status is off and Settings says so. Sessions still work.

**Manual checks**
- [ ] Ask Claude to do something that takes a while: the dot turns blue, then gray when it finishes.
- [ ] Trigger a permission prompt (for example, ask it to run a shell command): the dot turns yellow, and the header shows "1 need input".
- [ ] Your own hooks in `~/.claude/settings.json` still run.

---

## M12 — Notifications and Dock badge (2026-11-23)

**Done**
- When a session goes to **Needs input**, or from **Working** to **Idle**, and it isn't the visible session in the focused window, a notification is posted:
  - title "<project> · <session>", text "Needs your input" or "Finished";
  - clicking it focuses the window and selects the session.
- It uses `UNUserNotificationCenter` through `objc2`, with a delegate class for clicks and for banners while the app is in front. Permission is requested the first time. Under `cargo run` (no bundle id) it only logs.
- **Dock badge:** the total of sessions needing input, cleared at 0.
- Settings ▸ Notifications turns them off.
- The macOS code is type-checked by `scripts/check-macos.sh`, but it has never run, because no Mac is available here.

**Manual checks** (bundled app)
- [ ] The first notification asks for permission.
- [ ] Switch to another session, and let the first one finish a turn: a "Finished" banner. Clicking it brings the window forward and selects that session.
- [ ] A permission prompt in a hidden session: a "Needs your input" banner, and the Dock badge shows 1. Answering it clears the badge.
- [ ] No notification for the session you're looking at.

---

## M13 — Idle shutdown and memory (2026-11-23)

**Done**
- **Idle shutdown:** a session that has been Idle for N minutes (Settings; default 30, 0 = off) and isn't visible is stopped.
  - Each idle, hidden session gets one timer, for its exact deadline, with no periodic checks.
  - Working and Needs input sessions are never stopped.
  - Clicking a stopped session resumes it.
- **Verified with fake-claude:** with the limit at 1 minute, both hidden idle sessions were stopped after 60 s (`stopping idle session` in the log), and the visible one was not.
- **Memory display** (off by default): while the window is active, every 10 s on a background thread, the RSS of each running session's process tree is sampled and shown in the row tooltip.
- **Phase 2 acceptance:**
  - Status changes are pushed through the socket the moment a hook runs.
  - Idle CPU: see M9.
  - Notifications skip the visible session.
  - The user's hooks keep working (M10).

**Manual checks**
- [ ] Settings ▸ Stop idle sessions after 1 minute. Leave a finished session in the background for a minute: its dot becomes hollow. Click it: it resumes.
- [ ] A session that is working, or waiting for permission, is not stopped.
- [ ] Settings ▸ Show memory use: after 10 s, the row tooltip shows "Memory: … MB".

---

## M14–M17 — Phase 3 (2026-11-23)

**M14 · Several terminals.**
- `+` in the tab strip, or ⌘⇧T, adds a shell; each tab has ×.
- Shells start when first shown. After relaunch, the same number of tabs come back as fresh shells.

**M15 · Split view.**
- ⌘\ (or **Split**) shows a second pane: a terminal next to Claude, or the selected session next to a terminal.
- Drag a session from the sidebar onto either pane, or use the context menu's **Open in Split View**.
- The divider is resizable, and the split and its ratio are saved in `state.json`.

**M16 · Fork.**
- **Fork Session** (context menu, Session menu) runs `claude --resume <id> --fork-session --session-id <new>`, so the row appears at once.
- It gets the alias "Fork of <name>", because a fork copies the original's custom title.

**M17 · Worktrees.**
- **New Session in Worktree…** (File menu, ⌘⇧N; offered only for git repositories) runs `claude --worktree <name> --session-id …` in the main folder.
- The sidebar groups sessions by worktree under collapsible headers.
- Resuming or forking a worktree session runs in its worktree folder.
- `SessionIndex` reads and watches the worktrees' transcript directories.

**Verified with fake-claude:**
- the split showed Claude and a shell side by side;
- the fork launched with the expected arguments and appeared at the top;
- dragging a sidebar row onto the right pane resumed that session there;
- a pane never shows the same terminal or session as the other: showing the Terminal tab while the split shows that terminal moves the split to the selected session (fixed after testing);
- the worktree session created `.claude/worktrees/feature-x`, was grouped under "Worktree: feature-x", and its transcript landed in the worktree's encoded directory.

**Manual checks**
- [ ] ⌘⇧T twice: tabs "Terminal 1/2/3". Close one with ×. Relaunch: the same number of tabs.
- [ ] ⌘\ : Claude and a shell side by side. Drag the divider, then relaunch: the same split.
- [ ] Drag a session row onto the right pane: that session shows there, resumed if needed.
- [ ] Fork Session: a new row "Fork of …" with the same history (scroll up in Claude).
- [ ] In a git project, ⌘⇧N → name `feature-x`: Claude starts in `.claude/worktrees/feature-x` (`pwd` in a `!` shell command). The row sits under "Worktree: feature-x". Collapse and expand the group.

---

## Known limitations (all phases)

- **Nothing has run on macOS yet.** Everything was built and type-checked for `aarch64-apple-darwin`, and the UI was exercised on Linux under Xvfb. The macOS-specific parts (notifications, Dock badge, Metal rendering, FSEvents, Finder Trash, the bundle) are untested.
- **Relative times** ("5 min ago") refresh when the list changes, not on a timer. That's deliberate, for idle CPU.
- **Stopping** escalates faster than the plan's 3 s (250 ms per signal). That is gpui-cn's engine; see M1.
- **Linux quirk** (not a supported target): GPUI on Linux quits when the last window closes, so the welcome window only appears after closing the last project window on macOS.

---

## Change after Mac testing — new sessions start without asking for a name (2026-11-23)

The owner tested on a MacBook (everything worked) and asked that ⌘N not ask for a name. The name should come from Claude Code if possible, and from the first prompt otherwise.

**Can the name come from Claude Code?** Investigated with Claude Code 2.1.330:
- Claude Code can write an AI-generated title, as a `{"type":"ai-title","aiTitle":…}` transcript entry (`saveAiGeneratedTitle`). In this version that only happens for sessions synced to claude.ai (Remote Control / bridge `auto_title`) or when an SDK host sends a `generate_session_title` control request.
- A plain interactive session got **no** `ai-title`, even after two full turns with a realistic prompt (tested in a throwaway config).
- A `UserPromptSubmit` or `SessionStart` hook can also return `hookSpecificOutput.sessionTitle` to set the title. That would only push a name we computed ourselves into Claude Code, and it could overwrite a name the user set with `/rename`, so it is not used.

**Result**
- ⌘N (and the `+` buttons) start `claude --session-id <uuid>` at once, with no dialog and no `--name`.
- The row reads **"New session"** until the first prompt reaches the transcript (about a second, through the watcher). From then on it shows the first prompt, truncated to 80 characters.
- The precedence is unchanged: app alias → custom name (`/rename`, `--name`) → generated title (`ai-title`/`summary`; used automatically if Claude Code ever writes one for local sessions) → first prompt → "New session".
- **New Session in Worktree…** (⌘⇧N) now asks only for the worktree's name.
- Verified with fake-claude under Xvfb: ⌘N started the session without a dialog, and after the first prompt the row showed that prompt.

**Manual checks**
- [ ] ⌘N: Claude starts at once, and the row reads "New session".
- [ ] Send a first prompt: within about a second the row shows the prompt (truncated).
- [ ] `/rename Something` in Claude: the row switches to "Something".
- [ ] ⌘⇧N in a git project: the dialog asks only for the worktree name.

---

# Phases 4–6 (`docs/next-phases.md`)

The owner asked for Phases 4–6 only. **Phase 7 (per-session change review) is postponed**; its full specification is in `docs/TODO.md`.

All of it was built and exercised on Linux under Xvfb (with `scripts/fake-claude`, `scripts/fake-gh` and `scripts/fake-lsp`), and type-checked for macOS with `scripts/check-macos.sh`. None of it has run on a Mac yet.

## Decisions for Phases 4–6 (2026-11-23)

1. **gpui-component is enabled** for the code editor (`EditorState` + `Editor`), the diff view (`Diff`) and Markdown (`TextView`). See decision 1 at the top. License: 0BSD added for `enum-iterator`, **approved by the owner**.
2. **Right-side tabs are our own tab strip, not gpui-component's dock.** The Phase 3 panes (Claude, terminals, split view, drag a session into the split) already work and were tested on a Mac; replacing them with the dock would have meant two pane systems or a rewrite. Document tabs (files, diffs, commits, pull requests, CI runs, logs, the new-PR form) join the same strip, can be dragged into the split pane, and scroll sideways when there are many.
3. **The GitHub tab is always shown.** When `gh` is missing or signed out it shows only the setup steps (`brew install gh`, `gh auth login`), as 6.1 describes, with a button that runs `gh auth login` in a terminal tab and a "Check Again" button.
4. **Hunk buttons** (Stage / Unstage / Discard Hunk) sit just below each hunk, as gpui-component diff annotations. File buttons (Stage File, Unstage File, Discard…, Delete…) are in each file's header.
5. **Prompts** from panels and tabs share one dialog (`workspace::Prompt`), so every confirmation, name field and error message looks the same.
6. **"Ask Claude" mention syntax:** Claude Code 2.1.331 inserts `@path#L10 ` or `@path#L10-20 ` for an IDE selection (found in its bundle), so the app types exactly that.
7. **Typing into Claude** uses a paste (bracketed when Claude asks for it), so it can never submit. It goes to the session selected in the sidebar if it is running; otherwise a toast asks you to select one.

---

## M19 — Layout: sidebar tabs, document tabs, lazy restore (2026-11-23)

**Done**
- Sidebar tab strip **Sessions | Files | Git | GitHub** (⌘⇧1–4, View menu). Panels are created the first time they are shown.
- Document tabs on the right, after the Claude and Terminal tabs. A dirty file shows `●`. ⌘⇧W or × closes a tab (asks when a file has unsaved changes). Drag a document tab onto the split pane to show it there.
- `state.json` keeps the sidebar tab, the open document tabs, the shown one, the split pane's document, and the expanded folders. **Restore is lazy:** tabs come back as placeholders and load when first shown; GitHub tabs restored at launch wait until `gh` detection has finished.
- Closing a window or quitting with unsaved files asks: Save All / Don't Save / Cancel.

**Manual checks**
- [ ] Switch between the four sidebar tabs; ⌘⇧1–4 do the same.
- [ ] Open three files, a diff and a PR; quit and relaunch: the same tabs, the same one shown, and only that one loads (no `gh` call for hidden PR tabs).
- [ ] Drag a file tab onto the split pane.
- [ ] Edit a file, then ⌘W: the "Save changes?" dialog; Cancel keeps the window open.

## M20 — File tree and `FsWatchService` (2026-11-23)

**Done**
- `anveg_core::fswatch::FsWatchService`: one shared watcher, reference-counted subscriptions by path, debounced batches (150 ms). A file subscription watches its folder and reports only that file. Tested, including a 200-file burst arriving as a handful of batches.
- `anveg_core::files`: lazy one-level listing with `.gitignore`, `.ignore` and global excludes (`ignore` crate), `.git` hidden, ignored entries marked when "show ignored" is on.
- Files tab: roots (main and extra folders), folders listed only when expanded, **only expanded folders watched, and only while the tab is visible**. Git status colours and letters on files, a colour on folders with changes.
- Context menu: Open, Mention in Claude, New File…, New Folder…, Rename…, Reveal in Finder, Copy Path, Copy Relative Path, Move to Trash… (with confirmation).
- Quick open (⌘P, File ▸ Go to File…): fuzzy (`nucleo-matcher`), respects ignore files. The index is built on first use and reused until a file under the roots changes.

**Manual checks**
- [ ] Expand folders in a large repository: instant, and `target/`/`node_modules/` are hidden until the eye button is on.
- [ ] Ask Claude to create, rename and delete files: the tree follows within about a second.
- [ ] Right-click ▸ Mention in Claude with a running session: `@path ` appears in Claude's prompt, nothing is sent.
- [ ] ⌘P, type a few letters of a file name, ↓/↑, Enter: it opens. Esc closes.

## M21 — Editor without LSP (2026-11-23)

**Done**
- gpui-component's code editor: tree-sitter highlighting (Rust, TypeScript/TSX, JavaScript, Python, Go, JSON, TOML, YAML, Markdown, HTML, CSS, Bash, Swift, C, C++, diff), line numbers, find and replace (⌘F), go to line (⌘L, `line` or `line:col`), soft wrap (Wrap button, ⌥Z, and a Settings default), extra cursors with ⌥-click.
- Settings ▸ Editor ▸ Extra file types maps more extensions to these languages.
- Line endings (LF/CRLF), the trailing newline (or its absence) and the indentation style (tabs or 2/4/8 spaces) are detected and kept. ⌘S saves atomically; File ▸ Save All (⌘⌥S).
- Binary and non-UTF-8 files open read-only with a notice. Over 200,000 lines or 20 MB: read-only plain text with a warning.
- Markdown files have a Preview toggle (gpui-component's Markdown renderer).

**Manual checks**
- [ ] Open a 10,000-line file: it appears at once (target under 200 ms).
- [ ] Open a CRLF file, edit, save: still CRLF (`file` or `git diff` shows no line-ending change). Same for a file without a final newline.
- [ ] Open an image or a Latin-1 file: read-only notice.
- [ ] README.md ▸ Preview.

## M22 — External changes, terminal links, Mention / Ask Claude (2026-11-23)

**Done**
- Each open file is watched. A clean buffer reloads silently, keeping cursor and scroll. A dirty buffer shows **"This file changed on disk"** with Reload, Keep Mine and Compare (Compare opens a diff of disk ↔ your buffer). A deleted file says so. The app's own saves don't trigger the banner.
- ⌘-click on `path`, `path:line` or `path:line:col` in any terminal (Claude or a shell) opens the file there. Paths resolve against the terminal's working directory (OSC 7), then the project's folders; URLs still open in the browser.
- **Ask Claude** (button, ⌘⇧L, Edit menu) types `@path#L10-20 ` for the selected lines (or the cursor's line).

**Manual checks**
- [ ] With a file open and unchanged, have Claude edit it: the editor updates within a second, cursor kept.
- [ ] Type in the file, then have Claude edit it: the banner; try Reload, Keep Mine and Compare.
- [ ] In Claude's output, ⌘-click a `src/x.rs:12:5` reference: the file opens at that line.
- [ ] Select lines 3–5, ⌘⇧L: `@src/x.rs#L3-5 ` appears in Claude's prompt, not sent.

## M23 — LSP (2026-11-23)

**Done**
- New crate **`anveg_lsp`**: JSON-RPC over stdio with `lsp-types`; requests return futures (any executor); server-to-client requests get default answers (`workspace/configuration` → nulls); notifications go to a callback. URI and UTF-16 position helpers, incremental change computation. Tested against `scripts/fake-lsp` (python3).
- `lsp_manager.rs`: one server per (project, server command); the project's folders are the workspace folders. A server **starts when the first file in its language opens** and gets `shutdown`/`exit` **10 minutes after the last one closes**. After a crash it restarts with backoff (1, 2, 4 s), at most 3 times.
- Sync: `didOpen`, incremental `didChange`, `didSave`, `didClose`; Claude's edits to files that are not open go out as `workspace/didChangeWatchedFiles` (build and VCS folders filtered).
- Diagnostics (underlines, gutter, a "N problems" list in the status bar that jumps to the line), completion, hover, go to definition (opens other files), Format Document (⌥⇧F), format on save (Settings, default off).
- Settings ▸ Language servers: commands for Rust, TypeScript/TSX, JavaScript, Python and Go (defaults from the plan). A missing server shows an install hint in the editor's status bar.

**Manual checks**
- [ ] With rust-analyzer installed, open a `.rs` file: diagnostics, completion, hover and ⌘-click go to definition work.
- [ ] `ps` shows no language server before opening a file; close all `.rs` files and wait 10 minutes: rust-analyzer exits.
- [ ] Kill rust-analyzer (`kill`): it restarts; kill it 4 times: the status bar says it crashed too often.
- [ ] Turn on format on save; save a badly formatted file: it is formatted.

## M24 — `ProcessRunner`, repositories, status (2026-11-23)

**Done**
- `anveg_core::process`: runs a command with the captured environment and a working directory, with a timeout, cancellation, stdout/stderr capture, stdin, stderr progress lines and JSON decoding. **Commands that change a repository take that repository's lock**, so they never overlap (tested).
- `anveg_core::git`: everything through the `git` CLI. Reads run with `GIT_OPTIONAL_LOCKS=0` (so `git status` never rewrites the index and wakes the watcher); every command with `GIT_TERMINAL_PROMPT=0`.
- Repositories found per project folder (`rev-parse --show-toplevel`); several repositories get a switcher at the top of the Git tab; linked worktrees are listed with their branches.
- `git status --porcelain=v2 -z --branch`: Conflicts, Staged, Changes, Untracked, the branch, and ↑ahead ↓behind / "not published".
- Refreshes: index, HEAD and refs changes; working-tree changes (300 ms after the last one); window focus. **Only while the Files, Git or GitHub tab is visible. No polling.**

**Manual checks**
- [ ] In a 10,000-file repository, edit a file: the Git tab updates within about 300 ms (`git status refreshed ms=…` in the log at debug level).
- [ ] Switch to the Sessions tab and leave the app idle: no `git` processes (`ps`, or `sudo fs_usage`).

## M25 — `DiffView` and file diffs (2026-11-23)

**Done**
- `diff_view.rs` on gpui-component's virtualized diff: unified, syntax-highlighted, hunk headers, per-hunk buttons. Above 5,000 lines: "Large diff — Show anyway".
- Clicking a file in the Git tab opens `git diff [--cached]`; untracked files show as all-new; binary files show "Binary file changed". Clicking a line on the new side opens the file there.

## M26 — Staging, hunks, discard, commit (2026-11-23)

**Done**
- Stage/unstage files (`git add`, `git restore --staged`, with `git rm --cached` before the first commit), Stage All / Unstage All per section.
- Hunk staging builds a one-hunk patch and runs `git apply --cached` (`-R` to unstage). **Tested on CRLF files and on files without a final newline.**
- Discard (file or hunk) always asks, and **copies the current file to the Trash first**; untracked files are moved to the Trash.
- Commit box: summary (hint past 72 characters), description, Amend (pre-fills the last message), Commit (⌘↩), and **Stage All and Commit** when nothing is staged. Hook output (pre-commit…) is shown when a commit fails. Signing follows the user's git config.

**Manual checks**
- [ ] Stage one hunk of a CRLF file; `git diff --cached` shows just that hunk.
- [ ] Discard a hunk: the dialog, then the change is gone and a copy of the file is in the Trash.
- [ ] Commit with a failing pre-commit hook: the hook's output appears.

## M27 — Branches and remotes (2026-11-23)

**Done**
- Branches view: local and remote branches with their last commit. Click to check out (git's own message if changes block it; a remote branch checks out a tracking branch). New Branch… (from the current branch or any listed one), Rename…, Delete… — a branch with unmerged commits needs a second, explicit "Delete Anyway".
- Fetch, Pull (fast-forward only by default; Settings ▸ Git ▸ Pull: Merge or Rebase), Push (the first push publishes the branch with `--set-upstream`). Progress line from git's stderr and a Cancel button. Authentication failures explain how to set up a credential helper or use the Terminal tab.

**Manual checks**
- [ ] Push a new branch: it is published (`not published` disappears).
- [ ] Pull with diverged branches in fast-forward-only mode: git's refusal is shown.
- [ ] Fetch from a remote that needs a password without a credential helper: the explanation, and no hang.

## M28 — History and stash (2026-11-23)

**Done**
- History view: the current branch's last 200 commits; a click opens the commit's diff as a tab.
- Stash view: Stash Changes… (with a message, includes untracked files), Apply, Pop, Drop (asks).

---

## M29 — `gh` detection and PR list (2026-11-23)

**Done**
- `anveg_core::gh`: every call uses `--json` with explicit fields and `serde` with optional fields; rate limiting is recognised. `scripts/fake-gh` answers with fixtures for manual tests and logs every call (`FAKE_GH_LOG`).
- `gh --version` and `gh auth status` run after the login environment is captured, and again on window focus (at most every 30 s) until `gh` is ready. Enterprise hosts work through `gh`'s own config.
- GitHub tab: the repository (`gh repo view`), pull requests with filters **Mine / Review Requested / All Open** (number, title, author, branch, draft, review state, a checks icon), and the current branch's CI runs.

## M30 — PR detail and actions (2026-11-23)

**Done**
- A PR opens as a tab: title, author, branches, state, size; **Overview** (Markdown description, checks with status, reviews and the review decision, read-only comments) and **Files Changed** (`gh pr diff` in a `DiffView`).
- Open in Browser, Copy Link, Check Out (warns first when the working tree has changes), Mark Ready for Review (drafts), Merge… (squash, merge or rebase; GitHub's message when branch protection refuses), Close… (asks).

## M31 — Create PR and CI runs (2026-11-23)

**Done**
- New Pull Request (GitHub tab): a form tab with base (the repository's default branch), title and body pre-filled from the branch's commits, and Draft. It pushes the branch first when needed (publishing it if it has no upstream), runs `gh pr create`, then opens the new PR.
- CI runs: the latest run's status also shows in the Git tab header. A run opens as a tab with its jobs; **Show Failed Logs** (`gh run view --log-failed`, a read-only, searchable text tab), **Re-run Failed Jobs**, **Open in Browser**.

## M32 — Claude integrations and refresh rules (2026-11-23)

**Done**
- **Ask Claude to Fix** (failed checks in a PR, failed runs): saves the last 300 lines of the failed log to a temporary file and types a prompt naming the PR or run and the check, with `@<log file>`, into the active Claude session. **Never presses Enter.**
- **Open Linked Claude Sessions** (PR tab): runs `claude --from-pr <number>` in a new terminal tab.
- Refresh: when the GitHub tab is shown, on window focus, on ⟳, and **every 60 s only while the tab is visible and its window has focus**. Rate limits double the interval (up to 16×). Hidden: nothing runs (checked with the fake `gh` call log: no calls until the tab was shown; hidden PR tabs restored at launch made no calls).

**Manual checks (Phase 6)**
- [ ] Without `gh`: the GitHub tab shows the install and sign-in steps; after `brew install gh` + `gh auth login`, Check Again shows your PRs.
- [ ] PR list loads in under 2 s for a typical repository; switch filters.
- [ ] Open a PR: description, checks, reviews, comments; Files Changed shows the diff.
- [ ] Check Out with local changes: the warning first.
- [ ] Merge on a protected branch: GitHub's refusal is shown.
- [ ] New Pull Request from an unpublished branch: it is pushed, the PR is created and opens.
- [ ] A failed run: Show Failed Logs; Re-run Failed Jobs; Ask Claude to Fix types the prompt (not sent).
- [ ] Leave the Sessions tab shown for a few minutes: no `gh` processes.

---

## Phase 4–6 verification on Linux (2026-11-23)

Exercised under Xvfb in a demo repository (fake Claude, fake gh, fake language server):
- Files tab: ignore rules (`target/` hidden), git colours, context menu, Mention in Claude typed `@README.md ` into the session.
- Editor: highlighting, typing, dirty marker, completion popup, hover, a diagnostic and "1 problem", external change while dirty → banner → Compare and Reload, ⌘S keeping the trailing newline.
- Git tab: sections, a hunk staged from the diff, commit with ⌘↩ from the summary field, History → commit diff, New Branch… through the prompt.
- GitHub tab: PR list and runs, PR overview and Files Changed, failed log tab; restart → lazy restore.
- ⌘-click on `src/main.rs:6:4` in a shell opened the file at line 6, column 4.
- ⌘P quick open.
- Discard Hunk: the dialog, then the hunk was gone and a copy of the file was in the Trash.
- Release build: a 10,000-line Rust file reached its first frame **37 ms** after the click (target: under 200 ms). The log line `file opened (first frame) … ms=` measures it on a Mac.
- Release build, Git tab and a diff open, 30 s idle: **0.57 % CPU, no `git` or `gh` process**, one status refresh in total. RSS was 185 MB, mostly Mesa's software renderer (llvmpipe) under Xvfb; measure on a Mac before drawing conclusions.

**Bugs found and fixed while testing**
- The terminal's bounds were measured from a canvas laid out below the grid, so ⌘-click missed every path.
- Open git diff tabs did not update when files changed outside the app; they now reload whenever the Git status refreshes.
- GitHub tabs restored at launch were created before `gh` detection finished and never loaded; they now load when `gh` becomes ready. The GitHub tab's first refresh could also run before the branch was known (no CI runs); a refresh asked for while one runs now runs again afterwards.
- New document tabs could open off-screen in a long tab strip; the strip now scrolls and keeps the shown tab in view.

**Notes**
- In a dialog whose only action is destructive (Discard, Delete, Drop), Enter closes it without doing anything; the action needs a click.
- Git diff tabs follow outside changes only while the Files, Git or GitHub tab is shown (that is when git status is watched). Otherwise they update on the next git operation or when one of those tabs is shown again.
- `scripts/check-macos.sh` now needs `zig` on Linux (for the grammars' C code); it passes.

---

## Notes panel (owner's request, 2026-11-24)

**Done**
- A **toolbar on the right edge** of the window. Its first button (also ⌘⌥N, View ▸ Notes) shows or hides the **Notes panel**, which sits beside the right side's tabs and can be resized. Open/closed and the width are kept per window.
- Tabs: **Notes** (the default), the user's own tabs (`+`, for example "Milestone 3"; Rename Tab… and Delete Tab… — a deleted tab's notes move to Notes), and **Archive**.
- Every tab except Archive always ends with an **empty note**. Typing into it makes it a note (focus and cursor stay) and a new empty note appears below. Notes left empty are dropped the next time the file is loaded.
- Each note has **Send** and **New Session**:
  - **Send** pastes the note into the session selected in the sidebar (if it is running) and **presses Enter** 150 ms later. With no running session selected, a message says so and the note stays.
  - **New Session** starts `claude --session-id <id> … -- "<note>"`, so the note is the session's first message (checked with Claude Code: multi-line text and text starting with `-` both work). The session is named after it, as with any first prompt.
  - Either way the note moves to the **Archive**, which shows where it went ("Sent to “…”" / "Started session “…”", when, how many times). From the Archive a note can be sent again, Restored to the tab it came from, or deleted.
- Move… moves a note to another tab; deleting a note with text asks first.
- **Storage:** `~/Library/Application Support/Anveg Code/notes/<project-id>.json` (tabs and notes, written atomically 400 ms after the last keystroke, and at once when the window closes or the app quits). **Nothing is written in the project folder.** `anveg_core::notes` has the model, with tests.
- `scripts/fake-claude` accepts `-- <prompt>` and treats it as the first message.

**Verified under Xvfb:** writing notes (saved as typed), New Session with a two-line note (the fake Claude received it as its first message), Send into the running session (pasted and submitted), the Archive entries, a "Milestone 3" tab, restart (tabs, notes and the open panel came back), Send with no running session (message, note kept), and the always-present empty note.

**Manual checks**
- [ ] ⌘⌥N or the toolbar button: the panel opens and closes; drag its edge; relaunch: same state and width.
- [ ] Type in the empty note: a new empty one appears below.
- [ ] Select a running session, Send: the text appears in Claude's prompt and is submitted; the note is in Archive.
- [ ] New Session on a multi-line note: a new session starts with it as the first message.
- [ ] `+` ▸ "Milestone 3"; add notes; Move… one to Notes; Delete Tab…: its notes land in Notes.
- [ ] Archive ▸ Restore: the note goes back to its tab.

---

## Change: a clearer selected session (2026-11-24)

The owner found the selected session hard to see, especially in dark mode: the row used the theme's "secondary" surface, barely lighter than the dark sidebar.

**Done**
- The selected row (sessions list, file tree, quick open) is tinted with the theme's accent blue (30 % in dark mode, 16 % in light), and the selected session also gets a solid blue bar on its left edge and a medium-weight title. Hovering other rows is unchanged.
- Debug builds take `ANVEG_THEME=dark` or `ANVEG_THEME=light` to force the app theme, for checking on machines without a system appearance (Linux under Xvfb). Release builds ignore it.

**Manual checks**
- [ ] Dark mode: the selected session is obvious at a glance; ⌘⇧] / ⌘⇧[ move it.
- [ ] Light mode: the same.
- [ ] Multi-select with ⌘-click: every selected row is highlighted.

---

## Toolbar commands (owner's request, 2026-11-24)

**Done**
- Per-project **toolbar commands**: a name, a shell command, an optional shortcut, "Show in toolbar", and "New terminal each time". Stored with the project in `projects.json` (app data folder; nothing in the project).
- Their **buttons** sit in the top bar, right of the tabs, in the order set in the editor; a gear button (and File ▸ Toolbar Commands…) opens the editor as a tab: add, edit (saved on Enter or a moment after you stop typing), Run, reorder (↑/↓), show/hide in the toolbar, delete (asks).
- **Running a command** types it into an interactive login shell in the project's main folder, in a terminal tab named after the command:
  - by default each command has **its own tab** and reuses it: if the last run is still going (a program other than the shell is in the foreground), it sends **Ctrl-C** first, then runs again; if the tab's shell has exited, a new shell starts in it;
  - with **New terminal each time**, every run opens a new tab.
  - Command tabs are not restored after a restart (the next run makes them again).
- **Shortcuts:** typed like `cmd-shift-b` (`Cmd+Shift+B` is accepted too) or a function key (`f5`). They must use ⌘ or be a function key (every other key belongs to the terminal and Claude) and must not be one the app already uses; mistakes are explained under the field. Hidden commands still run from their shortcut. A shortcut only runs the command of the window's own project; elsewhere the key passes through.

**Verified under Xvfb:** adding a command in the editor (it appeared in the toolbar at once), an invalid shortcut (`ctrl-b`, explained), Build from the toolbar in its own tab, ⌘⇧B while it was running (`^C`, then a fresh run in the same tab), hiding Build (its button left; ⌘⇧B still ran it), and "New terminal each time" (two Hello tabs).

**Manual checks**
- [ ] Toolbar Commands: add "Build" = `cargo build`, shortcut `cmd-shift-b`; a ▷ Build button appears.
- [ ] Click it twice during a long build: the first run is stopped and it starts again in the same tab.
- [ ] Add "Run" with New terminal each time: each click opens a new tab.
- [ ] Turn off Show in toolbar: the button goes; the shortcut still works.
- [ ] Two projects with the same shortcut: each window runs its own command.

---

## Change: full-screen windows come back full screen, as tabs (2026-11-24)

The owner reported that windows closed in full screen reopened as screen-sized windows. `state.json` only knew "maximized", which a full-screen window was saved as.

**Done**
- `SavedBounds.fullscreen` (old files: `false`). A full-screen window saves its windowed frame plus `fullscreen: true`, and reopens with `WindowBounds::Fullscreen` (GPUI enters full screen right after the window opens).
- Project windows share a macOS **tab group** (`tabbingIdentifier` `dev.anveg.code.project`). With System Settings ▸ Desktop & Dock ▸ *Prefer tabs when opening documents* on "In Full Screen" (the default) or "Always", a project opened while the app is full screen (⌘O, Open Recent, the welcome window) becomes a tab of the full-screen window.
- **Restore on launch:** windowed windows open first. The first full-screen window opens full screen; once it is (up to 4 s), the other full-screen windows open at their frames and macOS adds them as its tabs, which puts them in full screen too. If tabs are set to "Never" (`AppleWindowTabbingMode` = `manual`), each full-screen window gets its own space instead. If the first never reaches full screen, the rest open full screen on their own.
- Tab order after a restart follows the order the windows were opened, not necessarily the order of the tabs when you quit.

**Not verified:** none of this can run on Linux (no full screen or window tabs under Xvfb). Checked: it type-checks for macOS, and windowed restore still works on Linux.

**Manual checks (Mac)**
- [ ] Put a project window in full screen, quit, relaunch: it opens in full screen.
- [ ] With two project windows tabbed in full screen, quit and relaunch: one full-screen window with both tabs.
- [ ] While in full screen, ⌘O another folder: it opens as a new tab.
- [ ] System Settings ▸ Prefer tabs: Never, two full-screen windows, relaunch: two full-screen spaces.
- [ ] A normal (windowed) window still reopens at its size and place.

---

## App icon (2026-11-24)

The owner picked option D, "Session Orbit", from six concepts on a Claude Design canvas: a prompt in the centre with three sessions orbiting it, coloured like the sidebar's status dots (blue working, amber needs input, grey idle), on a dark navy rounded square.

**Done**
- `assets/icon/AppIcon.svg` (source, on the macOS 824/1024 grid) and `assets/icon/AppIcon.icns` (16–1024 px, PNG entries, 11 sizes).
- `scripts/bundle-macos.sh` copies the icon into `Contents/Resources` and sets `CFBundleIconFile`; `scripts/install-macos.sh` touches the installed app so Finder and the Dock reload the icon.
- `scripts/render-icon.js` (Playwright's Chromium) renders the SVG at every size; `scripts/make-icns.py` packs the PNGs into the `.icns` without Apple tools.

**Manual checks (Mac)**
- [ ] `scripts/install-macos.sh --open`: the Dock, Finder, Launchpad and ⌘-Tab show the new icon.
- [ ] If an old generic icon sticks in the Dock, remove the app from the Dock and add it again (macOS caches icons).

---

## Session bar (owner's request, 2026-11-24)

A bar above every Claude session (main and split pane). The owner chose: compact next to the context meter, not in the ⋯ menu; buttons disabled when the session can't take them (except the few that work mid-turn, such as /btw); the context display can be turned off in Settings; and buttons of their own.

**Done**
- **Bar:** Model (shows the current model, sends `/model`), Effort (shows the level, `/effort`), the **context meter** (bar and %, amber from 70 %, red from 85 %, tokens in its tooltip) right next to **Compact** (`/compact`), **Stop** (Esc: interrupts a turn or dismisses a dialog), **Rewind** (`/rewind`), **Clear** (`/clear`, asks first), a **⋯ menu** (/context, /usage, /status, /memory, /init, /recap, /export, /plan, Side Question… = `/btw` with a question you type, Edit Session Buttons…), your **own buttons**, and a gear that opens their editor. Not shown when the session isn't running.
- **What is usable:** waiting for a prompt: everything. Working: Stop, /btw and own buttons marked "Usable while Claude is working". A permission or question dialog open: only Stop (typing would answer it). Ended: nothing.
- **Sending:** a one-line command is typed, a multi-line one pasted; Enter follows 150 ms later (as for /rename). The keyboard then goes to the session, so pickers such as /model can be driven at once.
- **Model, effort and context come from Claude Code's status line.** For sessions the app starts, its `--settings` file (in the app's data folder) sets `statusLine` to `anveg-hook statusline`, which forwards model, effort and context-window numbers to the app over the hook socket and then runs **your own** status line command (project `.claude/settings.local.json`, then `.claude/settings.json`, then `~/.claude/settings.json`) with the same input, so your terminal status line still shows. Nothing in your Claude Code configuration is changed.
- **Setting:** Settings ▸ Status and resources ▸ "Context, model and effort above sessions" (on by default; applies to sessions started afterwards). Off, the app does not set a status line; the meter is then **estimated** ("≈") from the transcript's last reply (input + cache tokens against 200k, or 1M once past 200k), and the model comes from the transcript.
- **Own buttons** (Session Buttons tab; also from the ⋯ menu): label, the text to send, "Usable while Claude is working", reorder, delete (asks). App-wide, in `settings.json`.
- Extra Lucide icons (brain, fold-vertical, circle-stop, eraser) through a small asset source (`app_assets.rs`) on top of gpui-kit's default set.

**Fixed on the way**
- Prompt buttons ran inside the window's own update, so an action that updated the window panicked ("already being updated"); this also affected "Don't Save" in the unsaved-files prompt. They now run deferred on the window.
- Text fields in the Toolbar Commands editor (and the new Session Buttons editor) were meant to save when they lose focus, but the input never reports that here; they now also save 600 ms after typing stops.

**Verified under Xvfb** (fake-claude now runs the status line command: +30k tokens per prompt, `/compact` → 10k, `/effort X`): the bar with "Fake 1", "Effort: high", 0 % → 30 % after two prompts → 5 % after Compact; Effort changing to "max"; amber at 75 % (dark theme); Clear asking first; the ⋯ menu; Side Question sending `/btw …`; adding two own buttons (both appear; one sends its text); the setting off: "≈25 %" from the transcript, the model from the transcript, the plain hooks file used.

**Manual checks**
- [ ] Start a session: Model and Effort show the real model and level after the first reply; the meter shows the same % as `/context`.
- [ ] Click Model: the /model picker opens and takes the arrow keys at once.
- [ ] During a long turn: only Stop, Side Question and "while working" buttons are enabled; Stop interrupts.
- [ ] On a permission prompt: only Stop is enabled.
- [ ] Compact: the meter drops after compaction.
- [ ] With your own status line in `~/.claude/settings.json`: it still shows at the bottom of Claude's UI.
- [ ] Settings: turn the status line off, start a new session: the meter shows "≈".

---

## Change: window tabs in full screen, no slide-down title bar (2026-11-25)

The owner reported two full-screen problems on their MacBook: (1) the other project windows' tabs could not be seen, so switching was only possible from the Window menu; (2) moving the pointer to the top slid macOS's title bar (window buttons) down over the app's own top rows. Expected, as in IntelliJ IDEA: tabs on top for switching windows, and nothing covering the UI. Asked for the window tabs row to sit above the Sessions/Files row.

**Done**
- **Window tabs row** (`window_tabs.rs`): in full screen, a full-width row at the very top of each project window, above the Sessions/Files row and the Claude/Terminal row. One tab per open project window (name, the folder in its tooltip, an amber dot when one of its sessions needs input, × to close that window); the current window's tab is marked. Order: the windows in this window's macOS tab group first, in tab order, then any other project windows (in another full-screen space, or not full screen) in opening order; clicking one brings that window forward (switching space if needed). On the right: **Exit Full Screen** (also ⌃⌘F; replaced by the green window button, see the follow-up). Not shown outside full screen.
- In full screen the sidebar's empty title bar (room for the window buttons) is dropped, so Sessions/Files sits on one line with Claude/Terminal.
- **The slide-down title bar is hidden in full screen** (macOS): the app observes `NSWindowDidEnterFullScreenNotification` and hides the window's title bar view (the view holding the window buttons and the native tab bar), and shows it again on `NSWindowWillExitFullScreenNotification`, before the exit animation. Revealing the menu bar then shows only the menu bar. The window buttons' jobs are covered by the row (exit, close) and the menus.
- `ModelEvent::WindowsChanged` (a window opened or closed) and `AttentionChanged` redraw the rows in every window.
- Debug builds: `ANVEG_WINDOW_TABS=1` shows the row outside full screen too.

**Fixed on the way:** the row read its own window through the model while drawing it, which panicked; it now reads itself directly.

**Verified on Linux** (Xvfb with the Openbox window manager, two project windows): no row in a normal window; the row appears when the window goes full screen; Sessions/Files lines up with Claude/Terminal; clicking the other window's tab brings it forward and its row marks itself; × closes the other window and the row updates; Exit Full Screen leaves full screen.

**Not verified:** the macOS parts (hiding the title bar, tab-group order, switching between tabs of one full-screen window) can only run on a Mac. They type-check for macOS. If the title bar still slides down, the hiding did not take; the tabs row works either way.

**Manual checks (Mac)**
- [ ] Two project windows, one full screen, ⌘O the other folder: both are tabs; the row at the top shows both; clicking switches without leaving full screen.
- [ ] Move the pointer to the top edge: only the menu bar appears; the row and the Sessions/Claude rows stay visible.
- [ ] Exit Full Screen in the row: the window returns to its size with its window buttons back.
- [ ] A session needing input in the other window: its tab shows an amber dot.
- [ ] × on the other window's tab closes it (asking first if it has running sessions).

**Follow-up (2026-11-26):** the owner confirmed it works on the Mac and asked for the window buttons in the row. The row now starts with **close, minimize and exit full screen drawn like macOS's** (red, grey, green; 12 px, glyphs on hover). Minimize is grey and does nothing, as macOS shows it in full screen. The separate Exit Full Screen button at the right end is gone (green does it). Red, like ⌘W, asks about running sessions and unsaved files first. Fixed on the way: closing through the action (red, or × on the current window's tab) did nothing when no element had focus; the buttons now call the close check directly, for other windows' tabs too. Verified under Xvfb + Openbox: the buttons in dark theme, glyphs on hover, green leaves full screen, red closes the window, × closes the other window.

---

## Change: welcome window closes properly; "Open Project…" (2026-11-26)

The owner reported that the welcome window's red button only seemed to minimize it, that it could not leave full screen, and asked to rename Open Folder to Open Project.

**Done**
- **Closing the welcome window quits** when no project window is open (red button or ⌘W), as IntelliJ IDEA's welcome screen does. Before, the window closed but the app stayed running with no window, which looked like a minimize. Quitting goes through the normal quit (nothing to ask with no project open).
- **⌘W on the welcome window** did nothing before (nothing in it had keyboard focus); the window now takes focus when it opens.
- **Opening a project from the welcome window now closes it.** Clicking a recent project (or Open Project…) left the welcome window open: it was asked to close while still handling the click, which silently failed. The close is now deferred, and it does not quit.
- **Full screen:** the title bar is only hidden for project windows (identified by their macOS tabbing identifier). The welcome window has no window-tabs row, so it keeps macOS's slide-down bar in full screen and can leave it with the green button as usual.
- **Renamed** "Open Folder…" to **"Open Project…"** in the File menu and on the welcome window (⌘O; it still picks a folder).

**Verified on Linux** (Xvfb + Openbox): closing the welcome window with the window manager's close button and with ⌘W quits; opening a recent project closes the welcome window and keeps the app running; the button reads Open Project….

**Manual checks (Mac)**
- [ ] With no project open, the welcome window's red button quits the app (no Dock dot left).
- [ ] Welcome window in full screen: moving to the top shows the window buttons; green leaves full screen.
- [ ] Project windows in full screen still have no slide-down bar.
- [ ] File ▸ Open Project… (⌘O) opens the folder picker.

---

## Chime when Claude finishes (owner's request, 2026-11-26)

**Done**
- When a session goes from working to idle (Claude finished its turn, the same moment as the "Finished" notification), the app plays a **chime** (first a soft two-note bowl tone; replaced by the "bass riff", see the follow-up below).
- It is synthesised in code (`anveg_core::chime`, a 16-bit mono WAV), written once to `<app data>/sounds/finished-v1.wav`, and played with macOS's `/usr/bin/afplay` on a background thread (Linux, for development: `paplay`, else `aplay`). No audio library and no new dependency. Several sessions finishing within 1.5 s give one chime.
- **Settings ▸ Status and resources ▸ Sound when Claude finishes:** Off / When away (only when that session is not in front of you: its window is not active or it is not shown in a pane) / **Always** (default), with ▷ to hear it.

**Verified under Xvfb:** a fake session finishing a turn writes the WAV and runs the player (none is installed in the container, so nothing is heard there). Unit tests check the WAV header and length, the peak level, a click-free start, a silent end, and that the low note still rings after a second.

**Manual checks (Mac)**
- [ ] Settings ▸ ▷ plays the chime.
- [ ] A session finishing a turn plays it once (Always).
- [ ] When away: no chime while you watch the session; a chime when it is hidden or the app is in the background.
- [ ] Off: silent.

**Follow-up (2026-11-26): a new chime, chosen by the owner.** Candidates were rendered with a small Python synthesiser and listened to first (melodic, then lower, then shorter, then L5 "deep bowls" with hip-hop/rock touches). The owner picked **"bass riff"**: a funky plucked bass climbing C2, E♭2, F2, G2 and landing on a deep bowl at C2 with an 808-style sub underneath (a sine gliding down to its pitch, gently saturated so laptop speakers hear it), in a small Schroeder room reverb; 2.8 s, peak about -7 dBFS. `anveg_core::chime` was ported from the Python sample for sample: its output is bit-identical to the file the owner approved (all 123,479 samples). The cached file is now `sounds/finished-v2.wav`, so the old chime is not reused. To hear it without the app: `cargo run -p anveg_core --example chime -- chime.wav`.

---

## Split diffs and the Git activity log (owner's request, 2026-11-26)

The owner asked for a split view of each changed file when committing (before on the left, read-only; after on the right, editable; changed rows and the changes inside them highlighted; changes revertable one by one) and a log of what was sent and received. Answers to the questions asked first: the log is about **git push / pull / fetch**; the sides **follow staging**; edits are **saved automatically**; split is the **default with a toggle**, commits and pull requests included.

**Done — split view (`split_diff_view.rs`, `anveg_core::side_diff`)**
- A changed file from the Git panel opens side by side, in two code editors (syntax colouring, line numbers, search):
  - **Changes:** staged version (read-only) | working file, **editable and saved automatically** half a second after typing stops (⌘Z undoes, including a Revert).
  - **Staged:** last commit | staged version, both read-only.
  - **New:** empty | working file.
- Changed lines are tinted (green added, red removed, blue changed); **what changed inside a line** is tinted more strongly (word-level diff of paired lines; skipped on very long or almost wholly changed lines). A pure insertion or deletion leaves a line marking the gap on the other side.
- A **middle strip** joins each change across the two sides with a band and holds its buttons: **Revert** (puts the left side back on the right; undoable) and **Stage** for working changes, **Unstage** for staged ones. Stage/Unstage write just that change into the index (`git hash-object` + `git update-index --cacheinfo`, keeping the file mode), so the working file is never touched.
- **Scrolling one side scrolls the other** to the matching line (equal stretches one to one, changed blocks in proportion). ↑/↓ jump to the previous/next change; the header shows the count.
- The tints are painted over the editors after they lay out each frame (the editor records its layout while painting), so they never lag behind scrolling.
- **Outside changes:** if the file changes on disk (Claude, a formatter) and there are no unsaved edits here, the right side reloads; with unsaved edits nothing is overwritten and a bar offers *Reload (drop my edits)* / *Keep mine (overwrite)*. A save also checks the disk first. CRLF files are shown and diffed as LF and written back as CRLF. Binary, non-UTF-8 and files over 4 MB show a message (the unified view still works).
- **Split / Unified** toggle on every diff (saved in `settings.json` as `diff_split`, default split). Commits, pull requests, "compare" and the activity log's diffs use gpui-component's read-only side-by-side mode, which also highlights changes inside lines.

**Done — Git activity log (`anveg_core::git_activity`, Git ▸ Activity)**
- Around every push, pull and fetch (Git panel, and the push made when creating a pull request) the app snapshots the branch refs before and after. The log shows, newest first:
  - **Push:** "Sent N commits to origin/x" (or "Sent new branch …"), the commits and the changed files.
  - **Fetch:** "Received N commits on origin/x" for each remote branch that moved.
  - **Pull:** "Pulled N commits from origin/x into x" (or the remote and local moves separately when they differ).
  - Failures, with git's message (e.g. a rejected push).
- **Changes** opens that range's diff (before → after, side by side); a commit opens its own diff; a file opens the range's diff.
- The toast after a push/pull now says what happened ("Push finished: sent 1 commit to origin/feature-x").
- Stored per project in `activity/<project-id>.json` in the app's data folder (the newest 100), never in the repository.

**Fixed on the way:** reloading the left side after a save reset its scroll position (it is now only replaced when it changed). The Git panel's view row wraps, so "Stash" and "Activity" fit a narrow sidebar.

**Verified under Xvfb** (with a local bare "remote" and a second clone as a teammate): the split view of a file with a changed line, inserted lines, a renamed function and a deleted line; synchronised scrolling; Revert (written to disk); Stage of one change (only that change staged, checked with `git diff --cached`); typing on the right (saved ~0.5 s later; ⌘Z); the staged view and Unstage; a change made outside the app reloading the right side; Split ↔ Unified; a commit side by side; dark theme; push (new branch), push, a rejected push, a teammate's commit pulled, the log after a restart, and the pull's Changes side by side. Unit tests: hunks, inline ranges, revert/accept, line mapping, partial staging in a real repository, and push/fetch/pull/failed push against a bare remote.

**Manual checks**
- [ ] Open a file Claude changed from Git ▸ Changes: the split view; scroll one side, the other follows.
- [ ] Revert one change; ⌘Z brings it back; the file on disk follows within a second.
- [ ] Stage one change: only it appears under Staged; the file on disk is unchanged.
- [ ] Edit on the right while Claude edits the same file: the bar appears instead of losing either change.
- [ ] Push, then Git ▸ Activity: what was sent; Changes shows it side by side.
- [ ] Pull a teammate's work: "Pulled N commits…", with the files.
