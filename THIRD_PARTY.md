# Third-party components

Rust crate dependencies, and their licenses, are checked with `cargo deny check licenses` (policy in `deny.toml`). This file lists components that ship with the app beyond ordinary crate code, and components whose license needs a note.

| Component | Where it comes from | License | Notes |
|---|---|---|---|
| libghostty-vt 1.0.1 (Ghostty terminal core) | `ghostty-vt-sys`: statically linked prebuilt XCFramework | MIT | Also ships Ghostty's terminfo entry (`xterm-ghostty`) and shell-integration scripts, which are written to the app's resources at runtime. |
| gpui-cn terminal (portions derived from Herdr and Muxy) | `gpui-cn` crate | Apache-2.0; Muxy portions MIT | See `LICENSE-MUXY` in the gpui-cn crate. |
| Lucide icons | `gpui-kit-assets` crate | ISC | Embedded SVG icons. |
| GPUI | `gpui-pre` crates (Zed) | Apache-2.0 | Only the Apache-2.0 GPUI crates are used; no Zed GPL code. |
| Light terminal palette | `crates/anveg_term/src/lib.rs` | Apache-2.0 (this project) | Written for Anveg Code; the dark palette is Ghostty's default. |
| gpui-component (editor, diff view, Markdown, tree-sitter highlighting) | `gpui-component` crate via `gpui-kit` | Apache-2.0 | Bundled tree-sitter grammars (Rust, TypeScript/TSX, JavaScript, Python, Go, JSON, TOML, YAML, Markdown, HTML, CSS, Bash, Swift, C, C++, diff) are MIT crates; their highlight queries ship inside the binary. |
| Language servers, `git`, `gh` | Installed by the user | Various | Run as separate processes; never bundled. |
| Transcript fixtures | `tests/fixtures/transcripts/` | Apache-2.0 (this project) | Recorded from Claude Code 2.1.330 test sessions and sanitized (no attachments, no personal data). |

## Licenses beyond the default allow-list

These were outside the default allow-list (build plan, section 3.2). The owner approved them on 2026-11-23:

| Crate | License | Pulled in by | Ships in the binary? |
|---|---|---|---|
| `nucleo-matcher` 0.3 | MPL-2.0 | `gpui-cn` (fuzzy matching) | yes |
| `option-ext` 0.2 | MPL-2.0 | `dirs-sys` ← `directories` / `dirs` | yes |
| `cbindgen` 0.28 | MPL-2.0 | build dependency of `gpui-pre-apple` | no (build tool) |
| `webpki-root-certs` 1.0 | CDLA-Permissive-2.0 | `ureq` ← build script of `ghostty-vt-sys` | no (build tool) |
| `enum-iterator` 2.3, `enum-iterator-derive` 1.5 | 0BSD | `gpui-component` (Phase 4 editor) | yes |
