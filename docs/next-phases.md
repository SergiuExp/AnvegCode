# Anveg Code — Next Phases (4–7)

> **Audience:** Claude Code, continuing the build after Phases 1–3.
> **Prerequisite:** `docs/build-plan.md` is implemented. Its ground rules (section 2), stack (section 3), and lightweight targets (section 1.2) still apply.
> **Phase 7 also requires** Phase 2's hook infrastructure (`anveg-hook` and the status socket).
>
> **Revisions**
> - 2026-11-23: The owner asked for Phases 4–6 only. **Phase 7 is postponed** to the next round; it is tracked in `docs/TODO.md`.
> - Decisions taken while building are recorded in `docs/PROGRESS.md`, which overrides this plan where they differ.

| Phase | Adds |
|---|---|
| 4 | File tree and code editor (gpui-component editor, syntax highlighting, LSP) |
| 5 | Git panel: status, diffs, staging, commits, branches |
| 6 | GitHub through `gh`: pull requests and CI checks |
| 7 | *(postponed)* "What did this session change": a diff review per Claude session |

---

## 0. Shared foundations

Build these at the start of Phase 4 and Phase 5. Phases 5–7 all reuse them.

### 0.1 Layout changes

- **Left sidebar:** a tab strip, **Sessions | Files | Git | GitHub**. The GitHub tab appears only when `gh` is available (6.1).
- **Right side:** one tab bar for everything opened there: Claude sessions, terminals, files, diffs, pull requests, and session change reviews. Use the component library's dock and tabs (resizable splits, draggable tabs).
- Save the layout and open tabs in `state.json`, and restore them on launch. Restoring is **lazy**: tabs come back as placeholders and load their content when first shown.

### 0.2 `ProcessRunner` (in `anveg_core`)

- Runs `git`, `gh`, and language servers with the captured login environment and a working directory.
- Has a timeout, can be cancelled, captures stdout and stderr, and can decode JSON output.
- Never runs on the UI thread.
- **Commands that change a repository run one at a time per repository.** This avoids `index.lock` conflicts.

### 0.3 `FsWatchService`

- A single shared `notify` watcher. Callers subscribe and unsubscribe by path.
- **Watch only what is visible or needed:** expanded folders, open files, and the `.git` metadata of repositories whose panel is open.
- Debounce events.
- Claude edits files constantly, so this service must handle bursts of events without spinning the CPU.

### 0.4 `DiffView` component

Built in Phase 5 and reused in Phases 6 and 7.

- Unified diff (a side-by-side mode can come later), with the editor's syntax highlighting, hunk headers, and a space for per-hunk action buttons.
- Virtualized: draws only the visible lines.
- **Large-diff guard:** above 5,000 lines, show "Large diff — Show anyway".

### 0.5 Lightweight rules (non-negotiable)

- Load everything lazily.
- **Language servers** start only when a file in their language is opened, and stop after 10 minutes with no open files in that language.
- **File watchers** cover only expanded folders and open repositories.
- **GitHub data** refreshes only while a GitHub view is visible.
- **No `git` or `gh` processes run while the app is idle.**

### 0.6 Licensing

- `git`, `gh`, and language servers run as **separate processes**, so their licenses don't affect Anveg Code.
- Tree-sitter grammar crates must pass `cargo deny`. Most are MIT or Apache-2.0.
- Longbridge's component library (including its editor) is Apache-2.0.
- **Never copy code from Zed's GPL crates** (for example `editor`, `git_ui`, `project_panel`, `terminal_view`). Reading them is fine.

---

## Phase 4 — File tree and code editor

### 4.1 File tree (sidebar "Files" tab)

- **Roots:** the main folder and every extra folder, each as a top-level node.
- **Lazy loading:** a folder is listed only when it is expanded.
  - Respect `.gitignore` and `.ignore`, and hide `.git`, using the `ignore` crate.
  - A toggle shows ignored files.
