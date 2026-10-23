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
| `session.rs` | `SessionStore` (global `Mutex<HashMap>` singleton); `UnlockedVault` holds kek/kdf/dek + last_activity + auto_lock; auto-lock timing and expired-session reclamation |
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
│   ├── fonts.ts             # Built-in preview font catalog (cross-platform fallback stacks)
│   ├── skins.ts             # Skin registry: built-in list, token groups, custom-skin CSS generation/injection/live preview
│   ├── locales/             # zh/en language packs + switching
│   ├── sticky.ts            # Sticky-window detection + note body read/write by file type
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

Keys and plaintext never enter the store.

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
- **HTML export**: `buildStandaloneHtml` reads runtime variables through `cssVar()`, so exports follow the active skin.

### 6.4 Search

- Scope: unlocked vaults (encrypted content matched after in-memory decryption) + plaintext files in the working directory + open tabs (in-memory content).
- Matching: character-level, case-insensitive substring; at most 3 hit snippets per file, 100 overall cap.
- Interaction: 300ms debounce, keyboard up/down selection (wrapping), Enter to open, Esc to close, keyword highlighting, deduplicated results.

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

### Known tech debt

- The full AntD import produces oversized chunks (>500KB) — on-demand imports / manualChunks optimization possible
- The single-file `.mdl` session has `dek=None`, so Rust's `search_vaults` misclassifies the vault type when it tests `dek.is_some()` (unlocked `.mdl` currently relies on the frontend tab-hit fallback; unfixed on the Rust side)
- macOS Overlay windows can't be dragged while unfocused (Tauri issue #4316)

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
