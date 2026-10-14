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
├── App.vue                  # Theme resolution (light/dark/system + matchMedia follows the system)
├── router/index.ts          # Hash-based four-screen routing
├── styles/global.css        # Global styles (CSS variables, incl. dark-theme variables)
├── components/
│   ├── TitleBar.vue         # Title bar (macOS traffic-light overlay + title)
│   └── VaultTreeNode.vue    # Recursive vault-tree node (folder expand/collapse + hover create/rename/delete)
├── stores/vault.ts          # Pinia store (sessions / open files / recent / starred / settings)
├── lib/
│   ├── tauri.ts             # invoke bridge for Rust commands + file dialog wrappers
│   ├── filewatch.ts         # External-modification watcher (file-stamp poller)
│   └── tooltip.ts           # Global v-tip directive (singleton bubble, never clipped by scrolling)
└── views/
    ├── UnlockView.vue       # Unlock (minimal: target vault + password + unlock/cancel)
    ├── EditorView.vue       # Main editing UI (the core)
    ├── SettingsView.vue     # Settings
    └── DialogsView.vue      # Dialog collection (new vault / change password / new encrypted file)
```

> The frontend uses the **Options API** throughout — no `<script setup>` (a hard user convention).

### 4.2 Main interface (EditorView) information architecture

- Five sidebar groups (drag-reorderable, each individually toggleable): **Open Files → Open Vaults → Starred → Recent → Working Directory**
- Top tab bar (drag-reorderable, independent toggle)
- Editing modes: edit / split / preview (a hand-written lightweight Markdown renderer)
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
| `search(query, workdirs?)` | Global search (vaults + plaintext files in the working directory) |
| `file_stamps(paths)` | Batch-query file mtime + size (external-change monitoring) |

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
["core:default", "core:window:allow-start-dragging", "notification:default", "autostart:default", "dialog:default"]
```

> Key point: `core:window:allow-start-dragging` is the prerequisite for window dragging (`core:default` doesn't include it — missing it makes dragging fail silently).

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

### 6.3 Theme

- Three settings: `light` / `dark` / `system`; `system` follows the OS via `matchMedia('(prefers-color-scheme: dark)')`.
- Dark-theme variables override under `html[data-theme='dark']` in `global.css`; hardcoded light colors in components were replaced with CSS variables.

### 6.4 Search

- Scope: unlocked vaults (encrypted content matched after in-memory decryption) + plaintext files in the working directory + open tabs (in-memory content).
- Matching: character-level, case-insensitive substring; at most 3 hit snippets per file, 100 overall cap.
- Interaction: 300ms debounce, keyboard up/down selection (wrapping), Enter to open, Esc to close, keyword highlighting, deduplicated results.

### 6.5 Auto-lock

- Rust's `SessionStore` tracks activity timestamps; on timeout (5 minutes by default) `reap_expired` zeroizes the keys; the frontend polls every 30 seconds.

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

## 8. To-do (current state)

- **M4 Editor**: CodeMirror integration (replacing the current textarea), syntax highlighting, autosave debounce pipeline
- **M5 Polish**: history snapshots, keyboard shortcut system, packaged distribution
- Known tech debt:
  - `vue-tsc --noEmit` carries legacy type errors (`unlockedVaults` getter, DragDropEvent, etc. — doesn't affect vite build)
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