- **Live updates:** watch expanded folders only (0.3). The tree updates as Claude creates, renames, and deletes files.
- **Actions:** open, New File, New Folder, Rename, Delete (moves to the Trash, with confirmation), Reveal in Finder, Copy Path, Copy Relative Path.
- **Mention in Claude:** types `@<relative path> ` into the active Claude session's prompt. It never presses Enter.
- **Git status colours** on file names, once Phase 5 exists.
- **Quick open (⌘P):** fuzzy file finder. The index is built lazily on first use, respects ignore files, and is updated by the watcher.

### 4.2 Code editor

**Component:** the gpui-component editor in code-editor mode. It provides Tree-sitter syntax highlighting, LSP integration points, and has been tested up to about 200K lines.

**Editing features:** line numbers; find and replace (⌘F while the editor is focused); go to line (⌘L); soft-wrap toggle; multiple cursors, if the component supports them.

**Languages:** bundle grammars for Rust, TypeScript/TSX, JavaScript, Python, Go, JSON, TOML, YAML, Markdown, HTML, CSS, Bash, Swift, and C/C++. Keep the list configurable.

**File handling:**
- UTF-8 only. Binary or non-UTF-8 files open as read-only, with a notice.
- Preserve line endings and the trailing newline. Detect the indentation style.
- Show a dirty marker for unsaved changes.
- ⌘S saves (atomic write). File ▸ Save All saves every open file.
- Closing a tab or window with unsaved changes asks first.

**External changes.** This matters a lot here, because Claude edits files while they are open.
- If the buffer has **no** unsaved changes: reload silently, keeping the cursor and scroll position.
- If it **does**: show a banner, *"This file changed on disk"*, with **Reload**, **Keep Mine**, and **Compare** (Compare opens a `DiffView`).

**Large files:** above 200K lines or 20 MB, open as read-only plain text with a warning.

**Markdown:** a preview toggle, using the component library's Markdown renderer.

**Links from terminals:** ⌘-click on `path:line[:col]` in any terminal (Claude output or the shell) opens that file at that line.

**Ask Claude about selection:** types a reference to the selected lines into the active Claude session's prompt, without pressing Enter. Check which file-and-line mention syntax the installed Claude Code supports. If it has none, insert `path:10-20`.

### 4.3 LSP (new crate `anveg_lsp`)

**Client:** JSON-RPC over stdio, using `lsp-types`. One server per (project, language). Its workspace folders are the project's roots.

**Server commands:** configurable in Settings, with these defaults: `rust-analyzer`, `typescript-language-server --stdio`, `pyright-langserver --stdio`, `gopls`.

Servers are found on the captured `PATH`. If one is missing, that language simply has no LSP, and a small hint says what to install.

**Features**, connected to the editor through the component library's provider interfaces: diagnostics (underlines, gutter markers, and a Problems list); completion; hover; go to definition (opens the file); format document (⌥⇧F); format on save (setting, default off).

**Keeping servers in sync:** send `didOpen`, `didChange` (incremental), `didSave`, and `didClose`. Also send `workspace/didChangeWatchedFiles` from `FsWatchService`, so that Claude's edits to files that aren't open are seen too.

**Server lifecycle:** start a server when the first file in its language opens. Send `shutdown` then `exit` after 10 minutes with no open files in that language. After a crash, restart with backoff, at most 3 times.

### 4.4 Phase 4 acceptance

- Opening a 10,000-line file takes under 200 ms.
- No language server processes run when no files are open.
- Expanding folders in a 50,000-file repository feels instant, because loading is lazy.
- External edits by Claude appear in clean buffers within 1 s.

---

## Phase 5 — Git panel (sidebar "Git" tab)

### 5.1 Approach
- **Use the `git` command-line tool for everything.** It honours the user's config, hooks, commit signing, and credential helpers, and has no licensing implications.
- Consider `gix` for faster read-only status only if measurements show the CLI is too slow.

