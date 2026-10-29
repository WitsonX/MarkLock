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

Images and attachments do not go into the container. They land in a sibling `<document name>.dat` directory, each one a complete `mlk/1` container of its own (its own DEK, wrapped by the session KEK), so nothing plaintext ever hits the disk:

```
note.mdlb              the vault container
note.dat/              attachment directory (sibling of the container)
  shot.png.mdat        header + ciphertext (the image bytes)
  contract.zip.mdat     non-image attachments are encrypted the same way
```

The `.dat` naming rule matches the frontend's `assetAnchorInfo`: a standalone file uses its own name, a file inside a vault uses the vault's name. After unlocking, the ciphertext is decrypted into the app cache at `<app_cache_dir>/mlk-assets/<hash(vault id)>/<.dat directory name>/<logical name>`, and the preview uses that cache directory as the base for relative links — the cache mirrors `<name>.dat/<logical name>` on disk, so a `note.dat/shot.png` reference in the body resolves without being rewritten.

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
- **Attachments share the lock**: each `.mdat` has its own small DEK, wrapped by the same session KEK, so a password change has to re-wrap them along with the bodies — swap only the body's key and every historical image becomes permanently undecryptable, and the failure is quiet (images simply stop showing). Directory vaults get this from walking the vault tree; standalone files and `.mdlb` vaults get it from `asset::rewrap_assets` pointed at the sibling `.dat`.
- **Memory zeroization**: both KEK and DEK implement `Zeroize` and are wiped actively on lock or session expiry, with no reliance on the GC.

### Read and write flow

- **Unlock**: master password + salt → Argon2id → KEK → unwrap the DEK → decrypt the whole file tree into memory. At personal-note volume the entire path stays under 300ms.
- **Write**: after unlocking, editing any file re-serializes and re-encrypts the whole tree and rewrites `.mdlb` in one piece. This "rewrite everything" is the simplification the single-file container buys — no incremental index and no fragment management — and at personal volume its cost is imperceptible.
- **Attachment cache**: opening a tab, finishing an unlock, and exporting all call `materialize_vault_assets` to decrypt that vault's `.mdat` files into the app cache (idempotent — entries already cached and no older than their source are skipped, so pasting one image does not re-decrypt the whole `.dat`). After a paste or drag-in writes new attachments the cache is refreshed *before* the link is inserted, otherwise the first preview render shows a broken image. An unreadable or undecryptable entry is skipped on its own and counted into `failed`, which the frontend reports as "N attachments could not be decrypted" instead of letting the whole vault preview collapse; a missing session (never unlocked / expired) returns `NotUnlocked` as a whole, which keeps "no session" from being misreported as "a pile of broken files".
- **iCloud "not downloaded here" files**: on macOS a file that iCloud has kept online-only has no local bytes, and reading it kicks off a download and returns error 11 immediately. Reads and writes now go through `crypto::io::read` / `write`, which check `UF_DATALESS` first, poke `NSFileManager` to start the download, then retry with 400ms backoff up to a 20-second ceiling, emitting an event before and after so the frontend can show a spinner. The six read/write commands moved into the blocking thread pool (`spawn_crypto`) for the same reason: waiting on the main thread freezes the WebView and the spinner never gets drawn.
- **Auto-lock**: `SessionStore` refreshes the activity timestamp on every read and write; on timeout (5 minutes by default) `reap_expired` zeroizes the matching keys, and closing a window calls `lock_all`. The frontend calls `reap_expired` every 30 seconds, and tabs from an expired vault fall back to a placeholder. Two details: **expiring only reports an error on read — the entry is not removed in place** (removal happens in `reap_expired`, which reports the ids; otherwise the frontend never learns the id and the UI stays frozen on a "unlocked" illusion), and **typing counts as activity** — `markDirty` calls `touch_session` throttled to once per 15 seconds per vault, because with autosave off a long typing session used to look idle and get locked. The reap poll also reads `session_ttl` first and flushes dirty tabs for any vault with under 90 seconds left (the keys still work at that point) before handing it to the backend, so unsaved content no longer disappears along with the key.

