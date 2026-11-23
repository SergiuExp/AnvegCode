# Anveg Code — Build Plan (Phases 1–3)

> **Audience:** Claude Code, building this app from an empty repository.
> **Companion document:** `anveg-code-next-phases.md` (Phases 4–7: file tree and editor, git panel, GitHub, per-session change review).
> **Facts about Claude Code** in section 5 were checked against the official docs in November 2026. Re-check flags with `claude --help` and the links in section 13 before relying on them.
>
> **Revisions**
> - 2026-11-23: Linux removed from Phases 1–3 and moved to section 12 (Deferred), next to Windows. macOS is the only target.
> - 2026-11-23: Decisions taken while building (component library, terminal option, license questions) are recorded in `docs/PROGRESS.md`, which overrides this plan where they differ.

---

## 1. What we are building

**Anveg Code** is a lightweight, native desktop app for managing many Claude Code sessions across projects.

- Each **project** opens in its own window. A project is one **main folder** plus optional **extra folders**.
- The **left side** of the window lists the project's Claude Code sessions.
- The **right side** shows the selected session: the real Claude Code terminal UI, running in an embedded terminal. A second tab holds a normal shell for the project.

The app does **not** reimplement Claude Code's chat interface. It runs the real `claude` CLI in a pseudo-terminal and adds project and session management around it.

Platform: **macOS**. Linux and Windows are deferred (section 12).

### 1.1 Design principles

1. **Lightweight first.** One process for all windows. No web runtime. No polling. Draw nothing that is not visible. Keep no `claude` process alive that is not needed.
2. **Claude Code is the interface.** Never parse or imitate its terminal UI.
3. **Read Claude Code's files, never write them.** The only exception: moving a session's transcript to the Trash when the user deletes that session.
4. **Core logic is GUI-free and unit-tested.** The UI layer is thin.

### 1.2 Lightweight targets (acceptance criteria)

| Target | How to measure |
|---|---|
| ~0% CPU when nothing changes (no timers, no polling; cursor blink off by default) | `top -pid <pid>` for 60 s with 3 windows open and all sessions idle |
| App memory < ~100 MB with 3 project windows and 5 running sessions, **excluding** the `claude` child processes | `ps -o rss= -p <app pid>` |
| Cold start to first window well under 1 s on Apple silicon | Log timestamps from process start to first frame |

### 1.3 Out of scope (all phases)

- A custom chat UI: Claude Code itself is the interface.
- AI or API calls made by the app itself.
- Web technology (no webviews, no Electron, no Tauri).
- A plugin system.

---

## 2. Ground rules for Claude Code while building

