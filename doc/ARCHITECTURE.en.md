# MarkLock — Architecture & Evolution

> This document records MarkLock's full architecture design and feature evolution from project kickoff to today — the first-hand reference for the team or anyone taking over.
> For quick start, environment requirements, and run commands, see [README](../README.en.md).

---

## 1. Project positioning

**MarkLock** is an encryptable Markdown viewer/editor — a desktop app.

- Shell: Tauri 2 (Rust); frontend: Vue 3 + Ant Design Vue; the crypto core lives entirely on the Rust side.
- All data stays local. Everything is ciphertext the moment it hits the disk; keys never leave the device, never enter JS, never get uploaded.
- Two encrypted forms:
  - **Vault `.mdlb`** (MarkLock Bundle) — a single-file container holding an encrypted file tree (filenames, directory structure, and contents are all ciphertext).
  - **File `.mdl`** (MarkDown Lock) — a single encrypted Markdown file.

---

## 2. Overall architecture

```
┌──────────────────────────────────────────────────────────────┐
│                       Frontend (Vue 3)                       │
│  views/ EditorView·UnlockView·SettingsView·DialogsView       │
│  components/ TitleBar·VaultTreeNode                          │
│  stores/vault.ts (Pinia)   lib/tauri.ts (invoke bridge)      │
│  lib/tooltip.ts (v-tip directive)                            │
│            │  invoke('xxx', {...})  ← path / password / text │
│            ▼                                                 │
│            │     (key material never passes through JS)      │
├──────────────────────────────────────────────────────────────┤
│                  Shell layer (Rust / Tauri 2)                │
│  lib.rs —— window management + ~30 Tauri commands            │
│  crypto/ —— crypto core (see below)                          │
│            │                                                 │
│            ▼                                                 │
│     File system (.mdlb single file / .mdl single file / md)  │
└──────────────────────────────────────────────────────────────┘
```

### Data-flow principles

1. **Keys never enter the frontend**: the password is only passed to Rust to derive the KEK; decrypted plaintext is returned by Rust to the frontend for display, but key material itself never crosses the JS boundary.
2. **Sessions live only in memory**: unlock state lives in Rust's `SessionStore` (a process-level singleton) and dies on restart; the frontend store persists only metadata — path, name, locked state — never an unlocked flag (forced to false on read-back).
3. **Locking = wiping keys**: when one vault or all vaults are locked, Rust zeroizes the corresponding in-memory keys; the frontend only clears the plaintext content of that vault's encrypted tabs — plain files are untouched.

---

## 3. Crypto core (Rust `src-tauri/src/crypto/`)

### 3.1 Module breakdown

| Module | Responsibility |
|---|---|
| `error.rs` | `CryptoError` unified error type (WrongPassword / BadHeader / IntegrityFailed / NotUnlocked / Io / WeakPassword, etc.); `Serialize` outputs human-readable Chinese messages |
| `format.rs` | `.mdl` / `.mdlb` file-format constants (`mlk/1`, `mlk-vault/1`) and header/body structures (KdfParams / FileHeader / VaultMeta / EncryptedBody); base64 utilities |
| `keys.rs` | `Kek` / `Dek` types (Zeroize-derived — zeroized on drop); `derive_kek` (Argon2id); `generate_dek`; DEK wrap/unwrap/re-wrap; `encrypt_body`/`decrypt_body` (AES-256-GCM) |
| `file.rs` | `.mdl` serialization/deserialization (4-byte header length + header JSON + nonce + ct) |
| `io.rs` | Atomic write (temp file → fsync → rolling backup → rename → fsync parent directory) and sidecar-file recognition; the recall backoff and before/after notification hook for iCloud "not downloaded here" files |
| `session.rs` | `SessionStore` (global `Mutex<HashMap>` singleton); `UnlockedVault` holds kek/kdf/dek + last_activity + auto_lock; auto-lock timing and expired-session reclamation |
| `asset.rs` | Encrypted images/attachments: `.dat/<logical name>.mdat` on disk, decrypt into the app cache on unlock, re-wrap on password change, clear the cache on lock/quit |
| `vault.rs` | Business flows: create/unlock/read-write/change password/lock, vault tree traversal, file add/delete/rename/move, **search** |

### 3.2 Key hierarchy

```
Master password + salt ──Argon2id(m=64MiB, t=3, p=4)──▶ KEK (32B, zeroized in memory)
      │  AES-256-GCM
      ▼
wrapped_dek ──▶ DEK (32B)
      │  AES-256-GCM
      ▼
Plaintext (markdown source / file tree JSON)
```

- **`.mdl` single file**: every file gets its own random DEK; the header carries its KDF parameters and wrapped_dek — an independent password per file.
- **`.mdlb` single-file vault**: the whole vault shares one DEK; the header stores the wrapped_dek of that vault-wide DEK, and the data area is the ciphertext of the JSON-serialized file tree (filenames + directory structure + contents). On disk it's a single opaque blob.
- **Changing the master password**: only re-wraps the DEK (`rewrap`) — the contents are never re-encrypted.
- **Images/attachments**: each attachment gets its own DEK, wrapped by the same session KEK, so it shares the master password's lock with the bodies — changing the password must re-wrap everything under `.dat` too.

### 3.3 File layout

```
Single-file vault (.mdlb)                    Single file (.mdl)
┌────────────────────────────┐           ┌────────────────────────────┐
│Header (plaintext JSON)     │           │Header (plaintext JSON)     │
│ format: "mlk-vault/1"      │           │ format: "mlk/1"            │
│ kdf: argon2id params + salt│           │ kdf / wrapped_dek / hint   │
│ wrapped_dek                │           │ wrapped_dek                │
│Data area (ciphertext)      │           │Data area (ciphertext)      │
│ AES-256-GCM ciphertext of  │           │ AES-256-GCM ciphertext of  │
│ the whole file-tree JSON   │           │ the body text              │
└────────────────────────────┘           └────────────────────────────┘

Attachments (a .dat directory sibling to the document, one standalone container each)
note.mdlb / note.mdl
note.dat/shot.png.mdat   header (wrapped_dek wrapped by the session KEK) + ciphertext image bytes
      ↓ decrypted into the app cache on unlock; preview resolves relative links against it
<app_cache_dir>/mlk-assets/<hash(vault id)>/note.dat/shot.png
```

---

## 4. Frontend structure

### 4.1 Directory

```
src/
├── main.ts                  # Entry: mounts Vue + AntD + Pinia + the v-tip directive; writes data-platform for platform detection
├── App.vue                  # Theme & skin resolution (light/dark/system + matchMedia follows the system; data-skin + custom-skin CSS injection)
├── router/index.ts          # Hash-based five-screen routing (the sticky window loads the same frontend, routed to /sticky)
├── styles/global.css        # Global styles (CSS variables, incl. dark-theme variables and derived preview/editor tokens)
├── styles/skins.css         # Built-in color skins (Nord, Nocturne, Catppuccin, Sepia, Solarized, Gruvbox — each shipping a light and a dark variant)
├── components/
│   ├── TitleBar.vue         # Title bar (macOS traffic-light overlay + title)
│   ├── VaultTreeNode.vue    # Recursive vault-tree node (folder expand/collapse + hover create/rename/delete)
│   ├── WorkdirNode.vue      # Recursive working-directory tree node
│   ├── CodeMirrorEditor.vue # CodeMirror 6 editor wrapper (markdown language + syntax highlighting)
│   ├── MarkdownPreview.vue  # Preview pane rendered by marked
│   ├── EncIcon.vue          # Encryption badge icon
│   └── AboutCard.vue        # About card (version / update check)
├── stores/vault.ts          # Pinia store (sessions / open files / recent / starred / settings)
├── lib/
│   ├── tauri.ts             # invoke bridge for Rust commands + file dialog wrappers
│   ├── filewatch.ts         # External-modification watcher (file-stamp poller)
│   ├── tooltip.ts           # Global v-tip directive (singleton bubble, never clipped by scrolling)
│   ├── preview.ts           # Shared marked render layer (code highlight/copy/task lists/image assets)
│   ├── wysiwyg.ts           # In-place preview editing: DOM→Markdown serializer + execCommand layer
│   ├── previewSearch.ts     # In-preview find: haystack over the rendered DOM, <mark> hits, navigation
│   ├── fonts.ts             # Built-in preview font catalog (cross-platform fallback stacks)
│   ├── skins.ts             # Skin registry: built-in list, token groups, custom-skin CSS generation/injection/live preview
│   ├── locales/             # zh/en language packs + switching
│   ├── sticky.ts            # Sticky-window detection + note body read/write by file type
│   ├── launchArgs.ts        # Quote-aware split of a quick-launch item's raw "launch arguments" string
│   ├── splash.ts            # Splash screen fade-out control
│   └── update.ts            # Lightweight update check (GitHub/Gitee dual source + rate gating)
└── views/
    ├── UnlockView.vue       # Unlock (minimal: target vault + password + unlock/cancel)
    ├── EditorView.vue       # Main editing UI (the core)
    ├── StickyView.vue       # Portable sticky note (independent always-on-top window)
    ├── SettingsView.vue     # Settings
    └── DialogsView.vue      # Dialog collection (new vault / change password / new encrypted file)
```

