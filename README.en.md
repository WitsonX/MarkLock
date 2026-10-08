<div align="center">

# MarkLock

**Encrypted Markdown viewer & editor** · local-first · zero upload

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-lightgrey)
[![Release](https://img.shields.io/github/v/release/WitsonX/MarkLock?label=release)](https://github.com/WitsonX/MarkLock/releases/latest)

[简体中文](README.md) · **English**

</div>

---

MarkLock is a desktop app built with Tauri 2 — Vue 3 + Ant Design Vue on the frontend, Rust for the shell and the entire crypto core.

Every byte hits the disk already encrypted. **Keys never leave the device, never enter JavaScript, never get uploaded.**

> 📐 Full architecture design and evolution timeline: **[doc/ARCHITECTURE.md](doc/ARCHITECTURE.md)** (Chinese).

## Download

Grab an installer from the [Latest Release](https://github.com/WitsonX/MarkLock/releases/latest) — the three regular installers are also attached to the [Gitee release page](https://gitee.com/Witson/MarkLock/releases), which downloads faster from mainland China:

| Platform | File |
|---|---|
| macOS (Apple Silicon / Intel) | `MarkLock_<ver>_aarch64.dmg` / `MarkLock_<ver>_x64.dmg` |
| Windows (x64) | `MarkLock_<ver>_x64-setup.exe` |
| Windows (x64, WebView2 bundled — GitHub only) | `MarkLock_<ver>_x64-setup-offline.exe` |

The default Windows build is only 3.5MB and downloads the WebView2 runtime while installing. If the target machine has no WebView2 and cannot reach Microsoft's CDN, the install aborts halfway — use `*-setup-offline.exe` there instead (about 209MB, the runtime ships inside it, no network needed). Windows 11 and updated Windows 10 already have the runtime, so the default build is fine.

The offline build is hosted on GitHub only: Gitee caps release attachments at 100MB per file, so a 209MB asset cannot be uploaded there. On a machine pulled from Gitee, install the WebView2 runtime first — Microsoft's official 64-bit standalone installer is at https://go.microsoft.com/fwlink/?linkid=2124701 — then use the small build.

> Installers carry an ad-hoc signature only — they are not notarized by Apple, so the first launch is blocked with "MarkLock cannot be opened because Apple cannot check it for malicious software". That is the standard un-notarized flow, not a malware finding. One-time bypass:
>
> 1. Click **Done** on that dialog;
> 2. Open **System Settings → Privacy & Security**, scroll to the *Security* section, find "MarkLock was blocked to protect your Mac" and click **Open Anyway**;
> 3. Confirm with your password — the app launches and the same install location won't ask again.
>
> Recent macOS versions dropped the "App anywhere" option, so the route above is the reliable one; on some builds right-clicking `MarkLock.app` → **Open** in Finder shows the same approval prompt.
> If macOS instead says the app is **"damaged"**, you grabbed an older build without a resource seal — clear the quarantine flag with `xattr -dr com.apple.quarantine /Applications/MarkLock.app`.
> Windows binaries are unsigned too: click **More info → Run anyway** on the SmartScreen prompt.

## Core concepts

| Concept | Description |
|---|---|
| Vault `.mdlb` | **MarkLock Bundle** — a single-file container holding an encrypted file tree. Filenames, directory structure, and contents are all ciphertext. One opaque blob you can drop into iCloud, Dropbox, or an external drive. |
| File `.mdl` | **MarkDown Lock** — a single encrypted Markdown file, usable standalone (e.g. `investment-notes.mdl` on your Desktop). |
| Master password | Lives only in your head. Derived into a KEK via Argon2id. Never written to disk, never transmitted. |
| DEK | Random per-vault data key, wrapped by the KEK and stored in the file header. Changing your master password only re-wraps the DEK — no re-encryption of contents. |

## Features

- **Vaults as single files** — arbitrary nested folders inside; unlock once and every file in the vault reuses the session, no repeated password prompts
- **Editor** — CodeMirror 6 with Markdown syntax highlighting, edit / split / preview modes, formatting toolbar, autosave (800 ms debounce, encrypted on write)
- **Workspace** — multiple vaults and single files open at once; VSCode-style sidebar (open files, open vaults, starred, recent, working directory); tabs or list switching
- **Privacy on lock** — once a vault locks, its open tabs, starred entries, and recent items show as `vault file` instead of leaking real filenames; contents and names restore on unlock
- **External-change watching** — reloads when another program rewrites an open file (2 s polling plus on-focus checks); unsaved edits get an amber badge instead of being clobbered
- **Full-text search** — decryption happens in memory only
- **Security** — idle auto-lock (5 min default), lock on sleep/screen lock, key material zeroized on lock, clipboard cleared after 60 s
- **Extras** — outline, starring, keyboard shortcuts, context menus, HTML export, plaintext export (double-confirmed as a destructive action)

## Cryptography

```
master password + salt ──Argon2id──▶ KEK (32 B, zeroized on drop)
                                        │  AES-256-GCM
                                        ▼
                                wrapped_dek ──▶ DEK (32 B, random)
                                        │  AES-256-GCM
                                        ▼
                                  Markdown source (ciphertext)
```

- **AEAD**: AES-256-GCM — authenticated, tamper-evident
- **KDF**: Argon2id (m = 64 MiB, t = 3, p = 4) — GPU/ASIC resistant
- **Memory hygiene**: `zeroize` wipes key material on drop
- **On-disk layout** (both `.mdl` and `.mdlb`): `[4-byte header length][header JSON][nonce][ciphertext]`, distinguished by the `format` field (`mlk/1` vs `mlk-vault/1`)
- **Path traversal guarded**: in-vault paths reject absolute paths and `..`
- Unlock is under 300 ms for personal-note-scale vaults

All crypto runs in Rust. The frontend only ever sees plaintext views and ciphertext bytes — never key material.

## Tech stack

| Layer | Choice | Why |
|---|---|---|
| Shell | Tauri 2 | Far smaller than Electron, system WebView, native Rust crypto |
| Frontend | Vue 3 + Vite + TypeScript | Options API components, mature ecosystem |
| UI kit | Ant Design Vue | Good fit for a desktop tool |
| Editor | CodeMirror 6 | Markdown highlighting, source mode, search / folding / completion |
| Crypto | `aes-gcm`, `argon2`, `zeroize` | Everything key-related stays in Rust |

## Interface

**Unlock**

![unlock](doc/screenshots/unlock.png)

**Editor** (split preview + vault tree in the sidebar)

![editor](doc/screenshots/editor.png)

**Settings → Security & Encryption**

![settings](doc/screenshots/settings.png)

## Development

### Prerequisites

| | macOS | Windows |
|---|---|---|
| Rust | rustup (stable) | rustup-init.exe (stable, MSVC) |
| Toolchain | Xcode Command Line Tools | Visual Studio Build Tools, "Desktop development with C++" workload |
| Node | 18+ | 18+ |
| WebView | System WKWebView | WebView2 Runtime (preinstalled on Win 11 / recent Win 10) |

On Windows you need both Rust *and* Build Tools — rustup provides the compiler, `link.exe` comes from Build Tools.

### Run

```bash
npm ci             # install per package-lock.json (preferred, reproducible)
npm install        # or a regular install; commit the lock file when deps change
npm run dev        # Vite dev server only (http://localhost:1420)
npm run build      # build frontend into dist/
npm run dev:tauri  # frontend + Tauri shell together
```

The first compile builds roughly 400 crates — a few minutes on macOS, longer on Windows. Incremental builds after that are seconds.

On Windows, every new terminal must have the MSVC environment injected before compiling; the easiest fix is to always use "Developer Command Prompt for VS".

### Test

```bash
cd src-tauri
cargo test --lib   # key round-trips, wrong password, tamper detection,
                   # rewrap on password change, full vault lifecycles,
                   # path traversal defense, plaintext directory search
```

## Contributing

Issues and PRs are welcome. For larger changes, please open an issue first to align on direction.

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
