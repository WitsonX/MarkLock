<div align="center">

# MarkLock

**Encrypted Markdown viewer & editor** · local-first · zero upload

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-lightgrey)
[![Release](https://img.shields.io/github/v/release/WitsonX/MarkLock?label=release)](https://github.com/WitsonX/MarkLock/releases/latest)

[简体中文](README.md) · **English**

</div>

---

A Markdown tool that lives on your machine and reads plain `.md` like any editor — but **everything is ciphertext the moment it hits the disk**. Notes, finances, and plans you'd rather not hand to anyone can be locked into a single encrypted file and carried around; everyday notes stay ordinary `.md`.

All key material is derived and used inside Rust. **Keys never leave the device, never enter JavaScript, never get uploaded** — no account, no cloud, no network call anywhere in the encryption path.

## Why this one

- **A vault is one file** — the whole tree, filenames and folder names included, encrypts into a single `.mdlb`. Drop it in iCloud, Dropbox, or an external drive; anyone who opens it sees an opaque blob.
- **Changing the password doesn't re-encrypt** — the master password only wraps the data key, so switching it is instant even on a huge vault.
- **Locking means amnesia** — idle timeout, screen lock, or sleep zeroizes the keys in memory, and tabs from a locked vault relabel themselves to `vault file` so even filenames stop leaking.
- **Small** — the default Windows installer is 3.5MB, macOS ships a single small app. It uses the system WebView instead of bundling a browser.
- **A note always at hand** — Sticky Notes pins one `.md` / `.mdl` file to a screen corner or the tray: hover, write, save — and what you save is still encrypted.
- **Open source and auditable** — Apache-2.0, the crypto core is covered by unit tests. Read the source and verify every claim above.

## Download

Grab an installer from the [Latest Release](https://github.com/WitsonX/MarkLock/releases/latest) — pick the file matching your platform. From mainland China the [Gitee release page](https://gitee.com/Witson/MarkLock/releases) downloads faster and carries every installer except the offline build.

- The default Windows build is only 3.5MB (it downloads the WebView2 runtime while installing). If the machine has no WebView2 and can't reach Microsoft's CDN, use `*-setup-offline.exe` instead (about 209MB, no network needed). Windows 11 and updated Windows 10 already ship the runtime, so the default build is fine.
- The offline build is GitHub-only (Gitee caps attachments at 100MB per file). On a Gitee machine, install the official [WebView2 runtime](https://go.microsoft.com/fwlink/?linkid=2124701) first, then use the small build.
- **Portable builds**: want whatever is on `master` right now? Grab an unpacked build from the [canary release](https://github.com/WitsonX/MarkLock/releases/tag/canary) — a `.app` in a zip on macOS, a bare `MarkLock.exe` on Windows. Every push to `master` rebuilds it under the same file names, and the About card shows `Canary <commit>`. GitHub-only, and it tracks unreleased code — stick to the stable installer if you just want it to work.

> Installers carry an ad-hoc signature only — they aren't notarized by Apple, so the first-launch "Apple cannot check it" prompt is expected. One-time bypass: click **Done**, then in **System Settings → Privacy & Security → Security** click **Open Anyway** and confirm with your password — the same location won't ask again (on some builds right-click → **Open** works too). If macOS says the app is **"damaged"**, it's an older build without a resource seal — run `xattr -dr com.apple.quarantine /Applications/MarkLock.app`. Windows binaries are unsigned too: click **More info → Run anyway** on SmartScreen.

## Interface

**Unlock**

![unlock](doc/screenshots/unlock.png)

**Editor** (split preview + vault tree in the sidebar)

![editor](doc/screenshots/editor.png)

**Settings → Security & Encryption**

![settings](doc/screenshots/settings.png)

**Sticky Notes** (left: the note window in preview mode with its app launcher; right: Settings → Sticky Notes)

![sticky](doc/screenshots/sticky.png)

## Core concepts

| Concept | Description |
|---|---|
| Vault `.mdlb` | **MarkLock Bundle** — a single-file container holding an encrypted file tree. Filenames, directory structure, and contents are all ciphertext. One opaque blob you can sync anywhere. |
| File `.mdl` | **MarkDown Lock** — a single encrypted Markdown file, usable standalone (e.g. `investment-notes.mdl` on your Desktop). |
| Master password | Lives only in your head. Derived into a KEK via Argon2id. Never written to disk, never transmitted. |
| DEK | Random data key, wrapped by the KEK and stored in the file header. Changing your master password only re-wraps the DEK — contents are never re-encrypted. |

## Features

### Encryption & vaults

- Create and open vaults, with password hints, a recent list, and failed-attempt throttling
- Arbitrarily nested folders inside a vault — create, rename, move, delete; unlock once and every file in it reuses the session
- Standalone `.mdl` files keep their own password, for the "just this one" case
- Plain `.md` opens and edits like normal, with right-click **Convert to encrypted file** (optionally deleting the plaintext); whole directories can be batch-encrypted
- Change master password, switch the cipher scheme (full re-encryption), export to plaintext (double-confirmed as destructive)

### Editing & browsing

- CodeMirror 6 source editing with Markdown highlighting, edit / split / preview modes, formatting toolbar
- Autosave with 800ms debounce, encrypted on write; can be turned off
- Full-text search (decryption happens in memory only), outline navigation, starring, HTML export
- VSCode-style sidebar — open files, open vaults, starred, recent, working directory; groups are reorderable and individually toggleable; switch documents by tabs or by list
- Browse the local filesystem tree in the working-directory view, with vaults shown as atomic nodes
- External-change watching reloads a file when another program rewrites it; if you have unsaved edits it only raises an amber badge instead of clobbering your work

### Sticky Notes

- Pin any `.md` / `.mdl` file as a sticky note, summoned by clicking the tray (menu-bar) icon
- Or use a **hot corner**: hold the pointer in a chosen screen corner for a moment; optionally require a modifier key so brushing past the corner never triggers it
- Edit / split / preview in one small window, saved straight to ciphertext; if the vault is locked you can type the password right in the note
- A row of local app shortcuts at the bottom doubles it as a launcher (icon-only or icon + label)

### Security & polish

- Idle auto-lock (5 min default), lock on minimize / sleep / screen lock, key material zeroized on lock, clipboard cleared after 60s
- Skin system: on top of light / dark / follow-system there are six color skins (Nord, Nocturne, Catppuccin, Sepia, Solarized, Gruvbox), switchable from the status bar; a skin covers the UI, the source editor, the Markdown preview and exported HTML, and advanced users can build their own (per-token palette plus free-form CSS)
- English / Chinese UI, adjustable editor font size plus preview font and spacing
- Full keyboard shortcut set: save, tab navigation (`⌘1…8` jumps to the Nth tab), in-editor find & replace, global search, view-mode switching, formatting, lock-everything
- One click to make MarkLock the default handler for `.md` / `.mdl` / `.mdlb`
- Optional launch-at-login (can start silently in the background), close-to-tray
- Restore the previous session on start; silent update checks with a one-click link to the release page

## Technical docs

Curious why filenames themselves are invisible in the ciphertext, why a password change skips re-encryption, or want to run and modify the code? Start here:

- **[doc/TECHNICAL.en.md](doc/TECHNICAL.en.md)** — on-disk format and key hierarchy, Rust crypto modules, the Tauri command surface, how Sticky Notes and hot corners work, dev setup, packaging, and Windows / ARM64 build errors
- **[doc/ARCHITECTURE.en.md](doc/ARCHITECTURE.en.md)** — architecture design and the feature evolution timeline
- **[doc/ui-prototype/](doc/ui-prototype/)** — static high-fidelity prototypes, no dependencies, open `index.html` in a browser

The Chinese originals ([doc/TECHNICAL.md](doc/TECHNICAL.md), [doc/ARCHITECTURE.md](doc/ARCHITECTURE.md)) are maintained in parallel; they are the authoritative versions if the two ever disagree.

In one line: **Tauri 2 shell + Vue 3 (Options API) + Ant Design Vue + CodeMirror 6, with the crypto core in Rust (`aes-gcm`, `argon2`, `zeroize`)**. Tauri over Electron buys a tiny installer, the system WebView, and real Rust for the encryption.

## Quick start

```bash
npm ci              # install frontend dependencies
npm run dev:tauri   # frontend + Tauri shell together (needs Rust)
npm run dev:app     # macOS: build a debug app and launch it — tray / sticky notes work
cd src-tauri && cargo test --lib   # crypto core unit tests
```

Prerequisites (macOS needs Xcode Command Line Tools; Windows needs both Rust and the "Desktop development with C++" workload from VS Build Tools) and per-error troubleshooting are in [doc/TECHNICAL.en.md](doc/TECHNICAL.en.md#development-and-debugging).

## Roadmap

1.0 already covers both encrypted forms (vault and single file), the CodeMirror editor, full-text search, starring, outline, external-change watching, Sticky Notes, and packaged distribution.

Requests and feedback belong in [Issues](https://github.com/WitsonX/MarkLock/issues).

## Contributing

Issues and PRs are welcome. For larger changes, please open an issue first to align on direction.

- Dev environment and run commands: [doc/TECHNICAL.en.md](doc/TECHNICAL.en.md#development-and-debugging)
- Frontend components use the **Options API** exclusively (no `<script setup>`, no Composition API)
- Run `cargo test --lib` in `src-tauri/` before submitting to confirm no crypto regressions

## Support the project

MarkLock is free and open source. If it helps you:

- ⭐ Star this repo so more people find it
- 🐛 File issues for bugs and feature requests
- ☕ Buy the author a coffee

<!-- Uncomment after placing QR images under doc/donate/ — see doc/assets/README.md
| WeChat Pay | Alipay |
|---|---|
| ![wechat](doc/donate/wechat.png) | ![alipay](doc/donate/alipay.png) |
-->

## License

[Apache License 2.0](LICENSE)

## Acknowledgements

- [Tauri](https://tauri.app/) — lightweight, secure desktop shell
- [Vue](https://vuejs.org/) · [Ant Design Vue](https://antdv.com/) — frontend and components
- [CodeMirror](https://codemirror.net/) — Markdown editor
- [aes-gcm](https://github.com/RustCrypto/AEADs) · [argon2](https://github.com/RustCrypto/password-hashes) · [zeroize](https://github.com/RustCrypto/traits/tree/master/zeroize) — crypto primitives and memory zeroization
