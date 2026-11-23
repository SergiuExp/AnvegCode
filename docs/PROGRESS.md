# Progress

Newest entries at the bottom. Each milestone lists what was done, the decisions taken, known issues, and a manual test checklist.

---

## Pinned versions

| Component | Version | Source |
|---|---|---|
| Rust toolchain | **1.98.1** (pinned in `rust-toolchain.toml`; GPUI's macOS backend needs ≥ 1.98) | rustup |
| GPUI | `gpui-pre` **=0.3.10** (snapshot of Zed's `crates/gpui`, Apache-2.0) | crates.io, pinned by gpui-kit 0.8.1 |
| gpui-kit (Longbridge) | **=0.8.1**, `default-features = false`, `features = ["assets"]` | crates.io |
| gpui-base | 0.8.1 (via gpui-kit) | crates.io |
| gpui-cn | **=0.9.0**, `default-features = false`, `features = ["ghostty"]` | crates.io |
| libghostty-vt | 1.0.1, Ghostty commit `108cacb17ed8990840cd14abfce43d8ed8503ca1` (`ghostty-vt` / `ghostty-vt-sys` 0.9.0+ghostty-vt.1.0.1) | prebuilt XCFramework on macOS, SHA-256-checked |
| Zig | 0.17 (source builds only; not needed on macOS) | |

There are no git dependencies: everything comes from crates.io with exact `=` pins, and `Cargo.lock` is committed.

Other notable crates: `notify` 8 + `notify-debouncer-full` 0.7 (file watching), `trash` 5, `sysinfo` 0.37 (memory display), and `objc2` 0.6 / `objc2-app-kit` / `objc2-user-notifications` 0.3 (Dock badge, notifications; macOS only).

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