1. **Work one milestone at a time** (section 9). A milestone is done only when all of these pass:
   `cargo build`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check licenses`.
   Then append a short entry to `docs/PROGRESS.md`: what was done, known issues, and a **manual test checklist** for the user.
2. **The user tests the GUI.** GUI behaviour cannot be verified automatically here. Give concrete manual checks, not "test the app".
3. **GPUI is pre-1.0 and has few docs.** Before using any GPUI or component-library API, read its source and examples at the pinned revision. Do not write GPUI code from memory, and do not guess API names.
4. **Licensing.**
   - GPUI (`crates/gpui` and the crates it depends on in the Zed repo) is **Apache-2.0**. Most of Zed is **GPL-3.0**.
   - You may *read* Zed's source to learn GPUI patterns. **Never copy code from Zed's GPL crates.** Check the crate's `Cargo.toml` license field and the file header before borrowing anything.
   - Every dependency must pass the `cargo deny` allow-list (section 3.2). Ask the user before adding any license to it.
5. **Pin every git dependency to an exact `rev`**, and commit `Cargo.lock`.
6. **No `unwrap()` or `expect()` on runtime paths** (IO, parsing, processes). Log errors with `tracing` and show them in the UI.
7. **Never block the UI thread** on file IO, parsing, or processes.
8. **Don't change the user's Claude Code configuration** (`~/.claude/settings.json` and so on) unless a step in this plan explicitly says so and the user has agreed.

---

## 3. Tech stack

### 3.1 Choices

| Concern | Choice | Notes |
|---|---|---|
| UI framework | **GPUI** (from the Zed repository) | Apache-2.0. Use the GPUI revision that the component library pins. |
| UI components | **Longbridge's GPUI component library**, https://github.com/longbridge/gpui-component | Apache-2.0. Lists, inputs, dialogs, tabs, menus, resizable panels, and later the code editor. It has been restructured (gpui-component / gpui-base / gpui-kit); use whatever the README currently recommends. *(See PROGRESS.md, M0/M1 decisions.)* |
| Terminal engine | **libghostty-vt** (Ghostty's terminal core) | MIT, with a stable 1.0 API. Accessed through one of the options in 3.3, always behind our own `TerminalEngine` trait. |
| Pseudo-terminal (PTY) | `portable-pty` (from the WezTerm project) | macOS now. Windows ConPTY support later. |
| File watching | `notify` + `notify-debouncer-full` | Uses FSEvents on macOS. |
| Data files | `serde`, `serde_json` | |
| IDs | `uuid` (v4) | |
| Logging | `tracing`, `tracing-subscriber`, `tracing-appender` | Log file in `~/Library/Logs/Anveg Code/` |
| Standard paths | `directories` | |
| Trash | `trash` crate, or `NSFileManager trashItemAtURL` via `objc2-foundation` | |
| Splitting user-supplied flags | `shell-words` | |
| License checks | `cargo-deny` | |

### 3.2 `deny.toml` license policy

- **Allow:** MIT, Apache-2.0, Apache-2.0 WITH LLVM-exception, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0, Unicode-DFS-2016, CC0-1.0, BSL-1.0.
- **Ask the user first:** MPL-2.0 or anything else.
- **Deny:** GPL-*, AGPL-*, LGPL-*.

Bundled fonts, icons, and other assets carry their own licenses. List them in `THIRD_PARTY.md`.

### 3.3 Terminal integration options (decide in Milestone 1)

Try these in order. Pick the **first** one that compiles against our pinned GPUI revision and passes the terminal checklist in section 5.5.

1. **`gpui-cn` with the `ghostty` feature.** A GPUI component library whose optional terminal component runs on libghostty-vt. Apache-2.0.
2. **`gpui-ghostty`** (https://github.com/Xuanwo/gpui-ghostty). Embeds a libghostty-vt terminal in GPUI. Apache-2.0.
3. **The `ghostty-vt` crate** (a safe Rust API over libghostty-vt) plus our own GPUI rendering element.
4. **Fallback: `gpui-terminal`**, built on `alacritty_terminal` (Apache-2.0). Pure Rust, so no Zig is needed.

Notes:

- **Zig is a build requirement** for the Ghostty-based options when building libghostty-vt from source. Check the exact Zig version each crate requires, and document it in the README.
- **GPUI version mismatch.** A terminal crate that depends on a different GPUI revision than the component library will not compile alongside it. If that happens, either:
  - vendor the crate into `third_party/` and adapt it to our revision (Apache-2.0 allows this; keep its LICENSE and NOTICE files), or
  - use option 3.
- **Whatever you pick, put it behind a trait.** Define `TerminalEngine` in `anveg_term` with these operations: feed bytes, resize, snapshot the visible grid and cursor for rendering, encode keys and mouse input to bytes, report title changes, read and set the selection, and set the scrollback limit. The rest of the app must not depend on the engine directly, so it can be switched to `alacritty_terminal` later (for example, for Windows).

---

## 4. Architecture

### 4.1 Workspace layout

```
anveg-code/
├── Cargo.toml                 # workspace
├── deny.toml
├── crates/
│   ├── anveg_core/            # NO GPUI dependency. All logic. Unit-tested.
│   │   ├── config.rs          # settings, projects, window state (JSON, atomic writes)
│   │   ├── claude_env.rs      # find `claude`, capture login-shell env, version, config dir
│   │   ├── transcripts.rs     # ~/.claude/projects discovery, path encoding, JSONL metadata parser
│   │   ├── session_index.rs   # per-project session list: cache, watcher, aliases
│   │   ├── launcher.rs        # build claude command lines (new / resume / fork)
│   │   ├── pty_host.rs        # spawn / resize / write / stop PTY processes
│   │   └── status.rs          # (Phase 2) status state machine + hook socket server
│   ├── anveg_term/            # TerminalEngine trait, Ghostty-based impl, GPUI TerminalView
│   ├── anveg_app/             # GPUI binary "anveg-code": windows, sidebar, dialogs, settings, menus
│   └── anveg_hook/            # (Phase 2) tiny binary invoked by Claude Code hooks
├── docs/PROGRESS.md
├── tests/fixtures/transcripts/   # sanitized sample JSONL files
├── scripts/fake-claude           # test double for `claude` (section 10)
├── README.md                     # build instructions (Rust, Zig, bundling)
└── THIRD_PARTY.md
```

### 4.2 Runtime model

```
  Project window (GPUI)  ──reads──►  SessionIndex  ◄──watches──  ~/.claude/projects/<encoded-dir>/
        │
        └──►  PtyHost  ──spawns──►  claude … / $SHELL   (each in its own PTY)
                 │  reader thread: bytes ──► TerminalEngine (per session) ──► "dirty" signal ──► UI
                 │  waiter thread: process exit ──► UI

  (Phase 2)  claude hook ──► anveg-hook ──unix socket──► StatusHub ──► sidebar, notifications, Dock badge