### Security defenses

- **Path traversal**: paths inside a vault reject absolute paths and `..` — `rel` can only address content inside the container.
- **Tamper resistance**: a failed GCM tag check refuses decryption.
- **Failed-attempt throttling**: after consecutive wrong passwords, attempts are rate-limited.
- **Clipboard**: decrypted content that was copied out is cleared automatically after 60 seconds; locking can clear the clipboard in one shot.
- **Sleep / screen-lock locking** (a toggle in settings): on macOS, Rust polls the session dictionary's `CGSSessionScreenIsLocked` once a second and emits `marklock://screen-locked` on the unlocked → locked transition — a precise signal. The mac frontend therefore does **not** use the "page hidden for a few seconds = the machine slept" heuristic (switching windows, being occluded or changing Space all make the page `hidden`, which would lock by mistake) and keeps only a heartbeat backstop with its gap widened to 30 seconds. Windows / Linux have no backend probe and keep the 5-second heuristic.
- **Attachment plaintext only ever lands in the app cache**: the `.dat` on disk is always ciphertext; the decrypted images live only under `<app_cache_dir>/mlk-assets`, and the matching vault subdirectory is deleted the moment the vault is locked, the app quits, or auto-lock fires. A force-kill or crash can miss that cleanup, so the whole directory is wiped at startup as a backstop — startup is guaranteed to have no sessions yet. State the boundary honestly: the cache directory is not an encrypted area, so anything able to read it (another process under the same user) can see plaintext images while a vault is unlocked. That is the price of "preview needs a file path", and what it buys is that bodies and disk only ever hold ciphertext.

## Rust crypto core

`src-tauri/src/crypto/`:

| Module | Responsibility |
|---|---|
| `mod.rs` | Aggregates and re-exports the submodules |
| `error.rs` | `CryptoError`, one unified error type, serialized into messages the frontend can read |
| `format.rs` | `.mdl` / `.mdlb` format constants, header/body structures, and the serialization boundary |
| `keys.rs` | KEK derivation, DEK wrap / unwrap / re-wrap |
| `file.rs` | Serialization and deserialization of a standalone `.mdl` file |
| `io.rs` | Atomic write + rolling backup + sidecar-file recognition; the recall backoff and before/after notification for iCloud "not downloaded here" files |
| `session.rs` | The unlock session table `SessionStore`, auto-lock timing, key zeroization |
| `asset.rs` | Encrypted images/attachments: writing `.dat/<logical name>.mdat` to disk, decrypting into the app cache on unlock, re-wrapping on password change, clearing the cache on lock |
| `vault.rs` | Vault-level operations: create / unlock / read-write / add-delete-rename-move / change password / lock / search |

Unit tests (`cargo test --lib` under `src-tauri/`) cover: key round-trips, wrong passwords, tamper detection, re-wrap on password change, the full flow for directory vaults, folders inside a vault, the full flow for single-file vaults (add-delete-rename-move + change password + single-file-on-disk verification), path traversal defense, plaintext-directory search, attachment encrypt/decrypt round-trips and tampering, the attachment write-decrypt-clear-cache lifecycle, attachment re-wrap following a password change, a broken attachment being skipped without taking the rest of the vault down, and a directory vault with plaintext attachments still allowing a password change. The CI release pipeline runs these tests before packaging.

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
| `touch_session(vaultId)` / `session_ttl(vaultId)` | Refresh a session's activity timestamp / read the seconds left before auto-lock (`null` when there is no session, 0 once expired) |

**Encrypted attachments (`.mdl` / `.mdlb`)**