> The frontend uses the **Options API** throughout — no `<script setup>` (a hard user convention).

### 4.2 Main interface (EditorView) information architecture

- Six sidebar groups (drag-reorderable, each individually toggleable; the toggles live in Settings → Appearance → Interface elements): **Open Files → Encrypted Vaults → Working Directory → Favorites → Recently Opened → Outline**
- Top tab bar (drag-reorderable, independent toggle)
- Editing modes: edit / split / preview (rendered by marked; task-list checkboxes are interactive)
- "More" menu: new file / new encrypted vault / copy to vault / preferences / sidebar visibility toggles
- **Small menu icons**: a few key items in the main menu and the custom right-click menu carry linear mini-icons (`MENU_ICONS` map → `menuIcon()` produces inline svg). Two traps are baked into the implementation: content injected with `v-html` cannot see scoped CSS (the `data-v` attribute is only attached to template nodes), so size and stroke must be attributes on the svg itself or WebKit renders it 0×0; and the icon is **absolutely positioned over the check gutter on the left** rather than being an in-flow element — the icon and the `✓` never appear together, so they occupy one slot, and items with or without an icon align their text at the same x.
- **Submenus in the custom context menu**: menu items gained a `kind:'sub'` shape (carrying an `items` array), and expansion is pure CSS `:hover` rather than JS state. The submenu is 224px wide, and `showCtxMenu` sets `subLeft` — flipping it to the left — when "cursor x + main-menu width + submenu width" would exceed the viewport, so it never gets clipped at the right edge. The three export entries live at this level, matching the macOS main menu's File → Export nesting.
- **Clicking an attachment in preview**: document types open a tab; binaries that can't be parsed as text (zip / image / pdf) now raise a save dialog and are copied over (a download flow) instead of producing a UTF-8 or file-header error on the first click.
- **Directory suffix on same-named tabs**: when several open files share a display name, those tabs and the matching "Open Files" sidebar entries pick up a small directory suffix (`listSubtitles` / `tabRelDir` in `EditorView.vue`). Items are grouped by display name and **only the colliding groups** get one; the suffix starts at the nearest directory segment and grows upward until it is unique inside that group, falling back to the whole relative directory when even that isn't unique (two paths that are prefixes of each other). The base depends on the vault type — inside a `.mdlb` it is the path after `#`, a folder vault is taken relative to the vault root, plaintext under the working directory relative to that root, and everything else as an absolute dirname. Unsaved drafts and locked vault files always return an empty string: the first has no disk location yet, the second must not leak a real path (the same rule the tab tooltip follows once locked).

### 4.3 State management (Pinia + localStorage)

Managed centrally by `stores/vault.ts`; localStorage keys:

| Key | Contents |
|---|---|
| `marklock.recent` | Recent/registered vault list (includes locked state; unlocked forced to false on read-back) |
| `marklock.settings` | Settings |
| `marklock.favorites` / `marklock.favoriteFiles` | Starred vaults / files |
| `marklock.recentFiles` | Recently opened files |
| `marklock.openFiles` | Tabs open last session (metadata only) |
| `marklock.sectionOrder` | Sidebar group order |
| `marklock.workdir` | Working-directory root |

Keys and plaintext never enter the store. `assetDir` on a tab is an exception worth naming: it is only the one path string "this vault's attachment cache root" — it carries no content and is never persisted (it is not in the `marklock.openFiles` field whitelist), and on lock it is cleared together with the backend cache, then materialized again on the next unlock.

---

## 5. Tauri command inventory

> The frontend `src/lib/tauri.ts` wraps all of them as a Promise API.

### 5.1 Single-file vault (.mdlb)

| Command | Description |
|---|---|
| `create_file_vault(path, password, hint?)` | Create a new single-file vault |
| `unlock_file_vault(vaultId, path, password, autoLockSecs?)` | Unlock (decrypts the whole file tree) |
| `list_file_vault(vaultId, path)` | List the recursive tree (without contents) |
| `read_file_vault` / `write_file_vault` | Read/write a file inside the vault |
| `create_in_file_vault` / `create_folder_in_file_vault` | Create a file/folder inside the vault |
| `delete_in_file_vault` / `rename_in_file_vault` | Delete/rename-move inside the vault |
| `change_file_vault_password` | Change the master password (re-wraps the vault-wide DEK) |

### 5.2 Directory vault (.mdlb directory, old model kept for compatibility)

| Command | Description |
|---|---|
| `create_vault` / `unlock_vault` | Create/unlock a directory vault |
| `list_dir` / `list_vault` | List one directory level / list the vault's recursive tree |
| `create_in_vault` / `create_folder_in_vault` | Create a file/folder inside the vault |
| `delete_in_vault` / `rename_in_vault` | Delete/rename-move inside the vault |

### 5.3 Single file (.mdl) and general

| Command | Description |
|---|---|
| `unlock` / `lock` / `lock_all` | Unlock a single file / lock one / lock all |
| `read_file` / `write_file` / `create_file` | Single-file read/write/create |
| `change_password` | Change the single file's master password |
| `read_plain_file` / `write_plain_file` | Plaintext read/write |
| `inspect_path` | Probe a path's type (directory / vault / file inside a vault / plaintext) |
| `unlocked_count` / `reap_expired` | Status query / reclaim expired sessions |
| `touch_session(vault_id)` / `session_ttl(vault_id)` | Refresh a session's activity timestamp / read seconds left before auto-lock |
| `search(query, workdirs?)` | Global search (vaults + plaintext files in the working directory) |
| `file_stamps(paths)` | Batch-query file mtime + size (external-change monitoring) |
| `set_backup_enabled(enabled)` | Sync the "keep previous version on save" toggle to the Rust side |
| `write_encrypted_asset(vaultId, destDir, name, base64)` / `copy_encrypted_asset(vaultId, destDir, srcPath)` | Write an attachment encrypted / copy one in encrypted; returns the logical name |
| `materialize_vault_assets(vaultId, datDir, datDirName)` | Decrypt the ciphertext attachments under `.dat` into the app cache; returns `{ cache_dir, failed }` |
| `copy_file_to(src, dest)` | Overwrite-style copy to a chosen path (the download flow when a binary attachment is clicked in preview) |
| `open_local_file(path)` | Hand an existing plain file on disk to the system default app (the "open file" action in the export-success message) |
| `launch_local(path, args)` | Quick-launch a local app and pass launch arguments through (macOS `open --args`, plus `-n` when arguments are present; Windows appends them; Linux ignores them) |
| `take_pending_open_paths()` | Drain the backend's buffer of "opened from the OS" paths (once on cold start, once after navigating back from a non-editor page) |
| `export_pdf(html, target)` | Hand self-contained HTML to a hidden WebView and write the rendered PDF to target (macOS: `createPDFWithConfiguration` page by page plus PDFKit merging; Windows: WebView2 `PrintToPdfStream`; Linux: reports unsupported) |
| `set_show_hide_shortcut(shortcut)` | Register / replace / clear the global "show / hide main window" hotkey (a Tauri accelerator string; empty string = clear). The previous accelerator is unregistered before the new one is registered; a failure returns a readable error and leaves the stored value untouched so the frontend can roll back (see §6.12) |

> Commands that can block on disk for a long time (unlock / read / write / attachment materialize / batch encryption) run in the blocking thread pool via `spawn_crypto`; running them synchronously on the main thread freezes the WebView.

### 5.4 External-change monitoring

An open tab may be rewritten by another program. `lib/filewatch.ts` exports the module-level singleton `externalChanges` (an `ExternalChangePoller` instance).
Every 2s (and when the window regains focus or returns to the foreground) it batch-calls `file_stamps` for every tab's on-disk path and compares mtime + size against the previous baseline:

- Singleton rather than component-owned: routing to the settings/unlock screens destroys EditorView, so the watcher only `detach()`es the callback (the timer and baseline survive); after `attach()` brings you back to the editor, the first comparison round catches changes made while you were away; it only `stop()`s when the toggle in settings is turned off
- Toggle: Settings → General → "Watch external changes" (`settings.watchExternalChanges`, on by default)
- Watch targets: plain file / `.mdl` / file in a directory vault = its own path; file inside a `.mdlb` vault = the container path (vault files aren't separate on-disk files)
- After a stamp change it **re-reads and compares the content per tab** (the container changing doesn't mean every open file changed) — only a genuine content difference counts as an external modification
- No unsaved changes → reload straight away and notify; unsaved changes → never clobber the user's content — the tab gets an amber badge + a notification, and you can reload via right-click or the "Reload" button in the status bar
- File gone (deleted/renamed) → red badge + notification; the tab's content is kept
- Skips drafts and any encrypted tab whose session no longer has plaintext (after locking, plaintext is zeroized — re-reading would pour it back into memory)
- Your own saves: after writing to disk, `sync()` refreshes the baseline; the content comparison rules out self-writes anyway

### 5.5 Permissions (capabilities/default.json)

```json
["core:default", "core:window:allow-start-dragging", "core:window:allow-set-size", "notification:default", "autostart:default", "dialog:default"]
```

> Key point: `core:window:allow-start-dragging` is the prerequisite for window dragging (`core:default` doesn't include it — missing it makes dragging fail silently); `core:window:allow-set-size` is likewise the prerequisite for the sticky note's drag-to-resize (`setSize`) — without it the drag is rejected silently.
>
> The sticky note's rounded corners need a transparent window. On macOS, `transparent(true)` is only actually applied when the Cargo feature `macos-private-api` is enabled (`tauri-runtime-wry` compiles it behind that feature; without it transparency is silently ignored, with a one-line notice in debug builds), and `app.macOSPrivateApi: true` makes `tauri-build` verify that feature is declared in `Cargo.toml` — both must be present. This combination is incompatible with Mac App Store distribution.

---

## 6. Key mechanisms

### 6.1 Window and title bar (macOS)

- `TitleBarStyle::Overlay` + `hidden_title(true)` + `traffic_light_position(14.0, 20.0)`: the native traffic lights overlay on the app-drawn toolbar's top-left corner; the system title bar is gone.
- Dragging: the titlebar carries `data-tauri-drag-region="deep"`; button areas use `="false"` to block dragging.
- Close: `CloseRequested` → `lock_all_vaults` + `app_handle().exit(0)` — clicking the red X quits the whole program.
- Cross-platform: macOS-specific code is wrapped in `#[cfg(target_os="macos")]`; the frontend detects the platform via `navigator.userAgent` (writes `data-platform`).

### 6.2 Drag and drop (two kinds — keep them apart)

- **Files/directories dragged in from outside**: `getCurrentWebviewWindow().onDragDropEvent()`, triggered only on the right-hand main panel (the sidebar area is reserved for reordering); routing: plaintext → open, directory → attach as working directory, `.mdl`/`.mdlb` → register + unlock.
- **List reordering**: pure mouse events (HTML5 draggable doesn't work in WKWebView) — movement past a threshold counts as a drag, `elementFromPoint` hits the target.

### 6.3 Theme & skins

- **Theme (light/dark)**: three settings `light` / `dark` / `system`; `system` follows the OS via `matchMedia('(prefers-color-scheme: dark)')`. Dark-theme variables override under `html[data-theme='dark']` in `global.css`; hardcoded light colors in components were replaced with CSS variables.
- **Skins (palette)**: **orthogonal** to light/dark, carried by `html[data-skin]`. Every skin in `skins.css` is written twice — `[data-skin='x'][data-theme='light']` and `…[data-theme='dark']` — so light values can never leak into dark mode.
- **Derived tokens**: `:root` declares the preview tokens `--md-*` and editor tokens `--cm-*` in terms of base variables (`var(--bg)` and friends), so a skin that only overrides base colors cascades into the preview and CodeMirror automatically; fine-tuning can override a derived token directly.
- **Custom skins**: stored in `settings.customSkins` (localStorage). `buildCustomSkinCss` in `lib/skins.ts` emits CSS with the same selector shape as the built-ins and writes it via `textContent` into `<style id="marklock-custom-skins">` (never innerHTML; variable names and values pass an allow-list / `;{}<>` filter). Editing uses a separate `<style id="marklock-skin-preview">` plus `data-skin="__preview__"` for WYSIWYG, cleaned up on cancel / save / unmount.
- **Built-ins as a starting point**: the "duplicate and edit" button on each card calls `forkSkin`, which uses `snapshotSkinTokens` to read the tokens a built-in **declares explicitly** in both its light and dark rules from `document.styleSheets` (cross-origin sheets throw and are skipped) under a fresh id. Inherited tokens stay inherited — a fork carries only what the source skin explicitly declares.
- **CodeMirror**: `oneDark` is gone; the theme is now `EditorView.theme(spec, { dark })` reading CSS variables, with syntax colors from `HighlightStyle.define` over `--cm-*` / `--hl-*` and `defaultHighlightStyle` kept only as a fallback.
- **HTML / PDF export**: `buildStandaloneHtml` reads runtime variables through `cssVar()`, so exports follow the active skin. PDF keeps background colors too (the Windows side asks for them explicitly with `SetShouldPrintBackgrounds(true)`), so code-block shading and table striping survive into the artifact.

### 6.4 Search

- Scope: unlocked vaults (encrypted content matched after in-memory decryption) + plaintext files in the working directory + open tabs (in-memory content).
- Matching: character-level, case-insensitive substring; at most 3 hit snippets per file, 100 overall cap.
- Interaction: 300ms debounce, keyboard up/down selection (wrapping), Enter to open, Esc to close, keyword highlighting, deduplicated results.
- ⌘F in Preview mode is not part of this: it does not switch views, it searches the rendered DOM — see §6.13.

### 6.5 Auto-lock

- Rust's `SessionStore` records `last_activity` per session; after the timeout (5 minutes by default; tiers 60/300/900/1800/0) the keys are zeroized. The frontend polls `reap_expired` every 30 seconds, and the affected vault's tabs fall back to their placeholder.
- **Expired ≠ removed in place**: `with_vault` on an expired session only returns `NotUnlocked` and keeps the entry; removal happens exclusively in `reap_expired`. Silently `remove`ing on the read path would leave the polling loop without the id, so the UI would stay stuck on a "unlocked" illusion, saves would keep failing with "not unlocked", and the state could never heal itself. The price is that an expired key lingers in memory for at most one poll interval.
- **Typing counts as activity**: the backend only sees reads/writes that touch disk, so with autosave off "typing without saving" looked idle and got locked. `markDirty` → `scheduleSessionTouch` now calls `touch_session` throttled to once per 15 seconds per vault (the command is just `with_vault(id, |_| Ok(()))`, which also re-checks expiry).
- **Flush before the deadline**: `reapExpired()` walks the unlocked vaults and, for any whose `session_ttl < 90` (under two poll intervals), runs `autoSaveEncryptedBeforeLock` first — the session is still valid at that moment, and a successful write also refreshes its timestamp — only then letting the backend reap. Dirty content no longer vanishes along with the key.
- **Sleep and screen lock** (`settings.lockOnSleep`): on macOS, Rust polls the session dictionary's `CGSSessionScreenIsLocked` once a second and emits `marklock://screen-locked` on the unlocked → locked transition — a precise signal. The frontend's "page hidden for N seconds = the machine slept" heuristic is unreliable on mac: switching windows, being occluded, or changing Space all make the page `hidden`, so a plain window switch would be misread as sleep. macOS therefore skips that heuristic and keeps only the heartbeat backstop with its gap widened to 30 seconds (a real suspend gap is far larger); Windows / Linux have no backend probe and keep the 5-second threshold.

### 6.6 Sticky note window (transparent corners and custom resize)

- **Rounded corners**: the sticky note is an undecorated window, so the radius comes from "transparent window + a self-drawn 12px radius and 1px border on `.sticky` + `overflow: hidden`". Three layers must let the desktop show through: Rust's `transparent(true)`, the frontend clearing the `html/body/#app` backgrounds under `html[data-window="sticky"]` (the marker is set in `main.ts` from `isStickyWindow()`), and the inline `#splash` in `index.html` getting the same radius — otherwise the first frame flashes square-tinted corners.
- **Resizing**: an invisible 16px hot zone in the bottom-right corner (with a nwse-resize cursor) tracks the pointer itself instead of using the system resize (`startResizeDragging`). Reason: on Windows the native sizing loop switches focus the instant it starts, WebView2 fires `blur`, and that collides with "hide on blur" — the note disappeared as soon as you began dragging the handle. The implementation uses pointer capture (so `pointermove` keeps arriving outside the window) plus a `requestAnimationFrame`-throttled `setSize`; `screenX/Y` are CSS pixels while window sizes are physical, so they're multiplied by `scaleFactor`, and the 300×220 floor matches Rust's `min_inner_size`. macOS shows no handle: an undecorated window's `styleMask` still carries `Resizable`, so edges already drag natively (and tao's `drag_resize_window` returns `NotSupported` on mac, so the loop can't be entered from code anyway) — an extra hot zone would only be noise.
- **Hiding on blur**: `blur` doesn't mean the window lost activation. On non-mac platforms the hide is gated on `sticky_is_active` first (Windows compares `GetForegroundWindow()` against this window's hwnd, bypassing WebView-internal focus); if the window is still frontmost, the blur is ignored. Opening the system file picker is blocked the same way through the `picking` flag.
- **"Open in main window" is a front-end-only cross-window event**: the note and the main window are two webviews in one process, and the router belongs to the main window, so the note can only hand the target over — it cannot navigate for it. The chain is `save(true)` (silent flush) → `showMainWindow()` → `emitTo('main', 'marklock://sticky-open-note', target)`, and the main window receives it in the same "two listeners" shape described in §6.10. The open dispatch mirrors session restore, and `kind='mdl'` must still be split on `vaultId === path` between a standalone .mdl and a file inside a directory vault — the two read through different commands, and treating both as a standalone .mdl just yields a placeholder tab that can never unlock. No new Rust command: `core:default` already pulls in `core:event:default`, which includes `allow-emit-to`.
- **The note's in-place editing reuses the serializer, not the view code**: the single source of truth for "edit the rendered result → parse it back" is `src/lib/wysiwyg.ts`, while the main editor and the note each hold their own freeze/debounce state. Their preview DOM shapes differ — the main editor wraps each block in `.mb` with a `data-l` line anchor for split scrolling, the note renders the whole document through `preview.ts` as bare block children — so the serializer has to accept both. Parity isn't assumed: the same cases are run against both shapes and compared for idempotent round-trips and byte-identical output from the format commands (they match). The read-only ⇄ edit switch is deliberately note-local state: the two webviews each own a Pinia instance yet share one localStorage key, so writing the global item would overwrite the other window, the same reasoning that keeps the note's `wordWrap` local.
- **The note's top keeps one toggle, not a format toolbar**: `.sticky-float` is a single right-aligned row holding the Edit / Split / Preview segment and the eye / pencil button to its left, with the container `pointer-events:none` and each control re-enabling clicks; the layer sits at .35 opacity and `:hover` — which matches along the DOM ancestor chain, so an event-transparent container still receives its children's hover — brings it to 1. The toolbar was dropped for width and value: 15 buttons plus 3 separators are ~437px while the note defaults to 400 logical points, and the fast way to reformat inside a note is simply to switch to Edit and type the markup. Three layouts were tried — both blocks on one row (measured: the English segment is 150px against a 136px reservation, so it covered the buttons), two stacked rows (the bar wrapped into two), and only the one button (measured row width 164px Chinese / 182px English, fits even the 300px minimum window, `overlap:-6`).
- **The tray button is a toggle**: clicking the tray (menu-bar) button bound to "open the sticky note" a second time hides the note instead of re-summoning it — it first `emit`s `marklock://sticky-hide` (the frontend flushes unsaved text in that listener) and then `hide()`, the same chain the tray menu's hide-note item uses. The check is only `is_visible()`, so a window that was never created counts as "not shown". One click opens, the next closes; there is no second entry point to keep in sync.

### 6.7 Images and attachments (ciphertext on disk + decrypted cache)

- **Why take a detour through the cache**: rendering an image in preview needs a file path, but attachments are `.mdat` ciphertext on disk and the rendering layer can't read them. The chain therefore has a fixed shape: unlock → decrypt into the app cache → preview uses the cache directory as `assetBaseDir`. Inside the cache the layout mirrors `<.dat directory name>/<logical name>`, so a relative link in the body hits without being rewritten.
- **Encrypted at write time**: pasting goes through `write_encrypted_asset` (base64 → container), dragging in goes through `copy_encrypted_asset` (disk bytes → container). The on-disk name is `logical-name.mdat`, and a collision appends `-1`/`-2` before the extension. Directory vaults (the folder model) and ordinary plaintext files still keep their images in plaintext — they are not wired into this path.
- **One broken image shouldn't take the vault down**: an entry that can't be read or decrypted is skipped on its own and counted into `failed`; the frontend reports "N attachments could not be decrypted". A missing session (never unlocked / expired) returns `NotUnlocked` for the whole call, kept distinct from "attachments are corrupt" — otherwise the entire directory fails to materialize, the frontend silently catches it, and every image disappears at once.
- **Cache lifetime**: `lock` / `lock_all` / `quit_app` / `reap_expired` delete the vault's cache subdirectory; a force-kill or crash can miss that, so `setup` (when there is provably no session yet) wipes the whole directory as a backstop. The store clears the tab's `assetDir` in step, and the next unlock materializes again.
- **Password change shares the lock**: an attachment's small DEK is wrapped by the session KEK, so changing the master password must re-wrap them too (standalone files and `.mdlb` vaults through `asset::rewrap_assets`; directory vaults while walking the vault tree, which handles `.dat` on the way). Both follow "compute all the new bytes first, then write", so a mid-way failure never leaves a half-new, half-old vault.
- **Export**: three artifacts — HTML / single-file HTML / PDF, see §6.11 — share one chain: `ensureAssetsMaterialized` runs first so the attachments are in the cache, then `inlineExportAssets` handles the local resources. Plain HTML copies images and non-image attachments into `<html name>.dat/` next to the exported file and rewrites them as relative links (if a single copy fails, that image falls back to an inline data URL); single-file HTML and PDF inline everything as data URLs. The success message carries inline "open file / open folder" actions.

### 6.8 iCloud "not downloaded here" files (recall compatibility)

- **Symptom**: on macOS a file iCloud has kept online-only has no local bytes, and reading it returns error 11 (`Resource deadlock avoided`) the instant the download is triggered. Surfacing that directly reads as "failed to read the file", when the real state is "still downloading".
- **Handling**: read and write entry points funnel through `crypto::io::read` / `write`, which first check the `UF_DATALESS` flag from `stat` to decide whether a recall is needed (no file handle opened, no side effects), poke `NSFileManager.startDownloadingUbiquitousItemAtURL` on the first collision, then retry with 400ms backoff up to a 20-second ceiling. Only error 11 is treated this way; other IO errors pass through unchanged.
- **Visibility**: a `marklock://cloud-recall(-done)` event is emitted at the start and the end, broadcast to every window; the frontend keys a spinner by path so a multi-second wait no longer looks like a silent hang.
- **Precondition**: these reads genuinely wait, so the six read/write commands must go through `spawn_crypto` (see the note in §5.3); executed synchronously on the main thread they freeze the WebView, the loading indicator never gets painted, and all the user sees is a hang.
- **Boundary**: global search still uses bare `fs::read` on the main thread and is not wired to recall — an iCloud-online-only note matches by filename only and won't stall the UI. That is a deliberate trade-off.

### 6.9 Launch arguments on quick-launch items

- **The raw string is what gets stored**: `QuickLaunchItem.args?: string` holds exactly what the user typed in Settings (empty means no arguments; older data has no such field), and it is split into an array only at click time by `splitLaunchArgs` in `lib/launchArgs.ts` — quoted segments (double or single) count as one piece, everything else splits on whitespace. Storing the raw string means revisiting or re-editing shows the user the very sentence they wrote, not a mangled array.
- **No injection surface**: arguments go to `Command::args` as a `Vec<String>`, one parameter at a time, never spliced into a shell string. This is a completely separate door from `open_url`'s scheme allowlist.
- **Platform differences**: macOS runs `open <path> --args …`, and because `open` merely activates an already-running application without delivering argv, **arguments always come with `-n` to start a fresh process** (no arguments keeps the old behavior — no extra instance). Windows appends them after `explorer <path>`. Linux's `xdg-open` has no pass-through semantics, so arguments are silently dropped.
- **Same path can appear twice**: the duplicate check loosened from "same path is a duplicate" to "same path and both argument-free", so one app can sit in the list several times with different arguments. That is why the list key changed from `item.path` to `path#i` — in the sticky bar too. Icon lookups still key on path, so those entries share one icon.
- **Drag to reorder**: pressing the grip at the head of an item only starts a drag after 4px of movement (the same pure mouse-event approach as §6.2, with `document.elementFromPoint` + `closest('[data-ql-index]')` to hit a row). Whether the pointer sits in the upper or lower half of the hovered row decides if the item lands **before or after** it, and the highlight line moves to that row's top or bottom edge — the line always marks the real landing spot. `store.reorderQuickLaunch(from, to)` reads `to` as "insert before original row `to`", and since pulling `from` out first shifts every row after it up by one, the insertion point drops accordingly; when that lands back on `from` the action returns without writing. The sticky note renders its launcher bar in exactly this array order.

### 6.10 How a file opened from the OS arrives

- **Entry points fork per platform**: macOS relies entirely on `RunEvent::Opened` (Finder double-click, dropping onto the Dock icon, "Open with", `open -a MarkLock file`); Windows reads argv on a cold start and forwards a second launch to the running instance through the single-instance plugin; Linux has no entry point.
- **Why a buffer is mandatory**: Tauri events don't queue. On a cold start `Opened` usually fires before the frontend registers `listen()`, so emitting directly loses the path forever. Paths therefore go into Rust's `PENDING_OPEN_PATHS` first and the event is only a "go fetch it" signal; `take_pending_open_paths` drains it in one shot, which also guarantees no duplicate tabs.
- **Two listeners, split by route**: while `EditorView` is mounted its own listener drains directly; on any non-editor screen (settings / unlock / dialogs) the always-alive `App.vue` takes over, sets `pendingOpenPathsNavigate` and routes back to `/editor`, and `EditorView` consumes the intent at the **end** of `onMounted` — `_sessionRestore` is assigned by then, so the fetch stays serialized with cold-start restoration. If the startup drain already took the paths, the second drain just gets an empty array; harmless.
- **A note handed over the same way**: `marklock://sticky-open-note` (see §6.6) uses the identical "two listeners + store flag + consumed at the end of onMounted" shape; only the payload differs — the OS entry points send paths that must be buffered in Rust, while the note sends a ready-made `StickyNoteTarget`, and since the main window stays alive (hidden, never destroyed) no event can be lost to a cold start, so no buffer is needed.

### 6.11 PDF export (a hidden WebView rendering silently)

- **Why no third-party engine**: the PDF has to lay out exactly like the on-screen preview, and standing up a second renderer (wkhtmltopdf / Typst / hand-written print CSS) means maintaining a second stylesheet. So the chain is "reuse the HTML the preview already produced, and let the system WebView print it".
- **Self-contained is a hard requirement**: the render window's load source is a `data:text/html;base64,…` URL (macOS) or a temp file behind `file://` (Windows), and neither gives relative links a base URL to resolve against. PDF therefore runs with `inlineAll`, turning every image and attachment into a data URL while the HTML is assembled.
- **One window per export, with an incrementing label**: `pdf-export-<seq>` (`AtomicU64`). A fixed label collides with `webview already exists` — the previous window's `destroy` only takes effect as the event loop gets to it, so reusing the name before that completes fails. The window is `visible(false)` + `skip_taskbar(true)` sized at 794×1123 logical px (A4 at 96dpi) so fonts and images lay out at something close to print resolution.
- **Shape of the orchestration**: the command thread dispatches "build the window" to the main thread and then blocks on the channel with `recv_timeout(120s)`. `on_page_load` acts only on the first `Finished` (an `AtomicBool` swallows later loads), waits 500ms for images and fonts to settle, then `run_on_main_thread(capture_pdf)`. `Sender` is not `Sync` while `on_page_load` demands an `Fn + Send + Sync` closure, so it is wrapped in `Arc<Mutex<Option<Sender>>>` and taken out at capture time. Teardown (destroy by label + delete the temp file) lives in `export_pdf`, so none of the three paths — success, platform error, timeout — leaves a window behind. Platform callbacks never destroy their own host window: on Windows the completion callback is invoked by WebView2 itself, and tearing down the host from inside it risks use-after-free.
- **macOS has to paginate itself**: `WKWebView.createPDFWithConfiguration`'s `rect` is only a single-page crop and never produces multiple pages, so a naive capture yields one truncated page. The fix is an injected JS pass that moves `.md`'s direct children into `overflow:hidden` containers of a fixed page height (841.89px), starting a new container when one overflows and returning the page count; then each page is captured at `rect.y = i × page height` and PDFKit stitches the singles together with `insertPage_atIndex`. Breaks fall on **block boundaries**, so no half-line straddles a page, and each container inherits `.md`'s original padding, which keeps the margins identical on every page.
- **Windows leaves it to the engine**: `ICoreWebView2_16::PrintToPdfStream` with `ICoreWebView2Environment6::CreatePrintSettings` (A4 in inches, all margins 0, no header/footer, backgrounds printed — code-block fills and table striping are background paint, so without it the PDF comes out white). Chromium's print layout paginates, so no JS pass runs. When an old Runtime lacks the interfaces the cast fails and the error says "WebView2 runtime is too old" instead of silently producing a blank PDF. `wait_for_async_operation` is deliberately not used: this function already runs inside a main-thread callback, and nesting another `GetMessage` pump risks re-entrancy.
- **Layout sizes differ per platform**: mac's `createPDF` emits pt (A4 = 595×842pt) while WebView2 lays out in CSS px (A4 = 794×1123px at 96dpi). The frontend therefore writes `body { width }` and `font-size` per platform (595 / 11.25 vs 793 / 15) so both convert to the same pt, and the same document reads at the same size on either machine; `@page { margin: 0 }` hands the margins to `.md`'s padding — the same padding the mac pagination containers inherit.
- **Platform gap**: Linux's WebKitGTK has neither `createPDF` nor WebView2's print interface, so off mac/Windows `spawn_pdf_window` compiles to a stub that reports "this platform cannot export PDF" over the channel without creating a window. The command stays registered, and the four in-app menus hide the entry via `isLinux` (the macOS main menu is only installed on mac anyway).
- **Writing reuses `crypto::io::write`**: a PDF is a plaintext artifact, but the chosen destination may sit in a directory iCloud has kept online-only, so it goes through the same recall-and-atomic-write path rather than a second write implementation.

### 6.12 The global "show / hide main window" hotkey

- **Why a system-level hook is the only option**: when the main window is hidden into the tray (`hide()`) or completely covered by another app, the WebView receives no key events at all, so in-app shortcuts are exactly the thing that cannot work — and that is the moment you want a one-key call-back. This hotkey therefore goes through `tauri-plugin-global-shortcut`, delivered by the OS regardless of whether this window is frontmost.
- **No key is claimed by default**: `settings.showHideWindowShortcut` is an empty string, which means "not registered". The hook only exists after the user records a combination in Settings → Shortcuts → Global. Shipping an app that already owns a system combination would not be acceptable.
- **Registration shape**: `set_show_hide_shortcut(shortcut)` first unregisters the accelerator remembered in `SHOW_HIDE_ACCEL`, then registers the new one; an empty string only clears. What is stored is the **string**, not a Shortcut object, because rebinding has to be able to unregister the old value — otherwise the user ends up with two keys both toggling the window. A failed registration (malformed accelerator / already taken) returns a readable error and **leaves the stored value unchanged**, so the frontend rolls the setting back and re-sends the previous value, keeping "what the UI shows" and "what is actually live" the same key.
- **Two places push it, only one of them reports errors**: `App.vue` syncs it once at startup, inside the `!isStickyWindow()` branch — the sticky note is a second WebView, and if it registered too, the second registration of the same accelerator would fail with `HotKey already registered`. After that, user edits are sent only by the settings page, which needs immediate feedback. The cost: that startup call ends in `.catch(() => {})`, so the setting still displays a hotkey that never fires.
- **The toggle decision**: `toggle_main_window` treats "visible + not minimized + focused" as the only branch that hides; everything else (hidden / minimized / visible but sitting behind another app) is unminimized, shown and focused. Better to have one press merely bring it forward than to let the hotkey hide a window the user can barely see and can no longer reach.
- **The recording side**: while recording, a `keydown` listener is attached to `window` in the **capture phase** (ahead of the editor, so the combination is not typed into the document). Key names come only from `KeyboardEvent.code` mapped through a whitelist (letters / digits / F1–F24 / a set of common control keys), and modifiers are normalized to `CmdOrCtrl` + `Alt` + `Shift`. Anything other than an F key must carry at least one modifier, otherwise the box only warns and keeps waiting; Esc cancels, Backspace / Delete clears, and the listener is removed on unmount.
- **The plugin callback does not discriminate**: `.with_handler` calls `toggle_main_window` on `Pressed` for *any* registered shortcut. That holds only because exactly one shortcut is ever registered; a second global shortcut requires dispatching by accelerator first.
- **Platform gaps**: the Linux backend is x11rb and only speaks X11. With no X connection (a Wayland session) its event thread exits immediately, `register` then posts to a dead thread and **returns success** — the hotkey appears set but never fires, with no error anywhere. On macOS F1–F4 are brightness / keyboard-light keys unless "use F1, F2 as standard function keys" is turned on, so a bare F key needs that setting. Recording performs no conflict check either: the hook is system-wide, so binding `CmdOrCtrl+S` steals Save from **every** application and turns it into a window toggle.

### 6.13 In-preview find

- **It searches the rendered result, not the source**: global search searches "every file's source", CodeMirror's panel searches "this document's source", this one searches "the DOM this document rendered into" — only the last can point at the sentence the user actually sees while looking at the preview. The three pipelines differ, so `src/lib/previewSearch.ts` is its own module rather than a reuse of either.
- **One haystack, `\n` padded at block boundaries**: concatenating the body text nodes of `.md` in document order and running one regex over it is fast, but the tail of one paragraph plus the head of the next would form a hit that exists nowhere on screen. Every block-level element is therefore padded front and back with a `\n` that **maps to no text node**, and a match that cannot be mapped back in full is dropped whole. Inline tags (`strong` / `em` / `a` / `code`) get no padding, so a hit split by inline syntax is still found — it just becomes several `<mark class="pv-search">` in the same group, and navigation lights the whole group. Since the padding is a newline, the `m` flag makes `^` / `$` land on block boundaries, matching the editor's per-line semantics.
- **Subtrees that are not searched**: controls the app injects itself (the code-block copy button), form controls, scripts and styles, embedded media, and `SVG` — `<mark>` is not a valid SVG child element, so wrapping one would break the graphic.
- **The highlight is transient chrome, and both exits have to clean it**: re-injecting `v-html` (switching tabs, an external reload, an attachment-cache refresh) discards the marks with the old DOM, so the `renderedPreview` watcher and `commitPreview` re-run the search against the new DOM, and `pvSearchNav` first checks that the current mark is still `isConnected` (in-place editing may have deleted the matched text) before deciding whether to scroll or re-search. The other exit is the dangerous one: typing a single character triggers serialization, so `rawHtml()` in `wysiwyg.ts` strips `mark.pv-search` / `mark.pv-hit` by class name before reading `outerHTML` — otherwise "search, then edit a word" would write a `<mark>` into the source, and the same tag would end up in HTML / PDF exports. The inline path was already transparent to `MARK`, and a `<mark>` the user wrote themselves (without those two classes) is untouched.
- **The bar does not squeeze the layout**: the outer `.pv-search-row` is a zero-height row with `position:sticky; top:10px; height:0; pointer-events:none`, and the `.pv-search-bar` that actually takes clicks hangs inside it — it floats above the body without costing it a line of height or blocking clicks below. The initial item is the first hit below the top of the viewport (browser-find behaviour), wrapping to the first when everything has scrolled past; the active item is aligned to the top third of the viewport, and a hit inside a horizontally scrollable code block scrolls that block too.
- **The entry point branches on the mode**: `findAction` opens this bar in Preview mode, while Edit / Split still focus the editor and open CodeMirror's panel (with replace). The native `menu:find` item and ⌘G / ⇧⌘G (intercepted only while the bar is open) use the same test. The bar only requires `pvCanEdit` (pure preview + plaintext available), independent of `settings.previewInlineEdit`, so a read-only preview can still search. The macOS main-menu entry was renamed from "Find in Editor" to plain "Find" accordingly.
- **The note has none of this**: the sticky note window reuses `wysiwyg.ts` but not `previewSearch.ts`.

### 6.14 "Closing a vault" and "locking a vault" read as the same thing: unavailable

- **The two states differ in the data**: locking leaves the entry in `recent` with `unlocked=false` (registration intact, the Rust-side session wiped); closing removes the whole entry (close vault / clear recent), so even the vault's name and colour are unfindable — while the tab stays open with its `vaultId` still pointing at that id.
- **"No such registration" has to mean unreadable**: four places — `isActiveLocked` / `activeVaultUnlocked` / `vaultSessionAlive` / `isVaultFileLocked` — used to disagree about what a missing `recent.find` result means, and two of them let it through as "optimistically unlocked, the backend's `NotUnlocked` will catch it". It does not catch it politely: materialize walks the attachments of a session-less vault one by one, every one fails, and the result is reported as "N attachments could not be decrypted" — "the vault is closed" turned into "your attachments are corrupt". All four now read a missing registration as **unavailable**, and plaintext tabs are excluded first via `isVaultFile`, otherwise a plain `.md` would look locked simply because its `vaultId` is not in `recent`.
- **Masking now covers the title bar and search**: tabs and the sidebar have long shown a locked vault file as "vault file", but the window title's `activeFileName` read `f.name` directly, so closing a vault left the real filename on top of the window; it goes through `fileDisplayName` now. The "open tabs" leg of global search skips those tabs too — neither name nor content is offered. The vault name and colour fall back to the favourites snapshot and then to a name derived from the vault root path, so the masked label never degrades into a half-sentence like " file".
- **One way to rebuild a registration**: `store.ensureVaultRegistered(id)` — return the `recent` hit if there is one, otherwise re-register from the favourites snapshot, otherwise derive from the `vaultId` itself (a `.mdlb` suffix means single-file vault, everything else is a directory vault). Three call sites: the in-place unlock submit, `recentOpenVault` and `recentOpenFile`. The in-place one is mandatory: `unlockVault` answers "vault not found" for an unregistered id, so the most natural path — close the vault, then type the password in the tab — used to fail every time.
- **The side effect is worth stating**: `ensureVaultRegistered` really writes the entry back into `recent` and persists it; it is not a read-only fallback. Unlocking in place after closing therefore re-registers the vault, with a fresh colour taken from the `VAULT_COLORS` rotation (unless the favourites snapshot carried one), which need not match the colour it had before being closed.

---

## 7. Evolution timeline (starting 2026-09-24)

### Phase 1: Prototype and skeleton (09-24)

| Milestone | Content |
|---|---|
| **M1 Prototype sign-off** | Produced `doc/ui-prototype/` — four static high-fidelity screens (unlock/editor/settings/dialogs) aligned to AntD visuals; established the "VSCode-style multi-vault workspace" information architecture: five sidebar groups + top tabs + edit/split/preview |
| **M2 Frontend skeleton** | Vite + Vue 3 + Ant Design Vue initialization; the four static screens built to match the prototype (Options API) |
| **M3 Crypto core** | Rust `crypto/`'s six modules landed: `.mdl` read/write, Argon2id unlock, DEK wrap/re-wrap, auto-lock timing; 10+ unit tests passing |
| **M2.5 Feature wiring** | Frontend and backend connected, **vault = directory-encryption model** (`.mlkv` directory + vault.json, later renamed to the `.mdlb` directory); Pinia + localStorage; real features across all four screens |

### Phase 2: Single-file vault rebuild and feature completion (09-25)

| Theme | Content |
|---|---|
| Direct-to-main start + drag-to-attach | Launch goes straight to the main UI; dropping a directory attaches it as the working directory |
| Opening files | Plain files read/write as plaintext + encrypted `.mdl` files go through the encryption path |
| Sidebar toggles + starring | Group visibility toggles, starring vaults and files, lock semantics fixed (locking only clears encrypted content) |
| Drag-and-drop rework | Supports files/directories outside the working directory; `inspect_path` routing; `pendingOpen` auto-opens after unlock |
| Session restore | Working directory + open tabs restored across launches (encrypted files need re-unlocking) |
| Extension rename | `.mdk`→`.mdl`, `.mlkv`→`.mdlb` (pure rename, format unchanged) |
| New-draft flow | The "+" in the tab bar opens a blank draft; target + password are chosen only at save time; password requirement relaxed to 1+ characters |
| Single file drops the "vault" concept | `.mdl` is no longer a vault — unlock in place via a password box inside the tab; fixed the bug of wrongly jumping to the unlock screen after locking |
| Drag reordering | Fixed order + drag reordering for starred/open files/vaults (HTML5 failed → pure mouse events → handle removed, whole-row dragging) |
| Folders in encrypted vaults | Recursive `VaultTreeNode` component; create folders/files inside a vault |
| More-menu expansion | New file / new encrypted vault / copy to vault; creating a vault inline when saving into one |
| In-vault file management | Delete/rename/move (with path-traversal protection) |
| **Single-file container rebuild** | Encrypted vaults moved from "directory + vault.json" to a **single-file container `.mdlb`** (one DEK shared vault-wide, the whole tree encrypted as one) |
| Simplified unlock flow | In-place password unlock + minimal unlock screen + cancellable |

### Phase 3: UI polish and search (09-26)

| Theme | Content |
|---|---|
| In-place unlock refreshes the tree | The vault tree reloads after a successful unlock (previously only tab contents were restored) |
| Tooltip rework | Pure CSS → global `v-tip` directive (singleton, never clipped by scrolling, position/delay/privacy lock/relative time) |
| Shortcut | Cmd/Ctrl+W closes the current tab (not the window) |
| Title bar removed | macOS Overlay traffic-light placement; **root cause of broken dragging = missing `allow-start-dragging` ACL permission**; closing = quitting; cross-platform adaptation; pixel-level traffic-light calibration (y=20) |
| Settings page made real | Real bindings + unimplemented items marked "coming soon" |
| Drag reordering | Tabs + sidebar groups (`sectionOrder`) |
| Two toggles + menu | "Open files" split into two independent toggles; the More menu closes on outside click |
| Theme | Light / dark / follow system |
| **Global search** | Vault-content search → extended to plaintext files in the working directory → fixed Chinese snippet offsets → keyboard up/down selection → open tabs included → keyword highlighting, taller panel, path line removed |

---

## 8. Progress & To-do (current state)

### Done (former M4/M5 plan items)

- **M4 Editor**: CodeMirror 6 integrated (replacing the textarea — markdown language, syntax highlighting, theme); autosave timer pipeline (`autoSaveSecs` configurable)
- **M5 Polish**: keyboard shortcut system (tab switching / reopen closed tab, etc., with a dedicated shortcuts section in Settings); packaged distribution (GitHub Actions: `release.yml` + `release-win-offline.yml` workflows); atomic writes + rolling backups (`.bak` series, "keep previous version on save" toggle) — the data foundation for "restore previous version"

### Remaining to-do

- A **frontend restore entry** for backup files (currently the Rust side only rolls `.bak` files to disk; there is no one-click "restore previous version" UI yet)
- **PDF export on Linux** (WebKitGTK has no silent print API — either drive it through CDP / `Page.printToPDF` or bring in an external engine; for now the entry point is simply not offered)

### Known tech debt

- The full AntD import produces oversized chunks (>500KB) — on-demand imports / manualChunks optimization possible
- The single-file `.mdl` session has `dek=None`, so Rust's `search_vaults` misclassifies the vault type when it tests `dek.is_some()` (unlocked `.mdl` currently relies on the frontend tab-hit fallback; unfixed on the Rust side)
- macOS Overlay windows can't be dragged while unfocused (Tauri issue #4316)
- **Attachment encryption only covers `.mdl` / `.mdlb`**: images in directory vaults (the folder model) and in ordinary plaintext files still land as plaintext in `.dat` (the discriminator is `isEncryptedAssetFile`). Wiring up directory vaults means deciding together the preview base directory, the scope of re-wrapping on password change, and whether "plaintext images inside a vault" is acceptable.
- **The sticky note doesn't use the attachment cache**: its preview base is the note's own directory on disk, so `.mdat` attachments don't render in a note whose source is a `.mdl` (the session lives in the same process, so adding a materialize step is feasible — not done).
- **Attachment plaintext sits in the app cache while unlocked**: `<app_cache_dir>/mlk-assets` is itself unencrypted and readable by other processes under the same user; it is cleared on lock / quit / auto-lock, and force-kill residue is handled by the startup wipe.
- **The directory vault's password-change rule**: entries inside a `.dat` directory and stray files with non-text extensions are skipped as attachments; a file whose name looks like a note (text extension, or no extension) but isn't a container blocks the whole password change — refusing is now safe (compute first, write after, the vault stays untouched) — but a plaintext `.json` left in the vault still blocks it.
- **Search doesn't trigger iCloud recall**: `search` still uses bare `fs::read` on the main thread, so an undownloaded file simply isn't readable and only the filename matches. Adding recall means moving it to a thread pool with a short per-file timeout, otherwise one search can wait 20 seconds.
- **Launch arguments are documented from the code, not from testing**: macOS `open -n … --args` depends on the target app doing its own single-instance forwarding; Windows goes through `explorer <path> <args…>`, and `explorer` has its own `/e`, `/root` semantics, so whether trailing arguments actually arrive is still unverified (if they don't, switch to `cmd /c start "" <path> <args>`); Linux's `xdg-open` has no pass-through entry — arguments vanish without any UI hint.
- **macOS always starts a new process when arguments are present**: argument-free launches keep the old behavior, but with arguments `-n` is unconditional and has no toggle — it is the only way `open` delivers argv. Apps that don't support multi-instance will pile up.
- **The sticky launcher bar doesn't show which item carries arguments**: only the hover title appends `path args`, while Settings has a primary-outlined "Args" button — the two surfaces aren't equivalent.
- **Double-clicking a file yanks the user back to the editor**: the trade-off buys "the file opens promptly", at the cost of whatever form state was in progress, such as an unsaved skin draft.
- **PDF pagination can never match page-for-page across platforms**: macOS cuts pages with an injected JS packer at block boundaries, Windows takes WebView2's own print layout, so the same document breaks differently on the two. Matching them exactly would mean one pagination strategy everywhere (run the script on Windows too, or switch mac to CSS paged media); accepted for now.
- **macOS pagination only breaks between blocks, never inside one**: a single block taller than a page (very long code block, long table, tall image) gets its overflow clipped by the page container's `overflow:hidden`. Heights are measured once, 500ms after `Finished`, so images still decoding measure too short and the result can be too few pages with the last one truncated.
- **A failed page capture on macOS is quiet**: a nil `createPDF` callback only logs one `eprintln!` line and is skipped during merging, so the artifact simply has one page fewer instead of raising an error. Louder would be better; this is the current behavior.
- **Self-contained HTML gets large for image-heavy documents**: PDF and single-file HTML inline every attachment as base64 (a 4/3 size increase); macOS travels as a very long `data:` URL and Windows as a temp file (long data URLs are rejected by WebView2). Extreme image density can still hit WebView URL-length or memory limits, failing as a 120-second timeout or a blank PDF — there is no pre-flight check.
- **No progress indication while exporting**: the backend can wait up to 120 seconds and the frontend only reports at the end; the menu has already closed, so nothing tells the user work is happening.
- **The Windows PDF path cannot be compiled locally**: `cargo check --target x86_64-pc-windows-msvc` dies in the `tauri-winres` build script (it wants `llvm-rc`), so the first real compile happens in CI. The interface chain (`ICoreWebView2_16` / `Environment6`) and the `IStream` read-back are likewise unverified at runtime until CI and a real machine confirm them.
- **A failed startup registration is silent**: the single `App.vue` sync ends in `.catch(() => {})`, so when the combination is already taken or the platform can't do it, Settings still displays the hotkey and the user only sees "the key does nothing". Either surface that failure once after startup, or show a "registered / not registered" state in the settings row.
- **The global hotkey has no conflict checking at all**: it is not compared against in-app shortcuts, nor against what the rest of the system already owns. Recording something as common as `CmdOrCtrl+S` claims it globally, and on Wayland the registration "succeeds" while doing nothing (see §6.12).
- **The plugin callback is written on the assumption that one shortcut exists**: any registered global shortcut's `Pressed` toggles the main window. Adding a second one (say "lock everything now") must come with accelerator dispatch, otherwise the new feature silently hijacks window toggling.
- **`toggle_main_window` uses `is_focused()` as "frontmost"**: with the main window visible but covered by another app, one press brings it forward instead of hiding it. That matches "make it reachable first", but it is not exactly what the word "toggle" promises — and no focus API distinguishes "visible and frontmost".
- **Recording accepts F1–F24, the warning text says F1–F12**: the `codeToAccelKey` whitelist and the `shortcutNeedMod` message disagree (beyond F12 there is no physical key on most Macs, yet the code allows F24).
- **In-preview find rescans everything on every keystroke, with no debounce**: each `input` rebuilds the haystack, re-runs the regex and re-wraps every `<mark>`. Unnoticeable on ordinary documents; a few hundred thousand characters or a code-heavy page will stutter while typing. Adding a debounce first requires deciding whether the stale highlights and hit count "lag behind" the input.
- **The regex mode takes user input directly and can wedge the webview**: catastrophic backtracking (`(a+)+$` and friends) has no timeout escape in main-thread JavaScript — the only way out is killing the window. Either cap haystack length / matching steps, or drop the regex mode.
- **"The highlight never reaches the source" rests on a class-name convention**: `rawHtml()` in `wysiwyg.ts` strips exactly two classes, `mark.pv-search` / `mark.pv-hit`. Any future globally injected marker (jump highlight, annotations, revisions) must be added to that selector too; miss one and `<mark>` tags land in the user's document.
- **⌘F on a locked tab in Preview mode does nothing**: `openPvSearch` is gated on `pvCanEdit` (plaintext required) and simply returns when that fails, without explaining why.
- **The sticky note has no find**: a small window arguably needs "find in this note" most, yet today you can only switch to Edit and scroll.
- **Unlocking in place after closing a vault re-registers it, possibly in another colour**: the derivation branch of `ensureVaultRegistered` takes its colour from the `VAULT_COLORS` rotation, which need not match what the entry had before it was closed (a favourited vault carries its colour in the snapshot and is unaffected). Preserving the original colour would mean keeping a snapshot when the registration is removed; nothing does that today.

---

## 9. Testing

Rust side, `cargo test --lib`, covering:

- Key round-trip, wrong password, tamper detection
- Re-wrap on password change
- Session locking and expiry reclamation
- Full directory-vault flow, folders inside a vault
- Full single-file-vault flow (add/delete/edit/move + password change + single file on disk)
- Path-traversal protection
- Plaintext-directory search (Chinese hits, case-insensitivity, skipping hidden files/ciphertext)
- Encrypted attachments: encrypt/decrypt round-trip and tamper detection, the write → decrypt-into-cache → clear-cache lifecycle, compatibility copying of legacy plaintext attachments
- Attachment re-wrap following a password change (the old key can no longer open them, non-container entries are skipped), a broken attachment being skipped on its own while the rest stays usable, and no re-decryption when the cache is already newer than the source
- A directory vault containing plaintext attachments (an image under `.dat` plus a stray png) still changes password successfully with the attachment bytes untouched; slipping in a file that "looks like a note but isn't a container" makes the change refuse, and the old password still unlocks and reads the body (proving no half-new/half-old state was written)

> PDF export has no automated test: it depends on a real WebView rendering and the OS print interface, which `cargo test --lib` cannot reach. After touching it, verify by hand on all three platforms, covering at least a text-only document, a document with images, and a very long code block (the last is what exposes page clipping).

> The global "show / hide main window" hotkey cannot be tested either — it needs a key event from outside the process. On real hardware, cover three states: the main window hidden in the tray, the main window fully covered by another app, the main window frontmost — one press each, confirming the first two call it back and the third hides it. Then rebind once and confirm the **old key stops working immediately** (if it still fires, the unregister step never ran).

> In-preview find is manual too: it reads the DOM the browser rendered, which `cargo test --lib` cannot reach. Cover at least five cases — ① a phrase formed by the end of one paragraph plus the start of the next must **not** hit; ② a hit split by `**bold**` or inline code lights the whole group and counts as one for ⌘G; ③ with in-place editing on, search then change a word and save — no `<mark>` may appear in the source, nor in an exported HTML; ④ highlights are still there after an external-change reload (re-injection re-runs the search); ⑤ Esc works from both focus positions — inside the input (it calls `stopPropagation`, so it does not also close another layer) and back in the body (the window-level fallback) — closing the bar and restoring the text each time.

---

## 10. Extension points

- **Add a Tauri command**: register the new function in `src-tauri/src/lib.rs` and add it to `invoke_handler!`; add the matching frontend wrapper in `src/lib/tauri.ts`.
- **Swap or extend crypto algorithms**: define a new KDF or cipher identifier in `crypto/format.rs`; implement the new derivation/encryption logic in `crypto/keys.rs`, keeping `Zeroize` semantics.
- **Add a vault form**: add a new vault-type branch in `crypto/vault.rs` (e.g. a cloud-synced vault, a multi-master-password vault).
- **Extend the frontend**: add routes and views in `src/router/index.ts`; extend state and actions in `src/stores/vault.ts` — holding to the "never touch keys" principle.

---

## 11. Troubleshooting

| Symptom | Diagnosis | Resolution |
|------|------|------|
| Unlock fails (`WrongPassword`) | KEK fails to unwrap the DEK, or the probe check fails | Verify the password; see `CryptoError::WrongPassword` |
| Header parse fails (`BadHeader`) | `.mdl` / `.mdlb` file corrupted or format mismatch | Check `header.format`, `cipher`, `kdf.algo` |
| Session expired (`NotUnlocked`) | Auto-lock timed out and `SessionStore` reclaimed the session | `reap_expired` returns the reclaimed ids — refresh the UI and unlock again |
| "Path escapes the vault" | A relative path contains `..`, or it's an absolute path | The frontend only passes vault-relative paths; the backend already rejects violations |
| No images render but bodies are fine | Attachments were never decrypted into the cache (lost session / a materialize failure swallowed by a silent catch) | Unlock the vault again; confirm the tab's cache root is non-empty |
| "N attachments could not be decrypted" | Some `.mdat` under `.dat` are corrupted, or weren't encrypted with the current master password | The rest still display; the broken ones need re-importing |
| Changing the master password reports a format/header error | The vault root or a subfolder holds a file whose name looks like a note but isn't an `mlk/1` container (plaintext `.json`, extensionless files, etc.) | Move it out of the vault or give it a non-text extension; plaintext attachments inside `.dat` are already skipped |
| Reading a file fails with error 11 (`Resource deadlock avoided`) on mac | An iCloud "not downloaded here" file is being recalled | Wait for the spinner to finish (20-second ceiling); if it keeps failing, right-click the file in Finder → "Download Now" |
| Clicking an argument-carrying launcher item does nothing | The target app doesn't read argv (on macOS the fresh `-n` instance lacks single-instance forwarding; on Windows `explorer` may swallow the trailing arguments) | First confirm the item launches at all without arguments, then adjust to the app's own argument syntax |
| Double-clicking a `.md` jumps to the editor but opens nothing (Linux) | Linux has no "open file" entry point (mac uses `RunEvent::Opened`, Windows uses argv plus the single-instance plugin) | Use in-app "Open file" for now; a Linux entry point is still a to-do |
| Export PDF reports "the WebView2 runtime is too old" | `PrintToPdfStream` needs `ICoreWebView2_16` (roughly Edge 115+) and `CreatePrintSettings` needs `Environment6` (roughly 105+); a failed cast means an old Runtime | Update the WebView2 Runtime (comes with Edge / OS updates) and retry; export single-file HTML meanwhile |
| Export PDF hangs and then reports "timed out or failed" | The command thread waits at most 120 seconds; a self-contained HTML that grew too large may never finish rendering in the hidden window | Drop some images or try "Export HTML (single file)"; if it still fails, check stderr for `[PDF-CAPTURE]` |
| The exported PDF is missing pages or content is cut off | macOS packs blocks into page-high containers, so a block taller than one page loses its overflow; a page whose capture failed is only logged to stderr and skipped at merge time | Split long code blocks / tables; for very tall images, use layout that breaks into smaller blocks |
| "Export PDF" is not in the menu on Linux | WebKitGTK has no silent print API, and `export_pdf` on that platform only returns "this platform cannot export PDF" | Intentional (see §6.11 and the §8 to-do); use "Export HTML (single file)" and print from the browser |
| A global hotkey was set but pressing it does nothing | The combination is already owned by another app and the registration failed during the startup sync (swallowed by `.catch`); or the session is Wayland, where x11rb has no X connection and registration "succeeds" | Re-record an unused combination in Settings → Shortcuts → Global (a failure now reports itself and rolls back); pick a letter with a modifier; on Wayland, switch to an X11 / XWayland session |
| One press brings the window forward instead of hiding it | Only "visible + not minimized + focused" hides; when another app covers the main window `is_focused()` is false | By design (reachability first) — press again and it hides; see §6.12 |
| Clicking the same tray button twice makes the note disappear | The button bound to "open the sticky note" is now a toggle: while the note is visible the next click hides it (flushing unsaved text first) | Not broken — click it again to reopen (see §6.6) |
| ⌘F in Preview misses a phrase that is clearly on screen | Find runs over the rendered DOM: a "match" spanning blocks (paragraph tail + next paragraph head) is dropped, `SVG` and app-injected controls are not searched, and a locked tab has no plaintext to search | Search a fragment that stays inside one paragraph, or switch to Edit / Split and use the editor's panel (which has replace); see §6.13 |
| Closing a vault turns its tabs into "vault file" and asks for a password in the body | Closing removes the whole entry from `recent`, and the front end treats that exactly like locked — which also means the filename stays hidden | Type the password right in the tab (it re-registers the vault on the way), or reopen the vault from the sidebar / recent list; see §6.14 |