### 5.2 Repositories
- For each project root, find its repository with `git rev-parse --show-toplevel`.
- A project can contain several repositories. The panel groups its contents by repository.
- Show linked worktrees with their branch names.

### 5.3 Status
- Run `git status --porcelain=v2 -z --branch`.
- Show sections **Staged**, **Changes**, **Untracked**, and **Conflicts**, plus the current branch and how far it is ahead of or behind its upstream.
- **Refresh when:** `.git/index`, `.git/HEAD`, or `.git/refs/**` change; the working tree changes (debounced to 300 ms); the window gains focus.
- **No polling.**

### 5.4 Diffs
- Clicking a file opens a `DiffView` built from `git diff [--cached] -- <file>`.
- Untracked files are shown as all-new.
- Binary files show "Binary file changed".

### 5.5 Staging and discarding
- **Stage or unstage a file:** `git add` / `git restore --staged`.
- **Stage or unstage a hunk:** build a patch, then run `git apply --cached` (with `-R` to unstage).
- **Stage All.**
- **Discard changes** (whole file or one hunk) always asks for confirmation. **Before discarding, copy the current file to the Trash**, so the user can recover it; `git restore` itself can't be undone.

### 5.6 Commit
- A message editor with a summary line and a body, plus a hint at 72 characters.
- **Commit** (⌘↩) commits staged changes. An **Amend** toggle amends the last commit.
- If nothing is staged, offer **Stage All and Commit**.
- The repository's own hooks (such as pre-commit) run as normal. If one fails, show its output.
- Commit signing works automatically through the user's git config.