| Command | Description |
|---|---|
| `write_encrypted_asset(vaultId, destDir, name, base64)` | A pasted image / binary → encrypted write as `<name>.mdat`, returns the logical name |
| `copy_encrypted_asset(vaultId, destDir, srcPath)` | A file dragged in from disk → encrypted copy into `.dat`, returns the logical name |
| `materialize_vault_assets(vaultId, datDir, datDirName)` | Decrypt into the app cache, returns `{ cache_dir, failed }` (the failure count is what the frontend reports) |

All three run in the `spawn_crypto` blocking thread pool; `lock` / `lock_all` / `quit_app` / `reap_expired` clear the vault's attachment cache on the way through.

**Other**

| Command | Description |
|---|---|
| `list_dir(dir)` | List one directory level (working-directory view) |
| `unlock_vault` / `create_vault` / `list_vault` / `create_in_vault` / `create_folder_in_vault` / `delete_in_vault` / `rename_in_vault` | Legacy directory-vault model, kept for compatibility |
| `copy_file_to(src, dest)` | Overwrite-style copy to a chosen path (the "save as" download flow when an attachment is clicked in preview) |
| `open_local_file(path)` | Hand an existing plain file on disk to the system default app (the "open file" action in the export-success message); only accepts paths that pass `is_file`, a separate policy from `open_url`'s scheme allowlist |
| `openFileDialog` / `openDirDialog` / `saveFileDialog` / `saveFileVaultDialog` / `saveAttachmentDialog` | File dialogs through `tauri-plugin-dialog` |

## Frontend structure and state

