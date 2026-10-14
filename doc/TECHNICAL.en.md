# MarkLock technical notes

Written for developers and anyone curious about the internals. For the product-level introduction see [../README.en.md](../README.en.md); the architecture evolution timeline is in [ARCHITECTURE.en.md](ARCHITECTURE.en.md).

- [Overall architecture](#overall-architecture)
- [Encryption design](#encryption-design)
- [Rust crypto core](#rust-crypto-core)
- [Tauri command surface](#tauri-command-surface)
- [Frontend structure and state](#frontend-structure-and-state)
- [Directory layout](#directory-layout)
- [How Sticky Notes works](#how-sticky-notes-works)
- [Development and debugging](#development-and-debugging)
- [Common build problems](#common-build-problems)
- [Coding conventions](#coding-conventions)

## Overall architecture

Three layers split by process boundaries — keys exist only in the innermost one:

```
┌─ WebView (Vue 3 + AntD, JS)                                    ┐
│  Sees only the plaintext view and ciphertext bytes, never keys │
│  src/stores/vault.ts  ← metadata (path / name / lock state)    │
│  src/lib/tauri.ts     ← invoke bridge layer                    │
└───────────────────┬────────────────────────────────────────────┘
                    │ Tauri IPC (commands + events)
┌───────────────────▼────────────────────────────────────────────┐
│ Rust shell (src-tauri/src/lib.rs)                              │
│  Windows / tray / menu bar / native menus / hot-corner polling │
│  SessionStore: keys stay in memory + auto-lock timer           │
├────────────────────────────────────────────────────────────────┤
│ Crypto core (src-tauri/src/crypto/)                            │
│  Argon2id derive → KEK → wrap/unwrap DEK → AES-GCM             │
└────────────────────────────────────────────────────────────────┘
```

- **Shell**: Tauri 2. Chosen over Electron because it is small (the Windows installer is 3.5MB by default), it uses the system WebView, and it lets the crypto core be written directly in Rust.
- **Frontend**: Vue 3 (Options API) + Vite + TypeScript + Ant Design Vue + CodeMirror 6.
- **Encryption**: entirely on the Rust side — `aes-gcm` / `argon2` / `zeroize`. JavaScript takes part in no key computation whatsoever.

## Encryption design

### Two ciphertext forms

| Form | Contents | Key |
|---|---|---|
| `.mdlb` single-file vault (MarkLock Bundle) | A single-file container holding an encrypted file tree (filenames + directory structure + bodies) | Vault master password; one DEK shared across the whole vault |
| `.mdl` encrypted file (MarkDown Lock) | The ciphertext of a single Markdown file | Its own password, its own DEK |

Making a vault a **single file** buys two direct benefits: on disk there is only one opaque blob — folder names and filenames are invisible too — and the whole blob can be dropped straight into iCloud / Dropbox / an external drive for syncing, with no directory-level consistency coordination needed.

### Disk layout

Both forms share one framing format, told apart by the `format` field in the header (`mlk-vault/1` vs `mlk/1`):

```
[4-byte header length][header JSON (plaintext)][nonce][ciphertext]
```

The header is plaintext and stores only public parameters:

```
format        : "mlk-vault/1" | "mlk/1"
kdf           : argon2id, m=64MB, t=3, p=4, salt
wrapped_dek   : the DEK wrapped by the KEK
```

The data area holds content encrypted with AES-256-GCM. For `.mdlb`, the plaintext is the JSON serialization of the whole file tree.

```
┌─ single-file vault (.mdlb)           ┐
│  header (plaintext JSON)             │
│    format / kdf / wrapped_dek        │
│  data area (ciphertext, AES-256-GCM) │
│    nonce + ct = JSON(file tree)      │
│      ├─ notes/first.md     "body..." │
│      ├─ project/plan.md    "body..." │
│      └─ ...                          │
└──────────────────────────────────────┘
```

### Key hierarchy

```
master password + salt ──Argon2id──▶ KEK (32B, Zeroize guarantees a wipe on drop)
                                │  AES-256-GCM
                                ▼
                        wrapped_dek ──▶ DEK (32B, random)
                                │  AES-256-GCM
                                ▼
                         file tree / Markdown body (ciphertext)
```

- **KDF**: Argon2id, `m=64MiB / t=3 / p=4`. Memory-hard parameters push the cost of GPU/ASIC brute force extremely high, and the price is about 300ms per unlock — acceptable.
- **AEAD**: AES-256-GCM. Authenticated encryption: if the ciphertext is rewritten or truncated from outside, opening is refused outright instead of yielding garbage.
- **KEK / DEK separation**: bodies are encrypted by a random DEK, and the DEK is wrapped by the KEK. **Changing the master password only re-wraps the DEK (`rewrap`) — contents are never re-encrypted**, so changing the password on a large vault is instant.
- **Memory zeroization**: both KEK and DEK implement `Zeroize` and are wiped actively on lock or session expiry, with no reliance on the GC.

### Read and write flow

- **Unlock**: master password + salt → Argon2id → KEK → unwrap the DEK → decrypt the whole file tree into memory. At personal-note volume the entire path stays under 300ms.
- **Write**: after unlocking, editing any file re-serializes and re-encrypts the whole tree and rewrites `.mdlb` in one piece. This "rewrite everything" is the simplification the single-file container buys — no incremental index and no fragment management — and at personal volume its cost is imperceptible.
- **Auto-lock**: `SessionStore` refreshes the activity timestamp on every read and write; on timeout (5 minutes by default) `reap_expired` zeroizes the matching keys, and closing a window calls `lock_all`. The frontend calls `reap_expired` every 30 seconds, and tabs from an expired vault fall back to a placeholder.

### Security defenses

- **Path traversal**: paths inside a vault reject absolute paths and `..` — `rel` can only address content inside the container.
- **Tamper resistance**: a failed GCM tag check refuses decryption.
- **Failed-attempt throttling**: after consecutive wrong passwords, attempts are rate-limited.
- **Clipboard**: decrypted content that was copied out is cleared automatically after 60 seconds; locking can clear the clipboard in one shot.

## Rust crypto core

`src-tauri/src/crypto/`:

| Module | Responsibility |
|---|---|
| `mod.rs` | Aggregates and re-exports the submodules |
| `error.rs` | `CryptoError`, one unified error type, serialized into messages the frontend can read |
| `format.rs` | `.mdl` / `.mdlb` format constants, header/body structures, and the serialization boundary |
| `keys.rs` | KEK derivation, DEK wrap / unwrap / re-wrap |
| `file.rs` | Serialization and deserialization of a standalone `.mdl` file |
| `session.rs` | The unlock session table `SessionStore`, auto-lock timing, key zeroization |
| `vault.rs` | Vault-level operations: create / unlock / read-write / add-delete-rename-move / change password / lock / search |

Unit tests (`cargo test --lib` under `src-tauri/`) cover: key round-trips, wrong passwords, tamper detection, re-wrap on password change, the full flow for directory vaults, folders inside a vault, the full flow for single-file vaults (add-delete-rename-move + change password + single-file-on-disk verification), path traversal defense, and plaintext-directory search. The CI release pipeline runs these tests before packaging.

## Tauri command surface

Wrapped for the frontend in `src/lib/tauri.ts`.

**Single-file vault `.mdlb`**

| Command | Description |
|---|---|
| `create_file_vault(path, password, hint?)` | Create a vault |
| `unlock_file_vault(vaultId, path, password, autoLockSecs?)` | Unlock — decrypts the whole file tree |
| `list_file_vault(vaultId, path)` | List the recursive tree (without bodies) |
| `read_file_vault(vaultId, path, rel)` / `write_file_vault(...)` | Read / write a file body inside the vault |
| `create_in_file_vault(vaultId, path, rel, content)` | Create a file |
| `create_folder_in_file_vault(vaultId, path, rel)` | Create a folder |
| `delete_in_file_vault(vaultId, path, rel)` | Delete a file / folder |
| `rename_in_file_vault(vaultId, path, old, new)` | Rename / move |
| `change_file_vault_password(vaultId, path, old, new)` | Change the master password (re-wraps the vault-wide DEK) |

**Single encrypted file `.mdl`**

| Command | Description |
|---|---|
| `create_file(path, password, content, hint?)` | Create an encrypted file |
| `unlock(vaultId, path, password, autoLockSecs?)` | Unlock |
| `read_file(vaultId, path)` / `write_file(vaultId, path, content)` | Read / write |
| `change_password(vaultId, path, old, new)` | Change the password |
| `lock(vaultId)` / `lock_all` | Lock one / lock all |
| `unlocked_count` / `reap_expired` | Status query / reclaim expired sessions |

**Other**

| Command | Description |
|---|---|
| `list_dir(dir)` | List one directory level (working-directory view) |
| `unlock_vault` / `create_vault` / `list_vault` / `create_in_vault` / `create_folder_in_vault` / `delete_in_vault` / `rename_in_vault` | Legacy directory-vault model, kept for compatibility |
| `openFileDialog` / `openDirDialog` / `saveFileDialog` / `saveFileVaultDialog` | File dialogs through `tauri-plugin-dialog` |

## Frontend structure and state

- **Views**: four screens in `src/views/` plus the sticky-note window; router in hash mode (`#/unlock`, `#/editor`, `#/settings`, `#/dialogs`, `#/sticky`).
- **State**: `src/stores/vault.ts` (Pinia) is the single owner of the registered vault list, open tabs, the active file, starred items, recents, and settings, persisted to localStorage (`marklock.recent` / `marklock.settings`). **Keys and plaintext never enter the store** — only metadata such as path / name / lock state.
- **Editor**: `src/components/CodeMirrorEditor.vue` wraps CodeMirror 6 and keeps one live instance per file so cursor and scroll position survive.
- **External-change watching**: `src/lib/filewatch.ts` is a module-level singleton that polls on-disk mtime / size every 2 seconds by default and checks immediately when the window regains focus. Files inside a vault are watched at container granularity, and only the files that are open get re-read.
- **Preview**: `src/lib/preview.ts` is the Markdown rendering layer shared by the main editor and Sticky Notes, with `src/components/MarkdownPreview.vue` as the matching component.
- **i18n**: `src/locales/{zh,en}.ts` — zh is the source text, en mirrors its structure one-to-one, and only user-facing copy is included.

## Directory layout

```
MarkLock/
├── package.json            # Vue/Vite/AntD/CodeMirror dependencies + tauri scripts
├── vite.config.ts          # outputs the build to dist, aligned with tauri frontendDist
├── src/                    # Vue 3 frontend (Options API, no <script setup>)
│   ├── main.ts             # mounts Vue + AntD + router + v-tip + theme/platform detection
│   ├── router/index.ts
│   ├── styles/global.css   # CSS-variable theming, ported from the prototype base.css
│   ├── components/         # TitleBar / CodeMirrorEditor / MarkdownPreview / EncIcon / VaultTreeNode / WorkdirNode / AboutCard
│   ├── stores/vault.ts     # Pinia store
│   ├── lib/                # tauri.ts bridge / filewatch.ts / preview.ts / fonts.ts / sticky.ts / appinfo.ts / update.ts / aboutDialog.ts / links.ts / platform.ts / splash.ts / tooltip.ts
│   ├── locales/            # zh.ts (source text) / en.ts
│   └── views/              # UnlockView / EditorView / SettingsView / DialogsView / StickyView
├── src-tauri/
│   ├── tauri.conf.json     # production release configuration
│   ├── tauri.dev.conf.json # development identity com.marklock.dev, can run beside the installed build
│   ├── tauri.win-offline.conf.json # WebView2 offline-installer override configuration
│   ├── capabilities/default.json
│   └── src/
│       ├── lib.rs          # windows/tray/menu bar/native menus/hot corners/sticky notes + crypto command registration
│       ├── crypto/         # see "Rust crypto core" above
│       ├── clipboard.rs    # clipboard read/write and the 60s auto-clear
│       ├── appinfo.rs      # app launcher entries: resolve local apps and their icons
│       ├── appinfo_win.rs  # Windows-side app information retrieval
│       └── assets.rs       # assets and file associations
├── scripts/                # gen-filetype-icons / gen-win-appicon / dev-vite.mjs
├── doc/                    # this document, ARCHITECTURE.md, ui-prototype/, screenshots/
└── .github/workflows/      # release.yml (three platforms) / release-win-offline.yml (offline package)
```

## How Sticky Notes works

Sticky Notes is an **independent lightweight window** that shares the same crypto sessions and preview rendering as the main window:

- **Window singleton**: created on the first summon and reused afterwards (create-once) — hidden rather than destroyed, so every pop-up doesn't rebuild the WebView and pay for it in latency and memory churn.
- **Summon paths**: clicking the tray (menu-bar) icon — left/right button behavior is selectable in settings, and on macOS the left click is reserved by the system for the menu, so only right click and no pop-up are offered — plus **screen hot corners**: the Rust side polls the mouse coordinates and triggers the pop-up once the pointer stays in a chosen corner continuously past the configured duration; a "must hold a modifier key" option adds a second confirmation so brushing past the corner never fires it. The note pops out from the corner that triggered it.
- **Content source**: any plaintext `.md` or encrypted `.mdl` file; if the containing vault is locked, the note shows a password field inline for unlocking. `.mdlb` is a multi-file container and is not used as a single-note source yet.
- **Editing and preview in one**: the edit / split / preview states reuse the main editor's CodeMirror and `preview.ts`, and saving goes through the same `write_file` / `write_file_vault` commands — so what lands on disk is ciphertext just the same.
- **App launcher**: the bottom of the note can hold local app shortcuts (icon only, or icon + label). macOS resolves the name and icon from local app information; on Windows `appinfo_win.rs` handles `.lnk` / UWP aliases, which is why it has its own `modName` naming (on Windows Command maps to the Win key).
- **Launch-at-login**: writes a macOS LaunchAgent / a Windows current-user startup entry / a Linux XDG autostart file, with an optional flag for starting silently in the background; if the tray icon is turned off, the window is forced visible so the app never becomes unreachable.

## Development and debugging

### Requirements

| | macOS | Windows |
|---|---|---|
| Rust | rustup(stable) | rustup-init.exe(stable, MSVC) |
| Compiler toolchain | Xcode Command Line Tools | Visual Studio Build Tools — install the "Desktop development with C++" workload |
| Node | 18+ (tested working on v22) | 18+ |
| WebView | WKWebView, built into the system | WebView2 Runtime (shipped with Windows 11 / recent Windows 10; older Windows 10 needs the Evergreen offline package) |

On Windows both Rust and Build Tools are mandatory: rustup provides the compiler only, while the linker `link.exe` comes from Build Tools.

**Install order on a brand-new Windows machine** (reopen the terminal after each step):

1. VS Build Tools — https://visualstudio.microsoft.com/zh-hans/downloads/ ("All downloads" → "Visual Studio Build Tools"); in the installer tick **only** the **"Desktop development with C++"** workload
2. Rust — download `rustup-init.exe` from https://rustup.rs/ and take every default
3. Node — the LTS build from https://nodejs.org/
4. WebView2 Runtime — on older Windows 10, download the Evergreen standalone installer from https://developer.microsoft.com/microsoft-edge/webview2/

Verify: `where.exe cargo`, `where.exe link`, and `node -v` all have to print output (in PowerShell don't use `where` — it's an alias for `Where-Object` and prints nothing silently).

### Running

```bash
npm ci             # install per package-lock.json (recommended — keeps dependency versions identical)
npm run dev        # Vite dev server only (http://localhost:1420)
npm run build      # build the frontend assets into dist/
npm run dev:tauri  # run frontend + Tauri shell together (needs a Rust environment)
npm run dev:app    # package a debug "MarkLock Dev.app" and launch it, starting Vite hot reload automatically
                   # frontend edits need no rebuild; a Rust change re-runs in about 5s
```

The first compile builds roughly 400 crates — a few minutes on a Mac, ten-plus minutes on Windows, which is normal; incremental compiles afterwards take seconds.

macOS 27+ does not accept menu-bar status items from an unpacked bare binary, so under `dev:tauri` the tray icon won't appear (the tray logic itself still runs) — a system limitation, not a defect. Use `npm run dev:app` to verify tray / menu bar / Sticky Notes behavior. It runs under the separate identity `com.marklock.dev` (see `src-tauri/tauri.dev.conf.json`), so it can run side by side with the installed version without interference, and that identity is for development only — it has no effect on official releases. The Vite instance started by `dev:app` stays in the background, and `dev:tauri` reuses it (see `scripts/dev-vite.mjs`), so switching between the two never collides on port 1420.

Tests and checks (under `src-tauri/`):

```bash
cargo test --lib   # crypto core unit tests
cargo check        # compile check
```

### Packaging and release

- `.github/workflows/release.yml`: triggered by a `v*` tag or a manual dispatch; runs `cargo test --lib` first, then builds the macOS aarch64 / macOS x86_64 / Windows x64 installers in parallel (`tauri-action`) and produces a **draft** release.
- `.github/workflows/release-win-offline.yml`: manual trigger only; `tauri.win-offline.conf.json` overrides `webviewInstallMode: offlineInstaller`, and the artifact is renamed and attached to the matching tag with `gh release upload --clobber`. The default package uses `downloadBootstrapper`, which downloads a 1.8MB bootstrapper at install time, and **a failed download aborts the whole NSIS install** (`installer.nsi:562-571`) — so a fully disconnected machine must use the offline package. The offline package actually weighs about 209MB (Microsoft's own x64 standalone runtime is 212MB; the "+around 127MB" figure in the Tauri docs is outdated), and Gitee caps a single attachment file at 100MB, so it is offered on GitHub only.
- The version number lives in **four** places: `package.json`, `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`, and the `[[package]] name = "marklock"` entry in `src-tauri/Cargo.lock`. `tauri-action` reads `tauri.conf.json`, so a miss there silently tags the wrong version.

## Common build problems

**`failed to run 'cargo metadata' ... program not found`** — cargo isn't on PATH. Verify with `where.exe cargo` or `Get-Command cargo`; no output means Rust isn't installed, and if there is output, reopen the terminal and try again.

**`linker link.exe not found`, or a link error about `mainCRTStartup` being unresolved** — the first means cargo didn't detect MSVC; the second means the linker architecture and the compile target don't match. One fix for both: inject the MSVC environment into the build terminal, then compile.

```cmd
call "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
```

Use your machine's actual installation path (locate it with `dir /s /b "C:\Program Files (x86)\Microsoft Visual Studio\*vcvars64.bat"`). On Windows you have to re-inject in every newly opened terminal; the easiest route is to use "Developer Command Prompt for VS" from the Start menu as the build terminal. macOS has no such requirement.

**ARM64 Windows virtual machine (Parallels / VMware on Apple Silicon)** — rustup installs an aarch64 host toolchain by default while Build Tools ticks only the x64/x86 MSVC tools, and the mismatch reports `link.exe not found` or an unresolved `mainCRTStartup`. `--target` can't rescue this mismatch — the Rust host toolchain has to move to x86_64 as a whole:

```bash
rustup toolchain install --force-non-host stable-x86_64-pc-windows-msvc
rustup default --force-non-host stable-x86_64-pc-windows-msvc
rustc -vV        # confirm host: x86_64-pc-windows-msvc
```

Both commands must carry `--force-non-host` (ARM64 Windows has an x64 emulation layer, so they do run).

**`failed to run ...\WixTools314\light.exe` (building the MSI)** — WiX 3 is an old ANSI tool and it fails when the project path contains Chinese characters or `productName` is Chinese. Workaround: `npm run build -- --bundles nsis` to build only the NSIS installer; or move the project to a pure-ASCII path and set `productName` to ASCII. The bare exe has actually finished compiling by then and is usable directly from `target/release/`.

**`error messaging the mach port for IMKCFRunLoopWakeUpReliable` (macOS logs)** — harmless noise from the input-method framework; Safari and Electron apps produce it too. Ignore it.

**The project directory weighs 3GB+** — `src-tauri/target/` is build output (debug builds are unoptimized and carry debug symbols), not files you downloaded, and `cargo clean` reclaims it. The crate sources actually downloaded live in `~/.cargo/registry`.

**Errors after copying the project from a Mac to Windows** — delete the copied `src-tauri/target/` and `node_modules/` (`@tauri-apps/cli` ships platform-specific native binaries) and run `npm install` again.

**The packaged build is blocked on first launch** — there is no Apple Developer ID, so installers carry only an ad-hoc signature and are not notarized; `spctl` will inevitably refuse them, as expected. An older build reporting the app as "damaged" means no resource seal was applied — `xattr -dr com.apple.quarantine /Applications/MarkLock.app` is enough.

## UI prototypes

`doc/ui-prototype/` is a purely static high-fidelity prototype (no external dependencies — double-click to view it) and also the visual baseline the Vue components were split against:

```
doc/ui-prototype/
├── index.html            # prototype navigation + embedded preview (entry point)
├── assets/base.css       # shared base styles (the AntD visual tone)
└── screens/
    ├── unlock.html       # 01 launch and unlock
    ├── editor.html       # 02 the main editor (VSCode-style workspace)
    ├── settings.html     # 03 settings (security and encryption as the core page)
    └── dialogs.html      # 04 the dialog set (new vault / unlock error states / change master password)
```

## Coding conventions

- Frontend components always use the **Options API** — never `<script setup>` or the Composition API.
- User-facing copy goes into the dictionary: edit `src/locales/zh.ts` (the source text), then mirror it in `en.ts` with a one-to-one structure; code comments stay out of the dictionary.
- Styling runs on CSS variables (`src/styles/global.css`), and components avoid hard-coded colors; theme and platform differences are detected in `main.ts` and attached to the root node.
- Before committing, run `cargo test --lib` in `src-tauri/` to confirm no regressions in the crypto core.
- For larger changes, open an issue first to align on direction.

## Tech stack at a glance

| Layer | Choice | Why |
|---|---|---|
| Shell | Tauri 2 | Small (vs. Electron), system WebView, native Rust for encryption |
| Frontend | Vue 3 + Vite + TypeScript | Options API components, mature ecosystem |
| Component library | Ant Design Vue | A good fit for desktop utility UI, and the visuals follow its default theme |
| Editor | CodeMirror 6 | Markdown syntax highlighting + source mode + search / folding / autocompletion |
| State | Pinia + localStorage | Metadata only — keys and plaintext never enter the store |
| Encryption | Rust: `aes-gcm` / `argon2` / `zeroize` | All key operations live on the Rust side |
