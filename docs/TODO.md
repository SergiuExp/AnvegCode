# TODO — next phase

Work that was planned but postponed. Each item keeps its full specification so it can be picked up without the original plan.

## Phase 7 — "What did this session change" (per-session diff review)

*Postponed by the owner on 2026-11-23: "implement only 4–6, postpone 7 to the next phase". Milestones M33–M36.*

**Builds on:** Phase 2's `anveg-hook` and status socket; Phase 5's `ProcessRunner` (`anveg_core::process`), git helpers (`anveg_core::git`) and `DiffView` (`anveg_app::diff_view`); Phase 4's editor ("Open in Editor") and Claude prompt typing (`ProjectWindow::type_into_claude`).

### 7.1 Goal

For any Claude session, show **which files it changed and how**, let the user review the diffs, and let them revert or stage parts of them. This must work even when several sessions work in the same repository at the same time.

### 7.2 Collecting the data

**1. Baseline snapshot (git repositories only).**
- The first time the app starts a session, record a snapshot of the working tree. This must not touch the user's index or stash:
  ```
  GIT_INDEX_FILE=<temporary index> git add -A
  GIT_INDEX_FILE=<temporary index> git write-tree      # prints a tree hash
  ```
  The snapshot includes untracked files that aren't ignored.
- Store `{session_id, repo, baseline_tree, head_commit, time}` in the app's data.
- **On each resume,** also record a *resume point* (a tree hash taken the same way), so the user can see changes since the last resume.

**2. Which files the session edited.**
- Add `PreToolUse` and `PostToolUse` hooks that match Claude Code's file-editing tools (currently `Edit`, `Write`, and `NotebookEdit`; **check the current tool names in the Claude Code tools reference**).
- `anveg-hook` forwards `tool_input.file_path` to the app, which keeps a per-session list of edited files.

**3. Before-images (exact per-session diffs).**
- On `PreToolUse`, if this session hasn't edited the file before, **`anveg-hook` itself** copies the file's current content into a content-addressed store: `Application Support/Anveg Code/snapshots/<sha256>`.
- It then reports the hash to the app.
- The hook does the copy itself, without waiting for the app, because Claude waits for `PreToolUse` hooks to finish before editing. Keep it fast (under 30 ms for normal files). Skip files over 20 MB and record them as "too large to snapshot".
- This produces correct diffs even for files outside git, and for files that other sessions also edit.

**4. Changes made through shell commands.** Formatters, code generators, `git mv`, and similar commands run through Claude's Bash tool can't be attributed to individual files by hooks. The "Everything since session start" scope (7.3) covers them, labelled *"may include changes made by you or other sessions"*.

**5. Folders that aren't git repositories** use before-images only.

**Storage hygiene:**
- Snapshots are de-duplicated by hash.
- A session's snapshots are deleted when the session is deleted, or after 30 days, matching Claude Code's default retention.
- Total size is capped by a setting (default 500 MB). When over the cap, delete the oldest snapshots first.

**Git garbage collection:** baseline trees aren't referenced by any branch or tag, so `git gc` may delete them after git's prune expiry (two weeks by default).
- If a baseline tree has disappeared, fall back to before-images and show a note.
- **Do not create refs in the user's repository** by default.

### 7.3 User interface

- **Sidebar row badge:** "N files changed", for sessions that edited files.
- **Opening the review:** a context-menu item **Review Changes**, and a **Changes** tab next to the Claude and Terminal tabs for the selected session.
- **Changes view:**
  - A file list with status (added, modified, deleted) and +/− line counts.
  - A scope switch:
    - **Files this session edited** (default): before-image → current file;
    - **Everything since session start**: baseline tree → working tree;
    - **Since last resume**: resume-point tree → working tree.
  - A `DiffView` per file.
- **Actions** on a file or hunk:
  - **Revert** to the before-image, with confirmation. The current version is copied to the Trash first.
  - **Stage** (git repositories only).
  - **Open in Editor** at that line.
  - **Ask Claude About This:** types a reference to the file and lines into *that session's* prompt, without pressing Enter.
- **Overlap warning:** if another session also edited the file after this session's baseline, show *"Also edited by <session name>"* and explain that the diff may include those changes.

### 7.4 Acceptance

- Hooks add less than 30 ms to each file edit.
- A 50-file review opens in under 500 ms.
- When no session is active, nothing runs.
- Reverting a hunk restores exactly the before-image content for that hunk.

### Milestones

| Milestone | Content |
|---|---|
| M33 | **Spike:** confirm Claude waits for `PreToolUse` hooks, confirm the current file-editing tool names, measure hook overhead |
| M34 | Baselines, edited-file tracking, before-image store (7.2) |
| M35 | Changes view and scopes (7.3) |
| M36 | Revert and stage actions, overlap warnings, storage limits |

## Smaller items left open

- Confirm the bundle identifier (`dev.anveg.code` is a placeholder in `scripts/bundle-macos.sh`).