- **Views**: four screens in `src/views/` plus the sticky-note window; router in hash mode (`#/unlock`, `#/editor`, `#/settings`, `#/dialogs`, `#/sticky`).
- **State**: `src/stores/vault.ts` (Pinia) is the single owner of the registered vault list, open tabs, the active file, starred items, recents, and settings, persisted to localStorage (`marklock.recent` / `marklock.settings`). **Keys and plaintext never enter the store** — only metadata such as path / name / lock state.
- **Editor**: `src/components/CodeMirrorEditor.vue` wraps CodeMirror 6 and keeps one live instance per file so cursor and scroll position survive.
- **External-change watching**: `src/lib/filewatch.ts` is a module-level singleton that polls on-disk mtime / size every 2 seconds by default and checks immediately when the window regains focus. Files inside a vault are watched at container granularity, and only the files that are open get re-read.
- **Preview**: `src/lib/preview.ts` is the Markdown rendering layer shared by the main editor and Sticky Notes, with `src/components/MarkdownPreview.vue` as the matching component. The base directory for relative links is pushed through `setAssetBaseDir`: encrypted files (`.mdl` / `.mdlb`) point at the attachment cache root, everything else at the anchor directory on disk. That base lives on `globalThis`, not module scope — the marked renderer's closure is captured when the module initializes, and dev hot reload leaves old and new module instances each holding their own variable, so a stale closure reads an empty base and the symptom is broken images and 404s. Clicking a link that is not a document in preview (a zip, an image) no longer opens a text tab; it raises a save dialog and goes through the copy-based download flow.
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
- **Rounded corners**: an undecorated window plus real window transparency (Rust `transparent(true)`; on macOS this needs the Cargo feature `macos-private-api` **and** `app.macOSPrivateApi` in the config) plus a self-drawn 12px radius and 1px border on `.sticky`; `overflow: hidden` clips the title bar and body into the rounded rect. The frontend keys off the `html[data-window="sticky"]` marker written in `main.ts` to make `html/body/#app` and the splash background show through — without it the four corners stay square.
- **Drag-to-resize**: an invisible 16px hot zone in the bottom-right corner drives the resize itself with pointer capture plus a `setSize` call throttled to one per frame (CSS pixels × `scaleFactor` for physical pixels, floored at the same 300×220 as Rust's `min_inner_size`). The system resize path is deliberately avoided: on Windows the native sizing loop switches focus the instant it starts, WebView fires `blur`, and that collides with "hide on blur". Accordingly the hide first asks `sticky_is_active` (on Windows it compares the foreground window handle directly) and ignores the blur while the window is still frontmost. macOS shows no hot zone — an undecorated window's edges already drag natively.
- **Summon paths**: clicking the tray (menu-bar) icon — the left and right buttons each get their own action in settings (open the menu / show the main window / open the sticky note; by default macOS pops the note on left click, other platforms show the main window, and the right button opens the menu); for any button not bound to the menu, the Rust side disables the system's automatic menu and handles the Click event (Down) instead, so the left click works on macOS too. Settings warns when neither button opens the menu — plus **screen hot corners**: the Rust side polls the mouse coordinates and triggers the pop-up once the pointer stays in a chosen corner continuously past the configured duration; a "must hold a modifier key" option adds a second confirmation so brushing past the corner never fires it. The note pops out from the corner that triggered it; the hot-corner path never steals focus, so "hide when not engaged" is the safety net — if the pointer stays put and clicks straight away (the desktop, the menu bar, another window), the note hides at once, since that click means the user wants whatever is under the pointer rather than the note; a click with the cursor inside the note does not count (that click landed on the note and is handed to the frontend's pointerdown → engage). When the cursor position or the note's bounds cannot be read, the click is treated as "not a yielding click" — better to stay open one extra time than to misread a click on the note as a click elsewhere.
- **Tray menu wording forks per platform**: macOS follows system conventions ("Open MarkLock", "Preferences", "Quit MarkLock", with two separators). On Windows / Linux the tray menu is drawn by the shell — font size, row height, left gutter and full-width separators can't be changed — so only the item count and label length are controllable; the labels are shortened ("Show window", "Settings", "Exit") and a single separator is kept before Exit so the menu stays compact. Labels come from `MENU_LANG` inside `build_tray_menu` and the menu is rebuilt when the UI language changes.
- **Content source**: any plaintext `.md` or encrypted `.mdl` file; if the containing vault is locked, the note shows a password field inline for unlocking. `.mdlb` is a multi-file container and is not used as a single-note source yet.
- **A read failure says which failure it is**: not being able to read the body must not be blanket-treated as "the vault is locked" — a plaintext note has no vault to unlock, and showing a password field instead leaves the person stuck on a form that does nothing when pressed. So the causes are split: a locked vault gets the unlock form, anything else (IO failure, the vault file moved) shows the backend's reason plus a retry button. The discriminator is a fixed Chinese literal in the backend error message — if that wording changes, this branch has to change with it.
- **Editing and preview in one**: the edit / split / preview states reuse the main editor's CodeMirror and `preview.ts`, and saving goes through the same `write_file` / `write_file_vault` commands — so what lands on disk is ciphertext just the same. The base directory for relative links is the note's **own directory on disk**; the sticky note does not hook into the main editor's attachment cache, so images stored as `.mdat` ciphertext currently do not render in a note whose source is a `.mdl` (notes on plain folders and ordinary files are unaffected).
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
- In a batch of files where the transform is "re-wrap the key one by one and write", always **compute every new byte first, then write** (`rewrap_assets` / `rewrite_dir_deks`): parsing-and-writing as you go leaves a half-new, half-old vault behind when something errors mid-way, which is far worse than failing outright.
- Disk read/write commands that can block for a long time go through `spawn_crypto` (the blocking thread pool) instead of running synchronously on the main thread — the WebView freezes while waiting and the frontend's loading indicator never gets painted.
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
| macOS system integration | `objc2` / `objc2-app-kit` / `objc2-foundation` | Sticky-note and hot-corner placement avoids the Dock (`NSScreen.visibleFrame`); iCloud "not downloaded here" files are recalled on demand (`NSFileManager`); screen-lock detection and cursor / button state go through raw CoreGraphics externs rather than another crate. Declared only for the macOS target |