```

- **One reader thread per PTY**, doing blocking reads. It feeds bytes into that session's `TerminalEngine`, which sits behind a mutex. It then sends at most one pending "dirty" notification to the UI.
- **The UI redraws a terminal only when it is visible and dirty.** Hidden sessions keep parsing their output, which is cheap, but are never drawn.
- **One waiter thread per child process** reports when the process exits.
- **Transcript parsing and file-watch handling** run on GPUI's background executor.
- **All windows live in one process** and share `SessionIndex`, `PtyHost`, and the config.

---

## 5. How the app uses Claude Code

Sources: links in section 13. Re-verify before relying on any of this.

### 5.1 Environment, binary, and config directory

- **Shell environment.** macOS apps started from Finder or the Dock do not inherit the user's shell `PATH`. At startup, on a background thread, capture the login-shell environment once:
  - Run `$SHELL -l -i -c '<print a marker, then env -0>'` with a 5 s timeout.
  - Parse only the output after the marker, so shell banners and noise are ignored.
  - If that hangs or fails, retry without `-i`. If that also fails, fall back to the app's own environment.
  - Use this environment for every child process: `claude`, the terminal tab, and (in Phase 2) hooks.
- **Finding `claude`.** Use the path from settings if set. Otherwise resolve `claude` on the captured `PATH`. Run `claude --version` and show the result in Settings. If `claude` is not found, the welcome window explains how to install Claude Code.
- **Config directory.** Claude Code keeps its data in `~/.claude` unless `CLAUDE_CONFIG_DIR` is set in the environment. At startup, run `claude auth status` once. Its JSON output includes a `configDirectory` field (Claude Code v2.1.268 and later); treat that as authoritative when present.

### 5.2 Where sessions are stored

- **Transcript path:** `<configDir>/projects/<encoded-cwd>/<session-id>.jsonl`.
- **Encoding rule:** `<encoded-cwd>` is the working-directory path with **every non-alphanumeric character replaced by `-`**.
  - If the encoded name is longer than **200 characters**, Claude Code truncates it to 200 and appends a hash of the full path.
  - Do not reimplement that hash. Instead, find directories whose names start with the 200-character prefix, and confirm the right one by reading a `cwd` value from a transcript inside it.
- **Canonical paths.** Canonicalize a project's main folder once, when the project is added (`std::fs::canonicalize`). Always spawn `claude` with exactly that path as its working directory, and compute the encoding from the same string. This avoids mismatches such as `/tmp` versus `/private/tmp`.
- **Retention.** Claude Code deletes transcripts older than its `cleanupPeriodDays` setting (default **30 days**), so old sessions will disappear from the list. Say so in the app's help text. The app does not change the user's settings.
- **The file format is internal.** Docs say the JSONL entry format is internal and changes between versions, so parse it defensively (5.3).

### 5.3 Reading session metadata

For each session, the app needs: display name, first prompt (used as a fallback title), last activity time, and optionally the git branch.

**Before writing the parser:**

1. Ask the user to create a few real sessions with the installed Claude Code version: one named with `-n`, one renamed with `/rename`, and one plain.
2. Save sanitized copies to `tests/fixtures/transcripts/`.
3. From those files, identify:
   - the entry that records a custom name set by `/rename` or `-n`;
   - the entry that holds Claude's generated title or summary;
   - the first real user prompt, skipping meta entries, tool results, slash-command wrappers, and sidechain or subagent entries;
   - the fields `timestamp`, `cwd`, `gitBranch`, and `sessionId`.

**Parser rules:**

- Parse line by line into `serde_json::Value`. Skip lines that fail to parse. Ignore unknown entry types and fields. Never panic.
- **Display-name precedence:** app alias → custom name → generated title or summary → first prompt (truncated to ~80 characters) → "Untitled session".
- **Last activity:** the newest timestamp seen; if there is none, the file's modification time.
- **Incremental parsing.** Transcripts are append-only.
  - Cache, per file: `{size, mtime, parsed_offset, metadata}`.
  - When a file changes, parse only the bytes after `parsed_offset`. Keep the offset at the last complete newline, so a half-written final line is re-read next time.
  - If the file is now smaller than `parsed_offset`, it was rewritten: parse it again from the start.
  - Store the cache in `sessions/<project-id>.json` (section 11).
- **Non-interactive sessions.** Claude Code hides sessions created with `claude -p` or the Agent SDK from its own session picker. If the fixtures show a reliable marker for them, hide them by default, with a toggle in Settings. If there is no marker, show them.

### 5.4 Launching, resuming, and stopping sessions

**New session:**
```
claude --session-id <uuid-v4> [-n "<name>"] [--add-dir <extra-folder> ...] [<default flags from Settings>]
```
- The working directory is the project's main folder.
- The app generates the UUID itself, so it can add a row for the session immediately.

**Resume:**
```
claude --resume <session-id> [--add-dir <extra-folder> ...] [<default flags>]
```
- **`--add-dir` and `--settings` are not restored on resume.** The app must pass them again every time. Doing this automatically is one of the app's benefits over the plain CLI.

**Never open the same session in two processes.** If one session is resumed in two places, both write into the same transcript and the messages interleave.
- **Within the app:** if the session is already running, just select it.
- **Outside the app:** before resuming, run `claude agents --json` **once** (never on a timer). It lists active sessions with `sessionId`, `pid`, `kind` (`interactive` or `background`), and `cwd`. If the session is already running elsewhere, ask: *"This session is open somewhere else. Open a copy (fork) / Cancel."* The copy uses `--fork-session`. If `claude agents` fails, skip this check.

**Child environment:** the captured login environment, plus:
- `TERM=xterm-256color`
- `COLORTERM=truecolor`
- `LANG=en_US.UTF-8`, but only if `LANG` is not already set
- (Phase 2) `ANVEG_HOOK_SOCKET=<socket path>`

**Stopping a session:**
1. Send `SIGHUP` to the child's process group (the same signal a closing terminal sends).
2. Wait up to 3 s.
3. Send `SIGKILL` to anything still running.

Claude Code saves transcripts continuously while it runs, so a stopped session can always be resumed.

### 5.5 Terminal checklist (Milestone 1 gate; the user verifies these manually)

- [ ] 24-bit color, alternate screen, and Unicode/emoji character widths render correctly.
- [ ] **Synchronized output:** no flicker or tearing while Claude streams a long answer.
- [ ] **Bracketed paste:** pasting 500 lines with ⌘V arrives as a single paste.
- [ ] **Enter** submits. **Shift+Enter** and **Option+Enter** insert a newline in Claude's prompt.
  - If Shift+Enter doesn't work, enable the kitty keyboard protocol in the key encoder.
  - If that isn't possible, map Shift+Enter to `ESC` + `CR`.
- [ ] These keys reach Claude Code: Esc, Esc Esc, Ctrl+C, Ctrl+R, Shift+Tab (cycles permission mode), arrow keys, Tab.
- [ ] **Ctrl+V reaches the PTY.** Claude Code uses it for image paste.
- [ ] Option works as Meta (setting, default on).
- [ ] Mouse-wheel scrolling works, and so does scrollback when Claude is not in the alternate screen.
- [ ] Selecting text and pressing ⌘C copies it.
- [ ] Resizing the window reflows correctly. PTY resizes are debounced to about 16 ms.
- [ ] ⌘-click on a URL opens it in the browser.
- [ ] All app shortcuts use ⌘, so they never collide with keys Claude Code needs. Every non-⌘ key goes to the terminal.

---

## 6. Phase 1 — Core (first macOS version), detailed spec

### 6.1 Projects and windows

- **Project record:** `{ id (uuid), name (default: main folder's name), main_folder, extra_folders[], created_at, last_opened_at }`, stored in `projects.json`.
- **Open a project** (⌘O): show a folder picker.
  - If a project with that main folder already exists, open or focus its window.
  - Otherwise create a new project.
- **One window per project.** Opening a project that is already open focuses its window. The window title is the project name.
- **Project Settings sheet** (from the window menu or the gear icon in the sidebar header):
  - rename the project;
  - list its folders;
  - add or remove extra folders (the main folder is fixed).
  - Folder changes apply only to sessions started afterwards. Show that note in the sheet.
- **Welcome window** (shown when no project windows are open): a recent-projects list (click to open, × to remove from the list) and an **Open Folder…** button.
- **File ▸ Open Recent** menu.
- **Reopen on launch:**
  - `state.json` stores, for each open window: project id, window bounds, selected session id, active tab (Claude or Terminal), and sidebar width.
  - Save it after changes (debounced to 500 ms) and on quit.
  - On launch, reopen every window from `state.json`, with the same size, position, selected session, and tab.
  - If a project's folder no longer exists, open its window with an error banner offering **Locate…** and **Remove Project**.
  - **Sessions are not started on launch.** A setting, **"Resume sessions that were running when I quit"** (default off), resumes the sessions recorded in `state.json`. They start only after the windows have appeared.

### 6.2 Session list (left side)

- **Source:** `SessionIndex` for the project's **main folder only**. Every session runs in the main folder.
- **Row contents:** display name, relative time ("5 min ago"), and a running dot (filled = process running). Tooltip: full name, session id, created time, last activity, git branch.
- **Order:** most recent activity first. Sessions just created and not yet written to disk appear at the top.
- **Filter field** at the top (⌘F focuses it): case-insensitive substring match on display name and first prompt.
- **New session** (`+` button or ⌘N): a small popover with an optional name field; Enter starts the session (6.3).
- **Live updates:** `notify` watches the project's transcript directory, non-recursively.
  - If that directory doesn't exist yet, watch its parent until it appears.
  - Debounce events by 200 ms, then parse incrementally (5.3).
- **Virtualized list:** draw only the visible rows. Projects can have hundreds of sessions.
- **Context menu:** Open, Rename…, Stop (only when running), Copy Session ID, Show Transcript in Finder, Delete…
- **Multi-select:** ⌘-click and ⇧-click. Stop and Delete apply to the whole selection.
- **⌘1–⌘9:** open the 1st–9th session in the current list order.
- **Empty state:** "No sessions yet", with a **New Session** button.

### 6.3 Session view (right side, "Claude" tab)

- **Clicking a session opens it.** If it isn't running, resume it (5.4). Then show its terminal and give it keyboard focus.
- **Several sessions can run at once.** Switching between them is instant, with no restart. Only the visible one is drawn.
- **When `claude` exits:**
  - keep the final screen, dimmed, with an overlay reading **"Session ended"** and a **Resume** button;
  - show the exit code if it was non-zero;
  - **Resume** starts it again with `--resume`.
- A new session whose process exits **before any transcript file exists** is removed from the list.
- The scrollback limit comes from Settings (default 10,000 lines).

### 6.4 Terminal tab

- A tab bar above the right side: **Claude** | **Terminal**. ⌘T switches between them.
- **Terminal** runs the user's `$SHELL` as a login shell (`-l`) in the project's main folder.
  - It starts the first time the tab is opened. There is one per project window.
  - If it exits, show "Process exited", with a **Restart** button.

### 6.5 Renaming

- **Rename…** opens an inline text field. The name must be non-empty and a single line; trim whitespace; maximum 100 characters.
- The new name is stored as an **app alias** in `sessions/<project-id>.json` and appears immediately. Claude Code does not need to be running.
- **If the session is running,** the app also sends `/rename <name>` followed by Enter to its terminal, so the name matches inside Claude Code.
  - **Phase 1:** send it immediately.
  - **Phase 2:** queue it until the session's status is *Idle* (7.1).
  - **Known limitation:** if the user has unsent text in Claude's prompt, the command is added to that text. Mention this in the rename field's help text.
- New sessions that have a name pass it with `-n`, so Claude Code and the app agree from the start.

### 6.6 Deleting sessions

- **Delete…** is available from the context menu and from ⌫ when the list has focus. It works on multi-selections.
- **Always ask for confirmation** (no "don't show again"): *"Delete N session(s)? Their transcripts will be moved to the Trash."* List the names.
- **Running sessions are stopped first** (5.4).
- **What moves to the Trash:** `<session-id>.jsonl`, plus a sibling directory named `<session-id>` if one exists. Check the fixtures to confirm what Claude Code stores there.
- Remove the session's alias and cache entries.
- **Touch no other Claude Code data.**
- **Errors** (permissions, missing file) appear as a toast. If the file is already gone, remove the row anyway.

### 6.7 Closing windows and quitting

**What stops:**
- Closing a project window (⌘W or the red button) stops that project's Claude sessions and its terminal-tab shell.
- Quitting (⌘Q, or Quit from the Dock) stops everything.

**Confirmation:**
- Shown when Claude sessions are running and the setting `confirm_on_close` is on (the default).
- A modal dialog: *"Stop N running session(s)? You can resume them later."* It lists the names and has the buttons **Stop and Close** (or **Stop and Quit**) and **Cancel**.
- It has a **"Don't show this again"** checkbox. Ticking it sets `confirm_on_close = false`. The user can turn the dialog back on in **Settings ▸ General**.
- macOS's native alert can't hold a custom checkbox here, so build this dialog with the component library's dialog.

**Implementation notes:**
- To intercept a window close, use GPUI's "window should close" hook; Zed uses it to warn about unsaved files.
- Intercept app quit the same way.
- During system logout or shutdown, don't block: skip the dialog and just stop.

**Shutdown order:**
1. Write `state.json`, so the windows can be restored next launch. Record which sessions were running, for the resume-on-launch setting.
2. Send `SIGHUP` to all children.
3. Wait up to 3 s.
4. Send `SIGKILL` to anything left.
5. Exit.

### 6.8 Settings (⌘,)

| Section | Setting | Default |
|---|---|---|
| General | Confirm before stopping running sessions on close or quit | on |
| General | Resume sessions that were running when I quit | off |
| Claude | Path to `claude` (shows the auto-detected path; can be overridden) | auto |
| Claude | Detected version (read-only) | — |
| Claude | Default extra flags, e.g. `--permission-mode plan` (split with `shell-words`) | empty |
| Claude | Config directory (read-only) | detected |
| Claude | Show non-interactive (`-p`/SDK) sessions (only if detectable, see 5.3) | off |
| Terminal | Font family / size | Menlo / 13 |
| Terminal | Scrollback lines | 10,000 |
| Terminal | Cursor blink (off saves idle CPU) | off |
| Terminal | Option key acts as Meta | on |

- Settings are stored in `settings.json` and apply immediately where possible.
- Phase 2 adds more settings (7.2–7.4).

### 6.9 Menus and shortcuts

- **Anveg Code:** About, Settings… ⌘,, Quit ⌘Q
- **File:** New Session ⌘N, Open Folder… ⌘O, Open Recent ▸, Close Window ⌘W
- **Edit:** Copy ⌘C, Paste ⌘V, Select All ⌘A, Find ⌘F (focuses the session filter)
- **View:** Toggle Claude/Terminal ⌘T, Next Session ⌘⇧], Previous Session ⌘⇧[
- **Session:** Rename…, Stop, Delete… (⌫ when the list has focus), Copy Session ID, Show Transcript in Finder
- **Window:** the standard macOS items

Keys without ⌘ always go to the focused terminal.

### 6.10 Appearance

- Follow the system's light or dark mode, and switch live.
- Ship a light and a dark terminal color palette.
- Use the native window buttons with a transparent title bar, the way GPUI apps usually look on macOS.
- No decorative animations. Anything animated costs idle CPU.

---

## 7. Phase 2 — Status and resource savings

### 7.1 Live status via Claude Code hooks

**`anveg_hook` binary.** A tiny program bundled inside the app (`Anveg Code.app/Contents/MacOS/anveg-hook`). Claude Code runs it for each hook event. It:
- reads the hook's JSON from stdin;
- connects to the Unix socket at `$ANVEG_HOOK_SOCKET` and sends one line of JSON: `{event, session_id, notification_type?, cwd, ts}`;
- exits.

It **must:**
- finish in under 20 ms (100 ms connect and write timeout);
- **print nothing to stdout or stderr** (for some events, Claude Code adds hook output to the conversation);
- **always exit 0**, even on errors.

**Hook configuration.** The app writes `hooks-settings.json` to its app-support directory and passes `--settings <that path>` to every `claude` it launches. Sketch:

```json
{
  "hooks": {
    "SessionStart":      [{ "hooks": [{ "type": "command", "command": "'<app>/Contents/MacOS/anveg-hook'" }] }],
    "UserPromptSubmit":  [{ "hooks": [{ "type": "command", "command": "'<app>/Contents/MacOS/anveg-hook'" }] }],
    "PreToolUse":        [{ "hooks": [{ "type": "command", "command": "'<app>/Contents/MacOS/anveg-hook'" }] }],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "'<app>/Contents/MacOS/anveg-hook'" }] }],
    "Notification":      [{ "matcher": "permission_prompt|idle_prompt|elicitation_dialog",
                            "hooks": [{ "type": "command", "command": "'<app>/Contents/MacOS/anveg-hook'" }] }],
    "Stop":              [{ "hooks": [{ "type": "command", "command": "'<app>/Contents/MacOS/anveg-hook'" }] }],
    "SessionEnd":        [{ "hooks": [{ "type": "command", "command": "'<app>/Contents/MacOS/anveg-hook'" }] }]
  }
}
```

Check the exact schema, event names, matcher syntax, and input fields against the hooks reference (section 13) before implementing. Every hook receives at least `session_id`, `cwd`, and `hook_event_name`.

**⚠ Spike this first (Milestone 10).** The docs say `--settings` values *override the same keys* in the user's settings files. Test whether a `hooks` key passed this way **replaces** the user's own hooks or is **merged** with them:
1. Put a test hook in `~/.claude/settings.json` that writes to a temp file.
2. Launch a session with our `--settings`.
3. Check whether both hooks fire.

If ours replaces theirs, **do not ship this approach**, because it would silently disable the user's hooks. Fall back to one of these, in order:
- **(a)** While at least one session is running, run `claude agents --json` every 3 s. It reports each session's `status` (`busy` / `waiting` / `idle`) and `waitingFor`. Stop the timer when no sessions are running.
- **(b)** With the user's explicit consent, add clearly marked Anveg hooks to `~/.claude/settings.json`, and remove them if the user disables the feature.

**Status rules:**

| Event | New status |
|---|---|
| Process started / `SessionStart` | Idle |
| `UserPromptSubmit`, `PreToolUse` | Working |
| `PermissionRequest`, `Notification` (`permission_prompt`, `elicitation_dialog`) | Needs input |
| `Stop`, `Notification` (`idle_prompt`) | Idle |
| `SessionEnd`, process exit | Stopped |

**Where status shows:**
- **Row dot:** Working (accent color), Needs input (yellow), Idle (gray), Stopped (hollow). Static colors only, no animation.
- **Sidebar header:** a count of sessions that need input.
- **Rename (6.5):** queued `/rename` commands are sent when the session becomes Idle.

### 7.2 Notifications and Dock badge

- **When:** a session goes to *Needs input*, or goes from *Working* to *Idle* (finished its turn), **and** it isn't the visible session in the focused window.
- **What:** a macOS notification, *"<project> · <session>: needs your input"* or *"… finished"*. Clicking it focuses the window and selects the session.
- **How:** `UNUserNotificationCenter` through the `objc2-user-notifications` crate.
  - This only works from a bundled `.app`. Under `cargo run`, log instead.
  - Ask for notification permission the first time one is needed.
- **Dock badge:** the total number of sessions needing input (`NSApp.dockTile.badgeLabel`). Clear it when the count is 0.
- **Setting:** Notifications on/off (default on).

### 7.3 Idle shutdown

- **Rule:** a session that has been *Idle* for **N minutes** and is **not visible** in any window is stopped (5.4). Its row then shows "not running", and clicking it resumes the session.
- **N:** setting "Stop idle sessions after N minutes" (default 30; 0 turns it off).
- **Never stop** *Working* or *Needs input* sessions.
- **Timers:** schedule one timer per idle session for its exact deadline. No periodic checks.

### 7.4 Memory display (optional)

- Setting "Show memory use" (default off).
- When it's on and the window is visible, show the memory (RSS) of each running session's process tree in the row tooltip.
- Sample it every 10 s, and only in that case. Use `libproc` or `sysinfo`.

### 7.5 Phase 2 acceptance

- Status changes appear within 1 s.
- About 0% CPU when every session is idle.
- Notifications appear only for sessions that are not visible.
- The user's own Claude Code hooks still work (the spike result is documented in `PROGRESS.md`).

---

## 8. Phase 3 — Comfort

1. **Several terminals per project.** The Terminal tab becomes a tab list: `+` adds a shell, ⌘⇧T opens a new terminal. Each shell starts lazily. On launch, terminal tabs are restored as fresh shells.
2. **Two sessions side by side.** The right side splits into two resizable panes. Each pane shows a session or a terminal. Drag a session from the sidebar onto a pane to show it there. The layout is saved in `state.json`.
3. **Fork a session.** A context-menu item **Fork Session** runs `claude --resume <id> --fork-session …`. The fork gets a new session id. Learn it from the `SessionStart` hook (its `source` is `fork`), or from the new transcript file appearing. Then add its row.
4. **Worktrees.** **New Session in Worktree…** asks for a name and runs `claude -w <name>` in the main folder, which creates `<repo>/.claude/worktrees/<name>`.
   - Sessions in a worktree are stored under the worktree path's encoded directory, so `SessionIndex` also watches the directories for `<main>/.claude/worktrees/*`.
   - The sidebar groups sessions: the main folder, plus one collapsible group per worktree.
   - Only offered when the main folder is a git repository.

---

## 9. Milestones

| # | Milestone | Done when |
|---|---|---|
| M0 | **Skeleton:** workspace, `deny.toml`, logging, a GPUI window using the component library's theme | Builds and runs. Pinned revisions are recorded in `PROGRESS.md`. |
| M1 | **Terminal spike:** one window running `$SHELL`, then `claude`, in a `TerminalView` (choose an option from 3.3) | **The user passes checklist 5.5 and approves the feel and performance.** Stop and ask before continuing. |
| M2 | **Core config:** settings, projects, and state files with atomic writes; login-environment capture; `claude` and config-directory detection | Unit tests pass. |
| M3 | **Projects and windows:** welcome window, open folder, one window per project, Project Settings sheet, recents, restore on launch | Manual checklist passes. |
| M4 | **Session index:** path encoding, discovery, defensive parser with fixtures, incremental cache, watcher; read-only sidebar list with filter | Parser tests pass. The list matches `claude --resume`'s picker for the same folder. |
| M5 | **Running sessions:** new, resume, stop; several at once; "Session ended" overlay with Resume; duplicate-open guard; ⌘1–9 | Manual checklist passes. |
| M6 | **Terminal tab** | Manual checklist passes. |
| M7 | **Rename** (alias + `/rename`), **delete** (Trash), context menu, multi-select | Manual checklist passes. |
| M8 | **Close/quit confirmation** with "Don't show this again", Settings window, menus and shortcuts, resume-on-launch setting | Manual checklist passes. |
| M9 | **Hardening and packaging:** measure the targets in 1.2 and fix regressions; bundle the `.app` (`cargo-packager` or `cargo-bundle`); ad-hoc code signing for local use; README with build steps (Rust, Zig) | Targets met and recorded in `PROGRESS.md`. |
| M10 | **Phase 2 spike:** hooks via `--settings`, including the merge-versus-replace test (7.1) | Decision recorded. |
| M11 | Status (7.1) | |
| M12 | Notifications and Dock badge (7.2) | |
| M13 | Idle shutdown and memory display (7.3, 7.4) | Phase 2 acceptance (7.5) met. |
| M14–M17 | Phase 3, items 1–4 (one milestone each) | Phase 3 checklists pass. |

---

## 10. Testing

**Unit tests (in `anveg_core`):**
- path encoding, including paths longer than 200 characters;
- the JSONL parser: fixtures, garbage lines, a half-written last line, a rewritten (shorter) file, unknown entry types;
- display-name precedence;
- the command-line builder: quoting, `--add-dir`, default flags, resume versus new;
- state-file round trips and atomic writes;
- (Phase 2) the status rules.

**Integration tests: `scripts/fake-claude`.** A small script that behaves enough like `claude` for tests:
- prints its arguments;
- writes a JSONL transcript in the right `projects/<encoded>/` directory under a temporary `CLAUDE_CONFIG_DIR`;
- echoes input back;
- appends a rename entry when it receives `/rename x`;
- exits on `/exit`.

Use it to test the launcher, the PTY host, and live list updates without making real Claude calls.

**Manual tests:** each milestone's checklist in `docs/PROGRESS.md`.

**Performance:** a script that reports the app's memory and CPU over 60 s of idle time, with 3 windows and 5 sessions open.

---

## 11. Files the app owns (macOS)

```
~/Library/Application Support/Anveg Code/
├── settings.json
├── projects.json
├── state.json                  # open windows, bounds, selections, sessions running at quit
├── sessions/<project-id>.json  # aliases + transcript metadata cache
├── hooks-settings.json         # (Phase 2)
└── run/hooks.sock              # (Phase 2)
~/Library/Logs/Anveg Code/anveg-code.log
```

- All writes are atomic: write a temporary file, then rename it over the original.
- The bundle identifier is a placeholder, `dev.anveg.code`. **Confirm it with the user before packaging.**

---

## 12. Deferred (not covered by this document)

- **Background sessions that keep running after the app quits.** Claude Code has its own supervisor for this (`claude --bg`, `claude attach <id>`, `claude agents --json`). It's still a research preview, so revisit it later.
- **Linux support.** When it is picked up: GPUI's Linux backend (Wayland and X11); XDG directories for config; the freedesktop Trash; `notify-rust` for notifications; the same login-shell environment capture as on macOS; no Dock badge, so show the needs-input count in the window title; ship a tarball or AppImage first.
- **Windows support.**
- **Phases 4–7:** see `anveg-code-next-phases.md`.

---

## 13. References

- Claude Code sessions (storage, resume, naming, retention): https://code.claude.com/docs/en/sessions
- Claude Code CLI reference: https://code.claude.com/docs/en/cli-reference
- Claude Code hooks guide: https://code.claude.com/docs/en/hooks-guide
- Claude Code hooks reference: https://code.claude.com/docs/en/hooks
- Claude Code agent view (`claude agents --json`): https://code.claude.com/docs/en/agent-view
- GPUI (in the Zed repo, `crates/gpui`): https://github.com/zed-industries/zed
- Zed license split (GPUI is Apache-2.0, most of Zed is GPL-3.0): https://zed.dev/software-overview
- Longbridge GPUI components: https://github.com/longbridge/gpui-component
- gpui-ghostty: https://github.com/Xuanwo/gpui-ghostty
- Ghostty / libghostty: https://github.com/ghostty-org/ghostty