### 5.7 Branches and remotes
**Branches:** list local and remote branches; **Checkout** (if the working tree has changes that block it, show git's own error message); **Create** from the current branch or the selected one; **Rename**; **Delete**, with confirmation. A branch with unmerged commits needs a second, explicit confirmation before it is force-deleted.

**Remotes:** **Fetch**; **Pull**, fast-forward only by default (a setting can change this); **Push** — the first push sets the upstream branch.

**Network rules:** always run with `GIT_TERMINAL_PROMPT=0`, so git never hangs waiting for a password. If authentication fails, show git's message and suggest setting up a credential helper, or running the command in the Terminal tab. Show progress, and make every network operation cancellable.

### 5.8 History
- The current branch's last 200 commits (`git log` with a custom format).
- Clicking a commit opens its diff in a `DiffView`.

### 5.9 Stash (end of phase, nice to have)
- List, apply, pop, and drop stashes.

### 5.10 Phase 5 acceptance
- In a 10,000-file repository, status refreshes within 300 ms of a change.
- No `git` processes run while the app is idle.
- Hunk staging works on files with CRLF and on files without a trailing newline.

---

## Phase 6 — GitHub through `gh` (sidebar "GitHub" tab)

### 6.1 Detecting `gh`
- At startup, run `gh --version` and `gh auth status`. If they failed, check again when a window gains focus.
- If `gh` is missing or not signed in, the GitHub tab shows only setup steps: `brew install gh`, then `gh auth login`.
- GitHub Enterprise hosts configured in `gh` work automatically.
- Find the repository with `gh repo view --json nameWithOwner,url`, run in the repository root.

### 6.2 Calling `gh`
- Always use `--json` with an explicit list of fields, and parse with `serde`.
- Treat missing fields as optional.

### 6.3 Pull request list
- **Filters:** Mine, Review Requested, All Open.
- **Command:** `gh pr list --json number,title,author,headRefName,isDraft,reviewDecision,statusCheckRollup,updatedAt` with `--search` for the filter.
- **Each row shows:** number, title, author, a draft badge, review state, and a summary icon for checks.

### 6.4 Pull request detail (opens as a tab on the right)
**Content:** title, author, and the source and target branches; the description, rendered as Markdown; checks and their status (`gh pr checks <n> --json …`); reviews and the overall review decision; changed files with diffs (`gh pr diff <n>`, shown in a `DiffView`); comments, read-only (`gh pr view <n> --json comments,reviews`).

**Actions:** Open in Browser, Copy Link; **Check Out** (`gh pr checkout <n>`; warn first if the working tree has changes); **Mark Ready for Review**; **Merge** (choose the method and confirm; if branch protection blocks the merge, show GitHub's message); **Close** (with confirmation).

### 6.5 Create a pull request
- Creates a PR from the current branch.
- Title and body are pre-filled from the branch's commits. Choose the base branch and whether it's a draft.
- Runs `gh pr create`, pushing the branch first if needed.

### 6.6 CI checks
**Current branch:** `gh run list --branch <branch> --json …`. The latest run's status is also shown in the Git panel header.

**Run detail:** jobs and their status; **Show Failed Logs** (`gh run view <id> --log-failed`, shown in a read-only text view); **Re-run Failed Jobs** (`gh run rerun <id> --failed`); **Open in Browser**.

### 6.7 Claude integration (small and optional)
- **Ask Claude to Fix**, on a failed check: saves the end of the failed log to a temporary file; types a prompt into the active Claude session that names the PR and the check and mentions that file with `@`; **never presses Enter**.
- **Open Linked Claude Sessions**, on a PR: `claude --from-pr <number>` opens Claude's session picker filtered to those sessions. Run that command in a new terminal tab.

### 6.8 Refresh rules
Refresh: when a GitHub view is shown; when the window gains focus; when the user clicks ⟳; **every 60 s, but only while a GitHub view is visible and its window has focus.** If GitHub reports rate limiting, show a message and back off.

### 6.9 Phase 6 acceptance
- No `gh` processes run while GitHub views are hidden.
- The PR list loads in under 2 s for a typical repository.

---

## Phase 7 — "What did this session change" *(postponed — see `docs/TODO.md`)*

The full specification of Phase 7 (sections 7.1–7.4 and milestones M33–M36) is kept in `docs/TODO.md`.

---

## Milestones

| Phase | Milestone | Content |
|---|---|---|
| 4 | M19 | Layout refactor: sidebar tabs, right-side dock and tabs, lazy tab restore |
| 4 | M20 | File tree (4.1), plus `FsWatchService` |
| 4 | M21 | Editor without LSP (4.2): open, edit, save, highlighting, large-file guard |
| 4 | M22 | External-change handling, terminal path links, Mention in Claude / Ask Claude |
| 4 | M23 | LSP client and lifecycle (4.3) |
| 5 | M24 | `ProcessRunner`, finding repositories, status (5.1–5.3) |
| 5 | M25 | `DiffView` (0.4) and file diffs (5.4) |
| 5 | M26 | Staging, hunk staging, discard, commit (5.5–5.6) |
| 5 | M27 | Branches and remotes (5.7) |
| 5 | M28 | History and stash (5.8–5.9) |
| 6 | M29 | `gh` detection and PR list (6.1–6.3) |
| 6 | M30 | PR detail and actions (6.4) |
| 6 | M31 | Create PR and CI runs (6.5–6.6) |
| 6 | M32 | Claude integrations and refresh rules (6.7–6.8) |
| 7 | M33–M36 | *Postponed* (`docs/TODO.md`) |

Each milestone ends the same way as in the build plan: build, clippy, tests, `cargo deny`, and a `PROGRESS.md` entry with a manual test checklist.

---

## References

- Claude Code hooks reference: https://code.claude.com/docs/en/hooks
- Claude Code sessions (`--from-pr`, storage, retention): https://code.claude.com/docs/en/sessions
- Claude Code CLI reference: https://code.claude.com/docs/en/cli-reference
- Longbridge GPUI components (editor, dock, Markdown): https://github.com/longbridge/gpui-component
- GitHub CLI manual: https://cli.github.com/manual/
- git porcelain v2 status format: `git help status`, section "Porcelain Format Version 2"
