# Third-party components

Rust crate dependencies, and their licenses, are checked with `cargo deny check licenses` (policy in `deny.toml`). This file lists components that ship with the app beyond ordinary crate code, and components whose license needs a note.

| Component | Where it comes from | License | Notes |
|---|---|---|---|
| libghostty-vt 1.0.1 (Ghostty terminal core) | `ghostty-vt-sys`: statically linked prebuilt XCFramework | MIT | Also ships Ghostty's terminfo entry (`xterm-ghostty`) and shell-integration scripts, which are written to the app's resources at runtime. |
| gpui-cn terminal (portions derived from Herdr and Muxy) | `gpui-cn` crate | Apache-2.0; Muxy portions MIT | See `LICENSE-MUXY` in the gpui-cn crate. |
| Lucide icons | `gpui-kit-assets` crate | ISC | Embedded SVG icons. |
| GPUI | `gpui-pre` crates (Zed) | Apache-2.0 | Only the Apache-2.0 GPUI crates are used; no Zed GPL code. |

## Licenses pending the owner's approval

These appear in the dependency graph and are not in the default allow-list (build plan, section 3.2):

| Crate | License | Pulled in by | Ships in the binary? |
|---|---|---|---|
| `nucleo-matcher` 0.3 | MPL-2.0 | `gpui-cn` (fuzzy matching) | yes |
| `option-ext` 0.2 | MPL-2.0 | `dirs-sys` ← `directories` / `dirs` | yes |
| `cbindgen` 0.28 | MPL-2.0 | build dependency of `gpui-pre-apple` | no (build tool) |
| `webpki-root-certs` 1.0 | CDLA-Permissive-2.0 | `ureq` ← build script of `ghostty-vt-sys` | no (build tool) |
